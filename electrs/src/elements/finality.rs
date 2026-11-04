//! SEQUENTIA: how final a confirmed transaction is, and the asset its fee is paid in.
//!
//! A Sequentia block is final against every competing Sequentia block once the
//! staking committee has certified it, and it is never final against Bitcoin:
//! every block names the Bitcoin block it is anchored to, and if Bitcoin
//! reorganises that block away the Sequentia block leaves the chain with it, at
//! any depth. A transaction's status therefore carries both halves:
//!
//! - `bitcoin_anchor`: the Bitcoin block the transaction's block is anchored to,
//!   and its depth, counted in Bitcoin blocks from the anchor up to the one the
//!   newest Sequentia block is anchored to (both counted, so the newest anchor
//!   is depth 1). Read from the index alone.
//! - `certification`: whether the committee certified the block, as the node
//!   judges it (`getblockheader`: `poscertified`, `poscountersigs`,
//!   `posquorum`). The quorum depends on the stake registered before the block,
//!   which this indexer never sees, so it asks the node, once per block.

use std::collections::HashMap;
use std::sync::RwLock;

use elements::{AssetId, BlockHash, Transaction};
use serde_json::Value;


/// The Bitcoin block a Sequentia block is anchored to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AnchorStatus {
    pub height: u32,
    pub hash: BlockHash,
    /// Bitcoin blocks from this anchor up to the newest Sequentia block's
    /// anchor, both counted.
    pub depth: u32,
}

/// The staking committee's certification of a Sequentia block.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Certification {
    pub certified: bool,
    /// Committee members whose signatures the block's certificate carries.
    pub signers: u32,
    /// Signatures the block needed, when the node knows it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub quorum: Option<u32>,
}

/// Read a node `getblockheader` (verbose) answer. `None` on a chain without
/// proof of stake, whose headers carry no `poscertified`.
pub fn certification_from_header_json(h: &Value) -> Option<Certification> {
    let certified = h.get("poscertified")?.as_bool()?;
    let signers = h.get("poscountersigs").and_then(Value::as_u64).unwrap_or(0) as u32;
    let quorum = h.get("posquorum").and_then(Value::as_u64).map(|q| q as u32);
    Some(Certification { certified, signers, quorum })
}

/// Depth of an anchor at `anchor_height` when the newest block is anchored at
/// `tip_anchor_height`. Anchors never go backwards along a chain, so the tip's
/// is at least as high; a lower one (an index caught mid-reorg) reads as 1.
pub fn anchor_depth(anchor_height: u32, tip_anchor_height: u32) -> u32 {
    tip_anchor_height.saturating_sub(anchor_height) + 1
}

/// Certifications by block hash. A block's certification never changes once
/// the node knows its quorum (it is judged against the stake its parent
/// leaves), so those are kept; an answer without a quorum is asked again.
pub struct CertificationCache {
    known: RwLock<HashMap<BlockHash, Option<Certification>>>,
}

const CERTIFICATION_CACHE_MAX: usize = 100_000;

impl CertificationCache {
    pub fn new() -> Self {
        CertificationCache { known: RwLock::new(HashMap::new()) }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<Option<Certification>> {
        self.known.read().unwrap().get(hash).cloned()
    }

    pub fn put(&self, hash: BlockHash, c: Option<Certification>) {
        // Final answers only: no PoS on this chain, or a known quorum.
        if c.as_ref().map_or(true, |c| c.quorum.is_some()) {
            let mut known = self.known.write().unwrap();
            if known.len() >= CERTIFICATION_CACHE_MAX {
                known.clear();
            }
            known.insert(hash, c);
        }
    }
}

/// A block's anchor with its depth, given the newest block's anchor height.
pub fn anchor_status(anchor: Option<(u32, BlockHash)>, tip_anchor_height: Option<u32>) -> Option<AnchorStatus> {
    anchor.map(|(height, hash)| AnchorStatus {
        height,
        hash,
        depth: anchor_depth(height, tip_anchor_height.unwrap_or(height)),
    })
}

/// The fee a transaction pays, by asset, in the order its fee outputs appear.
/// The node relays only transactions paying one fee asset; a block may still
/// hold one paying several, so all are returned.
pub fn fees_by_asset(tx: &Transaction) -> Vec<(AssetId, u64)> {
    let mut fees: Vec<(AssetId, u64)> = vec![];
    for out in tx.output.iter().filter(|o| o.is_fee()) {
        // is_fee() means an explicit asset and value.
        let (asset, value) = match (out.asset.explicit(), out.value.explicit()) {
            (Some(a), Some(v)) => (a, v),
            _ => continue,
        };
        match fees.iter_mut().find(|(a, _)| *a == asset) {
            Some((_, total)) => *total += value,
            None => fees.push((asset, value)),
        }
    }
    fees
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn certification_reads_the_node_header() {
        let c = certification_from_header_json(&json!({
            "hash": "00", "poscountersigs": 3, "posquorum": 2, "poscertified": true
        }))
        .unwrap();
        assert_eq!(c, Certification { certified: true, signers: 3, quorum: Some(2) });

        // Below the quorum: not certified (an escaping-stall block).
        let c = certification_from_header_json(&json!({
            "poscountersigs": 1, "posquorum": 2, "poscertified": false
        }))
        .unwrap();
        assert!(!c.certified);

        // Quorum not known yet: reported, and not kept.
        let c = certification_from_header_json(&json!({"poscountersigs": 3, "poscertified": false})).unwrap();
        assert_eq!(c.quorum, None);
        let cache = CertificationCache::new();
        let h: BlockHash = "0000000000000000000000000000000000000000000000000000000000000001".parse().unwrap();
        cache.put(h, Some(c));
        assert_eq!(cache.get(&h), None);

        // A chain without proof of stake.
        assert_eq!(certification_from_header_json(&json!({"height": 5})), None);
        cache.put(h, None);
        assert_eq!(cache.get(&h), Some(None));
    }

    #[test]
    fn depth_counts_both_ends() {
        assert_eq!(anchor_depth(100, 100), 1);
        assert_eq!(anchor_depth(100, 102), 3);
        assert_eq!(anchor_depth(103, 102), 1);
    }

    #[test]
    fn anchor_status_and_unconfirmed_status_json() {
        let h: BlockHash = "0000000000000000000000000000000000000000000000000000000000000002".parse().unwrap();
        let a = anchor_status(Some((10, h)), Some(12)).unwrap();
        assert_eq!(serde_json::to_value(&a).unwrap(), json!({"height": 10, "hash": h.to_string(), "depth": 3}));
        assert_eq!(anchor_status(None, Some(12)), None);
        // An unconfirmed status carries neither field.
        let s = crate::util::TransactionStatus::from(None);
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({"confirmed": false}));
    }
}
