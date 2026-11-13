#!/usr/bin/env python3
"""A small rate-limited faucet for a kit chain. Stdlib only.

POST /faucet {"public_key": "<64 hex digits>"} sends DRIP native units from the faucet account to
that key's public account. LEZ 0.4 admits a credit to a never-used account only with its public
key as evidence (`auth-transfer send --to-pk`); a transfer `--to` such an account reverts (the
wallet refuses to send it). So the faucet asks for the key, not the account id.

Limits: each account once a day, each client address PER_ADDRESS times a day, PER_DAY sends in
total, one send in flight at a time (a signer gets one transaction per block). A send is
confirmed by reading the recipient's balance: one that never arrives is forgotten, so the
account may ask again. GET /faucet describes the terms.

It listens on loopback only. Put it behind the same tunnel or reverse proxy as the RPC filter,
which must set the client-address header (FAUCET_CLIENT_IP_HEADER, default CF-Connecting-IP).

Environment (testnet/faucet/start.sh fills these from the kit home):
  FAUCET_WALLET_BIN   the kit's wallet binary
  FAUCET_WALLET       wallet home (LEE_WALLET_HOME_DIR) holding the faucet account's key;
                      use a wallet of its own, funded from the operator, not the operator's
  FAUCET_ACCOUNT      the faucet's public account id (base58, without "Public/")
  FAUCET_RPC          the sequencer, e.g. http://127.0.0.1:3040 (loopback)
  FAUCET_LISTEN       127.0.0.1:3046
  FAUCET_STATE        state file (default ./faucet-state.json)
  FAUCET_DRIP         units per send (default 100000000)
  FAUCET_PER_DAY      sends per day in total (default 50)
  FAUCET_PER_ADDRESS  sends per client address per day (default 1)
"""
import hashlib
import ipaddress
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WALLET_BIN = os.environ.get("FAUCET_WALLET_BIN", "wallet")
WALLET = os.environ.get("FAUCET_WALLET", "")
FAUCET = os.environ.get("FAUCET_ACCOUNT", "").strip()
FAUCET = FAUCET[len("Public/"):] if FAUCET.startswith("Public/") else FAUCET
RPC = os.environ.get("FAUCET_RPC", "http://127.0.0.1:3040")
LISTEN = os.environ.get("FAUCET_LISTEN", "127.0.0.1:3046")
STATE = os.environ.get("FAUCET_STATE", "faucet-state.json")
DRIP = int(os.environ.get("FAUCET_DRIP", 100_000_000))
# The faucet's own transfer must leave room for its fee reserve (gas limit x base fee + bytes x
# storage fee), or it is refused at admission. A public transfer costs well under this.
RESERVE = int(os.environ.get("FAUCET_RESERVE", 10_000_000))
PER_ADDRESS = int(os.environ.get("FAUCET_PER_ADDRESS", 1))
PER_DAY = int(os.environ.get("FAUCET_PER_DAY", 50))
CLIENT_IP_HEADER = os.environ.get("FAUCET_CLIENT_IP_HEADER", "CF-Connecting-IP")
CONFIRM_TRIES = int(os.environ.get("FAUCET_CONFIRM_TRIES", 60))
CONFIRM_SLEEP = float(os.environ.get("FAUCET_CONFIRM_SLEEP", 5))
DAY = 86400
PUBKEY = re.compile(r"^[0-9a-f]{64}$")
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
# Some edges (Cloudflare among them) answer Python-urllib's default User-Agent with 403.
UA = {"User-Agent": "lez-testnet-faucet/1", "Content-Type": "application/json"}


def account_of(pk_hex):
    """A public account id: base58(SHA256("/LEE/v0.3/AccountId/Public/" | public key))."""
    d = hashlib.sha256(b"/LEE/v0.3/AccountId/Public/\0\0\0\0\0" + bytes.fromhex(pk_hex)).digest()
    n, out = int.from_bytes(d, "big"), ""
    while n:
        n, r = divmod(n, 58)
        out = B58[r] + out
    return "1" * (len(d) - len(d.lstrip(b"\0"))) + out


def balance(account):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getAccountBalance", "params": [account]}).encode()
    with urllib.request.urlopen(urllib.request.Request(RPC, body, UA), timeout=20) as response:
        return int(json.load(response)["result"])


lock = threading.Lock()  # guards the state file
sending = threading.Lock()  # one transfer in flight at a time


def load():
    try:
        with open(STATE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {"sends": []}


def save(state):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


def forget(record):
    with lock:
        state = load()
        state["sends"] = [s for s in state["sends"] if s != record]
        save(state)


def confirm(process, record, before):
    """Waits until the tokens arrive: a reverted or lost send shows up as no balance change."""
    try:
        try:
            process.communicate(timeout=600)
        except subprocess.TimeoutExpired:
            process.kill()
        for _ in range(CONFIRM_TRIES):
            try:
                if balance(record["account"]) >= before + DRIP:
                    return
            except (OSError, ValueError, KeyError):
                pass
            time.sleep(CONFIRM_SLEEP)
        forget(record)
        print("send to %s never arrived" % record["account"], flush=True)
    finally:
        sending.release()


class Handler(BaseHTTPRequestHandler):
    server_version = "lez-testnet-faucet"
    sys_version = ""

    def log_message(self, fmt, *args):
        pass

    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def client(self):
        ip = self.headers.get(CLIENT_IP_HEADER, "").strip() if CLIENT_IP_HEADER else ""
        try:
            return str(ipaddress.ip_address(ip))
        except ValueError:
            return self.client_address[0]

    def do_GET(self):
        if self.path == "/faucet":
            return self.reply(200, {"amount": str(DRIP), "per_account": "once a day",
                                    "send": 'POST /faucet {"public_key": "<64 hex>"}'})
        self.reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/faucet":
            return self.reply(404, {"error": "not found"})
        try:
            length = min(int(self.headers.get("Content-Length", 0)), 4096)
            pk = json.loads(self.rfile.read(length))["public_key"].strip().lower()
        except (ValueError, KeyError, TypeError, AttributeError):
            return self.reply(400, {"error": 'send {"public_key": "<64 hex digits>"} (wallet account get --keys)'})
        if not PUBKEY.match(pk):
            return self.reply(400, {"error": "public_key must be 64 hex digits"})
        account = account_of(pk)
        address = self.client()
        if not sending.acquire(blocking=False):
            return self.reply(409, {"error": "the faucet is busy sending; try again in a minute"})
        handed_off = False  # once the confirming thread owns `sending`, it releases it
        try:
            with lock:
                now = time.time()
                state = load()
                sends = [s for s in state["sends"] if now - s["at"] < DAY]
                if any(s["account"] == account for s in sends):
                    return self.reply(429, {"error": "this account was funded in the last day"})
                if sum(s["address"] == address for s in sends) >= PER_ADDRESS:
                    return self.reply(429, {"error": "too many requests from your address today"})
                if len(sends) >= PER_DAY:
                    return self.reply(429, {"error": "the faucet's daily limit is spent; try tomorrow"})
                try:
                    before = balance(account)
                    funds = balance(FAUCET)
                except (OSError, ValueError, KeyError):
                    return self.reply(503, {"error": "cannot reach the chain; try again shortly"})
                if funds < DRIP + RESERVE:
                    return self.reply(503, {"error": "the faucet is empty until the operator refills it"})
                record = {"account": account, "address": address, "at": now}
                state["sends"] = sends + [record]
                save(state)
            env = dict(os.environ, LEE_WALLET_HOME_DIR=WALLET, LEE_WALLET_PASSWORD=os.environ.get("LEE_WALLET_PASSWORD", ""))
            process = subprocess.Popen(
                [WALLET_BIN, "auth-transfer", "send", "--from", "Public/" + FAUCET, "--to-pk", pk, "--amount", str(DRIP)],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, text=True)
            tx = None
            for line in process.stdout:
                found = re.search(r"Transaction hash is ([0-9a-f]{64})", line)
                if found:
                    tx = found.group(1)
                    break
            if not tx:
                process.wait()
                forget(record)
                return self.reply(502, {"error": "the transfer could not be sent; try again shortly"})
            # Answer now and confirm in the background: inclusion takes a block or two.
            threading.Thread(target=confirm, args=(process, record, before), daemon=True).start()
            handed_off = True
        finally:
            if not handed_off:
                sending.release()
        self.reply(200, {"account": "Public/" + account, "amount": str(DRIP), "tx": tx})


def main():
    host, _, port = LISTEN.rpartition(":")
    if not ipaddress.ip_address(host.strip("[]")).is_loopback:
        sys.exit("FAUCET_LISTEN must be a loopback address")
    if not FAUCET or not WALLET:
        sys.exit("set FAUCET_ACCOUNT and FAUCET_WALLET (testnet/faucet/start.sh does)")
    srv = ThreadingHTTPServer((host.strip("[]"), int(port)), Handler)
    print("faucet on %s, %d per send, from Public/%s" % (LISTEN, DRIP, FAUCET), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
