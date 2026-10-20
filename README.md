# sequentia-electrs

Sequentia's fork of [Blockstream electrs](https://github.com/Blockstream/electrs):
the Rust indexer that serves the Esplora REST API (plus an Electrum RPC server)
for the Sequentia sidechain and its Bitcoin testnet4 parent chain. It backs the
live block explorer and public API at https://sequentiatestnet.com/.

Sequentia is a Bitcoin sidechain for asset tokenization and disintermediated
exchanges, built as a fork of Blockstream Elements 23.3.3. Everything here is
**testnet software**; no mainnet is running yet (the node's `sequentia` chain
parameters are a placeholder).

One codebase produces two indexer binaries:

- the **Sequentia indexer** (`cargo build --features sequentia`), which
  understands Sequentia's block-header and transaction serialization, and
- the **plain Bitcoin indexer** (`cargo build`, no features), used for the
  Bitcoin testnet4 parent chain.

## Where this fits

| Repo | Role |
|---|---|
| [Sequentia](https://github.com/ConcatenaLabs/Sequentia) | The Sequentia node (`sequentiad`, a fork of Elements 23.3.3): consensus, anchoring, proof of stake, open fee market, plus the canonical protocol documentation in `doc/sequentia/`. This indexer reads blocks from that node. |
| [sequentia-explorer](https://github.com/ConcatenaLabs/sequentia-explorer) | Sequentia block explorer frontend (esplora fork); it consumes this indexer's REST API over HTTP. There is no build-time coupling between the two repos. |
| `sequentia-electrs` (this repo) | The electrs fork: Rust indexer + Esplora REST API for Sequentia and its Bitcoin testnet4 parent chain. |

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

## What it indexes

- Full indexing of the Sequentia testnet (`chain=test`), including the two
  Sequentia serialization changes: the 36-byte Bitcoin anchor in every block
  header and the extra denomination byte in asset issuances.
- The standard Esplora REST API in Elements mode (per-output asset ids,
  issuances, explicit fee outputs), plus the Sequentia-specific endpoints and
  fields listed under "REST API" below.
- An asset list built from on-chain issuances (no curated registry needed).
- A contract index: every taproot script-path spend, keyed by the leaf it
  reveals (a Simplicity program's commitment root, or a tapscript leaf's hash),
  and a decoder that reads each contract input: the program's cost against its
  budget, the witness values by the names the template gives them, the terms
  and state from its data leaf, and the outputs that carry the contract on.
  See "Contracts" under "REST API".
- Electrum RPC server (default port 51402 for Sequentia).
- Parent-chain indexing of Bitcoin testnet4 with the featureless binary
  (upstream electrs functionality).

## Known limitations

- Upstream electrs panics if a block it is fetching is reorged away;
  `run-electrs-supervised.sh` exists to restart it (see "Running").
- The `finalized` block field is declared in the code but deliberately never
  set (the `/block` handler serves purely from the index and makes no RPC
  call); use `GET /sequentia/checkpoints` for finality.
- A contract instance's page reads at most 5,000 spends of its commitment
  root and decodes each one; a root shared by far more spends needs an index
  by instance.
- The contract index is written as blocks are indexed: an index made before
  this indexer had it holds no contract rows for its older blocks, so an
  upgrade re-indexes from an empty database.

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
  and recomputed block hash match what `sequentiad` reported. Also contains
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

You need a running Sequentia node (`sequentiad` from the
[Sequentia](https://github.com/ConcatenaLabs/Sequentia) repo) on
`chain=test` with its RPC server enabled. Then:

```sh
cd electrs
./target/debug/electrs \
  --network sequentiatest \
  --daemon-rpc-addr 127.0.0.1:18776 \
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
- `--daemon-rpc-addr`: `18776` is the node's default `rpcport` on
  `chain=test`; match whatever `-rpcport` your node runs with (the public
  box uses a custom port).
- `--cookie` takes `USER:PASSWORD` matching the node's `rpcuser`/`rpcpassword`.
  Omit it and electrs reads the node's cookie file from
  `<daemon-dir>/testnet3/.cookie` instead; there is no `--cookie-file` flag.
- `--daemon-dir` is the node's data directory; `chain=test` stores blocks
  under its `testnet3/` subdirectory (handled automatically).
- `--jsonrpc-import` fetches blocks over RPC instead of parsing the node's
  `blk*.dat` files; it is what the production deployment uses.
- Default ports for `sequentiatest` if you omit the flags: REST `3003`,
  Electrum RPC `51402`, monitoring `44424`.
- `--contract-registry-url URL` names contract spends by the templates the
  [registry](https://github.com/ConcatenaLabs/sequentia-registry) at `URL` has
  verified (its `/contracts` list), and reads the registry's instance records.
  It is read at start and once a minute. Without it, contract spends are still
  indexed and decoded, unnamed.
- `--no-bitcoin-anchor` is for a local chain started without
  `-con_bitcoin_anchor` (a regtest harness chain, run with
  `--network liquidregtest`): its block headers carry no anchor. Every public
  Sequentia chain carries one, and electrs reads it unless told otherwise.

Smoke test:

```sh
curl 127.0.0.1:3003/blocks/tip/height
curl 127.0.0.1:3003/sequentia/anchorstatus
```

For an unattended deployment use `run-electrs-supervised.sh` (override
`NODE_DIR`, `RPC_ADDR`, `HTTP_ADDR`, `ELECTRUM_ADDR`, `DB_DIR` and
`CONTRACT_REGISTRY_URL` via environment variables; set `COOKIE=user:password` only if the node uses `rpcuser`/
`rpcpassword`, otherwise the node's `.cookie` file under `NODE_DIR` is read).
It exists because upstream electrs hard-panics when a
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
Everything below is Sequentia-specific.

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

### Contracts

A taproot script-path spend reveals one leaf. For a Simplicity leaf that is
the program's commitment root; a contract template keeps its parameters in a
data leaf beside the program, so its root is one constant per template. The
index keys every confirmed script-path spend by that root (or, for a
tapscript leaf, by its tapleaf hash). Its rows are written and deleted with
the block's other index rows, so after any reorganisation, including one a
Bitcoin reorganisation drives, the index holds only best-chain spends.

The decoder names a spend by the templates the registry has verified
(`--contract-registry-url`). It compiles each template's source with the
pinned SimplicityHL compiler (through
[sequentia-contracts](https://github.com/ConcatenaLabs/sequentia-contracts)),
walks the compiled program beside the pruned program the spend reveals to
name each witness value, and recomputes the spent output's script from the
template and those values. A spend is named, with an instance, only when that
recomputation gives the very script it spends; a value the spend does not
reveal (a term only a tapscript leaf holds) is taken from the registry's
instance record for that script, and is used only if the recomputation then
holds.

- `GET /tx/:txid/contracts`: for each input that spends a taproot output by a
  leaf, `kind` (`simplicity` or `tapscript`), `leaf_id`, `cmr`, the control
  block's `internal_key` and `merkle_path`, and the `annex`. For a Simplicity
  leaf: `program_bytes`, `witness_bytes`, `witness_stack_bytes`, `cost`
  (`milliwu`, `wu`: the pruned program's static bound, which the node checks)
  and `budget` (`wu`, and `wu_without_annex`, under the rule in `budget.rule`:
  `min(4 × witness stack bytes + 50, 4000050)`, one per byte on the public
  testnet below its flag-day height), `within_budget`. Named by a template:
  `template` (`hash`, `name`, `version`, `summary`, `leaf`), `path` (`name`,
  `who`, `effect`; a leaf with several paths is resolved by the spender's
  `Either` witness value whose alternatives match the paths in order),
  `witness` (each value by name, type and source, `pruned` when the path did
  not use it), `terms` and `state` (each parameter and slot by name, role and
  label, as hex), `data_leaf`, `recomputed`, `terms_from_registry`,
  `instance` (the parameters' bytes in the template's order, hex),
  `successors` (each output that carries the contract on: `vout`, `how` it
  was found, `same` script or `recomputed` from a new state, and the `state`
  it holds) and, for a template whose program reads its own input index,
  `owned_outputs` (outputs `2k` and `2k+1` of input `k`). Unnamed:
  `witness_values` in the order the program reads them. `outputs` gives, for
  each output a successor, the input it continues and its state.
- `GET /contracts/:leaf_id/spends?limit=N`: the spends of a root (or tapleaf
  hash), confirmed ones oldest first, then the mempool's, each with its
  `status`, `template`, `path`, `instance` and `state`; and the `templates`
  that root sits in.
- `GET /contracts/:leaf_id/instance/:params`: one instance, by its root and
  its parameters: `template`, `terms`, every spend as `chains` of coins (each
  spend's decoded `input`, the coin it spent, its `owned_outputs`, its
  `successors` with the state each holds and the spend of each), the spends of
  the instance's tapscript leaves among them, and the `current` unspent coins
  with their state.
- `GET /contracts/templates`: the templates held, with their Simplicity
  leaves' roots, and those refused.

```sh
$ curl https://sequentiatestnet.com/api/contracts/<root>/spends
{"leaf_id":"<root>","templates":[{"name":"sequentia/faucet-drip","version":1,...}],
 "spends":[{"txid":"...","vin":0,"path":"drip","instance":"...","status":{"confirmed":true,...}},...]}
```

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

The contract decoder's tests decode spends recorded from the regtest harness of
sequentia-contracts (`electrs/tests/fixtures/contracts/`): a vault's top-up,
partial repay and partial liquidation, a pool split claim and a faucet drip,
each against its template, and the refusals (a source that does not compile
to the descriptor's root, a registry record that does not recompute the
script, a cut-short witness):

```sh
cd electrs && cargo test --features sequentia --lib contracts
```

`electrs/tests/regtest/contract_index_reorg.py` checks the index across an
anchor-driven reorganisation on a local pair of chains: a Simplicity spend is
indexed, the parent chain orphans the block its block is anchored to, the
index drops it, and once it is mined again the index holds it once, at the
new block. Its header says how to run it (a Sequentia checkout's functional
test framework, the sequentia-contracts harness and `seqc`, and this electrs).

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
  [sequentia-explorer](https://github.com/ConcatenaLabs/sequentia-explorer)
  to match.
- Upstream documentation (`electrs/README.md`, `electrs/doc/usage.md`,
  `electrs/doc/schema.md`) is kept as-is; Sequentia specifics live in this
  README and in `SEQUENTIA-CHANGES.md`.

## License

- `electrs/` is MIT licensed (`electrs/LICENSE`).
- `rust-elements/` is CC0 (`rust-elements/LICENSE`).
