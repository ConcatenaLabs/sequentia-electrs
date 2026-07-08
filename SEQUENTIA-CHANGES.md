# Sequentia changes vs upstream

This document lists exactly what this fork changes relative to its upstreams.
It was produced from a full file diff against both upstream code bases on
2026-07-08 and replaces the original porting log.

Upstream provenance (vendored; the upstream `.git` directories were removed):

- `electrs/` - fork of `github.com/Blockstream/electrs`
  @ `c7956023c3bf2cb3ac8076a4f65ebdf14b509513`
- `rust-elements/` - the `elements` crate `0.26.1` from crates.io

Everything Sequentia-specific is gated behind a `sequentia` cargo feature in
both crates, so a featureless build remains plain Bitcoin electrs and a
`--features liquid` build remains Liquid-compatible.

## Why the fork exists: two serialization deltas

electrs decodes all blocks and transactions through `rust-elements`
(`elements::encode::deserialize`), both from the node's `blk*.dat` files
(`electrs/src/new_index/fetch.rs`) and over RPC (`electrs/src/daemon.rs`).
Sequentia (an Elements fork) changes the wire format in two places, so stock
rust-elements 0.26.1 misparses Sequentia data:

1. **Block header: a 36-byte Bitcoin anchor.** Sequentia inserts
   `m_anchor_height` (u32) + `m_anchor_hash` (32 bytes) between the Elements
   `height` field and the signed-block proof
   (`src/primitives/block.h` in the
   [Sequentia node](https://github.com/GracedEternalKingCabbageMan/Sequentia)):

   ```
   version, prev_blockhash, merkle_root, time,
   height,                    # Elements already has this
   m_anchor_height (u32),     # SEQUENTIA: inserted here ...
   m_anchor_hash   (32 bytes) # SEQUENTIA: ... before the proof
   proof { challenge: script, solution: script }
   ```

   The block hash commits to everything including the anchor but excluding the
   proof `solution` (exactly as Elements already excludes the solution).
   Without the fix, headers misparse and every computed block hash is wrong.

2. **Asset issuance: an extra denomination byte.** Sequentia's
   `CAssetIssuance` serializes one extra `uint8_t nDenomination` (default 8,
   the asset's decimal precision) after `inflation_keys`. It is present
   whenever an input carries an issuance, including the genesis policy-asset
   issuance, where stock rust-elements loses byte alignment and fails with
   `InvalidConfidentialPrefix(0x02)`.

## Changes in `rust-elements/`

- `Cargo.toml`: adds the empty `sequentia` feature.
- `src/block.rs`: `BlockHeader` gains
  `pub bitcoin_anchor: Option<(u32, BlockHash)>`. Under
  `#[cfg(feature = "sequentia")]` the 36 anchor bytes are read and written
  after `height` in all three places that must agree byte-for-byte with the
  node: `consensus_decode`, `consensus_encode`, and `block_hash` (the anchor
  is committed in the hash; the solution stays excluded). Non-sequentia
  builds keep the field as `None` and the wire format unchanged.
- `src/transaction.rs`: `AssetIssuance` gains a feature-gated
  `denomination: u8` (default 8), with hand-written `Encodable`/`Decodable`
  impls that place it after `inflation_keys`, byte-for-byte matching
  Sequentia's `CAssetIssuance`.
- `src/address.rs`: adds `AddressParams::SEQUENTIA_TESTNET`. Sequentia
  addresses are Bitcoin-testnet-identical (p2pkh 111, p2sh 196, bech32 HRP
  `tb`); confidential addresses are opt-in with blech32 HRP `tsqb` and
  blinded prefix 70. (Sequentia is transparent by default; confidentiality is
  opt-in, the reverse of Liquid.)
- `src/pset/map/input.rs`: PSET has no denomination field, so conversion to
  `AssetIssuance` defaults `denomination` to 8 under the feature.

## Changes in `electrs/`

- `Cargo.toml`: adds `sequentia = ["liquid", "elements/sequentia"]` (the
  Sequentia build is the Elements code path plus the decoder feature) and a
  `[patch.crates-io]` entry pointing `elements` at `../rust-elements`.
- `src/chain.rs`: adds `Network::SequentiaTestnet` (CLI name
  `sequentiatest`): p2p magic `0xe0ba01ef` (the node's `chain=test`
  `pchMessageStart`), `AddressParams::SEQUENTIA_TESTNET`, the chain's policy
  asset as native asset, no pegged asset (Sequentia has no peg), and a
  genesis-hash constant. Note: the `SEQUENTIA_TESTNET_GENESIS` constant still
  holds the pre-2026-07-05 genesis (`c2a0a99b...`; the live chain since the
  2026-07-05 re-genesis is `ddd11d54...`). It is only used for Electrum
  server discovery (`src/electrum/server.rs`), so indexing and REST are
  unaffected, but it should be updated.
- `src/config.rs`: default ports for `sequentiatest` (REST `3003`, Electrum
  RPC `51402`, monitoring `44424`) and the daemon block-directory mapping
  (`chain=test` stores blocks under the `testnet3/` datadir subdirectory).
  SequentiaTestnet does not index a Bitcoin parent inside this process; the
  parent chain gets its own featureless electrs instance.
- `src/elements/asset.rs`: `NATIVE_ASSET_ID_SEQUENTIA_TESTNET =
  c8eccacf0953e1931cd31e434d8319101cc36e6c38b0e2104d8687552fae3e40`, the
  chain's policy asset (verified against the live chain's genesis issuance).
- `src/util/transaction.rs`: `SEQUENTIA_INITIAL_ISSUANCE_PREVOUT =
  c699280e...`; the genesis policy-asset issuance spends a fabricated
  outpoint that is not a real UTXO, so prevout lookup must skip it (mirrors
  upstream's Liquid `*_INITIAL_ISSUANCE_PREVOUT` handling). Verified: the
  live chain's genesis issuance reports exactly this `issuance_prevout`.
- `src/daemon.rs`: two node-RPC passthroughs, `get_checkpoint_info`
  (`getcheckpointinfo`) and `get_anchor_status` (`getanchorstatus`).
- `src/new_index/query.rs`: exposes the two passthroughs and adds
  `list_issued_assets`: when no curated asset-metadata DB is configured, the
  `/assets/registry` endpoint enumerates every on-chain issued asset by
  scanning the issuance index (sorted by asset id for stable pagination; the
  policy asset is included since it is itself a genesis issuance).
- `src/new_index/mempool.rs`: the recent-transaction summary
  (`GET /mempool/recent`) gains per-asset explicit output totals
  (`out_values`), a `confidential` flag, and the actual `fee_asset`. On an
  any-asset chain a single native "value" cannot summarize a transaction and
  the fee is not necessarily paid in the policy asset (Sequentia's open fee
  market allows fees in any accepted asset).
- `src/rest.rs`:
  - Block JSON gains `bitcoin_anchor: {height, hash}` (parsed from the
    header; a Bitcoin testnet4 block on the public testnet).
  - Block JSON gains `pos_certificate`, decoded from the block's proof
    `solution` when it is a legacy embedded-member BLS certificate:
    `leader_sig`, `agg_sig(96)`, then one 258-byte record per member
    (`secp_pubkey(33) + vrf_proof(81) + bls_pubkey(48) + bls_pop(96)`).
    Solutions that are not this shape (genesis, escaping-stall blocks, and
    all blocks on the current public-committee chain, which carry only the
    leader and aggregate signatures) yield no `pos_certificate` field.
  - Block JSON declares a `finalized` field, currently never set: `/block`
    is served purely from the index with no per-request daemon RPC (so it
    keeps working when the node blips); finality is served by
    `GET /sequentia/checkpoints` instead.
  - New routes `GET /sequentia/checkpoints` and `GET /sequentia/anchorstatus`
    (the RPC passthroughs above).
  - Unit tests (`sequentia_cert_tests`) for the certificate parser:
    synthetic 1/3/51/100-member certificates with every field offset
    asserted, plus rejection of leader-only and 64-byte-MuSig2 solutions.

See the README's "REST API" section for the integrator-facing view of these
endpoints with live examples.

## Supporting tools in this repo (not part of the upstream trees)

- `anchor-decode-check/`: standalone validator that decodes a real captured
  Sequentia header (`getblockheader <hash> false` output) through the patched
  rust-elements and asserts the parsed anchor and the recomputed block hash
  match the node byte-for-byte. Run:
  `cd anchor-decode-check && cargo run --bin anchor-decode-check`.
  A second binary, `blockdiag`, decodes a full block plus each transaction
  individually to locate where a decoder loses byte alignment (this is how
  the issuance denomination delta was found).
- `run-electrs-supervised.sh` / `run-electrs-testnet4.sh` / `env.sh`: deploy
  and toolchain helpers, described in the README.

## How the port was validated

- `anchor-decode-check` passes against a real header: parsed anchor height
  and hash, and the recomputed block hash, match `elementsd` exactly.
- `cargo test --features sequentia sequentia_cert` covers the certificate
  parser (including a shape that was also observed live on a real 51-member
  committee block on the pre-2026-07-05 chain).
- The Sequentia REST additions (`bitcoin_anchor`, `/sequentia/checkpoints`,
  `/sequentia/anchorstatus`, `/mempool/recent` fields, on-chain
  `/assets/registry`) were re-verified against the live public testnet API
  (https://sequentiatestnet.com/api) on 2026-07-08.
