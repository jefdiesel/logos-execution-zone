#!/usr/bin/env python3
"""Offline tests for faucet.py: a fake sequencer (getAccountBalance) and a fake wallet binary
that "sends" by appending to a ledger the fake sequencer reads. Stdlib only.

  python3 testnet/faucet/test_faucet.py
"""
import http.client
import importlib.util
import json
import os
import stat
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
TMP = tempfile.mkdtemp(prefix="faucet-test-")
LEDGER = os.path.join(TMP, "ledger")
FAUCET_ACCOUNT = "FaucetAccount1111111111111111111111111111111"
PK = "11" * 32
PK2 = "22" * 32

# A fake `wallet auth-transfer send --from F --to-pk PK --amount N`.
WALLET = os.path.join(TMP, "wallet")
with open(WALLET, "w") as f:
    f.write("#!%s\nimport sys\na = sys.argv\n"
            "pk = a[a.index('--to-pk') + 1]; amount = a[a.index('--amount') + 1]\n"
            "open(%r, 'a').write(pk + ' ' + amount + '\\n')\n"
            "print('Transaction hash is ' + 'ab' * 32, flush=True)\n" % (sys.executable, LEDGER))
os.chmod(WALLET, os.stat(WALLET).st_mode | stat.S_IEXEC)


class FakeSequencer(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        FakeSequencer.agents.append(self.headers.get("User-Agent", ""))
        account = req["params"][0]
        if account == FAUCET_ACCOUNT:
            result = 10 ** 15
        else:
            result = 0
            if os.path.exists(LEDGER):
                for line in open(LEDGER):
                    pk, amount = line.split()
                    if faucet.account_of(pk) == account:
                        result += int(amount)
        out = json.dumps({"jsonrpc": "2.0", "id": 1, "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


FakeSequencer.agents = []
seq = ThreadingHTTPServer(("127.0.0.1", 0), FakeSequencer)
threading.Thread(target=seq.serve_forever, daemon=True).start()

os.environ.update(FAUCET_WALLET_BIN=WALLET, FAUCET_WALLET=TMP, FAUCET_ACCOUNT="Public/" + FAUCET_ACCOUNT,
                  FAUCET_RPC="http://127.0.0.1:%d" % seq.server_address[1],
                  FAUCET_STATE=os.path.join(TMP, "state.json"), FAUCET_DRIP="1000",
                  FAUCET_CONFIRM_SLEEP="0.05", FAUCET_PER_ADDRESS="1", FAUCET_PER_DAY="3")
spec = importlib.util.spec_from_file_location("faucet", os.path.join(HERE, "faucet.py"))
faucet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(faucet)
srv = ThreadingHTTPServer(("127.0.0.1", 0), faucet.Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()


def post(body, ip="198.51.100.1"):
    conn = http.client.HTTPConnection("127.0.0.1", srv.server_address[1], timeout=10)
    conn.request("POST", "/faucet", json.dumps(body), {"Content-Type": "application/json", "CF-Connecting-IP": ip})
    r = conn.getresponse()
    out = json.loads(r.read())
    conn.close()
    return r.status, out


def wait_idle():
    for _ in range(200):
        if faucet.sending.acquire(blocking=False):
            faucet.sending.release()
            return
        time.sleep(0.02)
    raise AssertionError("send never finished")


class FaucetTest(unittest.TestCase):
    def test_account_id_formula(self):
        # A key and the account id the stock rc3 wallet printed for it (EXAMPLE-RUN.txt, part 3).
        self.assertEqual(faucet.account_of("e5effde50afeb5857a7fcf35a5cc544ebb89a183355ccf3c3a7b55e675cdfecc"),
                         "6qHvPH6cLv3nVzG4EMg58jWUbNx2GaWYGmH8CAbBhefd")

    def test_flow(self):
        status, out = post({"public_key": "xyz"})
        self.assertEqual(status, 400)
        status, out = post({"account": "abc"})
        self.assertEqual(status, 400)
        status, out = post({"public_key": PK})
        self.assertEqual(status, 200, out)
        self.assertEqual(out["account"], "Public/" + faucet.account_of(PK))
        self.assertEqual(out["tx"], "ab" * 32)
        wait_idle()
        self.assertEqual(faucet.balance(faucet.account_of(PK)), 1000)
        # the same account again, from another address: refused
        status, out = post({"public_key": PK}, ip="198.51.100.2")
        self.assertEqual(status, 429)
        # another account from the same address: refused (one per address per day)
        status, out = post({"public_key": PK2})
        self.assertEqual(status, 429)
        # another account from another address: sent
        status, out = post({"public_key": PK2}, ip="198.51.100.3")
        self.assertEqual(status, 200, out)
        wait_idle()
        self.assertTrue(all(a.startswith("lez-testnet-faucet") for a in FakeSequencer.agents))
        state = json.load(open(os.environ["FAUCET_STATE"]))
        self.assertEqual(len(state["sends"]), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
