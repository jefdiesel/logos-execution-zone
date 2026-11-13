#!/usr/bin/env python3
"""Offline tests for lez-rpc-proxy.py: a fake sequencer on loopback, the proxy in front of it,
and the cases test-loopback.sh runs against a real chain. Stdlib only.

  python3 testnet/proxy/test_proxy.py        (or: python3 -m unittest discover testnet/proxy)
"""
import http.client
import importlib.util
import json
import os
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("lez_rpc_proxy", os.path.join(HERE, "lez-rpc-proxy.py"))
proxy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proxy)


class FakeSequencer(BaseHTTPRequestHandler):
    """Answers every JSON-RPC call with {"result": <method name>} and records what it received."""
    seen = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        FakeSequencer.seen.append(body)
        out = json.dumps({"jsonrpc": "2.0", "id": body.get("id"), "result": body.get("method")}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


def serve(server):
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    return server


class ProxyTest(unittest.TestCase):
    def setUp(self, **overrides):
        self.upstream = serve(ThreadingHTTPServer(("127.0.0.1", 0), FakeSequencer))
        cfg = dict(proxy.DEFAULTS)
        cfg.update(overrides)
        self.srv = serve(proxy.Server(("127.0.0.1", 0), proxy.Handler, cfg,
                                      ("127.0.0.1", self.upstream.server_address[1]), None,
                                      getattr(self, "allowed", None)))
        self.port = self.srv.server_address[1]
        FakeSequencer.seen = []

    def tearDown(self):
        self.srv.shutdown()
        self.srv.server_close()
        self.upstream.shutdown()
        self.upstream.server_close()

    def post(self, body, headers=None, method="POST", path="/"):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        h = {"Content-Type": "application/json"}
        h.update(headers or {})
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        try:
            conn.request(method, path, data if method == "POST" else None, h)
            r = conn.getresponse()
        except (BrokenPipeError, ConnectionResetError):
            # The proxy refused before reading the whole body (a size cap) and closed the socket.
            conn.close()
            return None, b""
        out = r.read()
        conn.close()
        return r.status, out

    def call(self, method, params=None, rid=1, **kw):
        return self.post({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or []}, **kw)

    def test_allowed_method_is_forwarded_and_reserialised(self):
        status, out = self.post({"jsonrpc": "2.0", "id": 7, "method": "getLastBlockId", "params": [], "extra": "x"})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(out)["result"], "getLastBlockId")
        self.assertEqual(FakeSequencer.seen[-1], {"jsonrpc": "2.0", "id": 7, "method": "getLastBlockId", "params": []})

    def test_every_allowed_method_passes(self):
        for m in sorted(proxy.ALLOWED - {"getBlockRange"}):
            self.assertEqual(self.call(m)[0], 200, m)

    def test_operator_and_unknown_methods_refused(self):
        for m in ("requeueCrossZoneDeadLetter", "getCrossZoneDeadLetters", "rpc.discover", "admin_x"):
            self.assertEqual(self.call(m)[0], 403, m)
        self.assertEqual(FakeSequencer.seen, [])

    def test_batch_refused(self):
        status, _ = self.post([{"jsonrpc": "2.0", "id": 1, "method": "getLastBlockId", "params": []}])
        self.assertEqual(status, 400)

    def test_block_range_span(self):
        self.assertEqual(self.call("getBlockRange", [0, 99])[0], 200)
        self.assertEqual(self.call("getBlockRange", [0, 100])[0], 400)
        self.assertEqual(self.call("getBlockRange", [5, 4])[0], 400)
        self.assertEqual(self.call("getBlockRange", {"start_block_id": 1, "end_block_id": 2})[0], 200)

    def test_methods_paths_and_types(self):
        self.assertEqual(self.post(b"", method="GET")[0], 405)
        self.assertEqual(self.post({"jsonrpc": "2.0", "id": 1, "method": "getLastBlockId"}, path="/x")[0], 404)
        self.assertEqual(self.call("getLastBlockId", headers={"Content-Type": "text/plain"})[0], 415)
        self.assertEqual(self.call("getLastBlockId", headers={"Upgrade": "websocket"})[0], 400)
        self.assertEqual(self.post(b"{not json")[0], 400)
        self.assertEqual(self.post(b"[" * 100000 + b"]" * 100000)[0], 400)   # deep nesting
        self.assertEqual(self.call("getLastBlockId", rid=True)[0], 400)

    def test_body_caps(self):
        big_read = {"jsonrpc": "2.0", "id": 1, "method": "getAccount", "params": ["A" * 20000]}
        self.assertIn(self.post(big_read)[0], (413, None))
        big_send = {"jsonrpc": "2.0", "id": 1, "method": "sendTransaction", "params": ["A" * 600000]}
        self.assertEqual(self.post(big_send)[0], 200)
        huge = {"jsonrpc": "2.0", "id": 1, "method": "sendTransaction", "params": ["A" * 800000]}
        self.assertIn(self.post(huge)[0], (413, None))

    def test_healthz_only_without_client_header(self):
        self.assertEqual(self.post(b"", method="GET", path="/healthz")[0], 200)
        self.assertEqual(self.post(b"", method="GET", path="/healthz", headers={"CF-Connecting-IP": "203.0.113.9"})[0], 405)

    def test_per_client_rate_limit(self):
        statuses = [self.call("getLastBlockId", headers={"CF-Connecting-IP": "198.51.100.7"})[0] for _ in range(80)]
        self.assertIn(429, statuses)
        self.assertEqual(statuses[0], 200)
        # another client is unaffected
        self.assertEqual(self.call("getLastBlockId", headers={"CF-Connecting-IP": "198.51.100.8"})[0], 200)

    def test_send_bucket(self):
        statuses = [self.call("sendTransaction", ["AAAA"], headers={"CF-Connecting-IP": "198.51.100.9"})[0] for _ in range(8)]
        self.assertEqual(statuses[:4], [200] * 4)
        self.assertIn(429, statuses[4:])


class AllowMethodTest(ProxyTest):
    allowed = proxy.ALLOWED | {"getCrossZoneDeadLetters"}

    def test_operator_and_unknown_methods_refused(self):
        self.assertEqual(self.call("getCrossZoneDeadLetters")[0], 200)
        self.assertEqual(self.call("requeueCrossZoneDeadLetter")[0], 403)


class ListenTest(unittest.TestCase):
    def test_refuses_non_loopback(self):
        with self.assertRaises(Exception):
            proxy.loopback_addr("0.0.0.0:3045")
        self.assertEqual(proxy.loopback_addr("127.0.0.1:3045"), ("127.0.0.1", 3045))


if __name__ == "__main__":
    unittest.main(verbosity=2)
