// Decode a real Sequentia block header (captured from elementsd via
// `getblockheader <hash> false`) using the patched rust-elements `sequentia`
// feature, and assert the parsed Bitcoin anchor and the recomputed block hash
// match what the node reports. This validates that the header byte layout
// (anchor inserted after height, solution excluded from the hash) is correct.

use elements::encode::deserialize;
use elements::BlockHeader;

fn from_hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn main() {
    let hex = include_str!("../header.hex");
    let expected_blockhash = include_str!("../expected_blockhash.txt").trim();
    let mut exp_anchor = include_str!("../expected_anchor.txt").lines();
    let exp_anchor_height: u32 = exp_anchor.next().unwrap().trim().parse().unwrap();
    let exp_anchor_hash = exp_anchor.next().unwrap().trim();

    let bytes = from_hex(hex);
    let header: BlockHeader = deserialize(&bytes).expect("decode Sequentia header");

    let (ah, ahash) = header.bitcoin_anchor.expect("bitcoin_anchor present");
    let bhash = header.block_hash().to_string();
    let ahash_s = ahash.to_string();

    println!("version       = {:#010x}", header.version);
    println!("height        = {}", header.height);
    println!("anchor_height = {}  (expect {})", ah, exp_anchor_height);
    println!("anchor_hash   = {}  (expect {})", ahash_s, exp_anchor_hash);
    println!("block_hash    = {}  (expect {})", bhash, expected_blockhash);

    assert_eq!(ah, exp_anchor_height, "anchor height mismatch");
    assert_eq!(ahash_s, exp_anchor_hash, "anchor hash mismatch");
    assert_eq!(bhash, expected_blockhash, "BLOCK HASH MISMATCH");
    println!("\nPASS: Sequentia header decoded; anchor parsed; block hash matches the node.");
}
