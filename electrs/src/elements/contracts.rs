//! SEQUENTIA: contracts. An index from a revealed leaf to the inputs that spend
//! it, and a decoder that says what each contract input did.
//!
//! A taproot script-path spend reveals one leaf: for a Simplicity leaf (leaf
//! version 0xbe) the program's 32-byte commitment root, for a tapscript leaf
//! (0xc4) the script. The index keys every confirmed script-path spend by the
//! leaf it reveals: the commitment root for a Simplicity leaf, the tapleaf hash
//! for a tapscript leaf. Its rows are written with the block's other history
//! rows and deleted with them when the block is reorganised away, so the index
//! follows the best chain, Bitcoin anchoring included, like every other index.
//!
//!     K{leaf id}{height}{txid}{vin} → ""
//!
//! The decoder names what a spend reveals, from the templates the registry has
//! verified (`--contract-registry-url`). A template keeps its parameters in a
//! data leaf beside its program, so its commitment root is one constant; the
//! spender supplies the parameters and storage slots as witness values, which
//! the program checks against that data leaf. The decoder compiles each
//! template's source with the pinned compiler, walks the compiled program beside
//! the pruned program the spend reveals to name every witness value, and then
//! recomputes the spent output's script from the template and those values. A
//! contract is named only when that recomputation gives the very script the
//! input spends, so a registry entry can never make a spend read as something
//! it is not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::{TryFrom, TryInto};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use bitcoin::hashes::{sha256, Hash, HashEngine};
use elements::{confidential, Transaction, TxOut};
use serde_json::{json, Value as Json};

use sequentia_contracts::descriptor::{Descriptor, Model, Node, Param, ScriptItem, SimplicityLeaf};
use sequentia_contracts::simplicity::jet::Elements;
use sequentia_contracts::simplicity::dag::DagLike;
use sequentia_contracts::simplicity::node::Inner;
use sequentia_contracts::simplicity::{BitIter, RedeemNode, Value as SimValue};
use sequentia_contracts::simplicityhl::ast::{self, ElementsJetHinter};
use sequentia_contracts::simplicityhl::named;
use sequentia_contracts::simplicityhl::types::{ResolvedType, TypeInner};
use sequentia_contracts::simplicityhl::value::StructuralValue;
use sequentia_contracts::simplicityhl::{Arguments, TemplateProgram, Value as HlValue, WitnessTypes};

use crate::new_index::db::DBRow;

pub const LEAF_SIMPLICITY: u8 = 0xbe;
pub const LEAF_TAPSCRIPT: u8 = 0xc4;
const INDEX_CODE: u8 = b'K';

/// Sequentia's Simplicity budget: `min(per_byte * w + 50, 4_000_050)` weight
/// units for an input whose witness stack, annex included, serializes to `w`
/// bytes. `per_byte` is 4 (src/script/interpreter.cpp, SIMPLICITY_BUDGET4);
/// the public testnet ran the 1-per-byte rule below its flag-day height.
const BUDGET_OFFSET: u64 = 50;
const BUDGET_MAX: u64 = 4_000_050;
/// The public testnet's genesis and the height from which its budget is 4 per
/// byte (chainparams.cpp, `simplicity_budget4_height`).
pub const TESTNET_GENESIS: &str = "ddd11d54c87a2bd94400fd31ce05d8e1110bb4b78e7103f738342086fc4ea92e";
pub const TESTNET_BUDGET4_HEIGHT: u32 = 101_810;

// ---------------------------------------------------------------------------
// The leaf a script-path spend reveals
// ---------------------------------------------------------------------------

/// A taproot script-path spend's revealed leaf.
#[derive(Debug, Clone)]
pub struct Revealed {
    pub version: u8,
    pub script: Vec<u8>,
    pub control: Vec<u8>,
    pub annex: Option<Vec<u8>>,
    /// The stack items below the script: for a Simplicity leaf, the witness
    /// bits then the program.
    pub args: Vec<Vec<u8>>,
}

impl Revealed {
    /// The index key's leaf id: the commitment root of a Simplicity leaf, the
    /// tapleaf hash of any other.
    pub fn leaf_id(&self) -> [u8; 32] {
        if self.version == LEAF_SIMPLICITY && self.script.len() == 32 {
            let mut id = [0u8; 32];
            id.copy_from_slice(&self.script);
            id
        } else {
            tapleaf_hash(self.version, &self.script)
        }
    }

    pub fn internal_key(&self) -> &[u8] {
        &self.control[1..33]
    }

    /// The sibling hashes from the leaf up to the root.
    pub fn path(&self) -> Vec<&[u8]> {
        self.control[33..].chunks(32).collect()
    }
}

/// Whether a script is a v1 witness program (P2TR).
pub fn is_p2tr(spk: &[u8]) -> bool {
    spk.len() == 34 && spk[0] == 0x51 && spk[1] == 0x20
}

/// The leaf a taproot script-path spend reveals, or None for a key-path spend
/// or a witness of another shape (BIP341: an annex is a last item of two or
/// more that starts with 0x50; the control block is then last).
pub fn revealed_leaf(witness: &[Vec<u8>]) -> Option<Revealed> {
    let mut stack: Vec<Vec<u8>> = witness.to_vec();
    let annex = if stack.len() >= 2 && stack.last().map_or(false, |a| a.first() == Some(&0x50)) {
        stack.pop()
    } else {
        None
    };
    if stack.len() < 2 {
        return None;
    }
    let control = stack.pop()?;
    let script = stack.pop()?;
    if control.len() < 33 || (control.len() - 33) % 32 != 0 || (control.len() - 33) / 32 > 128 {
        return None;
    }
    Some(Revealed { version: control[0] & 0xfe, script, control, annex, args: stack })
}

fn tagged(tag: &str) -> sha256::HashEngine {
    let t = sha256::Hash::hash(tag.as_bytes());
    let mut e = sha256::Hash::engine();
    e.input(t.as_ref());
    e.input(t.as_ref());
    e
}

fn compact_size(n: usize) -> Vec<u8> {
    match n {
        0..=0xfc => vec![n as u8],
        0xfd..=0xffff => {
            let mut v = vec![0xfd];
            v.extend_from_slice(&(n as u16).to_le_bytes());
            v
        }
        0x10000..=0xffff_ffff => {
            let mut v = vec![0xfe];
            v.extend_from_slice(&(n as u32).to_le_bytes());
            v
        }
        _ => {
            let mut v = vec![0xff];
            v.extend_from_slice(&(n as u64).to_le_bytes());
            v
        }
    }
}

pub fn tapleaf_hash(version: u8, script: &[u8]) -> [u8; 32] {
    let mut e = tagged("TapLeaf/elements");
    e.input(&[version]);
    e.input(&compact_size(script.len()));
    e.input(script);
    sha256::Hash::from_engine(e).to_byte_array()
}

pub fn tapdata_hash(data: &[u8]) -> [u8; 32] {
    let mut e = tagged("TapData");
    e.input(data);
    sha256::Hash::from_engine(e).to_byte_array()
}

/// The serialized size of a witness stack, as the node's budget counts it.
pub fn stack_size(stack: &[Vec<u8>]) -> usize {
    compact_size(stack.len()).len()
        + stack.iter().map(|i| compact_size(i.len()).len() + i.len()).sum::<usize>()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

pub fn index_key(leaf_id: &[u8; 32], height: u32, txid: &[u8], vin: u32) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + 32 + 4 + 32 + 4);
    k.push(INDEX_CODE);
    k.extend_from_slice(leaf_id);
    k.extend_from_slice(&height.to_be_bytes());
    k.extend_from_slice(txid);
    k.extend_from_slice(&vin.to_be_bytes());
    k
}

pub fn index_prefix(leaf_id: &[u8; 32]) -> Vec<u8> {
    let mut k = vec![INDEX_CODE];
    k.extend_from_slice(leaf_id);
    k
}

/// One row per script-path spend of a taproot output in a confirmed
/// transaction. `prevout` gives the spent output, or None when unknown.
pub fn index_rows<'a>(
    tx: &Transaction,
    txid: &[u8],
    height: u32,
    prevout: impl Fn(usize) -> Option<&'a TxOut>,
    rows: &mut Vec<DBRow>,
) {
    for (vin, txin) in tx.input.iter().enumerate() {
        if txin.is_coinbase() || txin.is_pegin {
            continue;
        }
        match prevout(vin) {
            Some(p) if is_p2tr(p.script_pubkey.as_bytes()) => (),
            _ => continue,
        }
        if let Some(leaf) = revealed_leaf(&txin.witness.script_witness) {
            rows.push(DBRow {
                key: index_key(&leaf.leaf_id(), height, txid, vin as u32),
                value: vec![leaf.version],
            });
        }
    }
}

/// A row read back: height, txid (internal byte order) and input index.
pub fn parse_index_key(key: &[u8]) -> Option<(u32, [u8; 32], u32)> {
    if key.len() != 1 + 32 + 4 + 32 + 4 {
        return None;
    }
    let height = u32::from_be_bytes(key[33..37].try_into().ok()?);
    let mut txid = [0u8; 32];
    txid.copy_from_slice(&key[37..69]);
    let vin = u32::from_be_bytes(key[69..73].try_into().ok()?);
    Some((height, txid, vin))
}

// ---------------------------------------------------------------------------
// Templates, from the registry
// ---------------------------------------------------------------------------

pub struct CompiledLeaf {
    pub name: String,
    pub program: SimplicityLeaf,
    pub commit: Arc<named::CommitNode>,
    pub witness_types: WitnessTypes,
    /// The program reads its own input index and does not require it to be 0:
    /// the rule that covenant input k owns outputs 2k and 2k+1.
    pub uses_2k: bool,
}

pub struct KnownTemplate {
    pub hash: String,
    pub name: String,
    pub version: u64,
    pub summary: String,
    pub model: Model,
    pub leaves: Vec<CompiledLeaf>,
}

#[derive(Default)]
pub struct Templates {
    pub by_hash: HashMap<String, Arc<KnownTemplate>>,
    /// A Simplicity root to the templates (and leaf index) it sits in.
    pub by_cmr: HashMap<[u8; 32], Vec<(Arc<KnownTemplate>, usize)>>,
    pub refused: HashMap<String, String>,
    /// The instances the registry lists, by script: the template hash, the
    /// parameters and the slots. A spend that does not reveal every term (a
    /// value only a tapscript leaf holds) is completed from these, and named
    /// only if the completed values recompute the script it spends.
    pub instances: HashMap<String, (String, BTreeMap<String, String>, BTreeMap<String, String>)>,
}

lazy_static! {
    pub static ref TEMPLATES: RwLock<Templates> = RwLock::new(Templates::default());
}

/// Compile a source (its includes resolved) to the program with its witness
/// nodes named.
pub fn compile_named(source: &str) -> Result<(Arc<named::CommitNode>, WitnessTypes), String> {
    let template = TemplateProgram::new(source, Box::new(ElementsJetHinter::new()))
        .map_err(|d| d.to_string())?;
    let program = ast::Program::analyze(template.resolved_program(), Box::new(ElementsJetHinter::new()))
        .map_err(|d| d.to_string())?;
    let commit = program
        .compile(Arguments::default(), false, Box::new(ElementsJetHinter::new()))
        .map_err(|d| d.to_string())?;
    Ok((commit, program.witness_types().shallow_clone()))
}

fn uses_2k(source: &str) -> bool {
    let s: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    s.contains("jet::current_index()") && !s.contains("eq_32(jet::current_index(),0)")
}

/// Build a template from its descriptor text and its leaves' sources (each
/// with its includes resolved), checking that every source compiles to the
/// root the descriptor names.
pub fn load_template(descriptor: &str, sources: &BTreeMap<String, String>) -> Result<KnownTemplate, String> {
    let d = Descriptor::parse(descriptor)?;
    let model = d.model()?;
    let t = &d.template;
    let mut leaves = Vec::new();
    for (node, _) in model.tree.leaves() {
        if let Node::Simplicity { name, program } = node {
            let source = sources
                .get(&program.source)
                .ok_or_else(|| format!("leaf {}: source {} is missing", name, program.source))?;
            let (commit, witness_types) = compile_named(source).map_err(|e| format!("leaf {}: {}", name, e))?;
            let cmr = hex(commit.cmr().as_ref());
            if cmr != program.cmr {
                return Err(format!("leaf {}: the source compiles to {}, the descriptor says {}", name, cmr, program.cmr));
            }
            leaves.push(CompiledLeaf {
                name: name.clone(),
                program: program.clone(),
                commit,
                witness_types,
                uses_2k: uses_2k(source),
            });
        }
    }
    Ok(KnownTemplate {
        hash: d.template_hash.clone(),
        name: t["name"].as_str().unwrap_or("").to_string(),
        version: t["version"].as_u64().unwrap_or(0),
        summary: t["summary"].as_str().unwrap_or("").to_string(),
        model,
        leaves,
    })
}

impl Templates {
    pub fn insert(&mut self, t: KnownTemplate) {
        let t = Arc::new(t);
        for (i, l) in t.leaves.iter().enumerate() {
            if let Some(cmr) = unhex(&l.program.cmr).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
                let list = self.by_cmr.entry(cmr).or_default();
                list.retain(|(o, _)| o.hash != t.hash);
                list.push((t.clone(), i));
            }
        }
        self.by_hash.insert(t.hash.clone(), t);
    }

    pub fn tapscript_templates(&self) -> Vec<Arc<KnownTemplate>> {
        self.by_hash
            .values()
            .filter(|t| t.model.tree.leaves().iter().any(|(n, _)| matches!(n, Node::Tapscript { .. })))
            .cloned()
            .collect()
    }
}

fn http_get(url: &str) -> Result<String, String> {
    let mut resp = ureq::get(url).call().map_err(|e| format!("{}: {}", url, e))?;
    resp.body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("{}: {}", url, e))
}

/// Read the registry's verified templates once: each one not yet held is
/// fetched, compiled and checked. Returns how many are held.
pub fn sync_registry(base: &str) -> Result<usize, String> {
    let base = base.trim_end_matches('/');
    let list: Json = serde_json::from_str(&http_get(&format!("{}/contracts", base))?).map_err(|e| e.to_string())?;
    let list = list.as_array().ok_or("the registry's /contracts is not a list")?;
    for s in list {
        let hash = match s["template_hash"].as_str() {
            Some(h) if h.len() == 64 => h.to_string(),
            _ => continue,
        };
        if s["verified"].as_bool() != Some(true) {
            continue;
        }
        {
            let held = TEMPLATES.read().unwrap();
            if held.by_hash.contains_key(&hash) || held.refused.contains_key(&hash) {
                continue;
            }
        }
        let entry: Json = match http_get(&format!("{}/contracts/{}", base, hash)).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
            Ok(e) => e,
            Err(e) => {
                warn!("contract template {}: {}", hash, e);
                continue;
            }
        };
        let descriptor = entry["descriptor"].as_str().unwrap_or("");
        let sources: BTreeMap<String, String> = entry["sources"]
            .as_object()
            .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
            .unwrap_or_default();
        match load_template(descriptor, &sources) {
            Ok(t) if t.hash == hash => {
                info!("contract template {} ({} v{}) loaded", hash, t.name, t.version);
                TEMPLATES.write().unwrap().insert(t);
            }
            Ok(t) => {
                TEMPLATES.write().unwrap().refused.insert(hash.clone(), format!("descriptor hash {}", t.hash));
            }
            Err(e) => {
                warn!("contract template {} refused: {}", hash, e);
                TEMPLATES.write().unwrap().refused.insert(hash, e);
            }
        }
    }
    // The registry's instances of every template held.
    let hashes: Vec<String> = TEMPLATES.read().unwrap().by_hash.keys().cloned().collect();
    let mut instances = HashMap::new();
    for hash in hashes {
        let list: Json = match http_get(&format!("{}/contracts/{}/instances", base, hash)).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
            Ok(l) => l,
            Err(e) => {
                warn!("contract instances of {}: {}", hash, e);
                continue;
            }
        };
        for i in list.as_array().into_iter().flatten() {
            let map = |v: &Json| -> BTreeMap<String, String> {
                v.as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default()
            };
            if let Some(spk) = i["script_pubkey"].as_str() {
                instances.insert(spk.to_string(), (hash.clone(), map(&i["params"]), map(&i["slots"])));
            }
        }
    }
    let mut held = TEMPLATES.write().unwrap();
    held.instances = instances;
    Ok(held.by_hash.len())
}

/// Keep the templates in step with the registry, once a minute.
pub fn spawn_registry_sync(base: String) {
    thread::Builder::new()
        .name("contract-registry".into())
        .spawn(move || loop {
            if let Err(e) = sync_registry(&base) {
                warn!("contract registry {}: {}", base, e);
            }
            thread::sleep(Duration::from_secs(60));
        })
        .expect("spawn contract-registry thread");
}

// ---------------------------------------------------------------------------
// Naming the witness values of a pruned program
// ---------------------------------------------------------------------------

/// Walk the compiled program and the program a spend reveals side by side: a
/// case the spend pruned is an assertion there, and each witness node of the
/// compiled program meets the value the spend gave it. Fails when the two are
/// not the same program.
pub fn name_witnesses(
    compiled: &Arc<named::CommitNode>,
    redeem: &Arc<RedeemNode>,
) -> Result<BTreeMap<String, SimValue>, String> {
    let mut out: BTreeMap<String, SimValue> = BTreeMap::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut stack = vec![(compiled.clone(), redeem.clone())];
    while let Some((c, r)) = stack.pop() {
        if !seen.insert((Arc::as_ptr(&c) as usize, Arc::as_ptr(&r) as usize)) {
            continue;
        }
        if c.cmr() != r.cmr() {
            return Err(format!("the programs differ at {} / {}", c.cmr(), r.cmr()));
        }
        match (c.inner(), r.inner()) {
            (Inner::Iden, Inner::Iden)
            | (Inner::Unit, Inner::Unit)
            | (Inner::Fail(_), Inner::Fail(_))
            | (Inner::Jet(_), Inner::Jet(_))
            | (Inner::Word(_), Inner::Word(_)) => {}
            (Inner::InjL(a), Inner::InjL(b))
            | (Inner::InjR(a), Inner::InjR(b))
            | (Inner::Take(a), Inner::Take(b))
            | (Inner::Drop(a), Inner::Drop(b)) => stack.push((a.clone(), b.clone())),
            (Inner::Comp(a1, a2), Inner::Comp(b1, b2))
            | (Inner::Pair(a1, a2), Inner::Pair(b1, b2))
            | (Inner::Case(a1, a2), Inner::Case(b1, b2)) => {
                stack.push((a1.clone(), b1.clone()));
                stack.push((a2.clone(), b2.clone()));
            }
            (Inner::Case(a1, _), Inner::AssertL(b1, _)) | (Inner::AssertL(a1, _), Inner::AssertL(b1, _)) => {
                stack.push((a1.clone(), b1.clone()))
            }
            (Inner::Case(_, a2), Inner::AssertR(_, b2)) | (Inner::AssertR(_, a2), Inner::AssertR(_, b2)) => {
                stack.push((a2.clone(), b2.clone()))
            }
            (Inner::Witness(name), Inner::Witness(value)) => {
                let name = name.as_inner().to_string();
                if let Some(prev) = out.get(&name) {
                    if prev != value {
                        return Err(format!("witness {} carries two values", name));
                    }
                }
                out.insert(name, value.shallow_clone());
            }
            _ => return Err("the programs differ in shape".into()),
        }
    }
    Ok(out)
}

/// A value's bytes when it is a word (an integer, a key, a signature).
pub fn word_bytes(v: &SimValue) -> Option<Vec<u8>> {
    let w = v.to_word()?;
    if w.len() % 8 != 0 {
        return None;
    }
    let bits: Vec<bool> = w.iter().collect();
    Some(bits.chunks(8).map(|c| c.iter().fold(0u8, |a, b| (a << 1) | (*b as u8))).collect())
}

/// Every integer word inside a value (a tuple's members, an Either's payload).
fn integer_words(v: &SimValue, out: &mut Vec<Vec<u8>>) {
    if let Some(b) = word_bytes(v) {
        if b.len() <= 8 {
            out.push(b);
        }
        return;
    }
    if let Some((l, r)) = v.as_product() {
        integer_words(&l.to_value(), out);
        integer_words(&r.to_value(), out);
    } else if let Some(l) = v.as_left() {
        integer_words(&l.to_value(), out);
    } else if let Some(r) = v.as_right() {
        integer_words(&r.to_value(), out);
    }
}

/// For a value of a nested `Either` type, the alternative taken, counting the
/// type's alternatives left to right without entering their payloads; and how
/// many alternatives there are.
fn either_alternative(ty: &ResolvedType, v: &SimValue) -> Option<(usize, usize)> {
    fn count(ty: &ResolvedType) -> usize {
        match ty.as_inner() {
            TypeInner::Either(l, r) => count(l) + count(r),
            _ => 1,
        }
    }
    fn index(ty: &ResolvedType, v: &SimValue) -> Option<usize> {
        match ty.as_inner() {
            TypeInner::Either(l, r) => {
                if let Some(x) = v.as_left() {
                    index(l, &x.to_value())
                } else {
                    Some(count(l) + index(r, &v.as_right()?.to_value())?)
                }
            }
            _ => Some(0),
        }
    }
    match ty.as_inner() {
        TypeInner::Either(..) => Some((index(ty, v)?, count(ty))),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Decoding one input
// ---------------------------------------------------------------------------

/// What the decoder needs to know about the chain.
#[derive(Clone, Copy)]
pub struct ChainRules {
    /// Weight units of budget per witness byte.
    pub budget_per_byte: u64,
}

impl ChainRules {
    pub fn for_height(genesis: Option<&str>, height: Option<u32>) -> Self {
        let old = genesis == Some(TESTNET_GENESIS) && height.map_or(false, |h| h < TESTNET_BUDGET4_HEIGHT);
        ChainRules { budget_per_byte: if old { 1 } else { 4 } }
    }
}

fn role_of<'a>(model: &'a Model, name: &str) -> Option<(&'a Param, &'static str)> {
    model
        .params
        .iter()
        .find(|p| p.name == name)
        .map(|p| (p, "param"))
        .or_else(|| model.slots.iter().find(|p| p.name == name).map(|p| (p, "slot")))
}

fn width_of(ty: &str) -> Option<usize> {
    sequentia_contracts::descriptor::type_width(ty)
}

fn value_json(p: &Param, hexv: &str) -> Json {
    json!({ "name": p.name, "type": p.ty, "role": p.role, "label": p.label, "hex": hexv })
}

/// A value the program computes rather than reads, which a known template
/// documents: the pool split's table hash is the hash of the rows it is given
/// (crates/sequentia-contracts/src/pool.rs, `table_hash`). The result is used
/// only if recomputing the output from it gives the spent script.
fn derived_values(t: &KnownTemplate, named: &BTreeMap<String, SimValue>, values: &mut BTreeMap<String, String>) {
    if t.model.params.iter().any(|p| p.name == "TABLE") && !values.contains_key("TABLE") {
        if let Some(rows) = named.get("ROWS") {
            // ROWS is a bounded list of (u256 script hash, u32 numerator).
            let mut words = Vec::new();
            fn tuples(v: &SimValue, out: &mut Vec<Vec<u8>>) {
                if let Some(b) = word_bytes(v) {
                    out.push(b);
                    return;
                }
                if let Some((l, r)) = v.as_product() {
                    tuples(&l.to_value(), out);
                    tuples(&r.to_value(), out);
                } else if let Some(l) = v.as_left() {
                    tuples(&l.to_value(), out);
                } else if let Some(r) = v.as_right() {
                    tuples(&r.to_value(), out);
                }
            }
            tuples(rows, &mut words);
            let mut e = sha256::Hash::engine();
            let mut i = 0;
            while i + 1 < words.len() {
                if words[i].len() == 32 && words[i + 1].len() == 4 {
                    e.input(&words[i]);
                    e.input(&words[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            values.insert("TABLE".into(), hex(sha256::Hash::from_engine(e).as_ref()));
        }
    }
}

/// A Simplicity input decoded against one template leaf.
struct Named {
    template: Arc<KnownTemplate>,
    leaf: usize,
    named: BTreeMap<String, SimValue>,
    params: BTreeMap<String, String>,
    slots: BTreeMap<String, String>,
    recomputed: bool,
    /// The terms read from the registry's instance record, not the spend.
    from_registry: Vec<String>,
}

fn try_template(t: &Arc<KnownTemplate>, li: usize, redeem: &Arc<RedeemNode>, spk: &[u8]) -> Option<Named> {
    let leaf = &t.leaves[li];
    let named = name_witnesses(&leaf.commit, redeem).ok()?;
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for w in &leaf.program.witness {
        let target = w.source.strip_prefix("param:").or_else(|| w.source.strip_prefix("slot:"));
        if let (Some(target), Some(v)) = (target, named.get(&w.name)) {
            if let Some(b) = word_bytes(v) {
                values.insert(target.to_string(), hex(&b));
            }
        }
    }
    derived_values(t, &named, &mut values);
    let mut params: BTreeMap<String, String> =
        t.model.params.iter().filter_map(|p| Some((p.name.clone(), values.get(&p.name)?.clone()))).collect();
    let mut slots: BTreeMap<String, String> =
        t.model.slots.iter().filter_map(|p| Some((p.name.clone(), values.get(&p.name)?.clone()))).collect();
    let complete = |p: &BTreeMap<String, String>, s: &BTreeMap<String, String>| {
        p.len() == t.model.params.len() && s.len() == t.model.slots.len()
    };
    let mut recomputed = complete(&params, &slots)
        && t.model.derive(&params, &slots).map_or(false, |x| x.script_pubkey == hex(spk));
    let mut from_registry = Vec::new();
    if !recomputed {
        // Terms this spend does not reveal, from the registry's record of the
        // instance whose script it spends; every revealed value must agree.
        let held = TEMPLATES.read().unwrap();
        if let Some((th, rp, rs)) = held.instances.get(&hex(spk)) {
            let agrees = |mine: &BTreeMap<String, String>, theirs: &BTreeMap<String, String>| {
                mine.iter().all(|(k, v)| theirs.get(k).map_or(true, |r| r == v))
            };
            if *th == t.hash && agrees(&params, rp) && agrees(&slots, rs) {
                let mut p2 = params.clone();
                let mut s2 = slots.clone();
                for (k, v) in rp {
                    if !p2.contains_key(k) {
                        p2.insert(k.clone(), v.clone());
                        from_registry.push(k.clone());
                    }
                }
                for (k, v) in rs {
                    if !s2.contains_key(k) {
                        s2.insert(k.clone(), v.clone());
                        from_registry.push(k.clone());
                    }
                }
                if complete(&p2, &s2) && t.model.derive(&p2, &s2).map_or(false, |x| x.script_pubkey == hex(spk)) {
                    params = p2;
                    slots = s2;
                    recomputed = true;
                } else {
                    from_registry.clear();
                }
            }
        }
    }
    Some(Named { template: t.clone(), leaf: li, named, params, slots, recomputed, from_registry })
}

/// The parameters as the instance key: each parameter's bytes in the
/// template's order, hex.
fn params_key(model: &Model, params: &BTreeMap<String, String>) -> Option<String> {
    model.params.iter().map(|p| params.get(&p.name).cloned()).collect::<Option<Vec<_>>>().map(|v| v.concat())
}

pub fn template_json(t: &KnownTemplate, leaf: &str) -> Json {
    json!({ "hash": t.hash, "name": t.name, "version": t.version, "summary": t.summary, "leaf": leaf })
}

fn paths_of(t: &KnownTemplate, leaf: &str) -> Vec<Json> {
    t.model
        .paths
        .iter()
        .filter(|p| p.leaf.as_deref() == Some(leaf))
        .map(|p| json!({ "name": p.name, "who": p.who, "effect": p.effect }))
        .collect()
}

/// Everything the decoder found about one input; `None` when it is not a
/// script-path spend of a taproot output.
pub struct Decoded {
    pub json: Json,
    /// For a recognised instance: the template, the parameters and the slots,
    /// so that the outputs can be read as its successors.
    pub instance: Option<(Arc<KnownTemplate>, BTreeMap<String, String>, BTreeMap<String, String>, bool)>,
}

pub fn decode_input(tx: &Transaction, vin: usize, prevout: &TxOut, rules: ChainRules) -> Option<Decoded> {
    let txin = tx.input.get(vin)?;
    let spk = prevout.script_pubkey.as_bytes();
    if !is_p2tr(spk) {
        return None;
    }
    let leaf = revealed_leaf(&txin.witness.script_witness)?;
    let leaf_id = leaf.leaf_id();
    let stack = &txin.witness.script_witness;
    let w = stack_size(stack) as u64;
    let w_no_annex = match &leaf.annex {
        Some(_) => stack_size(&stack[..stack.len() - 1]) as u64,
        None => w,
    };
    let budget = |bytes: u64| (bytes * rules.budget_per_byte + BUDGET_OFFSET).min(BUDGET_MAX);
    let mut j = json!({
        "vin": vin,
        "leaf_id": hex(&leaf_id),
        "leaf_version": leaf.version,
        "internal_key": hex(leaf.internal_key()),
        "merkle_path": leaf.path().iter().map(|h| hex(h)).collect::<Vec<_>>(),
        "annex": leaf.annex.as_ref().map(|a| json!({ "bytes": a.len(), "padding": a.len().saturating_sub(1) })),
        "witness_stack_bytes": w,
    });
    if leaf.version == LEAF_SIMPLICITY && leaf.script.len() == 32 && leaf.args.len() == 2 {
        j["kind"] = json!("simplicity");
        j["cmr"] = json!(hex(&leaf.script));
        let (wit_bits, program) = (&leaf.args[0], &leaf.args[1]);
        j["program_bytes"] = json!(program.len());
        j["witness_bytes"] = json!(wit_bits.len());
        j["budget"] = json!({
            "rule": { "per_witness_byte": rules.budget_per_byte, "offset": BUDGET_OFFSET, "max": BUDGET_MAX },
            "wu": budget(w),
            "wu_without_annex": budget(w_no_annex),
        });
        let redeem = match RedeemNode::decode::<_, _, Elements>(
            BitIter::from(program.iter().copied()),
            BitIter::from(wit_bits.iter().copied()),
        ) {
            Ok(r) => r,
            Err(e) => {
                j["decode_error"] = json!(e.to_string());
                return Some(Decoded { json: j, instance: None });
            }
        };
        if redeem.cmr().as_ref() != &leaf.script[..] {
            j["decode_error"] = json!("the program's root is not the leaf's");
            return Some(Decoded { json: j, instance: None });
        }
        let cost_milli: u64 = redeem.bounds().cost.to_string().parse().unwrap_or(0);
        j["cost"] = json!({ "milliwu": cost_milli, "wu": (cost_milli + 999) / 1000 });
        j["within_budget"] = json!(cost_milli <= budget(w) * 1000);

        let mut cmr = [0u8; 32];
        cmr.copy_from_slice(&leaf.script);
        let candidates = TEMPLATES.read().unwrap().by_cmr.get(&cmr).cloned().unwrap_or_default();
        let found: Vec<Named> = candidates.iter().filter_map(|(t, li)| try_template(t, *li, &redeem, spk)).collect();
        let pick = found.iter().position(|n| n.recomputed).or(if found.len() == 1 { Some(0) } else { None });
        match pick.map(|i| &found[i]) {
            Some(n) => {
                let t = &n.template;
                let cl = &t.leaves[n.leaf];
                j["template"] = template_json(t, &cl.name);
                j["max_cost_wu"] = json!(cl.program.max_cost_wu);
                j["uses_2k"] = json!(cl.uses_2k);
                j["recomputed"] = json!(n.recomputed);
                if !n.from_registry.is_empty() {
                    j["terms_from_registry"] = json!(n.from_registry);
                }
                let paths = paths_of(t, &cl.name);
                let mut path = if paths.len() == 1 { Some(paths[0].clone()) } else { None };
                let mut witness = Vec::new();
                for wv in &cl.program.witness {
                    let mut e = json!({ "name": wv.name, "type": wv.ty, "source": wv.source });
                    match n.named.get(&wv.name) {
                        Some(v) => {
                            if let Some(b) = word_bytes(v) {
                                e["hex"] = json!(hex(&b));
                            }
                            let name = sequentia_contracts::simplicityhl::str::WitnessName::from_str_unchecked(&wv.name);
                            if let Some(ty) = cl.witness_types.get(&name) {
                                if let Some(hv) = HlValue::reconstruct(&StructuralValue::from(v.shallow_clone()), ty) {
                                    e["value"] = json!(hv.to_string());
                                    if path.is_none() && wv.source == "spender" {
                                        if let Some((i, count)) = either_alternative(ty, v) {
                                            if count == paths.len() {
                                                path = Some(paths[i].clone());
                                                e["selects_path"] = json!(true);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        None => e["pruned"] = json!(true),
                    }
                    witness.push(e);
                }
                j["witness"] = json!(witness);
                j["path"] = match path {
                    Some(p) => p,
                    None => json!({ "candidates": paths }),
                };
                j["terms"] = json!(t.model.params.iter().filter_map(|p| Some(value_json(p, n.params.get(&p.name)?))).collect::<Vec<_>>());
                j["state"] = json!(t.model.slots.iter().filter_map(|p| Some(value_json(p, n.slots.get(&p.name)?))).collect::<Vec<_>>());
                if let Some(key) = params_key(&t.model, &n.params) {
                    j["instance"] = json!(key);
                }
                // The data leaf beside the program: path[0] when they share a branch.
                if let Some(sibling) = leaf.path().first() {
                    for (node, _) in t.model.tree.leaves() {
                        if let Node::Data { name, values } = node {
                            let all: BTreeMap<String, String> = n.params.iter().chain(n.slots.iter()).map(|(k, v)| (k.clone(), v.clone())).collect();
                            let bytes: Option<Vec<u8>> = values.iter().map(|v| unhex(all.get(v)?)).collect::<Option<Vec<_>>>().map(|v| v.concat());
                            if let Some(bytes) = bytes {
                                if tapdata_hash(&bytes) == *sibling {
                                    j["data_leaf"] = json!({ "name": name, "hash": hex(sibling), "matches": true });
                                }
                            }
                        }
                    }
                }
                let inst = (t.clone(), n.params.clone(), n.slots.clone(), cl.uses_2k);
                return Some(Decoded { json: j, instance: if n.recomputed { Some(inst) } else { None } });
            }
            None => {
                if found.len() > 1 {
                    j["candidates"] = json!(found.iter().map(|n| template_json(&n.template, &n.template.leaves[n.leaf].name)).collect::<Vec<_>>());
                }
                // Unregistered: the witness values in the order the program reads them.
                let mut raw = Vec::new();
                let mut seen = HashSet::new();
                for node in (&*redeem).post_order_iter::<sequentia_contracts::simplicity::dag::InternalSharing>() {
                    if let Inner::Witness(v) = node.node.inner() {
                        if seen.insert(node.node as *const _ as usize) {
                            raw.push(match word_bytes(v) {
                                Some(b) => json!({ "type": v.ty().to_string(), "hex": hex(&b) }),
                                None => json!({ "type": v.ty().to_string(), "value": v.to_string() }),
                            });
                        }
                    }
                }
                j["witness_values"] = json!(raw);
            }
        }
        return Some(Decoded { json: j, instance: None });
    }
    // A tapscript leaf: recognised by matching the script against each
    // template's tapscript leaves, its parameters read from their places.
    j["kind"] = json!(if leaf.version == LEAF_TAPSCRIPT { "tapscript" } else { "other" });
    j["script"] = json!(hex(&leaf.script));
    j["stack"] = json!(leaf.args.iter().map(|a| hex(a)).collect::<Vec<_>>());
    if leaf.version == LEAF_TAPSCRIPT {
        let templates = TEMPLATES.read().unwrap().tapscript_templates();
        for t in templates {
            for (node, _) in t.model.tree.leaves() {
                if let Node::Tapscript { name, items } = node {
                    if let Some(values) = match_tapscript(&t.model, items, &leaf.script) {
                        j["template"] = template_json(&t, name);
                        let paths = paths_of(&t, name);
                        j["path"] = if paths.len() == 1 { paths[0].clone() } else { json!({ "candidates": paths }) };
                        j["terms"] = json!(t.model.params.iter().filter_map(|p| Some(value_json(p, values.get(&p.name)?))).collect::<Vec<_>>());
                        return Some(Decoded { json: j, instance: None });
                    }
                }
            }
        }
    }
    Some(Decoded { json: j, instance: None })
}

/// Read a tapscript leaf's parameters out of a script, if it is that leaf; the
/// script is rebuilt from what was read and must come out the same.
pub fn match_tapscript(model: &Model, items: &[ScriptItem], script: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut at = 0usize;
    for item in items {
        match item {
            ScriptItem::Bytes(b) => {
                if script.get(at..at + b.len())? != &b[..] {
                    return None;
                }
                at += b.len();
            }
            ScriptItem::Push(p) => {
                let width = width_of(&role_of(model, p)?.0.ty)?;
                if *script.get(at)? as usize != width {
                    return None;
                }
                values.insert(p.clone(), hex(script.get(at + 1..at + 1 + width)?));
                at += 1 + width;
            }
            ScriptItem::Num(p) => {
                let width = width_of(&role_of(model, p)?.0.ty)?;
                let op = *script.get(at)?;
                let v: u64 = if op == 0 {
                    at += 1;
                    0
                } else if (0x51..=0x60).contains(&op) {
                    at += 1;
                    (op - 0x50) as u64
                } else if (1..=9).contains(&op) {
                    let b = script.get(at + 1..at + 1 + op as usize)?;
                    at += 1 + op as usize;
                    b.iter().rev().fold(0u64, |a, x| (a << 8) | *x as u64)
                } else {
                    return None;
                };
                let be = v.to_be_bytes();
                if width < 8 && be[..8 - width].iter().any(|x| *x != 0) {
                    return None;
                }
                values.insert(p.clone(), hex(&be[8 - width..]));
            }
        }
    }
    if at != script.len() {
        return None;
    }
    let rebuilt = model.script(items, &values).ok()?;
    if rebuilt != script {
        return None;
    }
    Some(values)
}

// ---------------------------------------------------------------------------
// Successors: the outputs that carry a decoded input's contract on
// ---------------------------------------------------------------------------

fn explicit_value(v: &confidential::Value) -> Option<u64> {
    match v {
        confidential::Value::Explicit(x) => Some(*x),
        _ => None,
    }
}

fn fit(v: u128, width: usize) -> Option<String> {
    if width > 16 || (width < 16 && v >> (8 * width) != 0) {
        return None;
    }
    let be = v.to_be_bytes();
    Some(hex(&be[16 - width..]))
}

fn as_u128(hexv: &str) -> Option<u128> {
    let b = unhex(hexv)?;
    if b.len() > 16 {
        return None;
    }
    Some(b.iter().fold(0u128, |a, x| (a << 8) | *x as u128))
}

/// The values each slot may take in a successor: its value now, zero and one,
/// the transaction's lock time and the input's relative lock, every integer the
/// spend's witness carries, and the old value plus or minus each explicit
/// amount the transaction moves. A candidate counts only if recomputing the
/// output from it gives an output's script.
fn slot_candidates(
    p: &Param,
    old: &str,
    tx: &Transaction,
    vin: usize,
    named: &[Vec<u8>],
) -> Vec<String> {
    let width = match width_of(&p.ty) {
        Some(w) if w <= 16 => w,
        _ => return vec![old.to_string()],
    };
    let mut nums: Vec<u128> = vec![0, 1, tx.lock_time.to_consensus_u32() as u128];
    let seq = tx.input[vin].sequence.0;
    nums.push((seq & 0xffff) as u128);
    for b in named {
        nums.push(b.iter().fold(0u128, |a, x| (a << 8) | *x as u128));
    }
    let mut amounts: Vec<u128> = tx.output.iter().filter_map(|o| explicit_value(&o.value)).map(u128::from).collect();
    for i in &tx.input {
        if let Some(a) = explicit_value(&i.asset_issuance.amount) {
            if a > 0 {
                amounts.push(a as u128);
            }
        }
    }
    amounts.push((seq & 0xffff) as u128);
    amounts.push(1);
    let mut out: Vec<String> = vec![old.to_string()];
    if let Some(o) = as_u128(old) {
        for a in &amounts {
            nums.push(o.wrapping_add(*a));
            if *a <= o {
                nums.push(o - a);
            }
        }
    }
    nums.extend(amounts);
    for n in nums {
        if let Some(h) = fit(n, width) {
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
    out
}

const MAX_SUCCESSOR_TRIES: usize = 40_000;

/// For each output of `tx` that continues the decoded input's contract: the
/// output index and the slots it holds.
pub fn successors(
    tx: &Transaction,
    vin: usize,
    t: &KnownTemplate,
    params: &BTreeMap<String, String>,
    slots: &BTreeMap<String, String>,
    witness: &BTreeMap<String, SimValue>,
) -> Vec<(usize, BTreeMap<String, String>, &'static str)> {
    let spks: HashMap<String, Vec<usize>> = tx.output.iter().enumerate().fold(HashMap::new(), |mut m, (i, o)| {
        if is_p2tr(o.script_pubkey.as_bytes()) {
            m.entry(hex(o.script_pubkey.as_bytes())).or_default().push(i);
        }
        m
    });
    let mut out = Vec::new();
    if spks.is_empty() {
        return out;
    }
    if let Ok(same) = t.model.derive(params, slots) {
        for i in spks.get(&same.script_pubkey).cloned().unwrap_or_default() {
            out.push((i, slots.clone(), "same"));
        }
    }
    if t.model.slots.is_empty() || !out.is_empty() {
        return out;
    }
    let mut words = Vec::new();
    for v in witness.values() {
        integer_words(v, &mut words);
    }
    let cands: Vec<Vec<String>> = t
        .model
        .slots
        .iter()
        .map(|p| slot_candidates(p, slots.get(&p.name).map(String::as_str).unwrap_or(""), tx, vin, &words))
        .collect();
    let total: usize = cands.iter().map(Vec::len).product();
    if total == 0 || total > MAX_SUCCESSOR_TRIES {
        return out;
    }
    let mut idx = vec![0usize; cands.len()];
    for _ in 0..total {
        let s: BTreeMap<String, String> =
            t.model.slots.iter().zip(idx.iter()).enumerate().map(|(k, (p, i))| (p.name.clone(), cands[k][*i].clone())).collect();
        if s != *slots {
            if let Ok(x) = t.model.derive(params, &s) {
                if let Some(is) = spks.get(&x.script_pubkey) {
                    for i in is {
                        out.push((*i, s.clone(), "recomputed"));
                    }
                }
            }
        }
        for k in 0..idx.len() {
            idx[k] += 1;
            if idx[k] < cands[k].len() {
                break;
            }
            idx[k] = 0;
        }
    }
    out
}

/// The witness values of a decoded instance input, again, for the successor
/// search.
fn named_values(t: &KnownTemplate, tx: &Transaction, vin: usize) -> BTreeMap<String, SimValue> {
    let txin = &tx.input[vin];
    let leaf = match revealed_leaf(&txin.witness.script_witness) {
        Some(l) if l.args.len() == 2 => l,
        _ => return BTreeMap::new(),
    };
    let redeem = match RedeemNode::decode::<_, _, Elements>(
        BitIter::from(leaf.args[1].iter().copied()),
        BitIter::from(leaf.args[0].iter().copied()),
    ) {
        Ok(r) => r,
        Err(_) => return BTreeMap::new(),
    };
    for l in &t.leaves {
        if l.program.cmr == hex(&leaf.script) {
            if let Ok(n) = name_witnesses(&l.commit, &redeem) {
                return n;
            }
        }
    }
    BTreeMap::new()
}

/// A whole transaction: each input decoded, and each output that carries an
/// input's contract on, with the state it holds.
pub fn decode_tx(tx: &Transaction, prevouts: &[Option<TxOut>], rules: ChainRules) -> Json {
    let mut inputs = Vec::new();
    let mut outputs: Vec<Json> = vec![Json::Null; tx.output.len()];
    for vin in 0..tx.input.len() {
        let d = match prevouts.get(vin).and_then(|p| p.as_ref()) {
            Some(p) => decode_input(tx, vin, p, rules),
            None => None,
        };
        let d = match d {
            Some(d) => d,
            None => {
                inputs.push(Json::Null);
                continue;
            }
        };
        let mut j = d.json;
        if let Some((t, params, slots, uses_2k)) = &d.instance {
            let named = named_values(t, tx, vin);
            let found = successors(tx, vin, t, params, slots, &named);
            let mut succ = Vec::new();
            for (vout, s, how) in &found {
                let state: Vec<Json> = t.model.slots.iter().filter_map(|p| Some(value_json(p, s.get(&p.name)?))).collect();
                succ.push(json!({ "vout": vout, "how": how, "state": state }));
                if outputs[*vout].is_null() {
                    outputs[*vout] = json!({
                        "successor_of": vin,
                        "leaf_id": j["leaf_id"].clone(),
                        "template": j["template"].clone(),
                        "instance": j["instance"].clone(),
                        "state": state,
                        "how": how,
                    });
                }
            }
            j["successors"] = json!(succ);
            if *uses_2k {
                j["owned_outputs"] = json!([2 * vin, 2 * vin + 1].iter().filter(|o| **o < tx.output.len()).collect::<Vec<_>>());
            }
        }
        inputs.push(j);
    }
    json!({ "inputs": inputs, "outputs": outputs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revealed_leaf_reads_annex_and_control() {
        let control = {
            let mut c = vec![0xbe | 1];
            c.extend([2u8; 32]);
            c.extend([3u8; 32]);
            c
        };
        let w = vec![vec![1], vec![2], vec![9u8; 32], control.clone(), vec![0x50, 0, 0]];
        let l = revealed_leaf(&w).unwrap();
        assert_eq!(l.version, 0xbe);
        assert_eq!(l.annex.as_deref(), Some(&[0x50u8, 0, 0][..]));
        assert_eq!(l.args.len(), 2);
        assert_eq!(l.leaf_id(), [9u8; 32]);
        assert_eq!(l.path().len(), 1);
        // A key-path spend reveals nothing.
        assert!(revealed_leaf(&[vec![0u8; 64]]).is_none());
        // A control block of a bad length is no script-path spend.
        assert!(revealed_leaf(&[vec![1], vec![0u8; 32], vec![0xbe; 40]]).is_none());
    }

    #[test]
    fn index_key_round_trips() {
        let k = index_key(&[7u8; 32], 1234, &[5u8; 32], 3);
        assert_eq!(&k[..33], &index_prefix(&[7u8; 32])[..]);
        assert_eq!(parse_index_key(&k), Some((1234, [5u8; 32], 3)));
    }

    // The templates of sequentia-contracts at the pinned commit, as the
    // registry holds them: the descriptor and each leaf's source with its
    // includes resolved.
    fn template_from_contracts(dir: &str) -> Result<KnownTemplate, String> {
        let root = sequentia_contracts::helpers_dir().join("../templates").join(dir);
        let descriptor = std::fs::read_to_string(root.join("descriptor.json")).map_err(|e| e.to_string())?;
        let d = Descriptor::parse(&descriptor)?;
        let mut sources = BTreeMap::new();
        for (node, _) in d.model()?.tree.leaves() {
            if let Node::Simplicity { program, .. } = node {
                let text = std::fs::read_to_string(root.join(&program.source)).map_err(|e| e.to_string())?;
                sources.insert(program.source.clone(), sequentia_contracts::expand(&text)?);
            }
        }
        load_template(&descriptor, &sources)
    }

    struct Fixture {
        tx: Transaction,
        prevouts: Vec<Option<TxOut>>,
    }

    // Transactions recorded from the contracts harness on regtest
    // (tests/fixtures/contracts): each with the transactions it spends from.
    fn fixtures(file: &str) -> Vec<Fixture> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contracts").join(file);
        let list: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let tx: Transaction = elements::encode::deserialize(&unhex(f["tx"].as_str().unwrap()).unwrap()).unwrap();
                let prevouts = tx
                    .input
                    .iter()
                    .map(|i| {
                        let raw = f["prevtxs"][i.previous_output.txid.to_string()].as_str()?;
                        let p: Transaction = elements::encode::deserialize(&unhex(raw).unwrap()).unwrap();
                        p.output.get(i.previous_output.vout as usize).cloned()
                    })
                    .collect();
                Fixture { tx, prevouts }
            })
            .collect()
    }

    fn state(j: &Json) -> Vec<(String, u128)> {
        j.as_array()
            .unwrap()
            .iter()
            .map(|v| (v["name"].as_str().unwrap().to_string(), as_u128(v["hex"].as_str().unwrap()).unwrap()))
            .collect()
    }

    const RULES: ChainRules = ChainRules { budget_per_byte: 4 };

    // One test, as the templates are process-wide.
    #[test]
    fn decodes_regtest_contract_spends() {
        // Before any template is held, a spend is decoded unnamed.
        let vault = fixtures("vault.json");
        let j = decode_tx(&vault[0].tx, &vault[0].prevouts, RULES);
        let i = &j["inputs"][0];
        assert_eq!(i["kind"], "simplicity");
        assert!(i["template"].is_null());
        assert_eq!(i["cost"]["wu"], 501);
        assert_eq!(i["budget"]["wu"], 5178);
        assert_eq!(i["witness_values"].as_array().unwrap().len(), 15);

        // A source that does not compile to the descriptor's root is refused.
        let root = sequentia_contracts::helpers_dir().join("../templates");
        let descriptor = std::fs::read_to_string(root.join("pignus_vault/descriptor.json")).unwrap();
        let wrong = sequentia_contracts::expand(&std::fs::read_to_string(root.join("faucet_drip/faucet_drip.simf")).unwrap()).unwrap();
        let mut sources = BTreeMap::new();
        sources.insert("pignus_vault.simf".to_string(), wrong);
        let err = load_template(&descriptor, &sources).err().unwrap();
        assert!(err.contains("the source compiles to"), "{}", err);

        for t in &["pignus_vault", "pool_split", "faucet_drip", "one_key"] {
            TEMPLATES.write().unwrap().insert(template_from_contracts(t).unwrap());
        }

        // The vault: a top-up keeps its state; a partial repay lowers the debt
        // by the lender's payment; a partial liquidation lowers it and moves
        // the last attestation time to the attestation's.
        let expect = [
            ("top_up", 501, 5178, "same", 210_000_000_000_000u128, 1_792_519_233u128),
            ("repay", 751, 6758, "recomputed", 160_000_000_000_000, 1_792_519_233),
            ("liquidate", 1653, 13466, "recomputed", 54_444_444_444_444, 1_792_519_415),
        ];
        for (f, (path, cost, budget, how, debt, last)) in vault.iter().zip(expect.iter()) {
            let j = decode_tx(&f.tx, &f.prevouts, RULES);
            let i = &j["inputs"][0];
            assert_eq!(i["template"]["name"], "pignus/vault");
            assert_eq!(i["path"]["name"], *path);
            assert_eq!(i["recomputed"], true);
            assert_eq!(i["cost"]["wu"], *cost);
            assert_eq!(i["budget"]["wu"], *budget);
            assert_eq!(i["data_leaf"]["matches"], true);
            assert_eq!(i["owned_outputs"], json!([0, 1]));
            assert_eq!(i["successors"][0]["vout"], 0);
            assert_eq!(i["successors"][0]["how"], *how);
            assert_eq!(state(&i["successors"][0]["state"]), vec![("DEBT_OUT".to_string(), *debt), ("LAST_TIME".to_string(), *last)]);
            assert_eq!(j["outputs"][0]["successor_of"], 0);
            assert_eq!(i["terms"].as_array().unwrap().len(), 12);
            assert!(i["witness"].as_array().unwrap().iter().any(|w| w["name"] == "PATH" && w["selects_path"] == true));
        }

        // The pool split: the table hash the program computes from the rows
        // is derived, and the output recomputes from it.
        let pool = fixtures("pool_split.json");
        let j = decode_tx(&pool[0].tx, &pool[0].prevouts, RULES);
        let i = &j["inputs"][0];
        assert_eq!(i["template"]["name"], "sequentia/pool-split");
        assert_eq!(i["path"]["name"], "claim");
        assert_eq!(i["recomputed"], true);
        assert_eq!(i["cost"]["wu"], 957);
        assert_eq!(i["budget"]["wu"], 9950);
        assert_eq!(i["terms"][0]["name"], "TABLE");
        assert_eq!(i["terms"][0]["hex"], "00c60fe602daa31b97ef51231d2a5a5c4655fb3721c4683e3eb395ead02a8ad3");

        // The drip reveals neither the recovery delay nor the treasury key, so
        // it is named but not recomputed, and has no instance ...
        let drip = fixtures("drip.json");
        let spk = hex(drip[0].prevouts[0].as_ref().unwrap().script_pubkey.as_bytes());
        let j = decode_tx(&drip[0].tx, &drip[0].prevouts, RULES);
        let i = &j["inputs"][0];
        assert_eq!(i["template"]["name"], "sequentia/faucet-drip");
        assert_eq!(i["recomputed"], false);
        assert!(i["instance"].is_null());
        assert_eq!(i["cost"]["milliwu"], 422_597);
        assert_eq!(i["budget"]["wu"], 5466);
        // ... a registry record with another treasury key does not complete it ...
        let drip_hash = "12986f202fbfb850f7699c5d5188f261f276de6c7038142f0a28bbb672b5af34".to_string();
        let mut params: BTreeMap<String, String> = i["terms"].as_array().unwrap().iter()
            .map(|v| (v["name"].as_str().unwrap().to_string(), v["hex"].as_str().unwrap().to_string())).collect();
        params.insert("RECOVERY_DELAY".into(), "00400004".into());
        params.insert("TREASURY_KEY".into(), "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into());
        TEMPLATES.write().unwrap().instances.insert(spk.clone(), (drip_hash.clone(), params.clone(), BTreeMap::new()));
        let j = decode_tx(&drip[0].tx, &drip[0].prevouts, RULES);
        assert_eq!(j["inputs"][0]["recomputed"], false);
        assert!(j["inputs"][0]["terms_from_registry"].is_null());
        // ... and the instance's own record does, and the output it pays back
        // into the reserve is read as the same contract.
        params.insert("TREASURY_KEY".into(), "d77d27a4946d1be7d647cb3e6d18ac15022dacd17fff723002f923ac0ccaf733".into());
        TEMPLATES.write().unwrap().instances.insert(spk, (drip_hash, params, BTreeMap::new()));
        let j = decode_tx(&drip[0].tx, &drip[0].prevouts, RULES);
        let i = &j["inputs"][0];
        assert_eq!(i["recomputed"], true);
        assert_eq!(i["terms_from_registry"], json!(["RECOVERY_DELAY", "TREASURY_KEY"]));
        assert_eq!(i["successors"][0]["how"], "same");

        // A program whose witness was cut short is not decoded.
        let mut cut = vault[1].tx.clone();
        let w = &mut cut.input[0].witness.script_witness;
        let n = w[0].len();
        w[0].truncate(n / 2);
        let j = decode_tx(&cut, &vault[1].prevouts, RULES);
        assert!(j["inputs"][0]["decode_error"].is_string());
        assert!(j["inputs"][0]["template"].is_null());
    }

    #[test]
    fn stack_size_counts_lengths() {
        assert_eq!(stack_size(&[vec![0u8; 3], vec![0u8; 300]]), 1 + 1 + 3 + 3 + 300);
    }
}

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

pub mod api {
    use super::*;
    use crate::chain::{OutPoint, Txid};
    use crate::new_index::Query;
    use elements::encode::{deserialize, serialize};
    use std::sync::Mutex;

    /// The most spends of one leaf an instance page reads.
    pub const MAX_SPENDS: usize = 5000;

    fn rules(query: &Query, height: Option<u32>) -> ChainRules {
        let genesis = query.chain().hash_by_height(0).map(|h| h.to_string());
        ChainRules::for_height(genesis.as_deref(), height)
    }

    fn prevouts(query: &Query, tx: &Transaction) -> Vec<Option<TxOut>> {
        tx.input
            .iter()
            .map(|i| {
                if i.is_coinbase() || i.is_pegin {
                    return None;
                }
                query.lookup_txn(&i.previous_output.txid)?.output.get(i.previous_output.vout as usize).cloned()
            })
            .collect()
    }

    fn status(query: &Query, txid: &Txid) -> Json {
        match query.chain().tx_confirming_block(txid) {
            Some(b) => json!({ "confirmed": true, "block_height": b.height, "block_hash": b.hash.to_string(), "block_time": b.time }),
            None => json!({ "confirmed": false }),
        }
    }

    lazy_static! {
        // Decoded transactions, by txid and the number of templates held when
        // they were decoded (a new template may name more).
        static ref CACHE: Mutex<HashMap<(Txid, usize), Arc<Json>>> = Mutex::new(HashMap::new());
    }

    /// `GET /tx/:txid/contracts`
    pub fn tx_contracts(query: &Query, txid: &Txid) -> Option<Arc<Json>> {
        let held = TEMPLATES.read().unwrap().by_hash.len();
        if let Some(j) = CACHE.lock().unwrap().get(&(*txid, held)) {
            return Some(j.clone());
        }
        let tx = query.lookup_txn(txid)?;
        let height = query.chain().tx_confirming_block(txid).map(|b| b.height as u32);
        let mut j = decode_tx(&tx, &prevouts(query, &tx), rules(query, height));
        j["txid"] = json!(txid.to_string());
        let j = Arc::new(j);
        let mut cache = CACHE.lock().unwrap();
        if cache.len() > 20_000 {
            cache.clear();
        }
        cache.insert((*txid, held), j.clone());
        Some(j)
    }

    /// The spends of a leaf: confirmed ones from the index (best chain only),
    /// oldest first, then those in the mempool.
    pub fn leaf_spends(query: &Query, leaf_id: &[u8; 32], limit: usize) -> Vec<(Txid, u32, Option<u32>)> {
        let mut out = Vec::new();
        for row in query.chain().store().history_db().iter_scan(&index_prefix(leaf_id)) {
            if out.len() >= limit {
                break;
            }
            let (height, txid, vin) = match parse_index_key(&row.key) {
                Some(x) => x,
                None => continue,
            };
            let txid: Txid = match deserialize(&txid) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Only rows of blocks on the best chain (a reorganised block's rows
            // are deleted; this also skips rows of a block being processed).
            match query.chain().tx_confirming_block(&txid) {
                Some(b) if b.height as u32 == height => out.push((txid, vin, Some(height))),
                _ => (),
            }
        }
        let mempool = query.mempool();
        for txid in mempool.txids() {
            if out.len() >= limit {
                break;
            }
            if let Some(tx) = mempool.lookup_txn(txid) {
                for (vin, i) in tx.input.iter().enumerate() {
                    if let Some(l) = revealed_leaf(&i.witness.script_witness) {
                        if l.leaf_id() == *leaf_id {
                            out.push((*txid, vin as u32, None));
                        }
                    }
                }
            }
        }
        out
    }

    fn output_json(tx: &Transaction, vout: usize) -> Json {
        let o = &tx.output[vout];
        json!({
            "vout": vout,
            "scriptpubkey": hex(o.script_pubkey.as_bytes()),
            "value": explicit_value(&o.value),
            "asset": match o.asset { confidential::Asset::Explicit(a) => Some(a.to_string()), _ => None },
        })
    }

    /// `GET /contracts/:leaf_id/spends`: each spend, with what the decoder
    /// read of it.
    pub fn spends_json(query: &Query, leaf_id: &[u8; 32], limit: usize) -> Json {
        let spends = leaf_spends(query, leaf_id, limit);
        let list: Vec<Json> = spends
            .iter()
            .map(|(txid, vin, _)| {
                let d = tx_contracts(query, txid);
                let input = d.as_ref().map(|d| d["inputs"][*vin as usize].clone()).unwrap_or(Json::Null);
                json!({
                    "txid": txid.to_string(),
                    "vin": vin,
                    "status": status(query, txid),
                    "template": input["template"],
                    "path": input["path"]["name"],
                    "instance": input["instance"],
                    "state": input["state"],
                })
            })
            .collect();
        let t = TEMPLATES.read().unwrap();
        let templates: Vec<Json> = t
            .by_cmr
            .get(leaf_id)
            .map(|v| v.iter().map(|(t, i)| template_json(t, &t.leaves[*i].name)).collect())
            .unwrap_or_default();
        json!({ "leaf_id": hex(leaf_id), "templates": templates, "spends": list, "limit": limit })
    }

    /// `GET /contracts/:leaf_id/instance/:params`: one instance's spends in
    /// order, as chains of coins: each spend's path, cost, witness and the
    /// state it found, the outputs it owns, the successor it made and the
    /// state that holds, and the spend of that successor.
    pub fn instance_json(query: &Query, leaf_id: &[u8; 32], key: &str) -> Option<Json> {
        let mut entries: Vec<Json> = Vec::new();
        let mut template = Json::Null;
        let mut terms = Json::Null;
        let mut ids: Vec<[u8; 32]> = vec![*leaf_id];
        let mut spks: HashSet<String> = HashSet::new();
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            i += 1;
            for (txid, vin, _) in leaf_spends(query, &id, MAX_SPENDS) {
                let d = match tx_contracts(query, &txid) {
                    Some(d) => d,
                    None => continue,
                };
                let input = &d["inputs"][vin as usize];
                let tx = match query.lookup_txn(&txid) {
                    Some(t) => t,
                    None => continue,
                };
                let prev = &tx.input[vin as usize].previous_output;
                let prev_spk = query
                    .lookup_txn(&prev.txid)
                    .and_then(|t| t.output.get(prev.vout as usize).map(|o| hex(o.script_pubkey.as_bytes())));
                let mine = if id == *leaf_id {
                    input["instance"].as_str() == Some(key)
                } else {
                    // A tapscript leaf of the instance: its coin must be one of
                    // the instance's states.
                    prev_spk.as_ref().map_or(false, |s| spks.contains(s))
                };
                if !mine {
                    continue;
                }
                if id == *leaf_id && template.is_null() {
                    template = input["template"].clone();
                    terms = input["terms"].clone();
                    // The instance's tapscript leaves: their hashes from the
                    // template, the terms and this spend's slots.
                    let held = TEMPLATES.read().unwrap();
                    if let Some(t) = template["hash"].as_str().and_then(|h| held.by_hash.get(h)) {
                        let params: BTreeMap<String, String> = input["terms"].as_array().into_iter().flatten()
                            .filter_map(|v| Some((v["name"].as_str()?.to_string(), v["hex"].as_str()?.to_string()))).collect();
                        let slots: BTreeMap<String, String> = input["state"].as_array().into_iter().flatten()
                            .filter_map(|v| Some((v["name"].as_str()?.to_string(), v["hex"].as_str()?.to_string()))).collect();
                        if let Ok(x) = t.model.derive(&params, &slots) {
                            for (node, _) in t.model.tree.leaves() {
                                if let Node::Tapscript { name, .. } = node {
                                    if let Some(h) = x.leaves.get(name).and_then(|l| unhex(&l.hash)) {
                                        if let Ok(h) = <[u8; 32]>::try_from(h) {
                                            if !ids.contains(&h) {
                                                ids.push(h);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(s) = &prev_spk {
                    spks.insert(s.clone());
                }
                for s in input["successors"].as_array().into_iter().flatten() {
                    if let Some(v) = s["vout"].as_u64() {
                        spks.insert(hex(tx.output[v as usize].script_pubkey.as_bytes()));
                    }
                }
                let successors: Vec<Json> = input["successors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        let v = s["vout"].as_u64().unwrap_or(0) as u32;
                        let spend = query.lookup_spend(&OutPoint { txid, vout: v });
                        let mut s = s.clone();
                        s["output"] = output_json(&tx, v as usize);
                        s["spent_by"] = match spend {
                            Some(sp) => json!({ "txid": sp.txid.to_string(), "vin": sp.vin }),
                            None => Json::Null,
                        };
                        s
                    })
                    .collect();
                let owned: Vec<Json> = input["owned_outputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_u64())
                    .map(|v| output_json(&tx, v as usize))
                    .collect();
                entries.push(json!({
                    "txid": txid.to_string(),
                    "vin": vin,
                    "status": status(query, &txid),
                    "prevout": { "txid": prev.txid.to_string(), "vout": prev.vout },
                    "input": input,
                    "owned_outputs": owned,
                    "successors": successors,
                }));
            }
        }
        if template.is_null() {
            return None;
        }
        // Chains: a spend whose coin no other spend made starts one; each
        // successor's spend continues it.
        let made: HashSet<(String, u64)> = entries
            .iter()
            .flat_map(|e| {
                let txid = e["txid"].as_str().unwrap_or("").to_string();
                e["successors"].as_array().cloned().unwrap_or_default().into_iter().map(move |s| (txid.clone(), s["vout"].as_u64().unwrap_or(0)))
            })
            .collect();
        let by_coin: HashMap<(String, u64), usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| ((e["prevout"]["txid"].as_str().unwrap_or("").to_string(), e["prevout"]["vout"].as_u64().unwrap_or(0)), i))
            .collect();
        let mut chains: Vec<Vec<Json>> = Vec::new();
        let mut used = vec![false; entries.len()];
        let mut starts: Vec<usize> = (0..entries.len())
            .filter(|i| {
                let p = &entries[*i]["prevout"];
                !made.contains(&(p["txid"].as_str().unwrap_or("").to_string(), p["vout"].as_u64().unwrap_or(0)))
            })
            .collect();
        let height = |e: &Json| e["status"]["block_height"].as_u64().unwrap_or(u64::MAX);
        starts.sort_by_key(|i| height(&entries[*i]));
        let mut current: Vec<Json> = Vec::new();
        for s in starts.into_iter().chain(0..entries.len()) {
            if used[s] {
                continue;
            }
            let mut chain = Vec::new();
            let mut at = Some(s);
            while let Some(i) = at {
                if used[i] {
                    break;
                }
                used[i] = true;
                let e = &entries[i];
                chain.push(e.clone());
                at = None;
                for succ in e["successors"].as_array().into_iter().flatten() {
                    let coin = (e["txid"].as_str().unwrap_or("").to_string(), succ["vout"].as_u64().unwrap_or(0));
                    match by_coin.get(&coin) {
                        Some(n) => at = Some(*n),
                        None if succ["spent_by"].is_null() => current.push(json!({
                            "txid": coin.0, "vout": coin.1, "state": succ["state"], "output": succ["output"],
                        })),
                        None => (),
                    }
                }
            }
            chains.push(chain);
        }
        Some(json!({
            "leaf_id": hex(leaf_id),
            "instance": key,
            "template": template,
            "terms": terms,
            "chains": chains,
            "current": current,
        }))
    }

    /// `GET /contracts/templates`
    pub fn templates_json() -> Json {
        let t = TEMPLATES.read().unwrap();
        let mut list: Vec<Json> = t
            .by_hash
            .values()
            .map(|t| {
                json!({
                    "hash": t.hash, "name": t.name, "version": t.version, "summary": t.summary,
                    "simplicity_leaves": t.leaves.iter().map(|l| json!({ "leaf": l.name, "cmr": l.program.cmr, "max_cost_wu": l.program.max_cost_wu })).collect::<Vec<_>>(),
                })
            })
            .collect();
        list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        json!({ "templates": list, "refused": t.refused.keys().collect::<Vec<_>>() })
    }

    pub fn txid_bytes(t: &Txid) -> Vec<u8> {
        serialize(t)
    }
}
