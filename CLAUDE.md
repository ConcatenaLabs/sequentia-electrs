# sequentia-electrs

Sequentia's fork of [Blockstream electrs](https://github.com/Blockstream/electrs): the Rust
indexer that serves the Esplora REST API (and an Electrum RPC server) for the Sequentia sidechain
and its Bitcoin testnet4 parent chain. It backs the live block explorer and public API.

This repository was split out of
[`sequentia-explorer`](https://github.com/ConcatenaLabs/sequentia-explorer), which
now holds only the frontend. The frontend consumes this indexer's REST API over HTTP; there is no
build-time coupling between the two.

`SEQUENTIA-CHANGES.md` is the authoritative, file-level list of what this fork changes versus both
upstreams. Read it before touching the decoder. Node and consensus conventions live in the
[`Sequentia`](https://github.com/ConcatenaLabs/Sequentia) repo.

## Layout, and why it matters

| Path | What |
|---|---|
| `electrs/` | The indexer and Esplora REST API. Sequentia chain params live in `electrs/src/chain.rs`. Upstream docs stay here (`electrs/README.md`, `electrs/doc/`). |
| `rust-elements/` | A vendored fork of the `elements` crate with a `sequentia` cargo feature. This is where the two serialization deltas are handled. |
| `anchor-decode-check/` | A standalone validator: decodes a captured header through `rust-elements` and asserts the parsed anchor and recomputed block hash match what the node reported. Also holds `blockdiag`, for locating misalignments. |
| `SEQUENTIA-CHANGES.md` | The precise diff against upstream, with file pointers. |

`electrs/` and `anchor-decode-check/` reach `rust-elements` through a `path = "../rust-elements"`
Cargo patch, so **the three directories must stay siblings at the repo root.**

## The two serialization deltas

Everything Sequentia-specific is gated behind the `sequentia` cargo feature in both crates, so a
featureless build stays plain Bitcoin electrs. The fork exists for exactly two wire-format
differences, and both are silent-corruption failures rather than clean errors:

1. **A 36-byte Bitcoin anchor in the block header**, inserted between the Elements `height` field
   and the signed-block proof. The block hash commits to it (while still excluding the proof
   solution, as Elements already does). Get this wrong and every computed block hash is wrong.
2. **An extra denomination byte in `CAssetIssuance`**, present whenever an input carries an
   issuance — including the genesis policy-asset issuance, where stock rust-elements loses byte
   alignment.

Encode, decode and hash must agree byte for byte with the node. Change one and you must change all
three.

## Build

Prerequisites: Rust, plus `clang` and `cmake` — the bundled RocksDB is compiled from source and its
bindings are generated with libclang. `env.sh` exists only for hosts where libclang cannot be
installed system-wide; with a system clang you do not need it.

```sh
cd electrs
cargo build --features sequentia      # Sequentia indexer
cargo build                           # plain Bitcoin indexer, for the testnet4 parent chain
```

**Both builds write to the same `target/debug/electrs`.** If you need both binaries, copy the first
aside before building the second — the run scripts assume you have. Add `--release` for production.

There is no CI.

## Running

- `run-electrs-supervised.sh` runs the Sequentia indexer under a restart-on-crash supervisor. That
  is not defensive padding: upstream electrs panics if a block it is fetching is reorged away, and
  Sequentia reorgs whenever Bitcoin reorgs. Fix crashes without assuming the supervisor can be
  removed.
- `run-electrs-testnet4.sh` runs the Bitcoin testnet4 parent-chain indexer, and expects the plain
  Bitcoin binary at `$ELECTRS_BTC_BIN`.

The explorer proxies `/api` to the Sequentia indexer and `/testnet4/api` to the parent-chain one;
the registry and the web wallet both read the Sequentia one.

## Known sharp edges, already documented

- The `finalized` block field is declared but deliberately never set — the `/block` handler serves
  purely from the index and makes no RPC call. Use `GET /sequentia/checkpoints` for finality.

It is recorded in `README.md`'s "Known limitations". If you fix it, update that list.

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
- Keep the divergence from upstream small and feature-gated, and keep `SEQUENTIA-CHANGES.md`
  accurate when you add to it, so upstream fixes stay mergeable.

<!-- BEGIN SHARED AGENT CONVENTIONS: identical in every Sequentia repo. Change it in all of them together. -->
## Working with git and GitHub here

These rules are the same in every Sequentia repository. They are repeated in each
one because this file is the only thing an agent is guaranteed to read, whatever
machine it is working from.

**Nothing pushed to GitHub credits Claude, Anthropic, or any AI tool.** No
`Co-Authored-By: Claude` trailer, no `Claude-Session:` trailer or `claude.ai`
link, no "Generated with Claude Code" in a commit message or a pull request body,
no `claude/*` branch names or session ids, and no mention in source, comments,
docs or issue text. Agent tooling offers several of these by default; compose the
message without them rather than stripping them afterwards.

**Author every commit as**
`GracedEternalKingCabbageMan <151803062+GracedEternalKingCabbageMan@users.noreply.github.com>`.
Never a personal address.

**Every change lands through a pull request that you merge yourself, at once.**
There is no reviewer on this project; the pull request exists so the reasoning is
recorded beside the diff. Branch, push, open it, merge it, delete the branch, all
in one sitting. Pushing straight to the default branch is the rule most often
broken here, and it is the one that costs the record. A pull request stays open
only when the repository owner asks for that specific one, and that never carries
over to the next.

**Name branches `area/short-description`**: `fix/`, `doc/`, `feature/`, `test/`,
`build/`, or the component being changed. Never a tool name, a session id, or
`worktree-*`.

**Write the subject as `area: what changed`**, one line, 72 characters at the
outside and 50 where you can manage it. Put the reasoning in the body, and
explain why rather than what.

**These repositories are public and world-readable.** Never commit private keys,
seeds, `wallet.dat`, RPC credentials, `.env` files or API tokens. Read the diff
before every commit. Secrets belong on the server and in offline backups.

**A file belongs to the repository whose code it describes.** Decide which repo
owns it before writing it; if it landed in the wrong one, move it rather than
deleting it.

**Documentation is part of the change, not a follow-up.** A change that makes a
README, a doc page, a runbook or a code comment wrong is not finished until that
text is right again, in the same pull request as the code. Before you open the
pull request, search the repository for whatever you renamed, moved or removed —
the old binary name, the old path, the old flag, the old command — and fix every
hit. If the change falsifies another repository's documentation, that repository
gets its own pull request in the same sitting. A stale instruction costs a new
user more than a missing one: they trust it, run it, it fails, and the failure
reads as broken software rather than as an out-of-date sentence.

**Write documentation to be timeless.** Assume the reader is new, arrived today,
and wants to know what the software is and how to use it right now. They do not
care what changed, what it used to be called, or which version added what. So
write in the present tense about current behaviour, and leave the history out:
no changelogs, no "new in", no "recently", no "coming soon", no status or
progress sections, no roadmaps, no dated notes. Quote a version number only where
the reader cannot act without it, and prefer pointing at the file that carries it
over copying the digits. Timeless does not mean thin — what the product is, who
it is for, and how to install, configure and use it all still belong there, in
full. Documentation written this way survives a release without an edit, which is
what keeps it true; the history already has homes in the git log, the tags and
the release notes.

**Push the same day you commit.** The testnet server pulls only from GitHub, so a
branch left on one laptop is invisible to every other machine and to the box.
<!-- END SHARED AGENT CONVENTIONS -->
