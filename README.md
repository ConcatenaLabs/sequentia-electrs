# sequentia-electrs

Sequentia's fork of [Blockstream electrs](https://github.com/Blockstream/electrs):
the Rust indexer that serves the Esplora REST API (plus an Electrum RPC server)
for the Sequentia sidechain and its Bitcoin testnet4 parent chain. It backs the
live block explorer and public API at https://sequentiatestnet.com/.

Sequentia is a Bitcoin sidechain for asset tokenization and decentralized
exchange, built as a fork of Blockstream Elements 23.3.3. Everything here is
**testnet software**; there is no mainnet.

One codebase produces two indexer binaries:

- the **Sequentia indexer** (`cargo build --features sequentia`), which
  understands Sequentia's block-header and transaction serialization, and
- the **plain Bitcoin indexer** (`cargo build`, no features), used for the
  Bitcoin testnet4 parent chain.

## Where this fits

| Repo | Role |
|---|---|
| [Sequentia](https://github.com/GracedEternalKingCabbageMan/Sequentia) | The Sequentia node (`elementsd` fork of Elements 23.3.3): consensus, anchoring, proof of stake, open fee market, plus the canonical protocol documentation in `doc/sequentia/`. This indexer reads blocks from that node. |
| [sequentia-explorer](https://github.com/GracedEternalKingCabbageMan/sequentia-explorer) | Sequentia block explorer frontend (esplora fork); it consumes this indexer's REST API over HTTP. There is no build-time coupling between the two repos. |
| `sequentia-electrs` (this repo) | The electrs fork: Rust indexer + Esplora REST API for Sequentia and its Bitcoin testnet4 parent chain. |

This repository was split out of `sequentia-explorer`, which now holds only the
frontend.

## Live instances

Both indexers run behind https://sequentiatestnet.com:

| Chain | REST base URL |
|---|---|
| Sequentia testnet | `https://sequentiatestnet.com/api` |
| Bitcoin testnet4 (parent chain) | `https://sequentiatestnet.com/testnet4/api` |

```sh
curl https://sequentiatestnet.com/api/blocks/tip/height
curl https://sequentiatestnet.com/api/sequentia/anchorstatus
curl https://sequentiatestnet.com/testnet4/api/blocks/tip/height
```

## Status

Working today (verified against the live public testnet on 2026-07-08):

- Full indexing of the Sequentia testnet (`chain=test`), including the two
  Sequentia serialization changes: the 36-byte Bitcoin anchor in every block
  header and the extra denomination byte in asset issuances.
- The standard Esplora REST API in Elements mode (per-output asset ids,
  issuances, explicit fee outputs), plus the Sequentia-specific endpoints and
  fields listed under "REST API" below.
- An asset list built from on-chain issuances (no curated registry needed).
- Electrum RPC server (default port 51402 for Sequentia).
- Parent-chain indexing of Bitcoin testnet4 with the featureless binary
  (upstream electrs functionality).

Known limitations:

- Upstream electrs panics if a block it is fetching is reorged away;
  `run-electrs-supervised.sh` exists to restart it (see "Running").
- The `finalized` block field is declared in the code but deliberately never
  set (the `/block` handler serves purely from the index and makes no RPC
  call); use `GET /sequentia/checkpoints` for finality.
- The `SEQUENTIA_TESTNET_GENESIS` constant in `electrs/src/chain.rs` still
  holds the pre-2026-07-05 genesis hash. It is only used for Electrum server
  discovery, so indexing and the REST API are unaffected, but the constant is
  stale relative to the live chain (genesis `ddd11d54...`).

## Layout

- `electrs/` - the indexer + Esplora REST API. Fork of `Blockstream/electrs`
  @ `c7956023c3bf2cb3ac8076a4f65ebdf14b509513`. Upstream docs live here
  (`electrs/README.md`, `electrs/doc/usage.md`, `electrs/doc/schema.md`).
- `rust-elements/` - vendored fork of the `elements` crate (0.26.1 from
  crates.io) with a `sequentia` cargo feature that handles Sequentia's header
  and issuance serialization. electrs and `anchor-decode-check` depend on it
  via a `path = "../rust-elements"` Cargo patch, so the two directories must
  stay siblings at the repo root.
- `anchor-decode-check/` - small standalone validator: decodes a captured
  Sequentia block header through `rust-elements` and asserts the parsed anchor
  and recomputed block hash match what `elementsd` reported. Also contains
  `blockdiag`, a helper that decodes a full block and each transaction
  individually to locate serialization misalignments.
- `env.sh` - toolchain env used on hosts without root access: points bindgen
  at a user-local libclang for the RocksDB build. With a system clang/libclang
  installed you do not need it.
- `run-electrs-supervised.sh` - runs the Sequentia indexer under a
  restart-on-crash supervisor (see "Running").
- `run-electrs-testnet4.sh` - runs the Bitcoin testnet4 parent-chain indexer.
- `SEQUENTIA-CHANGES.md` - the precise list of changes vs upstream electrs and
  rust-elements, with file pointers.

## Building

Prerequisites: Rust (via [rustup](https://rustup.rs/)), `clang` and `cmake`
(the bundled RocksDB is compiled from source and its Rust bindings are
generated with libclang). If you cannot install libclang system-wide, see
`env.sh` for the user-local alternative.

```sh
cd electrs
cargo build --features sequentia      # Sequentia indexer
cargo build                           # plain Bitcoin indexer (for testnet4)
```

Both builds write to `target/debug/electrs`, so if you need both binaries copy
the first one aside before building the second (the deploy scripts assume
this; `run-electrs-testnet4.sh` expects the Bitcoin binary at
`$ELECTRS_BTC_BIN`). Add `--release` for production use.

## Running

### Against a Sequentia node

You need a running Sequentia node (`elementsd` from the
[Sequentia](https://github.com/GracedEternalKingCabbageMan/Sequentia) repo) on
`chain=test` with its RPC server enabled. Then:

```sh
cd electrs
./target/debug/electrs \
  --network sequentiatest \
  --daemon-rpc-addr 127.0.0.1:18200 \
  --daemon-dir /path/to/sequentia-datadir \
  --cookie rpcuser:rpcpassword \
  --db-dir /path/to/electrs-db \
  --http-addr 127.0.0.1:3003 \
  --electrum-rpc-addr 127.0.0.1:51402 \
  --cors '*' \
  --jsonrpc-import -vv
```

Notes:

- `--network sequentiatest` selects the Sequentia testnet parameters
  (`electrs/src/chain.rs`).
- `--cookie` takes `USER:PASSWORD` matching the node's `rpcuser`/`rpcpassword`
  (or use `--cookie-file` to point at the node's `.cookie` file).
- `--daemon-dir` is the node's data directory; `chain=test` stores blocks
  under its `testnet3/` subdirectory (handled automatically).
- `--jsonrpc-import` fetches blocks over RPC instead of parsing the node's
  `blk*.dat` files; it is what the production deployment uses.
- Default ports for `sequentiatest` if you omit the flags: REST `3003`,
  Electrum RPC `51402`, monitoring `44424`.

Smoke test:

```sh
curl 127.0.0.1:3003/blocks/tip/height
curl 127.0.0.1:3003/sequentia/anchorstatus
```

For an unattended deployment use `run-electrs-supervised.sh` (override
`NODE_DIR`, `RPC_ADDR`, `HTTP_ADDR`, `ELECTRUM_ADDR`, `DB_DIR`, `COOKIE` via
environment variables). It exists because upstream electrs hard-panics when a
block it is fetching is reorged away; the wrapper restarts electrs, reuses the
DB across clean restarts, and wipes the DB only after a crash so a reorged or
reset chain re-indexes cleanly.

### The Bitcoin testnet4 parent indexer

Run the featureless binary against a `bitcoind` on testnet4:

```sh
./target/debug/electrs \
  --network testnet4 \
  --daemon-rpc-addr 127.0.0.1:48332 \
  --daemon-dir ~/.bitcoin \
  --cookie rpcuser:rpcpassword \
  --db-dir /path/to/t4-db \
  --http-addr 127.0.0.1:3004 \
  --electrum-rpc-addr 127.0.0.1:51403 \
  --cors '*' --jsonrpc-import -vv
```

Use `--jsonrpc-import` here: Bitcoin Core 28+ XOR-obfuscates its `blk*.dat`
files, which the file-based ingestion cannot read. `run-electrs-testnet4.sh`
wraps this invocation.

## REST API (for integrators)

The API is the standard
[Esplora HTTP API](https://github.com/Blockstream/esplora/blob/master/API.md),
including its Elements/Liquid extensions (every output carries an `asset` id,
transactions expose `issuance` data on inputs, fees are explicit fee outputs).
Everything below is Sequentia-specific and was verified against the live API
on 2026-07-08.

### Block objects: `bitcoin_anchor`

`GET /block/:hash` includes the Bitcoin block this Sequentia block is anchored
to. The anchor is parsed from the block header itself; on the public testnet
it refers to a Bitcoin **testnet4** block:

```sh
$ curl https://sequentiatestnet.com/api/block/<hash>
{
  "id": "...", "height": 7315, ...,
  "bitcoin_anchor": {
    "height": 143437,
    "hash": "0000000000332055be8f1dabdf957cdb4b02afe60097f9535b6facc75b9c6c6c"
  }
}
```

Block objects can also carry `pos_certificate`, the decoded proof-of-stake
BLS committee certificate: `leader_sig`, `agg_sig`, `signer_count`, plus one
of two form-specific fields. Blocks on the current public testnet (bitfield
form, public fixed-size committee) carry `signer_bitfield` - a hex bitfield
in the registered committee's order, bit `i` (LSB-first within each byte) set
meaning "committee member `i` signed"; `signer_count` is its popcount.
Legacy blocks whose solution embeds member records instead carry `members`
(per member `secp_pubkey`/`vrf_proof`/`bls_pubkey`/`bls_pop`). Solutions
that are neither (genesis, escaping-stall blocks) omit the field.

### `GET /sequentia/checkpoints`

Passthrough of the node's `getcheckpointinfo` RPC: checkpoint depth,
finalized height and known checkpoints.

```sh
$ curl https://sequentiatestnet.com/api/sequentia/checkpoints
{"checkpoints":[],"configured":[],"conflicts":[],"depth":2016,"finalized_height":-1}
```

### `GET /sequentia/anchorstatus`

Passthrough of the node's `getanchorstatus` RPC: the live parent-chain anchor
check (current anchor height/hash on Bitcoin testnet4 and whether anchor
validation is healthy). Cross-chain swap tooling uses this as its reveal gate.

```sh
$ curl https://sequentiatestnet.com/api/sequentia/anchorstatus
{"anchorhash":"00000000...","anchorheight":143437,"anchorstatus":"ok","tipheight":7313,"validateanchor":true}
```

### `GET /mempool/recent`: per-asset summaries

On a chain where fees may be paid in any asset and a transaction can move
several assets, a single "value" number is meaningless, so each recent-tx
entry additionally carries:

- `out_values`: per-asset explicit output totals, largest first
  (`[{"asset": "...", "value": 123}, ...]`),
- `confidential`: whether any output is blinded,
- `fee_asset`: the asset the fee was actually paid in.

### `GET /assets/registry`: on-chain asset list

When no curated asset-metadata database is configured (the default for
Sequentia), this endpoint lists every on-chain issued asset by scanning the
issuance index, so the explorer's Assets page reflects real issuances. The
usual per-asset endpoints (`GET /asset/:asset_id` etc.) work unchanged.

## Tests

Sequentia-specific unit tests live in `electrs/src/rest.rs` (the committee
certificate parser, exercised for 1/3/51/100-member certificates and for
non-certificate solutions):

```sh
cd electrs && cargo test --features sequentia sequentia_cert
```

The standalone header-decode validator checks a real captured Sequentia
header byte-for-byte (parsed anchor + recomputed block hash vs the node):

```sh
cd anchor-decode-check && cargo run --bin anchor-decode-check
```

Upstream's own test suite and docs are under `electrs/` (see
`electrs/README.md`).

## Contributing

- Development happens on `main`; open PRs against it.
- The full, file-level list of changes vs upstream is in
  [SEQUENTIA-CHANGES.md](SEQUENTIA-CHANGES.md). Keep it updated when you touch
  the fork surface.
- When the REST API surface changes, update the frontend in
  [sequentia-explorer](https://github.com/GracedEternalKingCabbageMan/sequentia-explorer)
  to match.
- Upstream documentation (`electrs/README.md`, `electrs/doc/usage.md`,
  `electrs/doc/schema.md`) is kept as-is; Sequentia specifics live in this
  README and in `SEQUENTIA-CHANGES.md`.

## License

- `electrs/` is MIT licensed (`electrs/LICENSE`).
- `rust-elements/` is CC0 (`rust-elements/LICENSE`).
