//! The signed release list written by dev/tools/release.py publish (see dev/tools/launchlib.py for the format).

use super::error::{Error, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The newest manifest format this launcher understands. 2: a `packs` list (one entry per addon folder).
pub const FORMAT: u32 = 2;
/// The core addon folder: in every release and always installed.
pub const CORE: &str = "DisasterZone";
/// The 3D rooms folder (an Extra; format 1 releases carry it without a packs list).
pub const ROOMS: &str = "DisasterZone_Rooms";
/// Folders the launcher writes itself (core/bridge.rs): never part of a release.
pub const RESERVED: [&str; 1] = [super::bridge::FOLDER];

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

/// One addon folder of a release, read from its TOC by launchlib.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pack {
    /// "core" for DisasterZone; a game's id is the one it registers with the Games hub
    pub id: String,
    pub folder: String,
    /// "core" | "extra" | "game"
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tagline: String,
    /// always installed (the core)
    #[serde(default)]
    pub required: bool,
    /// installed unless the user turns it off (Extras); games wait for the picker or the Games tab
    #[serde(default)]
    pub default_on: bool,
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
    /// format 2; filled in by check() for a format 1 release (DisasterZone + the 3D rooms)
    #[serde(default)]
    pub packs: Vec<Pack>,
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
    // (the format comes first: a newer manifest may not even parse as this launcher's Manifest)
    if let Ok(head) = serde_json::from_slice::<FormatOnly>(data) {
        check_format(head.format)?;
    }
    let mut m: Manifest = serde_json::from_slice(data).map_err(|e| Error::BadManifest(e.to_string()))?;
    m.check()?;
    Ok(m)
}

#[derive(Deserialize)]
struct FormatOnly {
    format: u32,
}

fn check_format(format: u32) -> Result<()> {
    if format > FORMAT {
        return Err(Error::Msg("this release needs a newer launcher: download the newest launcher".into()));
    }
    Ok(())
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

/// `DisasterZone` or `DisasterZone_<letters and digits>`: the only folder names a release may write.
pub fn pack_folder_name(name: &str) -> bool {
    match name.strip_prefix(CORE) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('_')
            .map(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or(false),
        None => false,
    }
}

/// A pack id the settings file and the window can carry as is.
pub fn pack_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// What a format 1 release holds, as packs.
fn legacy_packs(folders: &BTreeMap<String, BTreeMap<String, FileEntry>>) -> Vec<Pack> {
    let mut out = vec![Pack {
        id: "core".into(),
        folder: CORE.into(),
        kind: "core".into(),
        title: "DisasterZone".into(),
        tagline: String::new(),
        required: true,
        default_on: true,
    }];
    if folders.contains_key(ROOMS) {
        out.push(Pack {
            id: "rooms".into(),
            folder: ROOMS.into(),
            kind: "extra".into(),
            title: "3D rooms".into(),
            tagline: String::new(),
            required: false,
            default_on: true,
        });
    }
    out
}

impl Manifest {
    fn check(&mut self) -> Result<()> {
        check_format(self.format)?;
        if self.format < 2 && self.packs.is_empty() {
            self.packs = legacy_packs(&self.folders);
        }
        let bad = |s: String| Err(Error::BadManifest(s));
        let mut ids = BTreeSet::new();
        let mut listed = BTreeSet::new();
        for p in &self.packs {
            if !pack_folder_name(&p.folder) || RESERVED.contains(&p.folder.as_str()) {
                return bad(format!("bad pack folder {}", p.folder));
            }
            if !pack_id(&p.id) {
                return bad(format!("bad pack id {:?}", p.id));
            }
            if !ids.insert(p.id.as_str()) || !listed.insert(p.folder.as_str()) {
                return bad(format!("pack {} is listed twice", p.id));
            }
            let core = p.folder == CORE;
            let kind_ok = match p.kind.as_str() {
                "core" => core,
                "extra" | "game" => !core,
                _ => false,
            };
            if !kind_ok {
                return bad(format!("pack {} has kind {:?}", p.id, p.kind));
            }
            if core && !p.required {
                return bad(format!("{CORE} must be required"));
            }
            if !self.folders.contains_key(&p.folder) {
                return bad(format!("pack {} has no files", p.id));
            }
        }
        if !listed.contains(CORE) {
            return bad(format!("no {CORE} folder"));
        }
        for (folder, files) in &self.folders {
            if !listed.contains(folder.as_str()) {
                return bad(format!("unknown folder {folder}"));
            }
            for (rel, f) in files {
                if !safe_rel(rel) {
                    return bad(format!("unsafe path {folder}/{rel}"));
                }
                if !is_sha(&f.sha256) {
                    return bad(format!("bad hash for {folder}/{rel}"));
                }
            }
        }
        Ok(())
    }

    pub fn file_count(&self) -> usize {
        self.folders.values().map(|f| f.len()).sum()
    }

    pub fn pack(&self, id: &str) -> Option<&Pack> {
        self.packs.iter().find(|p| p.id == id)
    }

    /// The release has games of its own (format 2): the first time, the user picks which to keep.
    pub fn has_games(&self) -> bool {
        self.packs.iter().any(|p| p.kind == "game")
    }

    /// (bytes on disk, bytes to download) of one folder of the release.
    pub fn folder_size(&self, folder: &str) -> (u64, u64) {
        self.folders
            .get(folder)
            .map(|files| files.values().fold((0, 0), |(s, g), f| (s + f.size, g + f.gz)))
            .unwrap_or((0, 0))
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
    fn pack_folder_names() {
        for ok in ["DisasterZone", "DisasterZone_Rooms", "DisasterZone_HoldTheLine", "DisasterZone_A1"] {
            assert!(pack_folder_name(ok), "{ok}");
        }
        for bad in ["", "Disaster", "DisasterZone_", "DisasterZone__X", "DisasterZone_A-b", "DisasterZoneX", "Other",
            "DisasterZone_../x", "DisasterZone_A/B", "disasterzone"] {
            assert!(!pack_folder_name(bad), "{bad}");
        }
    }

    const FILE: &str = r#"{"sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":1,"gz":1}"#;

    fn parse(json: &str) -> Result<Manifest> {
        let (sk, rk) = test_key();
        verify_and_parse(&rk, json.as_bytes(), &sign(&sk, &rk, json.as_bytes()))
    }

    fn v2(packs: &str, folders: &str) -> String {
        format!(r#"{{"format":2,"name":"DisasterZone","channel":"stable","seq":1,"version":"1.0","build":"b","assetPack":1,"folders":{{{folders}}},"packs":[{packs}]}}"#)
    }

    const CORE_PACK: &str = r#"{"id":"core","folder":"DisasterZone","kind":"core","title":"DisasterZone","required":true,"defaultOn":true}"#;

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

    #[test]
    fn a_format_1_release_reads_as_core_plus_rooms() {
        let m = parse(&format!(
            r#"{{"format":1,"name":"DisasterZone","channel":"stable","seq":1,"version":"1.0","build":"b","assetPack":1,"folders":{{"DisasterZone":{{"a":{FILE}}},"DisasterZone_Rooms":{{"r":{FILE}}}}}}}"#
        ))
        .unwrap();
        let ids: Vec<&str> = m.packs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["core", "rooms"]);
        assert!(m.packs[0].required && m.packs[1].default_on && !m.has_games());
        // format 1 knows no other folder
        let other = r#"{"format":1,"name":"DisasterZone","channel":"stable","seq":1,"version":"1.0","build":"b","assetPack":1,"folders":{"DisasterZone":{},"DisasterZone_Chess":{}}}"#;
        assert!(parse(other).unwrap_err().to_string().contains("unknown folder"));
    }

    #[test]
    fn the_format_gate_comes_first() {
        // a format 3 release that this launcher could not even parse: still the friendly message
        let next = r#"{"format":3,"whatever":true}"#;
        let err = parse(next).unwrap_err().to_string();
        assert!(err.contains("needs a newer launcher"), "{err}");
        let next = v2(CORE_PACK, &format!(r#""DisasterZone":{{"a":{FILE}}}"#)).replace(r#""format":2"#, r#""format":3"#);
        assert!(parse(&next).unwrap_err().to_string().contains("needs a newer launcher"));
    }

    #[test]
    fn a_format_2_release_lists_its_packs() {
        let chess = r#"{"id":"chess","folder":"DisasterZone_Chess","kind":"game","title":"Karazhan Chess","tagline":"Play Medivh's echo.","required":false,"defaultOn":false}"#;
        let m = parse(&v2(
            &format!("{CORE_PACK},{chess}"),
            &format!(r#""DisasterZone":{{"a":{FILE}}},"DisasterZone_Chess":{{"c":{FILE},"d":{FILE}}}"#),
        ))
        .unwrap();
        assert!(m.has_games());
        assert_eq!(m.pack("chess").unwrap().title, "Karazhan Chess");
        assert_eq!(m.folder_size("DisasterZone_Chess"), (2, 2));
    }

    #[test]
    fn bad_packs_are_refused() {
        let core_files = format!(r#""DisasterZone":{{"a":{FILE}}}"#);
        let cases = [
            // a folder no pack lists
            (v2(CORE_PACK, &format!(r#"{core_files},"DisasterZone_Chess":{{"c":{FILE}}}"#)), "unknown folder"),
            // no core
            (v2(r#"{"id":"chess","folder":"DisasterZone_Chess","kind":"game"}"#, &format!(r#""DisasterZone_Chess":{{"c":{FILE}}}"#)), "no DisasterZone"),
            // the core not required
            (v2(&CORE_PACK.replace(r#""required":true"#, r#""required":false"#), &core_files), "must be required"),
            // a name outside DisasterZone(_X)
            (v2(&format!(r#"{CORE_PACK},{{"id":"x","folder":"Bagnon","kind":"game"}}"#), &format!(r#"{core_files},"Bagnon":{{"c":{FILE}}}"#)), "bad pack folder"),
            (v2(&format!(r#"{CORE_PACK},{{"id":"x","folder":"DisasterZone_A-B","kind":"game"}}"#), &format!(r#"{core_files},"DisasterZone_A-B":{{"c":{FILE}}}"#)), "bad pack folder"),
            // the launcher's own bridge folder
            (v2(&format!(r#"{CORE_PACK},{{"id":"b","folder":"DisasterZone_Bridge","kind":"extra"}}"#), &format!(r#"{core_files},"DisasterZone_Bridge":{{"c":{FILE}}}"#)), "bad pack folder"),
            // twice, a bad kind, a bad id, a pack without files
            (v2(&format!(r#"{CORE_PACK},{{"id":"core","folder":"DisasterZone_X","kind":"game"}}"#), &format!(r#"{core_files},"DisasterZone_X":{{"c":{FILE}}}"#)), "twice"),
            (v2(&format!(r#"{CORE_PACK},{{"id":"x","folder":"DisasterZone_X","kind":"core","required":true}}"#), &format!(r#"{core_files},"DisasterZone_X":{{"c":{FILE}}}"#)), "kind"),
            (v2(&format!(r#"{CORE_PACK},{{"id":"X y","folder":"DisasterZone_X","kind":"game"}}"#), &format!(r#"{core_files},"DisasterZone_X":{{"c":{FILE}}}"#)), "bad pack id"),
            (v2(&format!(r#"{CORE_PACK},{{"id":"x","folder":"DisasterZone_X","kind":"game"}}"#), &core_files), "no files"),
        ];
        for (json, needle) in cases {
            let err = parse(&json).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }
}
