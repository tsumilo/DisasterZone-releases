//! The signed release list written by dev/tools/release.py publish (see dev/tools/launchlib.py for the format).

use super::error::{Error, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The newest manifest format this launcher understands.
pub const FORMAT: u32 = 1;
/// The addon folders a release may contain (both go into Interface/AddOns). The first one is required.
pub const FOLDERS: [&str; 2] = ["DisasterZone", "DisasterZone_Rooms"];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
    pub gz: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ZipInfo {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub name: String,
    pub channel: String,
    pub seq: u64,
    pub version: String,
    pub build: String,
    pub asset_pack: u32,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub published: String,
    #[serde(default)]
    pub notes: serde_json::Value,
    pub folders: BTreeMap<String, BTreeMap<String, FileEntry>>,
    #[serde(default)]
    pub zip: Option<ZipInfo>,
}

/// The public half of the publisher's signing key (launcher/release_key.json, built in).
pub struct ReleaseKey {
    pub id: String,
    key: VerifyingKey,
}

#[derive(Deserialize)]
struct BuiltinKey {
    public: String,
}

#[derive(Deserialize)]
struct SigFile {
    key: String,
    sig: String,
}

impl ReleaseKey {
    pub fn from_hex(public_hex: &str) -> Result<Self> {
        let bytes: [u8; 32] = hex::decode(public_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| Error::Msg("bad public key".into()))?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Msg("bad public key".into()))?;
        Ok(ReleaseKey { id: hex::encode(Sha256::digest(bytes))[..16].to_string(), key })
    }

    pub fn builtin() -> Self {
        let k: BuiltinKey =
            serde_json::from_str(include_str!("../../../release_key.json")).expect("release_key.json");
        ReleaseKey::from_hex(&k.public).expect("release_key.json")
    }
}

/// Check the signature over the manifest's exact bytes, then parse and sanity-check it.
pub fn verify_and_parse(key: &ReleaseKey, data: &[u8], sig: &[u8]) -> Result<Manifest> {
    let s: SigFile = serde_json::from_slice(sig).map_err(|_| Error::BadSignature)?;
    if s.key != key.id {
        return Err(Error::Msg(format!(
            "this release is signed with a key this launcher does not know ({}): download the newest launcher",
            s.key
        )));
    }
    let raw: [u8; 64] = hex::decode(&s.sig)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(Error::BadSignature)?;
    key.key
        .verify(data, &Signature::from_bytes(&raw))
        .map_err(|_| Error::BadSignature)?;
    let m: Manifest = serde_json::from_slice(data).map_err(|e| Error::BadManifest(e.to_string()))?;
    m.check()?;
    Ok(m)
}

/// A path inside an addon folder that stays inside it on every OS.
pub fn safe_rel(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && !p.contains(':')
        && !p.contains('\0')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn is_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Manifest {
    fn check(&self) -> Result<()> {
        if self.format > FORMAT {
            return Err(Error::Msg(
                "this release needs a newer launcher: download the newest launcher".into(),
            ));
        }
        if !self.folders.contains_key(FOLDERS[0]) {
            return Err(Error::BadManifest(format!("no {} folder", FOLDERS[0])));
        }
        for (folder, files) in &self.folders {
            if !FOLDERS.contains(&folder.as_str()) {
                return Err(Error::BadManifest(format!("unknown folder {folder}")));
            }
            for (rel, f) in files {
                if !safe_rel(rel) {
                    return Err(Error::BadManifest(format!("unsafe path {folder}/{rel}")));
                }
                if !is_sha(&f.sha256) {
                    return Err(Error::BadManifest(format!("bad hash for {folder}/{rel}")));
                }
            }
        }
        Ok(())
    }

    pub fn file_count(&self) -> usize {
        self.folders.values().map(|f| f.len()).sum()
    }
}

pub fn object_path(sha: &str) -> String {
    format!("objects/{}/{}.gz", &sha[..2], sha)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub fn test_key() -> (SigningKey, ReleaseKey) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let rk = ReleaseKey::from_hex(&hex::encode(sk.verifying_key().to_bytes())).unwrap();
        (sk, rk)
    }

    pub fn sign(sk: &SigningKey, rk: &ReleaseKey, data: &[u8]) -> Vec<u8> {
        let sig = sk.sign(data);
        format!("{{\"key\": \"{}\", \"sig\": \"{}\"}}\n", rk.id, hex::encode(sig.to_bytes())).into_bytes()
    }

    #[test]
    fn verifies_a_python_signature() {
        // RFC 8032 test 1 (the same vector dev/tests/launcher/test_publish.py checks the Python side with)
        let rk = ReleaseKey::from_hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
        let sig = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";
        let raw: [u8; 64] = hex::decode(sig).unwrap().try_into().unwrap();
        assert!(rk.key.verify(b"", &Signature::from_bytes(&raw)).is_ok());
    }

    #[test]
    fn the_builtin_key_loads() {
        assert_eq!(ReleaseKey::builtin().id.len(), 16);
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for p in ["", "../x", "a/../b", "/abs", "C:/x", "a\\b", "a//b", "./a"] {
            assert!(!safe_rel(p), "{p}");
        }
        assert!(safe_rel("Core/Boot.lua"));
    }

    #[test]
    fn a_tampered_manifest_is_refused() {
        let (sk, rk) = test_key();
        let data = br#"{"format":1,"name":"DisasterZone","channel":"stable","seq":1,"version":"1.0","build":"b","assetPack":1,"folders":{"DisasterZone":{}}}"#;
        let sig = sign(&sk, &rk, data);
        assert!(verify_and_parse(&rk, data, &sig).is_ok());
        let bad = String::from_utf8(data.to_vec()).unwrap().replace("1.0", "2.0");
        assert!(matches!(verify_and_parse(&rk, bad.as_bytes(), &sig), Err(Error::BadSignature)));
        let evil = br#"{"format":1,"name":"DisasterZone","channel":"stable","seq":1,"version":"1.0","build":"b","assetPack":1,"folders":{"DisasterZone":{"../../x.lua":{"sha256":"00","size":1,"gz":1}}}}"#;
        assert!(verify_and_parse(&rk, evil, &sign(&sk, &rk, evil)).is_err());
    }
}
