# Sequentia block explorer — porting Blockstream Esplora

A web block explorer for the Sequentia testnet, built by porting Blockstream's
stack to Sequentia (an Elements/Bitcoin fork with Proof-of-Stake + Bitcoin
anchoring):

- **electrs** (`explorer/electrs`, Rust) — the indexer + Esplora REST API.
  Blockstream's electrs already supports Elements/Liquid (assets, confidential
  txs, pegs), which is a large head start since Sequentia is an Elements fork.
- **esplora** (`explorer/esplora`, Node/TS) — the web frontend that consumes the
  REST API (also has Liquid asset views to build on).
- **rust-elements** (`explorer/rust-elements`, vendored 0.26.1) — the block/tx
  decoder electrs uses. **This is where the core Sequentia work lives** (below).

### Upstream provenance (vendored — these are forks, the `.git` dirs were removed)

- `explorer/electrs` — fork of `github.com/Blockstream/electrs` @ `c7956023c3bf2cb3ac8076a4f65ebdf14b509513`
- `explorer/esplora` — fork of `github.com/Blockstream/esplora` @ `b1fee602d36c7e630271b16d24a487f863cbc98d`
- `explorer/rust-elements` — `elements` crate `0.26.1` from crates.io

Only source is committed; `target/` and `node_modules/` are gitignored — run
`npm install` (esplora) and `cargo build` (electrs) to rebuild.

## Toolchain (no system installs / no sudo)

`source explorer/env.sh` sets everything up:
- Rust via `rustup` (user-local `~/.cargo`, `~/.rustup`).
- `LIBCLANG_PATH` + `BINDGEN_EXTRA_CLANG_ARGS` point at a vendored libclang
  (extracted from `libclang1-18` + `libclang-common-18-dev` .debs into
  `~/.local/libclang/` via `apt-get download` + `dpkg-deb -x` — no root).

Build the (Elements baseline) indexer: `cd explorer/electrs && cargo build --features liquid`
(verified working). The Sequentia build adds `--features sequentia` (see below).

## How electrs ingests blocks (and why Sequentia needs a decoder change)

electrs decodes **all** blocks/headers through `rust-elements`
(`elements::encode::deserialize`), both when reading the node's raw `blk*.dat`
files (`src/new_index/fetch.rs`) and over RPC (`src/daemon.rs`). Its `Block` /
`BlockHeader` types are `elements::*` (`src/chain.rs`).

Sequentia's block **header** is an Elements header with one addition. From
`src/primitives/block.h` (legacy signed-block branch, which `chain=test` uses):

```
version, prev_blockhash, merkle_root, time,
height,                         # Elements already has this
m_anchor_height (u32),          # SEQUENTIA: inserted here ...
m_anchor_hash   (32 bytes),     # SEQUENTIA: ... before the proof
proof { challenge: script, solution: script }
```

i.e. Sequentia inserts a **36-byte Bitcoin anchor** between `height` and the
proof. The block hash commits to everything except the proof `solution` (the
BLS committee signature), exactly as Elements already excludes the solution.
rust-elements 0.26.1 has no anchor, so it misparses Sequentia headers and
computes wrong block hashes — this is THE port blocker.

### Sequentia's TWO serialization deltas from Elements (both fixed under `sequentia`)

1. **Block header** inserts a 36-byte Bitcoin anchor (`u32 anchor_height` +
   32-byte `anchor_hash`) between `height` and the proof (see below).
2. **Asset issuance** (`CAssetIssuance`, `src/primitives/confidential.h:208`)
   appends one extra `uint8_t nDenomination` (default 8) after `inflation_keys`
   — only present when an input carries an issuance. rust-elements 0.26.1 lacks
   it, so it lost byte-alignment on the **genesis** policy-asset issuance tx and
   failed with `InvalidConfidentialPrefix(0x02)`. Fixed by adding a gated
   `denomination: u8` to `AssetIssuance` (hand-written consensus enc/dec).

### The fix (Phase 2): a `sequentia` feature in vendored rust-elements

In `explorer/rust-elements/src/block.rs`, `BlockHeader` gains an
`Option<(u32, BlockHash)> bitcoin_anchor` field (always present; `None` for
non-Sequentia builds so Liquid is unaffected). Under `#[cfg(feature="sequentia")]`
the 36 anchor bytes are read/written **after `height`** in all three places that
must agree byte-for-byte with the C++:
- `consensus_decode` — read anchor → `Some(..)`,
- `consensus_encode` — write anchor,
- `block_hash` — include anchor in the hash (solution still excluded).

electrs then depends on the vendored crate (`elements = { path =
"../rust-elements", features = ["serde","sequentia"] }`) behind its own
`sequentia` feature.

## Chain params (Phase 1): a Sequentia network in electrs `src/chain.rs`

Add a `SequentiaTestnet` variant to the `Network` enum with:
- genesis hash `c2a0a99b4c307e8423b98140af1f539aa4e1feec25c62d655d91d8df51c7dfba` (chain=test),
- p2p magic from `pchMessageStart` (chain=test: `ef 01 ba e0`),
- address params: Bitcoin **testnet** format (Sequentia uses Bitcoin-identical
  addresses, not Liquid blech32),
- native/policy asset id = the chain's `subsidy_asset` (query a live node — TODO),
- `from_str("sequentiatest")`.

## Sequentia-specific data to surface (Phase 3 REST + Phase 4 UI)

> **Anchor parent = Bitcoin testnet4.** SequentiaTestnet anchors to Bitcoin
> **testnet4**, so a block's `anchor_height`/`anchor_hash` are a testnet4 block —
> the frontend should deep-link them to a testnet4 explorer. electrs itself does
> NOT connect to the parent (the anchor is just a header field it parses); the
> elementsd node validates the anchor against a local bitcoind testnet4. (Test
> rigs using `--local-parent` have a throwaway stand-in parent, so their anchor
> hashes won't resolve on real testnet4 — expected.)

| Data | Source | Status |
|---|---|---|
| Bitcoin anchor (per block: testnet4 height + hash) | block header (after Phase 2) | pending |
| PoS committee certificate (leader, quorum, signing members) | parse block `proof.solution` (BLS cert) + `getposschedule` | pending |
| Checkpoints & finality height | `getcheckpointinfo` RPC | pending |
| Assets / issuances | rust-elements + electrs (Elements) — mostly free | inherited |

## Phases / status

- [x] **Phase 0** — toolchain, vendored electrs/esplora/rust-elements, baseline build (electrs `--features liquid` builds).
- [x] **Phase 2a (the hard part) — Sequentia header decode, VALIDATED.** The
  `sequentia` feature in vendored rust-elements parses the 36-byte Bitcoin anchor
  after `height` and commits it in the block hash (solution still excluded).
  `explorer/anchor-decode-check` decodes a REAL `elementsregtest`/Sequentia-format
  header (a full 100-member BLS-cert block) and confirms anchor height/hash AND
  the recomputed block hash match elementsd byte-for-byte. Run:
  `cd explorer/anchor-decode-check && source ../env.sh && cargo run`.
- [x] **Phase 1 — DONE & VALIDATED against a live node.** `SequentiaTestnet`
  network in electrs (genesis/magic/address/asset, `--features sequentia`,
  `[patch.crates-io] elements = ../rust-elements`). Plus two live-node fixes:
  the genesis issuance `denomination` byte (above), and skipping the genesis
  issuance's fabricated input prevout (`SEQUENTIA_INITIAL_ISSUANCE_PREVOUT` in
  `util/transaction.rs`, mirroring Liquid's `*_INITIAL_ISSUANCE_PREVOUT`).
- [x] **Phase 2 — DONE & VALIDATED.** Header anchor + issuance decode confirmed
  byte-exact vs the node (block hash + txid match).
- [x] **Phase 3a — DONE.** Bitcoin anchor exposed in the block REST JSON as
  `bitcoin_anchor: {height, hash}` (`rest.rs` `BlockValue`/`BitcoinAnchorValue`).
  Verified: `/block/<h>` returns `{height:140731, hash:00000000006a…}`, matching
  the node's `getblockheader` exactly (and 140731 is a real **testnet4** block).
- [x] **Phase 3b — DONE & VALIDATED.** PoS BLS committee certificate decoded
  from `ext.solution` into `pos_certificate: {leader_sig, agg_sig, member_count,
  members:[{secp_pubkey, vrf_proof, bls_pubkey, bls_pop}]}` (`rest.rs`
  `parse_pos_bls_certificate`). Format `<leader_sig> <agg_sig(96)> <member(258)>…`
  where member = secp_pubkey(33) ‖ vrf_proof(**81**) ‖ bls_pubkey(48) ‖ bls_pop(96).
  NOTE: `VRF_PROOF_SIZE = 33+16+32 = 81` (`src/vrf.h`), so a member is **258**
  bytes — the `src/pos.h` comment saying 80/257 is off by one. Verified against
  block 1's 1-member cert (secp_pubkey matches the challenge's leader key).
- [x] **Phase 3c — checkpoints/finality (code done; live HTTP pending node).**
  `Daemon::get_checkpoint_info` + `Query::get_checkpoint_info` (getcheckpointinfo
  passthrough), REST route `GET /sequentia/checkpoints`, and a per-block
  `finalized` flag in the block JSON (`height <= finalized_height`). The
  underlying RPC is proven directly (`elements-cli getcheckpointinfo` →
  `{depth:2016, finalized_height:-1, checkpoints:[]}`); the endpoint is a thin
  passthrough. **Live-validated**: `GET /sequentia/checkpoints` →
  `{depth:2016, finalized_height:-1, checkpoints:[]}`, and blocks report
  `finalized:false` (correct — nothing finalized yet). Anchors advance with real
  testnet4 heights (block 1→#140762, block 2→#140768).
- [x] **Cert parser validated at scale** — `cargo test --features sequentia
  sequentia_cert` constructs synthetic certs for N=1,3,51,100 members and
  asserts every field offset; also asserts leader-only / 64-byte-MuSig2
  solutions are rejected. **Also validated LIVE on a real 51-member committee
  block** (the testnet was reconfigured to committee size 51 mid-session):
  electrs parsed `member_count=51`, each member 33/81/48/96, 51 distinct keys.
- [x] **Robustness for a volatile node.** `/block` is served purely from the
  index (no per-request daemon RPC) so it never hangs when the node blips;
  finality is only at `/sequentia/checkpoints`. `explorer/run-electrs-supervised.sh`
  keeps electrs up: reuses the DB across restarts (serves last-indexed state
  while the node is briefly down) and wipes only after a crash (recovering from
  an underlying chain reorg/reset). Needed because the shared testnet is being
  repeatedly reset/reorged by another process (heights seen bouncing 1↔52).
- [x] **Phase 4 — frontend live & VALIDATED (block view).** esplora flavor
  `flavors/sequentia-testnet/config.env` (title, `NATIVE_ASSET_ID`, `IS_ELEMENTS`).
  `client/src/views/block.js` adds a **Bitcoin anchor** row (deep-links to
  mempool.space/testnet4) and a **PoS committee** section (member count +
  parsed `leader_sig`/`agg_sig`/per-member secp & BLS pubkeys via `posCertRows`).
  Verified by server-side rendering block 1 → HTML contains the anchor (#140731),
  committee summary, and the parsed cert. Asset/issuance views inherited from
  esplora's Elements support. Launch: `explorer/run-sequentia-explorer.sh`.
  REMAINING polish: checkpoints UI (3c), 100-member cert at scale, branding.

### How to run (validated 2026-06-20)

Live `chain=test` node: `/home/aejkohl/seq-testnet/node000`, RPC `127.0.0.1:18200`,
cookie `seq:seq` (genesis `c2a0a99b…`, anchored to Bitcoin **testnet4**).

```
cd explorer/electrs && source ../env.sh && cargo build --features sequentia
./target/debug/electrs --network sequentiatest \
  --daemon-rpc-addr 127.0.0.1:18200 --daemon-dir /home/aejkohl/seq-testnet/node000 \
  --cookie seq:seq --db-dir /tmp/electrs-seq-db \
  --http-addr 127.0.0.1:3003 --electrum-rpc-addr 127.0.0.1:51402 --jsonrpc-import -vv
# REST: curl 127.0.0.1:3003/blocks/tip/height ; /block/<hash> ; /tx/<txid>
```

## Parent-chain mode (Sequentia testnet <-> Bitcoin testnet4)

The explorer toggles between Sequentia and its anchor parent, Bitcoin testnet4,
via esplora's network switcher. Two of everything, cross-linked:

| | backend | frontend | accent |
|---|---|---|---|
| Sequentia testnet | electrs `--features sequentia` -> chain=test node, REST :3003 | `:5001` (`run-sequentia-explorer.sh`) | violet `#7d3fa5` |
| Bitcoin testnet4 (parent) | plain Bitcoin electrs `--network testnet4 --jsonrpc-import` -> bitcoind :48332, REST :3004 | `:5002` (`run-testnet4-explorer.sh`) | Bitcoin orange `#f7931a` |

- The Bitcoin electrs is a **separate binary** (no `liquid`/`sequentia` feature);
  build it with `cargo build` then stash it (it shares `target/debug/electrs`
  with the sequentia build, so `cp` it aside and rebuild `--features sequentia`).
  Use `--jsonrpc-import` — Bitcoin Core 28 XOR-obfuscates `blk*.dat` (`blocks/xor.dat`),
  which the blk-file parser can't read.
- Both esplora instances share `MENU_ITEMS` (the `{name: url}` cross-link map);
  `MENU_ACTIVE` differs. The switcher is restyled (see below) as an "anchor link".

### Visual identity (frontend-design)

Grounded in the **official brand assets**: Sequentia's mark is an amber "S" on a
near-black disc and Concatena Labs' wordmark is the same amber, so the palette is
amber-gold `#f5b301` on dark `#141416`, against Bitcoin orange `#f7931a` for the
parent. The **accent encodes the chain** — switching networks recolors the
chrome. The **network switcher is the signature element**: a dark, parent/child
"anchor link" (each chain in its own brand color, ⚓ on the Sequentia option),
styled dark so the amber/orange accents stay legible.

Official logos (fetched from concatenalabs.com / sequentia.io):
- `www/img/icons/concatena-labs.png` — Concatena Labs navbar wordmark (on a dark
  brand chip, since the wordmark's "LABS" is white). Referenced from `navbar.js`.
- `www/img/icons/SequentiaTestnet-menu-logo.svg` — the official Sequentia S-mark
  (embedded PNG) for the switcher; `BitcoinTestnet4-menu-logo.svg` for the parent.

CSS in `flavors/sequentia-testnet/extras.css` (+ `parent-accent.css` for the
Bitcoin-orange override on the testnet4 view). (`logo.svg` is the now-unused
earlier recreation.)

## Running (target)

1. Run a Sequentia `chain=test` node (see `contrib/sequentia/bootstrap-autonomous-testnet.py --chain test`).
2. `source explorer/env.sh && cd explorer/electrs && cargo run --features sequentia -- \
     --network sequentiatest --daemon-rpc-addr 127.0.0.1:<rpcport> --cookie <user:pass> --http-addr 127.0.0.1:3000`
3. `cd explorer/esplora && npm install && API_URL=http://127.0.0.1:3000 npm run dev-server`
