#!/usr/bin/env python3
"""lez-rpc-proxy: a loopback-only JSON-RPC filter in front of a LEZ 0.4 sequencer.

Why it exists: the sequencer's RPC (jsonrpsee 0.26, `lez/sequencer/actors/rpc_server`) has no
caller auth, no CORS, no rate limit, accepts JSON-RPC batches, serves WebSocket on the same port,
takes 10 MiB bodies, and exposes `requeueCrossZoneDeadLetter` (a state-changing operator call,
"anyone who can reach the RPC can requeue"). A tunnel or reverse proxy forwards public traffic here,
never to the sequencer.

What it does, per request:
  - only `POST /` with `Content-Type: application/json` and a `Content-Length` (no chunked bodies,
    no Upgrade, no GET/OPTIONS except the loopback `GET /healthz`);
  - one JSON-RPC 2.0 object per request (arrays/batches refused), method on the allowlist,
    `getBlockRange` span at most MAX_RANGE blocks (the stock wallet asks for 100);
  - body caps: SEND_MAX for `sendTransaction`, READ_MAX for everything else;
  - per-client token buckets keyed on the client-address header your front end sets
    (--client-ip-header, default `CF-Connecting-IP`, which Cloudflare sets; trust it only when
    nothing but that front end can reach this port), a tighter bucket for
    `sendTransaction`, a global bucket, a cap on concurrent upstream calls and on open
    connections; over a limit it answers at once (HTTP 429/503 + JSON-RPC error), never queues;
  - forwards a re-serialised request (only jsonrpc/id/method/params) to the upstream sequencer
    with a timeout, and returns the upstream body as is (capped at RESP_MAX).

No third-party modules: any python3 >= 3.7 runs it (the macOS system /usr/bin/python3 included).

  lez-rpc-proxy.py [--listen 127.0.0.1:3045] [--upstream 127.0.0.1:3040] [--log-requests FILE]
                   [--client-ip-header NAME|none] [--allow-method NAME ...] [--set LIMIT=VALUE ...]

It refuses to listen on anything but a loopback address.
"""

import argparse
import http.client
import ipaddress
import json
import logging
import logging.handlers
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Methods a wallet, an app's CLI, a status snapshot or a block scanner uses: every method of
# lez/sequencer/service/rpc/src/lib.rs (v0.4.0-rc1 to rc3) except the two operator calls below.
# Add one with --allow-method NAME.
ALLOWED = {
    "sendTransaction",
    "getFeeState",
    "checkHealth",
    "getBlock",
    "getBlockRange",
    "getLastBlockId",
    "getAccountBalance",
    "getTransaction",
    "getAccountsNonces",
    "getAccountView",
    "getPublications",
    "getMessagePath",
    "getRecoveryBinding",
    "getProofsAndRoot",
    "getAccount",
    "getProgramIds",
    "getChannelId",
}
# Named here so the refusal is deliberate and testable: operator-only (no auth upstream).
DENIED = {"getCrossZoneDeadLetters", "requeueCrossZoneDeadLetter"}

DEFAULTS = dict(
    SEND_MAX=768 * 1024,  # a ~256 KB private transaction is ~342 KB as base64 JSON
    READ_MAX=16 * 1024,
    RESP_MAX=12 * 1024 * 1024,  # the upstream caps responses at 10 MiB itself
    MAX_RANGE=100,
    MAX_CONN=96,  # open client connections (threads)
    MAX_INFLIGHT=12,  # concurrent upstream calls
    UPSTREAM_TIMEOUT=30.0,
    IDLE_TIMEOUT=75.0,  # keep-alive idle; longer than cloudflared's keepAliveTimeout (30 s)
    # token buckets: (rate per second, burst)
    IP_ALL=(5.0, 60),  # matches the edge rule (50 per 10 s); the dry run used 10/s, burst 150
    IP_SEND=(0.2, 4),
    IP_RANGE=(4.0, 60),
    GLOBAL=(60.0, 300),
    MAX_CLIENTS=20000,  # per-IP table size before it is pruned
)


class Bucket:
    __slots__ = ("rate", "burst", "tokens", "stamp")

    def __init__(self, rate, burst):
        self.rate, self.burst, self.tokens, self.stamp = rate, burst, float(burst), time.monotonic()

    def take(self, n=1.0):
        now = time.monotonic()
        self.tokens = min(self.burst, self.tokens + (now - self.stamp) * self.rate)
        self.stamp = now
        if self.tokens >= n:
            self.tokens -= n
            return True
        return False


class Limits:
    def __init__(self, cfg):
        self.cfg = cfg
        self.lock = threading.Lock()
        self.clients = {}
        self.global_bucket = Bucket(*cfg["GLOBAL"])
        self.inflight = threading.BoundedSemaphore(cfg["MAX_INFLIGHT"])
        self.counters = {}

    def count(self, key):
        with self.lock:
            self.counters[key] = self.counters.get(key, 0) + 1

    def allow(self, ip, method=None):
        """method None: the per-client and global buckets, checked before the body is read.
        A method name: only that method's own bucket, checked after the parse."""
        cfg = self.cfg
        with self.lock:
            entry = self.clients.get(ip)
            if entry is None:
                if len(self.clients) >= cfg["MAX_CLIENTS"]:
                    # Drop idle entries (full buckets); if still full, refuse new clients.
                    now = time.monotonic()
                    for k in [k for k, v in self.clients.items() if now - v["all"].stamp > 60]:
                        del self.clients[k]
                    if len(self.clients) >= cfg["MAX_CLIENTS"]:
                        return "busy"
                entry = {
                    "all": Bucket(*cfg["IP_ALL"]),
                    "send": Bucket(*cfg["IP_SEND"]),
                    "range": Bucket(*cfg["IP_RANGE"]),
                }
                self.clients[ip] = entry
            if method is None:
                if not entry["all"].take():
                    return "client"
                if not self.global_bucket.take():
                    return "busy"
            elif method == "sendTransaction" and not entry["send"].take():
                return "client"
            elif method == "getBlockRange" and not entry["range"].take():
                return "client"
        return None


def rpc_error(code, message, rid=None):
    return json.dumps({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}}).encode()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 64

    def __init__(self, addr, handler, cfg, upstream, log, allowed=None, client_ip_header="CF-Connecting-IP"):
        super().__init__(addr, handler)
        self.cfg, self.upstream, self.log = cfg, upstream, log
        self.allowed = set(ALLOWED if allowed is None else allowed)
        self.client_ip_header = client_ip_header
        self.limits = Limits(cfg)
        self.open_conns = 0
        self.conn_lock = threading.Lock()
        self.health = (0.0, None)

    def process_request(self, request, client_address):
        # Shed before spawning a thread when too many connections are open.
        with self.conn_lock:
            if self.open_conns >= self.cfg["MAX_CONN"]:
                self.limits.count("shed_conn")
                try:
                    request.sendall(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                except OSError:
                    pass
                self.shutdown_request(request)
                return
            self.open_conns += 1
        super().process_request(request, client_address)

    def handle_error(self, request, client_address):
        # Count, never print: a traceback per bad request would fill the disk.
        self.limits.count("handler_error")

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            with self.conn_lock:
                self.open_conns -= 1


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "lez-rpc-proxy"
    sys_version = ""

    def setup(self):
        self.timeout = self.server.cfg["IDLE_TIMEOUT"]
        super().setup()

    def log_message(self, fmt, *args):  # silence the default stderr access log
        pass

    def client_ip(self):
        name = self.server.client_ip_header
        ip = self.headers.get(name, "").strip() if name else ""
        if ip:
            try:
                return str(ipaddress.ip_address(ip))
            except ValueError:
                pass
        return self.client_address[0]

    def reply(self, status, body, method="-", started=None, extra=None):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        if status >= 400 and status not in (429,):
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.server.limits.count("%s %d" % (method, status))
        if self.server.log is not None:
            ms = (time.monotonic() - started) * 1000 if started else 0
            self.server.log.info("%s %s %d %d %.0f", self.client_ip(), method, status, len(body), ms)

    def refuse_other(self):
        self.reply(405, rpc_error(-32600, "only POST / with a JSON-RPC body"))

    do_PUT = do_DELETE = do_PATCH = do_OPTIONS = do_HEAD = refuse_other

    def do_GET(self):
        # Loopback health for the local monitor; the public ingress must never route this path,
        # and a request carrying the client-address header (so: from the front end) is refused.
        name = self.server.client_ip_header
        if self.path == "/healthz" and (not name or self.headers.get(name) is None):
            stamp, cached = self.server.health
            if time.monotonic() - stamp > 5 or cached is None:
                ok, height = False, None
                try:
                    height = json.loads(self.upstream_call(b'{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}')[1])["result"]
                    ok = True
                except Exception:  # noqa: BLE001
                    pass
                cached = json.dumps({"ok": ok, "height": height, "counters": self.server.limits.counters}).encode()
                self.server.health = (time.monotonic(), cached)
            return self.reply(200, cached, "healthz")
        return self.refuse_other()

    def upstream_call(self, body):
        host, port = self.server.upstream
        conn = http.client.HTTPConnection(host, port, timeout=self.server.cfg["UPSTREAM_TIMEOUT"])
        try:
            conn.request("POST", "/", body=body, headers={"Content-Type": "application/json"})
            resp = conn.getresponse()
            data = resp.read(self.server.cfg["RESP_MAX"] + 1)
            if len(data) > self.server.cfg["RESP_MAX"]:
                raise ValueError("upstream response too large")
            return resp.status, data
        finally:
            conn.close()

    def do_POST(self):
        started = time.monotonic()
        cfg = self.server.cfg
        if self.path != "/":
            return self.reply(404, rpc_error(-32600, "not found"), started=started)
        if self.headers.get("Upgrade") or self.headers.get("Transfer-Encoding"):
            return self.reply(400, rpc_error(-32600, "no upgrades or chunked bodies"), started=started)
        if not self.headers.get("Content-Type", "").lower().startswith("application/json"):
            return self.reply(415, rpc_error(-32600, "Content-Type must be application/json"), started=started)
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError:
            return self.reply(411, rpc_error(-32600, "Content-Length required"), started=started)
        if length < 0 or length > max(cfg["SEND_MAX"], cfg["READ_MAX"]):
            return self.reply(413, rpc_error(-32600, "request too large"), started=started)
        # Per-client and global limits before the body is read, so a refused flood costs no read
        # or parse.
        verdict = self.server.limits.allow(self.client_ip())
        if verdict is not None:
            self.close_connection = True  # the unread body must not be parsed as the next request
            if verdict == "client":
                return self.reply(429, rpc_error(-32005, "rate limited; slow down"), started=started, extra={"Retry-After": "10", "Connection": "close"})
            return self.reply(503, rpc_error(-32005, "server busy; retry later"), started=started, extra={"Retry-After": "5"})
        raw = self.rfile.read(length)
        try:
            req = json.loads(raw)
        except Exception:  # noqa: BLE001  ValueError, RecursionError (deep nesting), MemoryError
            return self.reply(400, rpc_error(-32700, "parse error"), started=started)
        if not isinstance(req, dict):
            return self.reply(400, rpc_error(-32600, "batches are not served; send one request"), started=started)
        rid, method, params = req.get("id"), req.get("method"), req.get("params", [])
        if not isinstance(rid, (int, str)) or isinstance(rid, bool) or (isinstance(rid, str) and len(rid) > 64):
            return self.reply(400, rpc_error(-32600, "id must be a number or a short string"), started=started)
        if not isinstance(method, str) or method not in self.server.allowed:
            label = method if isinstance(method, str) and method in DENIED else "other"
            return self.reply(403, rpc_error(-32601, "method not served here", rid), label, started)
        if length > (cfg["SEND_MAX"] if method == "sendTransaction" else cfg["READ_MAX"]):
            return self.reply(413, rpc_error(-32600, "request too large for " + method, rid), method, started)
        if not isinstance(params, (list, dict)):
            return self.reply(400, rpc_error(-32602, "params must be a list or object", rid), method, started)
        if method == "getBlockRange":
            try:
                a, b = (params[0], params[1]) if isinstance(params, list) else (params["start_block_id"], params["end_block_id"])
                if not (isinstance(a, int) and isinstance(b, int) and 0 <= a <= b and b - a + 1 <= cfg["MAX_RANGE"]):
                    raise ValueError
            except (ValueError, KeyError, IndexError, TypeError):
                return self.reply(400, rpc_error(-32602, "getBlockRange spans at most %d blocks" % cfg["MAX_RANGE"], rid), method, started)
        if self.server.limits.allow(self.client_ip(), method) == "client":
            return self.reply(429, rpc_error(-32005, "rate limited; slow down", rid), method, started, {"Retry-After": "10"})
        if not self.server.limits.inflight.acquire(blocking=False):
            return self.reply(503, rpc_error(-32005, "server busy; retry later", rid), method, started, {"Retry-After": "5"})
        try:
            body = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}, separators=(",", ":")).encode()
            status, data = self.upstream_call(body)
        except (OSError, ValueError, http.client.HTTPException):
            return self.reply(502, rpc_error(-32603, "sequencer unavailable", rid), method, started)
        finally:
            self.server.limits.inflight.release()
        return self.reply(200 if status == 200 else 502, data, method, started)


def loopback_addr(text):
    host, _, port = text.rpartition(":")
    host = host.strip("[]")
    if not ipaddress.ip_address(host).is_loopback:
        raise argparse.ArgumentTypeError(text + " is not a loopback address")
    return host, int(port)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--listen", type=loopback_addr, default=("127.0.0.1", 3045))
    p.add_argument("--upstream", type=loopback_addr, default=("127.0.0.1", 3040))
    p.add_argument("--client-ip-header", default="CF-Connecting-IP",
                   help="header holding the real client address, set by your tunnel or reverse proxy "
                        "(X-Real-IP for nginx/Caddy setups); 'none' keys limits on the TCP peer")
    p.add_argument("--allow-method", action="append", default=[], metavar="NAME",
                   help="serve this JSON-RPC method too (repeatable)")
    p.add_argument("--log-requests", help="append one line per request (ip method status bytes ms) to this file")
    p.add_argument("--set", action="append", default=[], metavar="NAME=VALUE",
                   help="override a limit, e.g. --set IP_ALL=10,150 --set SEND_MAX=786432")
    args = p.parse_args(argv)
    cfg = dict(DEFAULTS)
    for item in args.set:
        name, _, value = item.partition("=")
        if name not in cfg:
            p.error("unknown limit " + name)
        cfg[name] = tuple(float(x) for x in value.split(",")) if isinstance(cfg[name], tuple) else type(cfg[name])(value)
    log = None
    if args.log_requests:
        log = logging.getLogger("requests")
        h = logging.handlers.RotatingFileHandler(args.log_requests, maxBytes=20 * 1024 * 1024, backupCount=3)
        h.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
        log.addHandler(h)
        log.setLevel(logging.INFO)
    socket.setdefaulttimeout(None)
    allowed = set(ALLOWED) | set(args.allow_method)
    for name in sorted(set(args.allow_method) & DENIED):
        print("WARNING: serving operator-only method %s to the public" % name, file=sys.stderr, flush=True)
    header = None if args.client_ip_header.lower() == "none" else args.client_ip_header
    srv = Server(args.listen, Handler, cfg, args.upstream, log, allowed, header)
    print("lez-rpc-proxy on %s:%d -> %s:%d" % (args.listen + args.upstream), flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
