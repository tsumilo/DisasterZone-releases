//! W-OTS (w = 16) one-time signatures under a Merkle tree: 2^height signatures per key. The same scheme as the hot
//! patches (dev/tools/patchlib.py signs, addon Core/Patch.lua VerifySignature checks); the bridge has its own key.
//! Signing a leaf twice would give the key away, so [`Signer::sign`] saves the next leaf before it signs.
//!
//! Signature: u32 leaf (big-endian) || 67 chain values || `height` auth-path nodes, 32 bytes each.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const W: u8 = 16;
const LEN: usize = 67;
const N: usize = 32;
const SEED_LEN: usize = 16;

type H = [u8; 32];

fn sha(parts: &[&[u8]]) -> H {
    let mut s = Sha256::new();
    for p in parts {
        s.update(p);
    }
    s.finalize().into()
}

fn f(seed: &[u8], leaf: u32, chain: u8, step: u8, x: &H) -> H {
    sha(&[seed, b"C", &leaf.to_be_bytes(), &[chain, step], x])
}

fn chain(seed: &[u8], leaf: u32, c: u8, mut x: H, start: u8, steps: u8) -> H {
    for s in start..start + steps {
        x = f(seed, leaf, c, s, &x);
    }
    x
}

fn sk(secret: &[u8], leaf: u32, c: u8) -> H {
    sha(&[secret, b"S", &leaf.to_be_bytes(), &[c]])
}

fn leaf_node(seed: &[u8], leaf: u32, pks: &[H]) -> H {
    let mut s = Sha256::new();
    s.update(seed);
    s.update(b"L");
    s.update(leaf.to_be_bytes());
    for p in pks {
        s.update(p);
    }
    s.finalize().into()
}

fn inner(seed: &[u8], height: u8, index: u32, l: &H, r: &H) -> H {
    sha(&[seed, b"N", &[height], &index.to_be_bytes(), l, r])
}

fn digits(seed: &[u8], leaf: u32, message: &[u8]) -> [u8; LEN] {
    let d = sha(&[seed, b"M", &leaf.to_be_bytes(), message]);
    let mut out = [0u8; LEN];
    let mut csum: u32 = 0;
    for (i, b) in d.iter().enumerate() {
        out[2 * i] = b >> 4;
        out[2 * i + 1] = b & 15;
        csum += (W as u32 - 1 - (b >> 4) as u32) + (W as u32 - 1 - (b & 15) as u32);
    }
    out[64] = ((csum >> 8) & 15) as u8;
    out[65] = ((csum >> 4) & 15) as u8;
    out[66] = (csum & 15) as u8;
    out
}

fn leaf_pk(secret: &[u8], seed: &[u8], leaf: u32) -> H {
    let pks: Vec<H> = (0..LEN as u8).map(|c| chain(seed, leaf, c, sk(secret, leaf, c), 0, W - 1)).collect();
    leaf_node(seed, leaf, &pks)
}

fn tree(seed: &[u8], leaves: Vec<H>) -> Vec<Vec<H>> {
    let mut levels = vec![leaves];
    let mut h = 0u8;
    while levels.last().unwrap().len() > 1 {
        h += 1;
        let prev = levels.last().unwrap();
        let next: Vec<H> = (0..prev.len() / 2).map(|i| inner(seed, h, i as u32, &prev[2 * i], &prev[2 * i + 1])).collect();
        levels.push(next);
    }
    levels
}

pub fn sig_len(height: u8) -> usize {
    4 + (LEN + height as usize) * N
}

/// The public half: what goes into the addon (Core/BridgeKey.lua).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicKey {
    pub seed: String,
    pub root: String,
    pub h: u8,
}

impl PublicKey {
    pub fn lua(&self) -> String {
        format!(
            "ns.BridgeKey = {{\n    seed = \"{}\",\n    root = \"{}\",\n    h = {},\n}}\n",
            self.seed, self.root, self.h
        )
    }
}

pub fn verify(key: &PublicKey, message: &[u8], sig: &[u8]) -> bool {
    let (Ok(seed), Ok(root)) = (hex::decode(&key.seed), hex::decode(&key.root)) else { return false };
    if sig.len() != sig_len(key.h) {
        return false;
    }
    let leaf = u32::from_be_bytes(sig[..4].try_into().unwrap());
    if (leaf as u64) >= (1u64 << key.h) {
        return false;
    }
    let d = digits(&seed, leaf, message);
    let pks: Vec<H> = (0..LEN)
        .map(|c| {
            let x: H = sig[4 + c * N..4 + (c + 1) * N].try_into().unwrap();
            chain(&seed, leaf, c as u8, x, d[c], W - 1 - d[c])
        })
        .collect();
    let mut node = leaf_node(&seed, leaf, &pks);
    let mut idx = leaf;
    let off = 4 + LEN * N;
    for h in 0..key.h as usize {
        let sib: H = sig[off + h * N..off + (h + 1) * N].try_into().unwrap();
        node = if idx & 1 == 1 { inner(&seed, h as u8 + 1, idx >> 1, &sib, &node) } else { inner(&seed, h as u8 + 1, idx >> 1, &node, &sib) };
        idx >>= 1;
    }
    node.as_slice() == root.as_slice()
}

/// The secret half on disk: `<path>` (JSON: secret, seed, height, root, next) + `<path>.leaves` (the leaf hashes).
#[derive(Serialize, Deserialize)]
struct KeyFile {
    secret: String,
    seed: String,
    height: u8,
    root: String,
    /// the next unused leaf
    next: u32,
}

pub struct Signer {
    path: PathBuf,
    file: KeyFile,
    secret: Vec<u8>,
    seed: Vec<u8>,
    levels: Vec<Vec<H>>,
}

#[derive(Debug)]
pub enum KeyError {
    Io(std::io::Error),
    Bad(String),
    /// every leaf has been used: a new key (and an addon release with it) is needed
    Spent,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Io(e) => write!(f, "{e}"),
            KeyError::Bad(e) => write!(f, "bad key file: {e}"),
            KeyError::Spent => write!(f, "the signing key is used up (make a new one and release the addon with it)"),
        }
    }
}

impl From<std::io::Error> for KeyError {
    fn from(e: std::io::Error) -> Self {
        KeyError::Io(e)
    }
}

fn leaves_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".leaves");
    PathBuf::from(s)
}

fn write_atomic(p: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, p)
}

impl Signer {
    /// A new key with 2^height signatures (height 16 takes a few seconds), written to `path`. `entropy`: 48 random
    /// bytes (32 secret + 16 seed).
    pub fn create(path: &Path, height: u8, entropy: &[u8; 48]) -> Result<Signer, KeyError> {
        let (secret, seed) = (entropy[..32].to_vec(), entropy[32..32 + SEED_LEN].to_vec());
        let leaves: Vec<H> = (0..1u32 << height).map(|i| leaf_pk(&secret, &seed, i)).collect();
        let mut raw = Vec::with_capacity(leaves.len() * N);
        for l in &leaves {
            raw.extend_from_slice(l);
        }
        let levels = tree(&seed, leaves);
        let file = KeyFile { secret: hex::encode(&secret), seed: hex::encode(&seed), height, root: hex::encode(levels.last().unwrap()[0]), next: 0 };
        write_atomic(&leaves_path(path), &raw)?;
        write_atomic(path, &serde_json::to_vec_pretty(&file).unwrap())?;
        Ok(Signer { path: path.to_path_buf(), file, secret, seed, levels })
    }

    pub fn open(path: &Path) -> Result<Signer, KeyError> {
        let file: KeyFile = serde_json::from_slice(&std::fs::read(path)?).map_err(|e| KeyError::Bad(e.to_string()))?;
        let secret = hex::decode(&file.secret).map_err(|e| KeyError::Bad(e.to_string()))?;
        let seed = hex::decode(&file.seed).map_err(|e| KeyError::Bad(e.to_string()))?;
        let raw = std::fs::read(leaves_path(path))?;
        if raw.len() != N << file.height {
            return Err(KeyError::Bad("the .leaves file does not match the key".into()));
        }
        let leaves: Vec<H> = raw.chunks(N).map(|c| c.try_into().unwrap()).collect();
        let levels = tree(&seed, leaves);
        if hex::encode(levels.last().unwrap()[0]) != file.root {
            return Err(KeyError::Bad("the leaves do not make the key's root".into()));
        }
        Ok(Signer { path: path.to_path_buf(), file, secret, seed, levels })
    }

    pub fn public(&self) -> PublicKey {
        PublicKey { seed: self.file.seed.clone(), root: self.file.root.clone(), h: self.file.height }
    }

    /// Signatures left.
    pub fn left(&self) -> u64 {
        (1u64 << self.file.height) - self.file.next as u64
    }

    /// Sign with the next leaf (saved as used before the signature exists).
    pub fn sign(&mut self, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        let leaf = self.file.next;
        if (leaf as u64) >= (1u64 << self.file.height) {
            return Err(KeyError::Spent);
        }
        self.file.next += 1;
        write_atomic(&self.path, &serde_json::to_vec_pretty(&self.file).unwrap())?;
        let d = digits(&self.seed, leaf, message);
        let mut out = Vec::with_capacity(sig_len(self.file.height));
        out.extend_from_slice(&leaf.to_be_bytes());
        for (c, dc) in d.iter().enumerate() {
            out.extend_from_slice(&chain(&self.seed, leaf, c as u8, sk(&self.secret, leaf, c as u8), 0, *dc));
        }
        let mut idx = leaf as usize;
        for h in 0..self.file.height as usize {
            out.extend_from_slice(&self.levels[h][idx ^ 1]);
            idx >>= 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entropy() -> [u8; 48] {
        let mut e = [0u8; 48];
        for (i, b) in e.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(1);
        }
        e
    }

    #[test]
    fn signs_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bridge_key.json");
        let mut s = Signer::create(&p, 4, &entropy()).unwrap();
        let pk = s.public();
        let a = s.sign(b"hello").unwrap();
        let b = s.sign(b"hello").unwrap();
        assert_eq!(a.len(), sig_len(4));
        assert!(verify(&pk, b"hello", &a) && verify(&pk, b"hello", &b));
        assert_ne!(a[..4], b[..4]); // (a new leaf each time)
        assert!(!verify(&pk, b"hellO", &a));
        let mut bad = a.clone();
        bad[100] ^= 1;
        assert!(!verify(&pk, b"hello", &bad));
        // the used leaves survive a restart
        let mut s2 = Signer::open(&p).unwrap();
        assert_eq!(s2.left(), 14);
        let c = s2.sign(b"x").unwrap();
        assert_eq!(u32::from_be_bytes(c[..4].try_into().unwrap()), 2);
    }

    #[test]
    fn a_spent_key_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Signer::create(&dir.path().join("k.json"), 1, &entropy()).unwrap();
        s.sign(b"1").unwrap();
        s.sign(b"2").unwrap();
        assert!(matches!(s.sign(b"3"), Err(KeyError::Spent)));
    }

    /// The same bytes as dev/tools/patchlib.py (the hot patches' signer, which addon Core/Patch.lua checks):
    /// patchlib.keygen(2, secret=bytes(range(32)), seed=bytes(range(16))) and its sign(key, 1, b"DZB1test").
    #[test]
    fn same_bytes_as_patchlib() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = [0u8; 48];
        for (i, b) in e.iter_mut().enumerate() {
            *b = if i < 32 { i as u8 } else { (i - 32) as u8 };
        }
        let mut s = Signer::create(&dir.path().join("k.json"), 2, &e).unwrap();
        assert_eq!(s.public().root, "688bb06c4d8fa74ee64812ec9bf6533b6b87a86748ed3a81602bb27b8dad6fae");
        s.sign(b"skip leaf 0").unwrap();
        let sig = s.sign(b"DZB1test").unwrap();
        assert_eq!(sig.len(), 2212);
        assert_eq!(hex::encode(Sha256::digest(&sig)), "046e8c3560463523ccef55d3fcfdb0efc965c713b564d495f311537dfaeabefe");
    }
}
