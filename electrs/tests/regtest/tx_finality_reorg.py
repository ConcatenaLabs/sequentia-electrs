#!/usr/bin/env python3
"""A transaction's finality in its status, and the index across anchor-driven reorgs.

A proof-of-stake Sequentia chain (a three-member committee, quorum two)
anchored to a parent chain that stands in for Bitcoin (a second regtest node,
as the node's own feature_pos_escaping_stall.py does). electrs runs under
run-electrs-supervised.sh, as it does in production.

1. A transaction paying its fee in the chain's policy asset and one paying it
   in an issued asset, in a certified block anchored to parent block P: each
   status carries the anchor (P, depth 1) and the certification (3 signers,
   quorum 2); each fee names its asset. Two parent blocks and one Sequentia
   block later the depth reads 3.
2. A block certified by fewer than the quorum (an escaping-stall block): the
   status of the transaction in it reads certified false, 1 signer.
3. The parent orphans P: the node disconnects every block anchored at or above
   it, and electrs must undo their rows (the address history loses the
   transactions, the statuses read unconfirmed, each transaction listed once,
   from the mempool); mined again, the rows are added again at the new block.
4. electrs is killed (SIGKILL) and, while it is down, the parent orphans the
   new anchor too. The supervisor restarts electrs on the same database, which
   must undo the stale rows and add the new ones without re-indexing from
   scratch.

Run from a Sequentia checkout's functional test framework:

    SEQUENTIA_REPO=<Sequentia checkout> ELECTRS=<electrs built with --features sequentia> \\
    python3 tests/regtest/tx_finality_reorg.py \\
        --configfile=<Sequentia checkout>/test/config.ini --tmpdir=<dir> [--nocleanup]

FINALITY_RECORD=<file> writes the API answers read at each step.
"""
import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, os.path.join(os.environ["SEQUENTIA_REPO"], "test", "functional"))
from test_framework.address import byte_to_base58  # noqa: E402
from test_framework.authproxy import JSONRPCException  # noqa: E402
from test_framework.key import ECKey  # noqa: E402
from test_framework.messages import COIN, COutPoint, CTransaction, CTxIn, CTxOut, CTxOutAsset  # noqa: E402
from test_framework.script import CScript, OP_TRUE  # noqa: E402
from test_framework.test_framework import BitcoinTestFramework  # noqa: E402
from test_framework.util import assert_equal, get_auth_cookie, get_datadir_path, p2p_port, rpc_port  # noqa: E402

SUPERVISOR = os.environ.get("SUPERVISOR") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "run-electrs-supervised.sh")
PARENT_BLOCK_SECONDS = 600
PARENT_WARMUP_BLOCKS = 12
FEE = 100_000


def make_staker():
    k = ECKey()
    k.generate(compressed=True)
    return byte_to_base58(k.get_bytes() + b'\x01', 239), k.get_pubkey().get_bytes().hex()


def free_port():
    # Below the ephemeral range, which outgoing sockets use.
    while True:
        p = 20000 + int.from_bytes(os.urandom(2), "big") % 10000
        s = socket.socket()
        try:
            s.bind(("127.0.0.1", p))
            return p
        except OSError:
            continue
        finally:
            s.close()


class TxFinalityReorg(BitcoinTestFramework):
    def set_test_params(self):
        self.setup_clean_chain = True
        self.num_nodes = 2
        self.stakers = [make_staker() for _ in range(3)]
        self.rec = {}

    def skip_test_if_missing_module(self):
        self.skip_if_no_wallet()

    def setup_network(self, split=False):
        self.nodes = []
        chain = "elementsregtest"
        self.add_nodes(1, [["-port=%d" % p2p_port(0), "-rpcport=%d" % rpc_port(0), "-validatepegin=0",
                            "-initialfreecoins=0", "-con_blocksubsidy=5000000000", "-anyonecanspendaremine=1",
                            "-signblockscript=51"]], chain=[chain])
        self.start_node(0)
        parent = self.nodes[0]
        self.parent_time = parent.getblockheader(parent.getblockhash(0))["time"]
        u, p = get_auth_cookie(get_datadir_path(self.options.tmpdir, 0), chain)
        self.add_nodes(1, [[
            "-port=%d" % p2p_port(1), "-rpcport=%d" % rpc_port(1), "-validatepegin=0",
            "-anyonecanspendaremine=1", "-signblockscript=51", "-par=1",
            "-con_pos=1", "-posvrf=1", "-posaggcommittee=1", "-poscommitteesize=3", "-posslotinterval=1",
            "-con_blocksubsidy=0", "-initialfreecoins=1000000000000", "-con_connect_genesis_outputs=1",
            "-acceptnonstdtxn=1", "-txindex=1",
            "-con_bitcoin_anchor=1", "-validateanchor=1", "-anchorpollinterval=1", "-anchorminconf=1",
            "-mainchainrpchost=127.0.0.1", "-mainchainrpcport=%d" % rpc_port(0),
            "-mainchainrpcuser=%s" % u, "-mainchainrpcpassword=%s" % p,
            "-parentgenesisblockhash=%s" % parent.getblockhash(0),
        ] + ["-staker=%s:1" % pub for _, pub in self.stakers]], chain=[chain])
        self.start_node(1)
        parent.createwallet(wallet_name="w", descriptors=True)

    # -- chain helpers ---------------------------------------------------
    def advance_parent(self, blocks):
        """Parent blocks one Bitcoin interval apart, so median-time-past moves."""
        parent = self.nodes[0]
        addr = parent.getnewaddress()
        for _ in range(blocks):
            self.parent_time += PARENT_BLOCK_SECONDS
            parent.setmocktime(self.parent_time)
            self.generatetoaddress(parent, 1, addr, sync_fun=self.no_op)

    def produce(self, signers=3):
        wifs = [w for w, _ in self.stakers]
        last = None
        for _ in range(30):
            try:
                return self.node.generateposblock(wifs[0], wifs[1:signers])
            except Exception as e:  # the slot has not opened yet
                last = e
                time.sleep(1.1)
        raise AssertionError("no block produced: %s" % last)

    def free_coin(self):
        genesis = self.node.getblock(self.node.getblockhash(0), 2)
        for tx in genesis["tx"]:
            for out in tx["vout"]:
                if out["scriptPubKey"]["hex"] == "51" and out.get("value", 0) > 0 and self.node.gettxout(tx["txid"], out["n"]):
                    return tx["txid"], out["n"], int(out["value"] * COIN), out["asset"]
        raise AssertionError("no free coin")

    # -- electrs ---------------------------------------------------------
    def api(self, path, raw=False):
        with urllib.request.urlopen(self.electrs_url + path, timeout=30) as r:
            body = r.read()
            self.last_cache_control = r.headers.get("Cache-Control")
            return body.decode() if raw else json.loads(body)

    def wait_for(self, what, cond, timeout=120):
        end = time.time() + timeout
        last = None
        while time.time() < end:
            try:
                v = cond()
                if v:
                    return v
            except Exception as e:
                last = e
            time.sleep(0.5)
        raise AssertionError("timed out waiting for %s (%s)" % (what, last))

    def wait_synced(self):
        tip = self.node.getbestblockhash()
        self.wait_for("electrs at the node's tip %s" % tip, lambda: self.api("/blocks/tip/hash", raw=True) == tip)
        # The mempool is read after the blocks; let it catch up too.
        mem = set(self.node.getrawmempool())
        self.wait_for("electrs' mempool", lambda: set(self.api("/mempool/txids")) == mem)

    def start_electrs(self):
        http, electrum, monitor = free_port(), free_port(), free_port()
        u, p = get_auth_cookie(self.node.datadir, "elementsregtest")
        self.electrs_url = "http://127.0.0.1:%d" % http
        self.db = os.path.join(self.options.tmpdir, "electrs-db")
        env = dict(os.environ, ELECTRS_BIN=os.environ["ELECTRS"], NETWORK="liquidregtest",
                   ELECTRS_ARGS="--parent-network regtest --monitoring-addr 127.0.0.1:%d" % monitor,
                   NODE_DIR=os.path.join(self.node.datadir, "elementsregtest"),
                   RPC_ADDR="127.0.0.1:%d" % rpc_port(1), COOKIE="%s:%s" % (u, p), DB_DIR=self.db,
                   HTTP_ADDR="127.0.0.1:%d" % http, ELECTRUM_ADDR="127.0.0.1:%d" % electrum, BACKOFF="2")
        self.electrs_log = os.path.join(self.options.tmpdir, "electrs.log")
        self.supervisor = subprocess.Popen(["bash", SUPERVISOR], env=env, stdout=open(self.electrs_log, "w"),
                                           stderr=subprocess.STDOUT, start_new_session=True)

    def electrs_pid(self):
        with open("/proc/%d/task/%d/children" % (self.supervisor.pid, self.supervisor.pid)) as f:
            kids = [int(k) for k in f.read().split()]
        for k in kids:
            with open("/proc/%d/comm" % k) as f:
                if f.read().strip() == "electrs":
                    return k
        return None

    def stop_electrs(self):
        os.killpg(self.supervisor.pid, signal.SIGTERM)
        self.supervisor.wait()

    def status(self, txid):
        return self.api("/tx/%s/status" % txid)

    def chain_txids(self, addr):
        return [t["txid"] for t in self.api("/address/%s/txs/chain" % addr)]

    # -- the test --------------------------------------------------------
    def run_test(self):
        parent = self.nodes[0]
        node = self.node = self.nodes[1]
        self.advance_parent(PARENT_WARMUP_BLOCKS)
        self.produce()

        self.log.info("Fund a wallet from the genesis free coin, issue an asset, accept it for fees")
        node.createwallet(wallet_name="w", descriptors=True)
        w = node.get_wallet_rpc("w")
        txid, n, amount, pol = self.free_coin()
        pol_out = CTxOutAsset(b"\x01" + bytes.fromhex(pol)[::-1])
        fund = CTransaction()
        fund.nVersion = 2
        fund.vin = [CTxIn(COutPoint(int(txid, 16), n))]
        fund.vout = [CTxOut(5000 * COIN, bytes.fromhex(w.getaddressinfo(w.getnewaddress())["scriptPubKey"]), pol_out),
                     CTxOut(amount - 5000 * COIN - FEE, CScript([OP_TRUE]), pol_out), CTxOut(FEE, b"", pol_out)]
        node.sendrawtransaction(fund.serialize().hex())
        self.produce()
        X = w.issueasset(assetamount=1000, tokenamount=0, blind=False, fee_asset=pol)["asset"]
        self.produce()
        rates = node.getfeeexchangerates()
        rates[X] = 100000000
        node.setfeeexchangerates(rates)

        dest = w.getnewaddress()
        t_pol = w.sendtoaddress(address=dest, amount=1, fee_asset_label=pol)
        t_x = w.sendtoaddress(address=dest, amount=2, assetlabel=X, fee_asset_label=X)
        self.advance_parent(1)
        P, P_h = parent.getbestblockhash(), parent.getblockcount()
        B = self.produce()["hash"]
        assert_equal(node.getblockheader(B)["anchorhash"], P)
        assert {t_pol, t_x} <= set(node.getblock(B)["tx"])
        hdr = node.getblockheader(B)
        self.log.info("block %s anchored to parent block %s (%d): poscertified %s, %d of quorum %d",
                      B, P, P_h, hdr["poscertified"], hdr["poscountersigs"], hdr["posquorum"])

        self.start_electrs()
        try:
            self.check(parent, node, w, X, pol, dest, t_pol, t_x, P, P_h, B)
        finally:
            self.stop_electrs()
            out = os.environ.get("FINALITY_RECORD")
            if out:
                with open(out, "w") as f:
                    json.dump(self.rec, f, indent=1)

    def mine_again(self, txid):
        """Into a block once more, if a reorganisation left it out."""
        bh = self.node.getrawtransaction(txid, True).get("blockhash")
        if bh and self.node.getblockheader(bh)["confirmations"] > 0:
            return
        for _ in range(5):
            if txid in self.node.getrawmempool():
                break
            try:
                self.node.sendrawtransaction(self.t_s_hex)
            except JSONRPCException as e:
                assert "non-final" in e.error["message"], e.error
                self.produce()  # its locktime is still ahead of the tip
        self.produce()

    def fee_of(self, txid):
        """The fee as the node's wallet sees it: the transaction's fee output."""
        tx = self.node.getrawtransaction(txid, True)
        fees = [(o["asset"], int(round(o["value"] * COIN))) for o in tx["vout"] if o["scriptPubKey"]["type"] == "fee"]
        assert_equal(len(fees), 1)
        return fees[0]

    def check(self, parent, node, w, X, pol, dest, t_pol, t_x, P, P_h, B):
        self.wait_synced()

        self.log.info("1. status: anchor and certification; fee: its asset")
        st = self.status(t_pol)
        self.rec["1_status"] = st
        assert_equal(self.last_cache_control, "public, max-age=10")
        assert_equal((st["confirmed"], st["block_hash"]), (True, B))
        assert_equal(st["bitcoin_anchor"], {"height": P_h, "hash": P, "depth": 1})
        assert_equal(st["certification"], {"certified": True, "signers": 3, "quorum": 2})
        for txid, asset in ((t_pol, pol), (t_x, X)):
            tx = self.api("/tx/%s" % txid)
            self.rec["1_tx_" + txid[:8]] = {"fee": tx["fee"], "fee_asset": tx.get("fee_asset"), "status": tx["status"]}
            assert_equal((tx["fee_asset"], tx["fee"]), self.fee_of(txid))
            assert_equal(tx["fee_asset"], asset)
            assert "fees" not in tx
            assert_equal(tx["status"], st)
        listing = self.api("/address/%s/txs" % dest)
        assert_equal(sorted(t["txid"] for t in listing), sorted([t_pol, t_x]))
        assert all(t["status"] == st for t in listing)
        blocktxs = self.api("/block/%s/txs" % B)
        assert all(t["status"]["certification"]["certified"] for t in blocktxs)
        bs = self.api("/block/%s/status" % B)
        self.rec["1_block_status"] = bs
        assert_equal((bs["in_best_chain"], bs["bitcoin_anchor"], bs["certification"]),
                     (True, st["bitcoin_anchor"], st["certification"]))
        self.log.info("   %s: %s", t_pol, json.dumps(st))

        self.advance_parent(2)
        self.produce()
        self.wait_synced()
        st = self.status(t_pol)
        assert_equal(st["bitcoin_anchor"]["depth"], 3)
        self.rec["1_depth_after_two_parent_blocks"] = st["bitcoin_anchor"]
        self.log.info("   two parent blocks and one Sequentia block later: depth %d", st["bitcoin_anchor"]["depth"])

        self.log.info("2. a block below the quorum (escaping stall) reads certified false")
        t_s = w.sendtoaddress(address=dest, amount=3, fee_asset_label=pol)
        self.t_s_hex = node.getrawtransaction(t_s)
        self.advance_parent(3)
        S = self.produce(signers=1)
        assert_equal(S["countersignatures"], 1)
        assert t_s in node.getblock(S["hash"])["tx"]
        self.wait_synced()
        st_s = self.status(t_s)
        self.rec["2_status_stall_block"] = st_s
        assert_equal(st_s["certification"], {"certified": False, "signers": 1, "quorum": 2})
        assert_equal(node.getblockheader(S["hash"])["poscertified"], False)
        self.log.info("   %s: %s", t_s, json.dumps(st_s))
        self.produce()
        self.wait_synced()

        ours = sorted([t_pol, t_x, t_s])
        assert_equal(sorted(self.chain_txids(dest)), ours)

        # The explorer's own check of these statuses (sequentia-explorer,
        # tx-finality.regtest.test.mjs), while the chain holds both kinds of block.
        check = os.environ.get("EXPLORER_CHECK")
        if check:
            self.log.info("   the explorer's check: %s", check)
            subprocess.run(check, shell=True, check=True, env=dict(
                os.environ, ELECTRS_URL=self.electrs_url, TX_CERTIFIED=t_pol, TX_UNCERTIFIED=t_s, BLOCK_CERTIFIED=B))

        self.log.info("3. the parent orphans P: the rows go, and come back at the new block")
        h_b = node.getblockheader(B)["height"]
        parent.invalidateblock(P)
        self.advance_parent(2)
        self.wait_for("the node to leave block %s" % B,
                      lambda: node.getblockcount() < h_b or node.getblockhash(h_b) != B, timeout=60)
        self.log.info("   anchor-driven reorg: node tip %d (was above %d)", node.getblockcount(), h_b)
        # The wallet stamps a transaction's locktime with the height it was made
        # at, so one made above the new tip is not final and leaves the mempool;
        # the others return to it.
        self.wait_for("the transactions back in the node's mempool", lambda: {t_pol, t_x} <= set(node.getrawmempool()))
        self.wait_synced()
        assert_equal(self.chain_txids(dest), [])
        listing = self.api("/address/%s/txs" % dest)
        in_mempool = sorted(set(ours) & set(node.getrawmempool()))
        assert_equal(sorted(t["txid"] for t in listing), in_mempool)
        assert all(t["status"] == {"confirmed": False} for t in listing)
        assert_equal(self.status(t_pol), {"confirmed": False})
        assert_equal(self.api("/block/%s/status" % B)["in_best_chain"], False)
        self.rec["3_after_reorg"] = {"node_tip": node.getblockcount(), "listing": [t["status"] for t in listing]}

        B2 = self.produce()["hash"]
        self.mine_again(t_s)
        self.wait_synced()
        P2 = node.getblockheader(B2)["anchorhash"]
        assert P2 != P
        assert_equal(sorted(self.chain_txids(dest)), ours)
        st2 = self.status(t_pol)
        self.rec["3_status_remined"] = st2
        assert_equal((st2["block_hash"], st2["bitcoin_anchor"]["hash"], st2["bitcoin_anchor"]["depth"]), (B2, P2, 1))
        assert_equal(st2["certification"]["certified"], True)
        self.log.info("   re-mined at %s anchored to %s: each transaction listed once", B2, P2)

        self.log.info("4. electrs killed; the parent orphans the new anchor while it is down")
        pid = self.electrs_pid()
        indexed_before = self.api("/blocks/tip/height")
        tip_before = self.api("/blocks/tip/hash", raw=True)
        # The supervisor is held while the chain moves, so the restart meets
        # the reorganisation in its database rather than while running.
        os.kill(self.supervisor.pid, signal.SIGSTOP)
        os.kill(pid, signal.SIGKILL)
        parent.invalidateblock(P2)
        self.advance_parent(2)
        h_b2 = node.getblockheader(B2)["height"]
        self.wait_for("the node to leave block %s" % B2,
                      lambda: node.getblockcount() < h_b2 or node.getblockhash(h_b2) != B2, timeout=60)
        B3 = self.produce()["hash"]
        P3 = node.getblockheader(B3)["anchorhash"]
        self.mine_again(t_s)
        os.kill(self.supervisor.pid, signal.SIGCONT)
        self.wait_for("the supervisor to restart electrs", lambda: self.electrs_pid() not in (None, pid))
        self.wait_synced()
        assert_equal(sorted(self.chain_txids(dest)), ours)
        st3 = self.status(t_pol)
        self.rec["4_status_after_restart"] = st3
        assert_equal((st3["block_hash"], st3["bitcoin_anchor"]["hash"]), (B3, P3))
        assert_equal(self.api("/block/%s/status" % B2)["in_best_chain"], False)
        with open(self.electrs_log) as f:
            log = f.read()
        starts = [l for l in log.splitlines() if "blocks were indexed" in l]
        self.rec["4_supervisor_log"] = [l for l in log.splitlines() if "[supervisor]" in l or "blocks were indexed" in l
                                        or "processing reorg" in l or "headers were loaded" in l]
        assert_equal(len(starts), 2)
        restart = log.split("[supervisor] start #2")[1]
        assert "headers were loaded, tip at %s" % tip_before in restart, "the restart did not load its old tip"
        assert "processing reorg" in restart, "the restart did not undo the stale block"
        restarted_with = int(starts[1].split(" blocks were indexed")[0].split()[-1])
        assert restarted_with >= indexed_before, "the restart re-indexed from an empty database"
        self.log.info("   restarted on its database (%d blocks indexed before the kill); now at %s anchored to %s",
                      restarted_with, B3, P3)


if __name__ == "__main__":
    TxFinalityReorg().main()
