// Diagnose the electrs `InvalidConfidentialPrefix(2)` panic: decode a full
// Sequentia block (header + txs) and each transaction individually, to locate
// where rust-elements loses byte alignment.

use elements::encode::deserialize;
use elements::{Block, BlockHeader, Transaction};

fn from_hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn main() {
    // Accept a block hex file as argv[1] (default block1.hex), tx file argv[2].
    let args: Vec<String> = std::env::args().collect();
    let block_path = args.get(1).cloned().unwrap_or_else(|| "block1.hex".into());
    let txs_path = args.get(2).cloned().unwrap_or_else(|| "block1_txs.txt".into());
    println!("== decoding {block_path} ==");
    let block_hex = std::fs::read_to_string(&block_path).expect("read block hex");
    let bytes = from_hex(&block_hex);
    println!("full block: {} bytes", bytes.len());

    // (1) Header decode from the front of the block (won't require full
    // consumption because we deserialize the prefix only via Block).
    match deserialize::<BlockHeader>(&bytes) {
        Ok(_) => println!("[unexpected] header consumed the WHOLE block (no txs?)"),
        Err(e) => println!("header-as-whole-block error (expected, txs follow): {e}"),
    }

    // (2) Full block decode — this is what electrs does and where it panics.
    match deserialize::<Block>(&bytes) {
        Ok(b) => println!(
            "[OK] full block decoded: {} txs, header height {}",
            b.txdata.len(),
            b.header.height
        ),
        Err(e) => println!("[FAIL] full block decode: {e}"),
    }

    // (3) Each tx individually (raw hex straight from getrawtransaction).
    let txs = std::fs::read_to_string(&txs_path).unwrap_or_default();
    for line in txs.lines() {
        let mut it = line.splitn(2, ' ');
        let tag = it.next().unwrap_or("");
        let hex = match it.next() {
            Some(h) => h,
            None => continue,
        };
        let tb = from_hex(hex);
        match deserialize::<Transaction>(&tb) {
            Ok(t) => println!(
                "[OK] {tag}: {} bytes -> txid {} ({} in, {} out)",
                tb.len(),
                t.txid(),
                t.input.len(),
                t.output.len()
            ),
            Err(e) => println!("[FAIL] {tag}: {} bytes -> {e}", tb.len()),
        }
    }
}
