# sequentia-electrs

Sequentia's fork of [Blockstream electrs](https://github.com/Blockstream/electrs):
the Rust indexer that serves the Esplora REST API for the Sequentia sidechain and
its Bitcoin testnet4 parent chain.

This repository was split out of `sequentia-explorer`, which now holds only the
Esplora frontend. The frontend talks to the indexer over HTTP (the Esplora REST
API); there is no build-time coupling between the two repos.

## Layout

- `electrs/` — the indexer + Esplora REST API. Fork of `Blockstream/electrs`
  @ `c7956023c3bf2cb3ac8076a4f65ebdf14b509513`. Sequentia chain params live in
  `electrs/src/chain.rs`.
- `rust-elements/` — vendored fork of the `elements` crate (`0.26.1`) with a
  `sequentia` feature that parses the 36-byte Bitcoin anchor in Sequentia block
  headers. This is where the core block/tx decoder work lives; electrs and
  `anchor-decode-check` depend on it via a `path = "../rust-elements"` Cargo
  dependency, so the two directories must stay siblings at the repo root.
- `anchor-decode-check/` — small Rust utility that decodes a Sequentia header's
  anchor through `rust-elements` and checks the parse.
- `env.sh` — toolchain env for building electrs without system installs.
- `run-electrs-supervised.sh` — runs Sequentia electrs against the shared testnet,
  restarting it across chain reorgs/resets (Blockstream electrs hard-panics on a
  reorg of a block it is fetching).
- `run-electrs-testnet4.sh` — runs the Bitcoin testnet4 parent-chain indexer.
- `PORTING.md` — the full porting guide (anchor decoding, chain params, build).

## Build

```sh
cd electrs && source ../env.sh
cargo build --features sequentia      # Sequentia indexer
# the Bitcoin testnet4 parent indexer is a separate, featureless binary:
cargo build                           # plain Bitcoin electrs
```

See `PORTING.md` for the complete build/run matrix and the Sequentia-specific
decoder changes.

## Frontend

The Esplora frontend that consumes this indexer lives in `sequentia-explorer`.
When the REST API surface here changes, bump accordingly and update the frontend
to match.
