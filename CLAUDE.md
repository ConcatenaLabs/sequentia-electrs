# sequentia-electrs

Sequentia's fork of [Blockstream electrs](https://github.com/Blockstream/electrs): the Rust
indexer that serves the Esplora REST API, both for the Sequentia sidechain and for its Bitcoin
testnet4 parent chain.

This repository was split out of
[`sequentia-explorer`](https://github.com/GracedEternalKingCabbageMan/sequentia-explorer), which
now holds only the Esplora frontend. The frontend talks to this indexer over HTTP; there is no
build-time coupling between the two.

Node and consensus conventions live in the
[`Sequentia`](https://github.com/GracedEternalKingCabbageMan/Sequentia) repo.

## Layout, and why it matters

| Path | What |
|---|---|
| `electrs/` | The indexer and Esplora REST API. Sequentia chain params live in `electrs/src/chain.rs`. |
| `rust-elements/` | A vendored fork of the `elements` crate with a `sequentia` feature that parses the 36-byte Bitcoin anchor in Sequentia block headers. This is where the core block and transaction decoder work lives. |
| `anchor-decode-check/` | A small utility that decodes a header's anchor through `rust-elements` and checks the parse. |
| `PORTING.md` | The full porting guide: anchor decoding, chain params, and the complete build and run matrix. |

`electrs/` and `anchor-decode-check/` depend on `rust-elements` through a
`path = "../rust-elements"` Cargo dependency, so **the three directories must stay siblings at the
repo root.** Moving or nesting one breaks both builds.

## Build

```sh
cd electrs && source ../env.sh
cargo build --features sequentia      # the Sequentia indexer
cargo build                           # plain Bitcoin electrs, for the testnet4 parent chain
```

The two indexers are the **same crate built with different features**: the parent-chain binary is
the featureless build. `env.sh` supplies the toolchain environment so electrs can be built without
system installs. `PORTING.md` has the complete matrix.

There is no CI.

## Running

- `run-electrs-supervised.sh` runs the Sequentia indexer against the shared testnet and **restarts
  it across chain reorgs and resets**. This is not defensive padding: upstream Blockstream electrs
  hard-panics on a reorg of a block it is currently fetching, and Sequentia reorgs whenever Bitcoin
  reorgs. Do not remove the supervision loop; fix crashes without assuming they can be eliminated.
- `run-electrs-testnet4.sh` runs the Bitcoin testnet4 parent-chain indexer.

The explorer proxies `/api` to the Sequentia indexer and `/testnet4/api` to the parent-chain one,
and the registry and the web wallet both read the Sequentia one.

## Working in this repo

- **Repository is public.** Never commit keys, seeds, RPC credentials, `.env` files or tokens.
- **Commit author:**
  `GracedEternalKingCabbageMan <151803062+GracedEternalKingCabbageMan@users.noreply.github.com>`
- **Always open a pull request, then merge it yourself immediately.** The PR exists so the change
  and its reasoning are recorded, not because anyone is waiting to review it. There is no review
  process. If you are ever told to leave one specific PR open, that applies to that PR only and
  never becomes the default.
- PRs go against `main`, which is the remote default.
- **Deployment is pull-only.** The server pulls this repo from GitHub and builds there. Never edit
  source on the server and never copy source or binaries onto it.
- Keep the divergence from upstream electrs small and confined to `rust-elements` plus the chain
  params, so upstream fixes stay mergeable.
