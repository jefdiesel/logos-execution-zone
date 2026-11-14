# Letting other people use your chain

By default the kit binds everything to `127.0.0.1`. This page is about opening the RPC to other
people without getting hurt. It is the setup of a LEZ 0.4 chain that has served the public this
way, with the names and addresses taken out.

## Read this first: what a kit chain is

- **One sequencer, yours.** It orders every transaction. It can censor, reorder or rewrite
  history, and nobody else checks it.
- **No anchoring.** The `standalone` build mocks the Logos Blockchain: it always says it's this
  node's turn, accepts every block publish at once, and returns a zero last-irreversible block.
  There is no data availability and no finality beyond your own database.
- **What is checked:** private transactions are verified against LEZ's privacy circuit by your
  sequencer. Public execution and the ledger as a whole are checked by nobody else.
- **A reset wipes everything:** balances, programs and history.
- **The default genesis key is public.** A chain started with `--genesis-key default` funds
  accounts whose private key is in the LEZ repository. Never expose one. Use
  `--genesis-key new` (the default) or `--genesis-pubkey`.

Say all of this on whatever page tells people your URL.

## The path

```
client --https--> TLS front end (tunnel or reverse proxy; rate limit, method/path rule)
       --http 127.0.0.1--> lez-rpc-proxy.py (filter) --http 127.0.0.1--> sequencer_service
```

Nothing listens on a public interface. Don't use `up.sh --public-bind` for this; it exists for a
private network behind a firewall.

## 1. The filter: `proxy/lez-rpc-proxy.py`

The sequencer's RPC has no caller auth, no CORS, no rate limit, accepts batches, serves WebSocket
on the same port, takes 10 MiB bodies, and exposes `requeueCrossZoneDeadLetter`, a
state-changing operator call with no auth. The filter sits in front of it. Python 3.7+, no
third-party modules.

```sh
testnet/proxy/start.sh                     # 127.0.0.1:<port+5> -> 127.0.0.1:<port>; down.sh stops it
python3 testnet/proxy/test_proxy.py        # offline tests (a fake sequencer)
testnet/proxy/test-live.sh                 # against your running chain, through the filter
```

What it lets through:

- only `POST /` with `Content-Type: application/json` and a `Content-Length`; no chunked bodies,
  no `Upgrade`, no GET (except `GET /healthz` from loopback without the client header);
- one JSON-RPC object per request, or a batch of at most 20 (`--max-batch N`; `--max-batch 0`
  refuses every batch with 400). Each call in a batch is checked exactly as a single request
  would be, and one refused call refuses the whole batch (nothing is forwarded); an empty batch,
  a nested array or more than N calls gets 400. The whole batch is one body and must fit the body
  cap below (16 KiB unless it carries a `sendTransaction`). A batch of N counts as N requests in
  the per-client bucket, so batching never multiplies a client's rate. Tools that list or search
  (for example `lezreg list`) use batches;
- methods on the allowlist only (403 otherwise). The default list is every method of the
  sequencer's RPC except the two operator calls: `sendTransaction`, `getFeeState`,
  `checkHealth`, `getBlock`, `getBlockRange`, `getLastBlockId`, `getAccountBalance`,
  `getTransaction`, `getAccountsNonces`, `getAccountView`, `getPublications`, `getMessagePath`,
  `getRecoveryBinding`, `getProofsAndRoot`, `getAccount`, `getProgramIds`, `getChannelId`.
  That is what the stock wallet, program deploys and private transactions use. Refused:
  `getCrossZoneDeadLetters`, `requeueCrossZoneDeadLetter`, anything else.
- `getBlockRange` spans of at most 100 blocks (the wallet asks for 100);
- bodies up to 768 KiB for `sendTransaction` (a private transaction is about 340 KB of JSON; a
  96 KiB program segment fits) and 16 KiB for everything else.

It re-serialises `{jsonrpc, id, method, params}` before forwarding, answers 429/503 at once over
a limit (never queues), and never logs per request unless `--log-requests FILE` is given.

**Limits** (per client, keyed on the client-address header your front end sets):
all methods 5/s with a burst of 60 (matches a 50-per-10-s edge rule); `sendTransaction` 1 per 5 s,
burst 4; `getBlockRange` 4/s, burst 60; global 60/s, burst 300; 12 concurrent upstream calls;
96 open connections. Change any with `--set NAME=VALUE` (e.g. `--set IP_ALL=10,150`).

**Client address header.** `--client-ip-header` (default `CF-Connecting-IP`, set by Cloudflare).
Use `X-Real-IP` behind nginx or Caddy configured to set it, or `none` to key on the TCP peer
(then every client behind one front end shares a bucket). Trust the header only because nothing
but your front end can reach the filter's loopback port.

**Adding methods.** `testnet/proxy/start.sh --allow-method NAME` (repeatable). A future LEZ
release may add RPC methods your clients need; compare `lez/sequencer/service/rpc/src/lib.rs`
with the list above when you upgrade. Naming an operator method prints a warning.

**Measured** (on the chain this is based on, through the filter): a public transfer is 11
requests; a 5-segment program deploy 75 requests over 84 s, at most about 20 in one second.

## 2. TLS and the front end

Pick one:

- **A tunnel** (no open inbound port; the machine dials out). `cloudflared/config.example.yml`
  routes only `POST /` to the filter. With Cloudflare, add a WAF custom rule that blocks
  everything but HTTPS JSON `POST /` without a query or `Upgrade`, before the request enters the
  tunnel:
  ```
  http.host eq "rpc.example.org" and (not ssl or http.request.method ne "POST"
    or http.request.uri.path ne "/" or len(http.request.uri.query) > 0
    or not any(lower(http.request.headers["content-type"][*]) contains "application/json")
    or len(http.request.headers["upgrade"]) > 0)
  ```
  plus one rate-limit rule: 50 requests per 10 s per IP, block 10 s. Turn Bot Fight Mode off for
  the zone or it challenges the wallet. Neither the tunnel nor these rules cost anything; no
  Worker is involved. Cloudflare answers Python-urllib's default `User-Agent` with 403, so any
  Python client you write must send its own `User-Agent` (the faucet does).
- **A reverse proxy with TLS** (Caddy, nginx) on a host with ports 80/443 open, forwarding only
  `POST /` to `127.0.0.1:<port+5>` and setting `X-Real-IP`; start the filter with
  `--client-ip-header X-Real-IP`, and rate-limit there too.

Check from another machine: `POST /` with `getLastBlockId` gives a height; `GET /` is refused; a
batch of two allowed calls gives two results and a batch of 21 gives 400; `getCrossZoneDeadLetters`
gives 403.

## 3. What not to expose

- the sequencer's RPC port itself (only the filter may reach it);
- the metrics port (`<port+1>`, always loopback; it is Prometheus text about your node);
- `GET /healthz` of the filter (counters; loopback only, and the tunnel config doesn't route it);
- the faucet's port directly, the operator wallet, `<home>/chain/home` (it holds the channel
  signing key), anything under `<home>/wallets`.

## 4. A faucet: `faucet/faucet.py`

Users need native tokens for fees. A zero-balance account is refused at admission, and the
payer must hold the fee *reserve* (gas limit x execution base fee + bytes x storage base fee),
not just the fee. On rc3 a transaction that reverts is charged its whole gas limit.

The faucet takes a **public key**, not an account id: on LEZ 0.4 only `--to-pk` can credit
someone else's account that has never been used. A transfer `--to Public/<id>` of such an account
reverts ("declared public but not admitted"): the rc3 wallet refuses to send it, and forced
through it is charged its whole gas limit.

```sh
testnet/faucet/start.sh        # first time: creates <home>/wallets/faucet and prints the funding command
testnet/faucet/start.sh        # again, once funded: listens on 127.0.0.1:<port+6>
curl -s -X POST http://127.0.0.1:3046/faucet -H 'Content-Type: application/json' \
  -d '{"public_key":"<64 hex digits from: wallet account get --keys --account-id Public/<id>>"}'
python3 testnet/faucet/test_faucet.py      # offline tests (fake wallet and sequencer)
```

Rules: one send per account per day, `FAUCET_PER_ADDRESS` (1) per client address per day,
`FAUCET_PER_DAY` (50) in total, one send in flight; it pays from a wallet of its own so the
operator key never sits behind a public endpoint; a send is confirmed by reading the recipient's
balance, and one that never arrives is forgotten so the user may ask again. Route `POST /faucet`
to it from your front end (a separate path or host) with the same client-address header; it
listens on loopback only.

## 5. Keeping it up

- `sequencer/watch.sh` (every 5 minutes): stops the tunnel and the chain when the disk is nearly
  full, restarts a crashed or frozen chain once an hour, and stops the tunnel if that doesn't
  help, so the public sees "down" rather than a stall. `sequencer/rotate-logs.sh` hourly.
- `service/` has a systemd unit and a launchd plist for the sequencer. Both raise the open-file
  limit (launchd's default soft limit is 256), restart only on failure, and wait 60 s between
  restarts.
- **Back up** the home before any upgrade: stop the chain (`down.sh`), copy `<home>` (on macOS
  `cp -cR` makes an instant clone), start again. The keys to keep are `<home>/wallets` and
  `<home>/chain/home/channel_signing_key`. A restore must reuse the same home.

## 6. Risk list

| Risk | What happens | What limits it |
|---|---|---|
| You are the only sequencer | you can censor, reorder, rewrite | say so; nothing else |
| No anchoring (standalone) | no DA, no finality | say so; a future LEZ testnet anchors |
| Spam of valid transactions | mempool fills, disk grows | fees; mempool 200; filter buckets; edge rate limit |
| Program deploys | state grows, and every later block writes more | deploy fees (about 24 units per byte at base fees 8); watch the disk |
| Disk full | sequencer panics and stops | `watch.sh` stops it before that; the WAL patch (PATCHES.md) keeps RocksDB small |
| Open-file limit | RocksDB opens fail, compaction stops, crash loop | the WAL patch caps open files at 200; the service files raise the limit |
| Reset | every balance and program gone | announce it; keep the old home (`reset.sh --keep-old`) |
| Default genesis key | anyone can spend those funds | never expose a `--genesis-key default` chain |
| Faucet drained | users can't pay fees | per-account, per-address and daily caps; its own wallet |
