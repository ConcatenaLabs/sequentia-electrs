#!/usr/bin/env python3
"""The contract index follows an anchor-driven reorg.

A Sequentia chain anchored to a parent chain that stands in for Bitcoin (a
second regtest node, as the node's own feature_bitcoin_anchoring.py does), with
Simplicity active. A one-key Simplicity output is paid and spent in a block
anchored to a parent block P; electrs indexes the spend under the program's
commitment root. The parent then orphans P: the anchor watcher disconnects the
Sequentia block, and the index must drop the spend's row; the spend returns to
the mempool, is mined again on the parent's new branch, and the index must hold
it once, at the new block.

Run from a Sequentia checkout's functional test framework and the regtest
harness of sequentia-contracts (for its Simplicity helpers and seqc):

    SEQUENTIA_REPO=<Sequentia checkout> BITCOIND=<sequentiad> BITCOINCLI=<sequentia-cli> \\
    CONTRACTS_DIR=<sequentia-contracts checkout> SEQC=<seqc> \\
    ELECTRS=<electrs built with --features sequentia> \\
    python3 tests/regtest/contract_index_reorg.py --configfile=<config.ini> --tmpdir=<dir>

`config.ini` is the one the harness writes (sequentia-contracts harness/run.py).
"""
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, os.path.join(os.environ["CONTRACTS_DIR"], "harness", "lib"))
from simplicity import *  # noqa: E402,F401,F403
from test_framework.util import assert_equal, get_auth_cookie, get_datadir_path, p2p_port, rpc_port  # noqa: E402


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class ContractIndexReorg(SimBase, BitcoinTestFramework):
    NAME = "contract_index_reorg"

    def set_test_params(self):
        self.chain_params()
        self.num_nodes = 2

    def sync_all(self, nodes=None):
        pass  # two chains, never connected

    def setup_network(self, split=False):
        child_args = self.extra_args[0]
        parent_args = ["-port=%d" % p2p_port(1), "-rpcport=%d" % rpc_port(1), "-validatepegin=0",
                       "-initialfreecoins=0", "-con_blocksubsidy=5000000000", "-anyonecanspendaremine=1",
                       "-signblockscript=51"]
        self.add_nodes(2, [child_args, parent_args], chain=["elementsregtest", "elementsregtest"])
        self.start_node(1)
        parent = self.nodes[1]
        u, p = get_auth_cookie(get_datadir_path(self.options.tmpdir, 1), "elementsregtest")
        self.anchor_args = ["-con_bitcoin_anchor=1", "-validateanchor=1", "-anchorpollinterval=1",
                            "-mainchainrpchost=127.0.0.1", "-mainchainrpcport=%d" % rpc_port(1),
                            "-mainchainrpcuser=%s" % u, "-mainchainrpcpassword=%s" % p,
                            "-parentgenesisblockhash=%s" % parent.getblockhash(0)]
        self.start_node(0, extra_args=child_args + self.anchor_args)
        parent.createwallet(wallet_name="w", descriptors=True)
        # The harness's wallets are legacy, holding the key the framework mines to.
        self.nodes[0].createwallet(wallet_name="", descriptors=False)
        self.nodes[0].importprivkey(privkey=self.nodes[0].get_deterministic_priv_key().key, label="coinbase", rescan=True)

    # The harness's boot and fund pay fees in the framework's BITCOIN_ASSET; an
    # anchored chain has another policy asset, so pay in the one it has.
    def boot(self):
        node = self.node = self.nodes[0]
        self.generate(node, 101, sync_fun=self.no_op)
        self.pol = node.dumpassetlabels()["bitcoin"]
        node.sendtoaddress(address=node.getnewaddress(), amount=1000000, fee_asset_label=self.pol)
        self.generate(node, 1, sync_fun=self.no_op)
        self.X = node.issueasset(assetamount=1000000, tokenamount=0, blind=False, fee_asset=self.pol)["asset"]
        self.generate(node, 1, sync_fun=self.no_op)
        self.X_OUT = self.asset_out(self.X)
        self.rates0 = node.getfeeexchangerates()

    def fund(self, spk, atoms, asset, mine=True):
        spk = bytes(spk)
        addr = program_to_witness(spk[0] - 0x50 if spk[0] else 0, spk[2:])
        txid = self.node.sendtoaddress(address=addr, amount=Decimal(atoms) / COIN, assetlabel=asset, fee_asset_label=self.pol)
        if mine:
            self.generate(self.node, 1, sync_fun=self.no_op)
        return self.utxo_of(txid, spk)

    def api(self, path):
        with urllib.request.urlopen(self.electrs_url + path, timeout=30) as r:
            return json.loads(r.read())

    def wait_indexed(self, what, cond, timeout=120):
        end = time.time() + timeout
        while time.time() < end:
            try:
                v = cond()
                if v:
                    return v
            except Exception:
                pass
            time.sleep(0.5)
        raise AssertionError("electrs: timed out waiting for " + what)

    def run_test(self):
        parent = self.nodes[1]
        parent_addr = parent.getnewaddress()
        self.generatetoaddress(parent, 5, parent_addr, sync_fun=self.no_op)
        self.boot()
        node = self.node
        self.whitelist(self.X)

        sec = generate_privkey()
        pk = compute_xonly_pubkey(sec)[0]
        prog = SimProg(src("p2pk.simf"), {"PK": vpub(pk)})
        AMT, FEE = 1_0000_0000, 300
        u = self.fund(prog.spk, AMT, self.X)
        tx = self.mktx([u], [self.out(AMT - FEE, self.wallet_spk(), self.X_OUT), self.fee(FEE, self.X_OUT)])
        sig = sign_schnorr(sec, self.sim_sighash(prog, tx, 0))
        self.sim_satisfy(prog, tx, 0, {"SIG": vsig(sig)})
        txid = node.sendrawtransaction(tx.serialize().hex())

        # The spend's block is anchored to a fresh parent block P.
        self.generatetoaddress(parent, 1, parent_addr, sync_fun=self.no_op)
        P = parent.getbestblockhash()
        B = self.generate(node, 1, sync_fun=self.no_op)[0]
        assert_equal(node.getblockheader(B)["anchorhash"], P)
        H = node.getblockcount()
        self.log.info("spend %s in block %s (height %d), anchored to parent block %s", txid, B, H, P)

        # electrs, indexing this chain.
        http, electrum, monitor = free_port(), free_port(), free_port()
        u_, p_ = get_auth_cookie(node.datadir, "elementsregtest")
        self.electrs_url = "http://127.0.0.1:%d" % http
        log = open(os.path.join(self.options.tmpdir, "electrs.log"), "w")
        self.electrs = subprocess.Popen([
            os.environ["ELECTRS"], "-vv", "--network", "liquidregtest", "--parent-network", "regtest",
            "--daemon-dir", os.path.join(node.datadir, "elementsregtest"),
            "--daemon-rpc-addr", "127.0.0.1:%d" % rpc_port(0), "--cookie", "%s:%s" % (u_, p_),
            "--jsonrpc-import", "--db-dir", os.path.join(self.options.tmpdir, "electrs-db"),
            "--http-addr", "127.0.0.1:%d" % http, "--electrum-rpc-addr", "127.0.0.1:%d" % electrum,
            "--monitoring-addr", "127.0.0.1:%d" % monitor], stdout=log, stderr=subprocess.STDOUT)
        try:
            self.check(node, parent, prog, txid, B, H, P, parent_addr)
        finally:
            self.electrs.terminate()
            self.electrs.wait()

    def check(self, node, parent, prog, txid, B, H, P, parent_addr):
        cmr = prog.cmr.hex()
        spends = lambda: self.api("/contracts/%s/spends" % cmr)["spends"]
        rec = {}
        before = self.wait_indexed("the spend at block %s" % B,
                                   lambda: [s for s in spends() if s["status"].get("block_hash") == B])
        assert_equal([(s["txid"], s["vin"], s["status"]["block_height"]) for s in spends()], [(txid, 0, H)])
        d = self.api("/tx/%s/contracts" % txid)["inputs"][0]
        assert_equal((d["kind"], d["cmr"], d["within_budget"]), ("simplicity", cmr, True))
        rec["before"] = {"spends": spends(), "anchor": node.getblockheader(B)["anchorhash"],
                         "cost": d["cost"], "budget": d["budget"]["wu"]}
        self.log.info("indexed: %s", before)

        # The parent orphans P and grows a different branch: the anchor watcher
        # must disconnect B, and the index must drop the spend's row.
        parent.invalidateblock(P)
        self.generatetoaddress(parent, 2, parent.getnewaddress(), sync_fun=self.no_op)
        end = time.time() + 60
        while node.getblockcount() >= H and node.getblockhash(H) == B and time.time() < end:
            time.sleep(0.25)
        assert node.getblockcount() < H or node.getblockhash(H) != B, "the anchored chain did not reorganise"
        assert txid in node.getrawmempool(), "the spend did not return to the mempool"
        self.log.info("anchor-driven reorg: block %s disconnected, tip now %d", B, node.getblockcount())
        tip = node.getbestblockhash()
        self.wait_indexed("electrs to follow the reorg",
                          lambda: urllib.request.urlopen(self.electrs_url + "/blocks/tip/hash").read().decode() == tip)
        after = spends()
        assert_equal([(s["txid"], s["status"]["confirmed"]) for s in after], [(txid, False)])
        assert_equal(self.api("/block/%s/status" % B)["in_best_chain"], False)
        rec["after_reorg"] = {"spends": after, "tip": tip}

        # Mined again on the parent's new branch: indexed once, at the new block.
        B2 = self.generate(node, 1, sync_fun=self.no_op)[0]
        H2 = node.getblockcount()
        assert B2 != B
        assert_equal(node.getblockheader(B2)["anchorhash"], parent.getbestblockhash())
        again = self.wait_indexed("the spend at block %s" % B2,
                                  lambda: [s for s in spends() if s["status"].get("block_hash") == B2])
        assert_equal([(s["txid"], s["vin"], s["status"]["block_height"], s["status"]["block_hash"]) for s in spends()],
                     [(txid, 0, H2, B2)])
        rec["after_remine"] = {"spends": spends(), "anchor": node.getblockheader(B2)["anchorhash"]}
        self.log.info("re-mined and re-indexed: %s", again)
        out = os.environ.get("REORG_RECORD")
        if out:
            with open(out, "w") as f:
                json.dump(rec, f, indent=1)


if __name__ == "__main__":
    ContractIndexReorg().main()
