//! Install, update and roll back the addon folders.
//!
//! An update never writes into the live folders. It
//!   1. compares the installed files with the manifest (sha256),
//!   2. downloads only the missing objects into a cache (each checked against its hash),
//!   3. builds complete new folders in a staging area (unchanged files copied from the live folder),
//!   4. backs up the SavedVariables, then swaps: live folders -> backups/<stamp>, staged folders -> AddOns.
//! A failure before step 4 leaves the game untouched; a failure during the swap puts the old folders back.
//! The work area is <flavor>/Interface/DZLauncher (same volume as AddOns, so the swap is a rename; WoW only
//! reads Interface/AddOns).

use super::error::{msg, Error, Result};
use super::manifest::{object_path, FileEntry, Manifest, FOLDERS};
use super::source::Source;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PARALLEL_DOWNLOADS: usize = 8;

pub struct Layout {
    pub flavor_dir: PathBuf,
}

impl Layout {
    pub fn new(flavor_dir: impl Into<PathBuf>) -> Self {
        Layout { flavor_dir: flavor_dir.into() }
    }
    pub fn addons(&self) -> PathBuf {
        self.flavor_dir.join("Interface").join("AddOns")
    }
    pub fn work(&self) -> PathBuf {
        self.flavor_dir.join("Interface").join("DZLauncher")
    }
    pub fn cache(&self) -> PathBuf {
        self.work().join("cache")
    }
    pub fn stage(&self) -> PathBuf {
        self.work().join("stage")
    }
    pub fn backups(&self) -> PathBuf {
        self.work().join("backups")
    }
    pub fn wtf(&self) -> PathBuf {
        self.flavor_dir.join("WTF")
    }
}

// ---------------------------------------------------------------------------------------------------------------
// What is installed
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub present: bool,
    pub version: Option<String>,
    pub build: Option<String>,
    pub asset_pack: Option<u32>,
    /// A development copy (build id "dev"): the publisher's own source tree. Never updated.
    pub dev: bool,
    /// The folder is a link or junction (a development setup). Never updated.
    pub linked: bool,
    pub rooms: bool,
}

fn quoted_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let i = text.find(key)? + key.len();
    let rest = &text[i..];
    let a = rest.find('"')? + 1;
    let b = rest[a..].find('"')? + a;
    Some(&rest[a..b])
}

pub fn installed(layout: &Layout) -> Installed {
    let dir = layout.addons().join(FOLDERS[0]);
    let mut out = Installed::default();
    let toc = match fs::read_to_string(dir.join(format!("{}.toc", FOLDERS[0]))) {
        Ok(t) => t,
        Err(_) => return out,
    };
    out.present = true;
    out.version = toc.lines().find_map(|l| {
        let l = l.trim();
        let (k, v) = l.strip_prefix("##")?.split_once(':')?;
        (k.trim().eq_ignore_ascii_case("version")).then(|| v.trim().to_string())
    });
    if let Ok(info) = fs::read_to_string(dir.join("Core").join("BuildInfo.lua")) {
        out.build = quoted_after(&info, "id").map(str::to_string);
        out.asset_pack = info
            .find("assetPack")
            .and_then(|i| info[i..].split(['=', ',', '\n']).nth(1))
            .and_then(|v| v.trim().parse().ok());
    }
    out.dev = out.build.as_deref() == Some("dev");
    out.linked = fs::symlink_metadata(&dir).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    out.rooms = layout.addons().join(FOLDERS[1]).join(format!("{}.toc", FOLDERS[1])).is_file();
    out
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

/// {relative path with '/': (sha256, size)} of every file under `dir` ({} when it does not exist).
pub fn scan(dir: &Path) -> Result<BTreeMap<String, (String, u64)>> {
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.map_err(|e| Error::Msg(e.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
        out.insert(rel, sha256_file(entry.path())?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// (folder, path, entry) to download (or take from the cache)
    pub fetch: Vec<(String, String, FileEntry)>,
    /// files to delete: installed but not in the release
    pub remove: Vec<(String, String)>,
    pub unchanged: usize,
    /// compressed bytes still to download (cached objects are not counted)
    pub download_bytes: u64,
    pub up_to_date: bool,
}

pub fn make_plan(layout: &Layout, m: &Manifest) -> Result<Plan> {
    let mut plan = Plan { fetch: vec![], remove: vec![], unchanged: 0, download_bytes: 0, up_to_date: false };
    for folder in FOLDERS {
        let have = scan(&layout.addons().join(folder))?;
        let empty = BTreeMap::new();
        let want = m.folders.get(folder).unwrap_or(&empty);
        for (rel, f) in want {
            if have.get(rel).map(|(sha, _)| sha == &f.sha256).unwrap_or(false) {
                plan.unchanged += 1;
            } else {
                if !layout.cache().join(&f.sha256).is_file() {
                    plan.download_bytes += f.gz;
                }
                plan.fetch.push((folder.to_string(), rel.clone(), f.clone()));
            }
        }
        for rel in have.keys() {
            if !want.contains_key(rel) {
                plan.remove.push((folder.to_string(), rel.clone()));
            }
        }
    }
    plan.up_to_date = plan.fetch.is_empty() && plan.remove.is_empty();
    Ok(plan)
}

// ---------------------------------------------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// "download" | "stage" | "swap" | "done"
    pub step: &'static str,
    pub done: u64,
    pub total: u64,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

pub fn no_progress() -> ProgressFn {
    Arc::new(|_| {})
}

// ---------------------------------------------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------------------------------------------

/// Unpack and check one object: at most `size` bytes, matching the hash.
fn unpack(gz: &[u8], f: &FileEntry) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(f.size as usize);
    flate2::read::GzDecoder::new(gz)
        .take(f.size + 1)
        .read_to_end(&mut out)
        .map_err(|e| Error::Msg(format!("damaged download: {e}")))?;
    if out.len() as u64 != f.size || hex::encode(Sha256::digest(&out)) != f.sha256 {
        return msg("a downloaded file does not match the release (nothing was changed): try again");
    }
    Ok(out)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("part");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

async fn download(layout: &Layout, src: &Source, plan: &Plan, progress: &ProgressFn) -> Result<()> {
    let cache = layout.cache();
    fs::create_dir_all(&cache)?;
    let mut todo: Vec<FileEntry> = vec![];
    for (_, _, f) in &plan.fetch {
        if !cache.join(&f.sha256).is_file() && !todo.iter().any(|t| t.sha256 == f.sha256) {
            todo.push(f.clone());
        }
    }
    let total: u64 = todo.iter().map(|f| f.gz).sum();
    let done = Arc::new(AtomicU64::new(0));
    progress(Progress { step: "download", done: 0, total });
    let results: Vec<Result<()>> = stream::iter(todo)
        .map(|f| {
            let cache = cache.clone();
            let done = done.clone();
            let progress = progress.clone();
            async move {
                let gz = src.get(&object_path(&f.sha256), false).await?;
                let data = unpack(&gz, &f)?;
                write_atomic(&cache.join(&f.sha256), &data)?;
                let d = done.fetch_add(f.gz, Ordering::SeqCst) + f.gz;
                progress(Progress { step: "download", done: d, total });
                Ok(())
            }
        })
        .buffer_unordered(PARALLEL_DOWNLOADS)
        .collect()
        .await;
    results.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Stage + swap
// ---------------------------------------------------------------------------------------------------------------

/// Build the complete release folders under stage/: unchanged files from the live folder, the rest from the cache.
/// Every staged file is checked against the manifest afterwards.
fn stage(layout: &Layout, m: &Manifest, plan: &Plan, progress: &ProgressFn) -> Result<PathBuf> {
    let root = layout.stage().join(&m.build);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let total = m.file_count() as u64;
    let mut n = 0u64;
    let fetched: std::collections::HashSet<(&str, &str)> =
        plan.fetch.iter().map(|(a, b, _)| (a.as_str(), b.as_str())).collect();
    for (folder, files) in &m.folders {
        for (rel, f) in files {
            let target = root.join(folder).join(rel);
            fs::create_dir_all(target.parent().unwrap())?;
            let from = if fetched.contains(&(folder.as_str(), rel.as_str())) {
                layout.cache().join(&f.sha256)
            } else {
                layout.addons().join(folder).join(rel)
            };
            fs::copy(&from, &target)?;
            let (sha, size) = sha256_file(&target)?;
            if sha != f.sha256 || size != f.size {
                return msg(format!("{folder}/{rel} changed while updating: try again"));
            }
            n += 1;
            if n % 25 == 0 || n == total {
                progress(Progress { step: "stage", done: n, total });
            }
        }
    }
    Ok(root)
}

fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for i in 0..5 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(200 * (i + 1)));
    }
    Err(last.unwrap())
}

/// UTC "YYYYMMDD-HHMMSS".
pub fn stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}{mo:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    #[serde(default)]
    pub id: String,
    pub version: Option<String>,
    pub build: Option<String>,
    pub time: String,
    /// why it was made: "update" | "restore"
    pub reason: String,
    #[serde(default)]
    pub saved_variables: usize,
}

/// Copy every DisasterZone*.lua(.bak) under WTF/Account/**/SavedVariables into `dest`, keeping the paths.
fn backup_saved_variables(layout: &Layout, dest: &Path) -> Result<usize> {
    let account = layout.wtf().join("Account");
    if !account.is_dir() {
        return Ok(0);
    }
    let mut n = 0;
    for entry in walkdir::WalkDir::new(&account).max_depth(5).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy();
        let in_sv = entry.path().parent().and_then(|p| p.file_name()).map(|p| p == "SavedVariables").unwrap_or(false);
        if entry.file_type().is_file() && in_sv && name.starts_with("DisasterZone") {
            let rel = entry.path().strip_prefix(layout.wtf()).unwrap();
            let target = dest.join("WTF").join(rel);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::copy(entry.path(), &target)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Swap the folders under `staged` (one per addon folder) into AddOns. The live folders go to a new backup, which
/// also gets a copy of the SavedVariables. Folders of FOLDERS that the new set lacks are moved out too.
fn swap_in(layout: &Layout, staged: &Path, reason: &str, keep: usize) -> Result<Option<String>> {
    let before = installed(layout);
    let id = format!("{}-{}", stamp(), before.version.clone().unwrap_or_else(|| "none".into()));
    let bdir = layout.backups().join(&id);
    fs::create_dir_all(&bdir)?;
    let sv = backup_saved_variables(layout, &bdir)?;
    fs::create_dir_all(layout.addons())?;

    let mut moved_out: Vec<&str> = vec![];
    let mut moved_in: Vec<&str> = vec![];
    let undo = |moved_out: &[&str], moved_in: &[&str]| {
        for f in moved_in {
            let _ = fs::rename(layout.addons().join(f), staged.join(f));
        }
        for f in moved_out {
            let _ = fs::rename(bdir.join(f), layout.addons().join(f));
        }
    };
    for folder in FOLDERS {
        let live = layout.addons().join(folder);
        if live.exists() {
            if let Err(e) = rename_retry(&live, &bdir.join(folder)) {
                undo(&moved_out, &moved_in);
                return msg(format!(
                    "could not move the old {folder} folder ({e}). Close WoW and any window showing the folder, then try again."
                ));
            }
            moved_out.push(folder);
        }
    }
    for folder in FOLDERS {
        if staged.join(folder).is_dir() {
            if let Err(e) = rename_retry(&staged.join(folder), &layout.addons().join(folder)) {
                undo(&moved_out, &moved_in);
                return msg(format!("could not put the new {folder} folder in place ({e}); nothing was changed"));
            }
            moved_in.push(folder);
        }
    }
    let _ = fs::remove_dir_all(staged);
    if moved_out.is_empty() && sv == 0 {
        let _ = fs::remove_dir_all(&bdir);
        return Ok(None);
    }
    let info = BackupInfo {
        id: id.clone(),
        version: before.version,
        build: before.build,
        time: stamp(),
        reason: reason.into(),
        saved_variables: sv,
    };
    fs::write(bdir.join("backup.json"), serde_json::to_vec_pretty(&info).unwrap())?;
    prune_backups(layout, keep)?;
    Ok(Some(id))
}

pub fn list_backups(layout: &Layout) -> Vec<BackupInfo> {
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(layout.backups()) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Ok(data) = fs::read(e.path().join("backup.json")) {
                if let Ok(mut info) = serde_json::from_slice::<BackupInfo>(&data) {
                    info.id = id;
                    out.push(info);
                }
            }
        }
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

fn prune_backups(layout: &Layout, keep: usize) -> Result<()> {
    for old in list_backups(layout).into_iter().skip(keep.max(1)) {
        fs::remove_dir_all(layout.backups().join(&old.id))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// The two operations
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub version: String,
    pub downloaded: u64,
    pub changed: usize,
    pub removed: usize,
    pub backup: Option<String>,
}

pub fn refuse_dev(layout: &Layout) -> Result<()> {
    let i = installed(layout);
    if i.dev || i.linked {
        return msg(
            "this DisasterZone folder is a development copy (build \"dev\" or a linked folder): the launcher leaves it alone",
        );
    }
    Ok(())
}

/// Bring the addon folders to exactly what `m` lists.
pub async fn update(layout: &Layout, src: &Source, m: &Manifest, keep: usize, progress: ProgressFn) -> Result<Outcome> {
    refuse_dev(layout)?;
    let plan = make_plan(layout, m)?;
    if plan.up_to_date {
        return Ok(Outcome { version: m.version.clone(), downloaded: 0, changed: 0, removed: 0, backup: None });
    }
    download(layout, src, &plan, &progress).await?;
    let staged = stage(layout, m, &plan, &progress)?;
    progress(Progress { step: "swap", done: 0, total: 1 });
    let backup = swap_in(layout, &staged, "update", keep)?;
    let _ = fs::remove_dir_all(layout.cache());
    progress(Progress { step: "done", done: 1, total: 1 });
    Ok(Outcome {
        version: m.version.clone(),
        downloaded: plan.download_bytes,
        changed: plan.fetch.len(),
        removed: plan.remove.len(),
        backup,
    })
}

/// Put the addon folders back to a backup. The backup stays; the current folders become a new backup.
pub fn restore(layout: &Layout, id: &str, keep: usize) -> Result<Option<String>> {
    refuse_dev(layout)?;
    if !super::manifest::safe_rel(id) || id.contains('/') {
        return msg("unknown backup");
    }
    let bdir = layout.backups().join(id);
    if !bdir.join(FOLDERS[0]).is_dir() {
        return msg("that backup has no addon folder");
    }
    let staged = layout.stage().join(format!("restore-{id}"));
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    for folder in FOLDERS {
        let from = bdir.join(folder);
        if !from.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&from).follow_links(false) {
            let entry = entry.map_err(|e| Error::Msg(e.to_string()))?;
            let target = staged.join(folder).join(entry.path().strip_prefix(&from).unwrap());
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target)?;
            } else if entry.file_type().is_file() {
                fs::copy(entry.path(), &target)?;
            }
        }
    }
    // keep one more, or the backup being restored could be pruned by the new one
    swap_in(layout, &staged, "restore", keep + 1)
}

#[cfg(test)]
mod tests {
    use super::super::manifest::tests::{sign, test_key};
    use super::super::manifest::{verify_and_parse, ReleaseKey};
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::io::Write;

    struct Fixture {
        _tmp: tempfile::TempDir,
        pub_dir: PathBuf,
        layout: Layout,
        sk: SigningKey,
        rk: ReleaseKey,
        seq: u64,
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let pub_dir = tmp.path().join("pub");
            let layout = Layout::new(tmp.path().join("WoW").join("_anniversary_"));
            fs::create_dir_all(layout.addons()).unwrap();
            let (sk, rk) = test_key();
            Fixture { _tmp: tmp, pub_dir, layout, sk, rk, seq: 0 }
        }

        /// Publish a release with these files (DisasterZone + optional rooms); returns the verified manifest.
        fn publish(&mut self, version: &str, build: &str, files: &[(&str, &str)], rooms: &[(&str, &str)]) -> Manifest {
            self.seq += 1;
            let mut folders: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
            let mut all: Vec<(&str, String, String)> = vec![
                ("DisasterZone", "DisasterZone.toc".into(), format!("## Version: {version}\n")),
                ("DisasterZone", "Core/BuildInfo.lua".into(), format!("DisasterZoneBuild = {{\n    id = \"{build}\",\n    assetPack = 2,\n}}\n")),
            ];
            all.extend(files.iter().map(|(p, t)| ("DisasterZone", p.to_string(), t.to_string())));
            all.extend(rooms.iter().map(|(p, t)| ("DisasterZone_Rooms", p.to_string(), t.to_string())));
            for (folder, rel, text) in &all {
                let sha = hex::encode(Sha256::digest(text.as_bytes()));
                let obj = self.pub_dir.join(object_path(&sha));
                let z = gz(text.as_bytes());
                fs::create_dir_all(obj.parent().unwrap()).unwrap();
                fs::write(&obj, &z).unwrap();
                folders.entry(folder.to_string()).or_default().insert(
                    rel.clone(),
                    serde_json::json!({"sha256": sha, "size": text.len(), "gz": z.len()}),
                );
            }
            let m = serde_json::json!({"format": 1, "name": "DisasterZone", "channel": "stable", "seq": self.seq,
                "version": version, "build": build, "assetPack": 2, "folders": folders});
            let data = serde_json::to_vec_pretty(&m).unwrap();
            let sig = sign(&self.sk, &self.rk, &data);
            fs::create_dir_all(self.pub_dir.join("channels")).unwrap();
            fs::write(self.pub_dir.join("channels/stable.json"), &data).unwrap();
            fs::write(self.pub_dir.join("channels/stable.json.sig"), &sig).unwrap();
            verify_and_parse(&self.rk, &data, &sig).unwrap()
        }

        fn src(&self) -> Source {
            Source::Dir(self.pub_dir.clone())
        }

        fn read(&self, folder: &str, rel: &str) -> Option<String> {
            fs::read_to_string(self.layout.addons().join(folder).join(rel)).ok()
        }
    }

    #[tokio::test]
    async fn fresh_install_then_an_update_touches_only_what_changed() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1"), ("Core/B.lua", "b1")], &[("DisasterZone_Rooms.toc", "r")]);
        let out = update(&fx.layout, &fx.src(), &m, 3, no_progress()).await.unwrap();
        assert_eq!(out.changed, 5);
        assert_eq!(out.backup, None); // nothing was installed, nothing to back up
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert!(installed(&fx.layout).rooms);
        assert_eq!(installed(&fx.layout).build.as_deref(), Some("b1"));
        assert_eq!(installed(&fx.layout).version.as_deref(), Some("1.0"));

        // a stray file in the live folder, then 1.1 changes A and drops B
        fs::write(fx.layout.addons().join("DisasterZone/stray.txt"), "x").unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2"), ("Core/C.lua", "c")], &[("DisasterZone_Rooms.toc", "r")]);
        let plan = make_plan(&fx.layout, &m2).unwrap();
        let fetched: Vec<&str> = plan.fetch.iter().map(|(_, r, _)| r.as_str()).collect();
        assert_eq!(fetched, vec!["Core/A.lua", "Core/BuildInfo.lua", "Core/C.lua", "DisasterZone.toc"]);
        assert_eq!(plan.remove.len(), 2); // B.lua + stray.txt
        let out = update(&fx.layout, &fx.src(), &m2, 3, no_progress()).await.unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a2"));
        assert_eq!(fx.read("DisasterZone", "Core/B.lua"), None);
        assert_eq!(fx.read("DisasterZone", "stray.txt"), None);
        assert_eq!(fx.read("DisasterZone_Rooms", "DisasterZone_Rooms.toc").as_deref(), Some("r"));
        let backups = list_backups(&fx.layout);
        assert_eq!(backups.len(), 1);
        assert_eq!(Some(backups[0].id.clone()), out.backup);
        assert_eq!(backups[0].version.as_deref(), Some("1.0"));
        assert!(make_plan(&fx.layout, &m2).unwrap().up_to_date);
        assert!(!fx.layout.cache().exists());
    }

    #[tokio::test]
    async fn a_bad_object_changes_nothing() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[]);
        update(&fx.layout, &fx.src(), &m, 3, no_progress()).await.unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[]);
        let sha = &m2.folders["DisasterZone"]["Core/A.lua"].sha256;
        fs::write(fx.pub_dir.join(object_path(sha)), gz(b"evil")).unwrap();
        let err = update(&fx.layout, &fx.src(), &m2, 3, no_progress()).await.unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert_eq!(installed(&fx.layout).build.as_deref(), Some("b1"));
    }

    #[tokio::test]
    async fn a_development_copy_is_left_alone() {
        let mut fx = Fixture::new();
        let dz = fx.layout.addons().join("DisasterZone");
        fs::create_dir_all(dz.join("Core")).unwrap();
        fs::write(dz.join("DisasterZone.toc"), "## Version: 9.9\n").unwrap();
        fs::write(dz.join("Core/BuildInfo.lua"), "DisasterZoneBuild = {\n    id = \"dev\",\n    assetPack = 7,\n}\n").unwrap();
        assert!(installed(&fx.layout).dev);
        assert_eq!(installed(&fx.layout).asset_pack, Some(7));
        let m = fx.publish("1.0", "b1", &[], &[]);
        let err = update(&fx.layout, &fx.src(), &m, 3, no_progress()).await.unwrap_err();
        assert!(err.to_string().contains("development copy"));
        assert_eq!(fs::read_to_string(dz.join("DisasterZone.toc")).unwrap(), "## Version: 9.9\n");
    }

    #[tokio::test]
    async fn restore_puts_a_backup_back_and_keeps_saved_variables() {
        let mut fx = Fixture::new();
        let sv = fx.layout.wtf().join("Account/ME/SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join("DisasterZone.lua"), "DisasterZoneDB = {}").unwrap();
        fs::write(sv.join("Other.lua"), "x").unwrap();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[("DisasterZone_Rooms.toc", "r")]);
        update(&fx.layout, &fx.src(), &m, 3, no_progress()).await.unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[]);
        let id = update(&fx.layout, &fx.src(), &m2, 3, no_progress()).await.unwrap().backup.unwrap();
        assert!(!installed(&fx.layout).rooms); // 1.1 has no rooms folder: the old one went to the backup
        let b = fx.layout.backups().join(&id);
        assert!(b.join("WTF/Account/ME/SavedVariables/DisasterZone.lua").is_file());
        assert!(!b.join("WTF/Account/ME/SavedVariables/Other.lua").exists());
        assert_eq!(list_backups(&fx.layout)[0].saved_variables, 1);

        std::thread::sleep(Duration::from_millis(1100)); // (backup ids are per second)
        restore(&fx.layout, &id, 3).unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert!(installed(&fx.layout).rooms);
        let all = list_backups(&fx.layout);
        assert_eq!(all.len(), 3); // first install (saved variables only), the update, the restore
        assert_eq!(all[0].reason, "restore");
        assert_eq!(all[0].version.as_deref(), Some("1.1"));
        assert!(restore(&fx.layout, "../x", 3).is_err());
    }

    #[tokio::test]
    async fn old_backups_are_pruned() {
        let mut fx = Fixture::new();
        for i in 0..4 {
            let m = fx.publish(&format!("1.{i}"), &format!("b{i}"), &[("Core/A.lua", &format!("a{i}"))], &[]);
            update(&fx.layout, &fx.src(), &m, 2, no_progress()).await.unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
        let all = list_backups(&fx.layout);
        assert_eq!(all.iter().map(|b| b.version.clone().unwrap()).collect::<Vec<_>>(), vec!["1.2", "1.1"]);
    }

    #[test]
    fn stamp_looks_right() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
    }
}
