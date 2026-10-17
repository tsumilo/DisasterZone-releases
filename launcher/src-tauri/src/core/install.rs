//! Install, update and roll back the addon folders.
//!
//! A release is a set of folders, one per pack (manifest.rs): the core DisasterZone, Extras (the 3D rooms) and one
//! per game. Which of them this PC keeps is the user's choice (state.rs `packs`, see wanted()); the core is always in.
//! An update never writes into the live folders. It
//!   1. compares the installed files of every wanted folder with the manifest (sha256),
//!   2. downloads only the missing objects into a cache (each checked against its hash),
//!   3. builds complete new copies of the folders that change in a staging area (unchanged files copied from the
//!      live folder),
//!   4. backs up the SavedVariables, then swaps: the live folders that change or go -> backups/<id>, staged folders
//!      -> AddOns. Folders that don't change are not touched, so a backup holds only what its update replaced.
//! A failure before step 4 leaves the game untouched; a failure during the swap puts the old folders back.
//! DisasterZone_* folders the release doesn't list (the launcher's own DisasterZone_Bridge, a pack a release dropped)
//! are never touched. The work area is <flavor>/Interface/DZLauncher (same volume as AddOns, so the swap is a rename;
//! WoW only reads Interface/AddOns).

use super::error::{msg, Error, Result};
use super::manifest::{object_path, pack_folder_name, pack_id, safe_rel, FileEntry, Manifest, CORE, RESERVED, ROOMS};
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

/// A folder name a release may write: DisasterZone(_X), never one the launcher writes itself.
pub fn release_folder(name: &str) -> bool {
    pack_folder_name(name) && !RESERVED.contains(&name)
}

// ---------------------------------------------------------------------------------------------------------------
// What is installed
// ---------------------------------------------------------------------------------------------------------------

/// A pack folder next to DisasterZone (its TOC has an X-DZ-Pack line, or it is the 3D rooms folder).
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPack {
    pub id: String,
    pub folder: String,
    /// "extra" | "game"
    pub kind: String,
    pub title: String,
    pub linked: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub present: bool,
    pub version: Option<String>,
    pub build: Option<String>,
    pub asset_pack: Option<u32>,
    /// A development copy (build id "dev"): the publisher's own source tree. Never updated.
    pub dev: bool,
    /// DisasterZone or a DisasterZone_* folder next to it is a link or junction (a development setup). Never updated.
    pub linked: bool,
    pub rooms: bool,
    pub packs: Vec<InstalledPack>,
}

fn quoted_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let i = text.find(key)? + key.len();
    let rest = &text[i..];
    let a = rest.find('"')? + 1;
    let b = rest[a..].find('"')? + a;
    Some(&rest[a..b])
}

/// The first `## key: value` of a TOC (keys ignore case).
pub fn toc_field(toc: &str, key: &str) -> Option<String> {
    toc.lines().find_map(|l| {
        let (k, v) = l.trim().strip_prefix("##")?.split_once(':')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}

/// A TOC title as the window shows it: no colour codes, no "DisasterZone · " in front
/// ("|cffff8000Disaster|rZone · Azeromon" -> "Azeromon"). The same rule as launchlib.clean_title.
pub fn clean_title(raw: &str) -> String {
    let c: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '|' && c.get(i + 1) == Some(&'c') && c.len() >= i + 10 && c[i + 2..i + 10].iter().all(|x| x.is_ascii_hexdigit()) {
            i += 10;
        } else if c[i] == '|' && c.get(i + 1) == Some(&'r') {
            i += 2;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    let t = out.trim();
    for prefix in ["DisasterZone · ", "DisasterZone - ", "DisasterZone "] {
        if let Some(rest) = t.strip_prefix(prefix).map(str::trim).filter(|r| !r.is_empty()) {
            return rest.to_string();
        }
    }
    t.to_string()
}

fn read_text(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

fn toc_path(layout: &Layout, folder: &str) -> PathBuf {
    layout.addons().join(folder).join(format!("{folder}.toc"))
}

/// The folder is installed: its TOC is there (an empty folder left behind doesn't count).
pub fn on_disk(layout: &Layout, folder: &str) -> bool {
    toc_path(layout, folder).is_file()
}

/// The DisasterZone_* folders in AddOns a release could own (not the core, not the launcher's own).
fn pack_dirs(layout: &Layout) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(layout.addons())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n != CORE && release_folder(n))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

pub fn installed(layout: &Layout) -> Installed {
    let dir = layout.addons().join(CORE);
    let mut out = Installed::default();
    let others = pack_dirs(layout);
    out.linked = is_link(&dir) || others.iter().any(|f| is_link(&layout.addons().join(f)));
    for folder in &others {
        let Some(toc) = read_text(&toc_path(layout, folder)) else { continue };
        let id = toc_field(&toc, "X-DZ-Pack").or_else(|| (folder == ROOMS).then(|| "rooms".to_string()));
        let Some(id) = id.filter(|i| pack_id(i) && i != "core") else { continue };
        out.packs.push(InstalledPack {
            id,
            kind: toc_field(&toc, "X-DZ-Kind").unwrap_or_else(|| if folder == ROOMS { "extra" } else { "game" }.into()),
            title: toc_field(&toc, "Title").map(|t| clean_title(&t)).unwrap_or_else(|| folder.clone()),
            linked: is_link(&layout.addons().join(folder)),
            folder: folder.clone(),
        });
    }
    out.rooms = on_disk(layout, ROOMS);
    let Some(toc) = read_text(&toc_path(layout, CORE)) else { return out };
    out.present = true;
    out.version = toc_field(&toc, "Version");
    if let Ok(info) = fs::read_to_string(dir.join("Core").join("BuildInfo.lua")) {
        out.build = quoted_after(&info, "id").map(str::to_string);
        out.asset_pack = info
            .find("assetPack")
            .and_then(|i| info[i..].split(['=', ',', '\n']).nth(1))
            .and_then(|v| v.trim().parse().ok());
    }
    out.dev = out.build.as_deref() == Some("dev");
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

/// Bytes of every file under `dir` (0 when it does not exist).
pub fn dir_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

// ---------------------------------------------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------------------------------------------

/// The folders this PC keeps up to date: required packs, packs chosen on (the picker, the Games tab), and packs
/// nobody chose either way that are installed (a folder copied in by hand counts) or on by default (Extras).
pub fn wanted(layout: &Layout, m: &Manifest, choice: &BTreeMap<String, bool>) -> Vec<String> {
    m.packs
        .iter()
        .filter(|p| p.required || choice.get(&p.id).copied().unwrap_or_else(|| p.default_on || on_disk(layout, &p.folder)))
        .map(|p| p.folder.clone())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPlan {
    pub folder: String,
    /// "install" (not there yet) | "update" | "same" | "remove" (installed, no longer wanted)
    pub action: &'static str,
    /// files to download (or take from the cache)
    pub fetch: usize,
    /// files to delete: installed but not in the release
    pub remove: usize,
    /// compressed bytes still to download for this folder
    pub download_bytes: u64,
    /// what the folder takes on disk now
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// (folder, path, entry) to download (or take from the cache)
    pub fetch: Vec<(String, String, FileEntry)>,
    /// files to delete inside the folders that stay: installed but not in the release
    pub remove: Vec<(String, String)>,
    /// every pack of the release that is wanted or installed, in the manifest's order
    pub folders: Vec<FolderPlan>,
    pub unchanged: usize,
    /// compressed bytes still to download (cached objects are not counted)
    pub download_bytes: u64,
    pub up_to_date: bool,
}

impl Plan {
    fn with_action(&self, actions: &[&str]) -> Vec<String> {
        self.folders.iter().filter(|f| actions.contains(&f.action)).map(|f| f.folder.clone()).collect()
    }
    /// The folders that get a new copy (installed or updated).
    pub fn put(&self) -> Vec<String> {
        self.with_action(&["install", "update"])
    }
    /// The folders that leave AddOns.
    pub fn dropped(&self) -> Vec<String> {
        self.with_action(&["remove"])
    }
}

/// What it takes to bring the `wanted` folders to the release (and to take out the installed ones that aren't).
pub fn make_plan(layout: &Layout, m: &Manifest, wanted: &[String]) -> Result<Plan> {
    let mut plan =
        Plan { fetch: vec![], remove: vec![], folders: vec![], unchanged: 0, download_bytes: 0, up_to_date: false };
    let empty = BTreeMap::new();
    for p in &m.packs {
        let folder = p.folder.as_str();
        let live = layout.addons().join(folder);
        let mut fp = FolderPlan { folder: folder.into(), action: "same", fetch: 0, remove: 0, download_bytes: 0, disk_bytes: 0 };
        if !p.required && !wanted.iter().any(|w| w == folder) {
            if on_disk(layout, folder) {
                fp.action = "remove";
                fp.disk_bytes = dir_bytes(&live);
                plan.folders.push(fp);
            }
            continue;
        }
        let have = scan(&live)?;
        fp.disk_bytes = have.values().map(|(_, n)| n).sum();
        let want = m.folders.get(folder).unwrap_or(&empty);
        for (rel, f) in want {
            if have.get(rel).map(|(sha, _)| sha == &f.sha256).unwrap_or(false) {
                plan.unchanged += 1;
            } else {
                if !layout.cache().join(&f.sha256).is_file() {
                    fp.download_bytes += f.gz;
                }
                fp.fetch += 1;
                plan.fetch.push((folder.to_string(), rel.clone(), f.clone()));
            }
        }
        for rel in have.keys() {
            if !want.contains_key(rel) {
                fp.remove += 1;
                plan.remove.push((folder.to_string(), rel.clone()));
            }
        }
        fp.action = if !on_disk(layout, folder) {
            "install"
        } else if fp.fetch + fp.remove > 0 {
            "update"
        } else {
            "same"
        };
        plan.download_bytes += fp.download_bytes;
        plan.folders.push(fp);
    }
    plan.up_to_date = plan.folders.iter().all(|f| f.action == "same");
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

/// Build complete new copies of the folders that change under stage/: unchanged files from the live folder, the rest
/// from the cache. Every staged file is checked against the manifest afterwards.
fn stage(layout: &Layout, m: &Manifest, plan: &Plan, progress: &ProgressFn) -> Result<PathBuf> {
    let root = layout.stage().join(&m.build);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let put = plan.put();
    let empty = BTreeMap::new();
    let total: u64 = put.iter().map(|f| m.folders.get(f).map_or(0, |x| x.len()) as u64).sum();
    let mut n = 0u64;
    let fetched: std::collections::HashSet<(&str, &str)> =
        plan.fetch.iter().map(|(a, b, _)| (a.as_str(), b.as_str())).collect();
    for folder in &put {
        fs::create_dir_all(root.join(folder))?;
        for (rel, f) in m.folders.get(folder).unwrap_or(&empty) {
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

/// UTC "YYYYMMDD-HHMMSS" of a unix time.
fn stamp_at(secs: u64) -> String {
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

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// UTC "YYYYMMDD-HHMMSS".
pub fn stamp() -> String {
    stamp_at(now().as_secs())
}

/// A new backup's id: "YYYYMMDD-HHMMSS.mmm-<version>". To the millisecond, so ids sort in the order the backups were
/// made (a restore applies them in that order); ids from launcher 0.3 (no milliseconds) sort before.
fn backup_id(layout: &Layout, version: &str) -> String {
    let version: String =
        version.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    loop {
        let t = now();
        let id = format!("{}.{:03}-{version}", stamp_at(t.as_secs()), t.subsec_millis());
        if !layout.backups().join(&id).exists() {
            return id;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    #[serde(default)]
    pub id: String,
    pub version: Option<String>,
    pub build: Option<String>,
    pub time: String,
    /// why it was made: "update" | "restore" | "remove"
    pub reason: String,
    #[serde(default)]
    pub saved_variables: usize,
    /// the addon folders it holds, as they were before (a backup from launcher 0.3: every folder in it)
    #[serde(default)]
    pub folders: Vec<String>,
    /// folders its operation added (they weren't there before): a restore takes them out again
    #[serde(default)]
    pub added: Vec<String>,
    /// bytes of the folders it holds
    #[serde(default)]
    pub bytes: u64,
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

/// Put the staged copies of `put` (folders under `staged`) in place and take `drop` out of AddOns. Every live folder
/// that goes (replaced or dropped) moves into a new backup, which also gets a copy of the SavedVariables; folders of
/// `put` that weren't there are recorded as added. Nothing else in AddOns is touched.
fn swap_in(
    layout: &Layout,
    staged: Option<&Path>,
    put: &[String],
    drop: &[String],
    reason: &str,
    keep: usize,
) -> Result<Option<String>> {
    let before = installed(layout);
    let id = backup_id(layout, before.version.as_deref().unwrap_or("none"));
    let bdir = layout.backups().join(&id);
    fs::create_dir_all(&bdir)?;
    let sv = backup_saved_variables(layout, &bdir)?;
    fs::create_dir_all(layout.addons())?;

    let mut moved_out: Vec<String> = vec![];
    let mut moved_in: Vec<String> = vec![];
    let mut added: Vec<String> = vec![];
    let undo = |moved_out: &[String], moved_in: &[String]| {
        if let Some(staged) = staged {
            for f in moved_in {
                let _ = fs::rename(layout.addons().join(f), staged.join(f));
            }
        }
        for f in moved_out {
            let _ = fs::rename(bdir.join(f), layout.addons().join(f));
        }
        let _ = fs::remove_dir_all(&bdir);
    };
    for folder in put.iter().chain(drop) {
        let live = layout.addons().join(folder);
        if fs::symlink_metadata(&live).is_ok() {
            if let Err(e) = rename_retry(&live, &bdir.join(folder)) {
                undo(&moved_out, &moved_in);
                return msg(format!(
                    "could not move the old {folder} folder ({e}). Close WoW and any window showing the folder, then try again."
                ));
            }
            moved_out.push(folder.clone());
        } else if put.contains(folder) {
            added.push(folder.clone());
        }
    }
    if let Some(staged) = staged {
        for folder in put {
            if staged.join(folder).is_dir() {
                if let Err(e) = rename_retry(&staged.join(folder), &layout.addons().join(folder)) {
                    undo(&moved_out, &moved_in);
                    return msg(format!("could not put the new {folder} folder in place ({e}); nothing was changed"));
                }
                moved_in.push(folder.clone());
            }
        }
        let _ = fs::remove_dir_all(staged);
    }
    // (a first install with no saved variables yet: there is nothing to go back to)
    if moved_out.is_empty() && sv == 0 && (added.is_empty() || !before.present) {
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
        bytes: moved_out.iter().map(|f| dir_bytes(&bdir.join(f))).sum(),
        folders: moved_out,
        added,
    };
    fs::write(bdir.join("backup.json"), serde_json::to_vec_pretty(&info).unwrap())?;
    prune_backups(layout, keep)?;
    Ok(Some(id))
}

/// Every backup, newest first.
pub fn list_backups(layout: &Layout) -> Vec<BackupInfo> {
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(layout.backups()) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Ok(data) = fs::read(e.path().join("backup.json")) {
                if let Ok(mut info) = serde_json::from_slice::<BackupInfo>(&data) {
                    info.id = id;
                    if info.folders.is_empty() && info.added.is_empty() {
                        // (launcher 0.3 didn't list them: every addon folder in it)
                        let mut found: Vec<String> = fs::read_dir(e.path())
                            .map(|rd| {
                                rd.flatten()
                                    .filter(|d| d.path().is_dir())
                                    .map(|d| d.file_name().to_string_lossy().to_string())
                                    .filter(|n| release_folder(n))
                                    .collect()
                            })
                            .unwrap_or_default();
                        found.sort();
                        info.folders = found;
                    }
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

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(from).follow_links(false) {
        let entry = entry.map_err(|e| Error::Msg(e.to_string()))?;
        let target = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub version: String,
    pub downloaded: u64,
    pub changed: usize,
    pub removed: usize,
    pub backup: Option<String>,
    /// the folders it installed, updated or took out
    pub folders: Vec<FolderPlan>,
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

/// Bring the `wanted` folders (see wanted()) to exactly what `m` lists, and take out the installed packs that aren't
/// wanted. One version for everything: the core and every kept pack move together.
pub async fn update(
    layout: &Layout,
    src: &Source,
    m: &Manifest,
    wanted: &[String],
    keep: usize,
    progress: ProgressFn,
) -> Result<Outcome> {
    refuse_dev(layout)?;
    let plan = make_plan(layout, m, wanted)?;
    if plan.up_to_date {
        return Ok(Outcome { version: m.version.clone(), downloaded: 0, changed: 0, removed: 0, backup: None, folders: vec![] });
    }
    download(layout, src, &plan, &progress).await?;
    let staged = stage(layout, m, &plan, &progress)?;
    progress(Progress { step: "swap", done: 0, total: 1 });
    let backup = swap_in(layout, Some(&staged), &plan.put(), &plan.dropped(), "update", keep)?;
    let _ = fs::remove_dir_all(layout.cache());
    progress(Progress { step: "done", done: 1, total: 1 });
    Ok(Outcome {
        version: m.version.clone(),
        downloaded: plan.download_bytes,
        changed: plan.fetch.len(),
        removed: plan.remove.len(),
        backup,
        folders: plan.folders.into_iter().filter(|f| f.action != "same").collect(),
    })
}

/// Take one pack out of AddOns: its folder moves into a new backup (reason "remove"), so Backups can put it back.
pub fn remove(layout: &Layout, folder: &str, keep: usize) -> Result<Option<String>> {
    if folder == CORE || !release_folder(folder) {
        return msg("that folder can't be removed");
    }
    refuse_dev(layout)?;
    if fs::symlink_metadata(layout.addons().join(folder)).is_err() {
        return Ok(None);
    }
    swap_in(layout, None, &[], &[folder.to_string()], "remove", keep)
}

/// Go back to how the addon folders were before backup `id` was made. Every backup from the newest down to `id` is
/// applied, newest first, so each folder ends up as the oldest of them found it: a backup holds only the folders its
/// operation changed, and applying one alone could put an old core next to newer packs. The backups stay; the
/// folders this changes become a new backup.
pub fn restore(layout: &Layout, id: &str, keep: usize) -> Result<Option<String>> {
    refuse_dev(layout)?;
    if !safe_rel(id) || id.contains('/') {
        return msg("unknown backup");
    }
    let all = list_backups(layout);
    let Some(pos) = all.iter().position(|b| b.id == id) else { return msg("unknown backup") };
    // folder -> its copy in a backup, or None: it wasn't there
    let mut target: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
    for b in &all[..=pos] {
        let bdir = layout.backups().join(&b.id);
        for f in b.folders.iter().filter(|f| release_folder(f)) {
            if !bdir.join(f).is_dir() {
                return msg(format!("the backup {} is missing its {f} folder", b.id));
            }
            target.insert(f.clone(), Some(bdir.join(f)));
        }
        for f in b.added.iter().filter(|f| release_folder(f)) {
            target.insert(f.clone(), None);
        }
    }
    if target.is_empty() {
        return msg("that backup has no addon folder");
    }
    if matches!(target.get(CORE), Some(None)) {
        return msg("that backup is from before DisasterZone was installed: there is no version to go back to");
    }
    let staged = layout.stage().join(format!("restore-{id}"));
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    let (mut put, mut drop) = (vec![], vec![]);
    for (folder, from) in &target {
        match from {
            Some(from) => {
                copy_tree(from, &staged.join(folder))?;
                put.push(folder.clone());
            }
            None => drop.push(folder.clone()),
        }
    }
    // keep the backups just restored: the new one would otherwise push the oldest of them out
    swap_in(layout, Some(&staged), &put, &drop, "restore", (keep + 1).max(pos + 2))
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

    type Files<'a> = &'a [(&'a str, &'a str)];

    /// (id, kind) of a test pack folder: the 3D rooms, or a game named after the folder.
    fn pack_of(folder: &str) -> (String, &'static str) {
        if folder == ROOMS {
            ("rooms".into(), "extra")
        } else {
            (folder.trim_start_matches("DisasterZone_").to_lowercase(), "game")
        }
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

        /// Publish a format 2 release: DisasterZone with these files, plus pack folders (each gets its TOC with the
        /// pack lines; the 3D rooms are an Extra, any other folder a game). Returns the verified manifest.
        fn publish(&mut self, version: &str, build: &str, files: Files, packs: &[(&str, Files)]) -> Manifest {
            self.seq += 1;
            let mut folders: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
            let mut all: Vec<(String, String, String)> = vec![
                (CORE.into(), "DisasterZone.toc".into(), format!("## Title: |cffff8000Disaster|rZone\n## Version: {version}\n")),
                (CORE.into(), "Core/BuildInfo.lua".into(), format!("DisasterZoneBuild = {{\n    id = \"{build}\",\n    assetPack = 2,\n}}\n")),
            ];
            all.extend(files.iter().map(|(p, t)| (CORE.to_string(), p.to_string(), t.to_string())));
            let mut packs_json = vec![serde_json::json!({"id": "core", "folder": CORE, "kind": "core",
                "title": "DisasterZone", "tagline": "", "required": true, "defaultOn": true})];
            for (folder, pfiles) in packs {
                let (id, kind) = pack_of(folder);
                all.push((folder.to_string(), format!("{folder}.toc"),
                    format!("## Title: |cffff8000Disaster|rZone · {id}\n## X-DZ-Pack: {id}\n## X-DZ-Kind: {kind}\n")));
                all.extend(pfiles.iter().map(|(p, t)| (folder.to_string(), p.to_string(), t.to_string())));
                packs_json.push(serde_json::json!({"id": id, "folder": folder, "kind": kind, "title": id,
                    "tagline": "", "required": false, "defaultOn": kind == "extra"}));
            }
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
            let m = serde_json::json!({"format": 2, "name": "DisasterZone", "channel": "stable", "seq": self.seq,
                "version": version, "build": build, "assetPack": 2, "folders": folders, "packs": packs_json});
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

        /// Update with these choices ({id: on}); the rest as wanted() decides.
        async fn update_with(&self, m: &Manifest, choice: &[(&str, bool)], keep: usize) -> Result<Outcome> {
            let choice: BTreeMap<String, bool> = choice.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            update(&self.layout, &self.src(), m, &wanted(&self.layout, m, &choice), keep, no_progress()).await
        }

        /// The folders a backup directory holds (WTF = saved variables).
        fn backup_dirs(&self, id: &str) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(self.layout.backups().join(id))
                .unwrap()
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            v.sort();
            v
        }
    }

    const ROOMS_FILES: Files = &[("Rooms/A.lua", "r")];

    #[tokio::test]
    async fn fresh_install_then_an_update_touches_only_what_changed() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1"), ("Core/B.lua", "b1")], &[(ROOMS, ROOMS_FILES)]);
        let out = fx.update_with(&m, &[], 3).await.unwrap();
        assert_eq!(out.changed, 6); // 4 core files, the rooms' TOC and file (on by default)
        assert_eq!(out.backup, None); // nothing was installed, nothing to back up
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        let i = installed(&fx.layout);
        assert!(i.rooms);
        assert_eq!(i.build.as_deref(), Some("b1"));
        assert_eq!(i.version.as_deref(), Some("1.0"));
        assert_eq!(i.packs.iter().map(|p| (p.id.as_str(), p.kind.as_str(), p.title.as_str())).collect::<Vec<_>>(),
            vec![("rooms", "extra", "rooms")]);

        // a stray file in the live folder, then 1.1 changes A and drops B
        fs::write(fx.layout.addons().join("DisasterZone/stray.txt"), "x").unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2"), ("Core/C.lua", "c")], &[(ROOMS, ROOMS_FILES)]);
        let plan = make_plan(&fx.layout, &m2, &wanted(&fx.layout, &m2, &BTreeMap::new())).unwrap();
        let fetched: Vec<&str> = plan.fetch.iter().map(|(_, r, _)| r.as_str()).collect();
        assert_eq!(fetched, vec!["Core/A.lua", "Core/BuildInfo.lua", "Core/C.lua", "DisasterZone.toc"]);
        assert_eq!(plan.remove.len(), 2); // B.lua + stray.txt
        assert_eq!(plan.put(), vec!["DisasterZone"]);
        let out = fx.update_with(&m2, &[], 3).await.unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a2"));
        assert_eq!(fx.read("DisasterZone", "Core/B.lua"), None);
        assert_eq!(fx.read("DisasterZone", "stray.txt"), None);
        assert_eq!(fx.read("DisasterZone_Rooms", "Rooms/A.lua").as_deref(), Some("r"));
        let backups = list_backups(&fx.layout);
        assert_eq!(backups.len(), 1);
        assert_eq!(Some(backups[0].id.clone()), out.backup);
        assert_eq!(backups[0].version.as_deref(), Some("1.0"));
        assert_eq!(backups[0].folders, vec!["DisasterZone"]);
        assert!(make_plan(&fx.layout, &m2, &wanted(&fx.layout, &m2, &BTreeMap::new())).unwrap().up_to_date);
        assert!(!fx.layout.cache().exists());
    }

    #[tokio::test]
    async fn a_bad_object_changes_nothing() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[]);
        fx.update_with(&m, &[], 3).await.unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[]);
        let sha = &m2.folders["DisasterZone"]["Core/A.lua"].sha256;
        fs::write(fx.pub_dir.join(object_path(sha)), gz(b"evil")).unwrap();
        let err = fx.update_with(&m2, &[], 3).await.unwrap_err();
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
        let err = fx.update_with(&m, &[], 3).await.unwrap_err();
        assert!(err.to_string().contains("development copy"));
        assert_eq!(fs::read_to_string(dz.join("DisasterZone.toc")).unwrap(), "## Version: 9.9\n");
        assert!(remove(&fx.layout, "DisasterZone_Rooms", 3).unwrap_err().to_string().contains("development copy"));
    }

    /// A directory link the way a developer makes one (a junction on Windows); false when the OS refuses.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[tokio::test]
    async fn a_linked_pack_folder_makes_it_a_development_copy() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[("DisasterZone_Chess", &[("Chess.lua", "c")])]);
        fx.update_with(&m, &[("chess", true)], 3).await.unwrap();
        let src = fx._tmp.path().join("devkit_chess");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("DisasterZone_Chess.toc"), "## X-DZ-Pack: chess\n").unwrap();
        fs::rename(fx.layout.addons().join("DisasterZone_Chess"), fx._tmp.path().join("old_chess")).unwrap();
        if !link_dir(&src, &fx.layout.addons().join("DisasterZone_Chess")) {
            eprintln!("could not make a directory link here: skipped");
            return;
        }
        assert!(installed(&fx.layout).linked);
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[("DisasterZone_Chess", &[("Chess.lua", "c2")])]);
        let err = fx.update_with(&m2, &[("chess", true)], 3).await.unwrap_err();
        assert!(err.to_string().contains("development copy"), "{err}");
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert!(src.join("DisasterZone_Chess.toc").is_file());
    }

    #[tokio::test]
    async fn restore_puts_a_backup_back_and_keeps_saved_variables() {
        let mut fx = Fixture::new();
        let sv = fx.layout.wtf().join("Account/ME/SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join("DisasterZone.lua"), "DisasterZoneDB = {}").unwrap();
        fs::write(sv.join("Other.lua"), "x").unwrap();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[(ROOMS, ROOMS_FILES)]);
        fx.update_with(&m, &[], 3).await.unwrap();
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[(ROOMS, ROOMS_FILES)]);
        let id = fx.update_with(&m2, &[], 3).await.unwrap().backup.unwrap();
        let b = fx.layout.backups().join(&id);
        assert!(b.join("WTF/Account/ME/SavedVariables/DisasterZone.lua").is_file());
        assert!(!b.join("WTF/Account/ME/SavedVariables/Other.lua").exists());
        assert_eq!(list_backups(&fx.layout)[0].saved_variables, 1);
        assert_eq!(fx.backup_dirs(&id), vec!["DisasterZone", "WTF"]); // (the rooms didn't change)

        restore(&fx.layout, &id, 3).unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert!(installed(&fx.layout).rooms);
        let all = list_backups(&fx.layout);
        assert_eq!(all.len(), 3); // first install (saved variables only), the update, the restore
        assert_eq!(all[0].reason, "restore");
        assert_eq!(all[0].version.as_deref(), Some("1.1"));
        assert_eq!(all[0].folders, vec!["DisasterZone"]);
        // the first install's backup: nothing to go back to
        assert_eq!(all[2].added, vec!["DisasterZone", "DisasterZone_Rooms"]);
        assert!(restore(&fx.layout, &all[2].id, 3).unwrap_err().to_string().contains("before DisasterZone was installed"));
        assert!(restore(&fx.layout, "../x", 3).is_err());
        assert!(restore(&fx.layout, "nope", 3).is_err());
    }

    #[tokio::test]
    async fn a_core_only_update_swaps_only_disasterzone() {
        let mut fx = Fixture::new();
        let chess: Files = &[("Chess.lua", "c1"), ("Media/board.tga", "tga")];
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[(ROOMS, ROOMS_FILES), ("DisasterZone_Chess", chess)]);
        // the Bridge folder the launcher writes, and a pack of some later release: never touched
        for f in ["DisasterZone_Bridge", "DisasterZone_Later"] {
            fs::create_dir_all(fx.layout.addons().join(f)).unwrap();
            fs::write(fx.layout.addons().join(f).join(format!("{f}.toc")), "## X-DZ-Pack: later\n").unwrap();
        }
        fx.update_with(&m, &[("chess", true)], 3).await.unwrap();
        assert_eq!(fx.read("DisasterZone_Chess", "Chess.lua").as_deref(), Some("c1"));
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[(ROOMS, ROOMS_FILES), ("DisasterZone_Chess", chess)]);
        let plan = make_plan(&fx.layout, &m2, &wanted(&fx.layout, &m2, &BTreeMap::new())).unwrap();
        let actions: Vec<(&str, &str)> = plan.folders.iter().map(|f| (f.folder.as_str(), f.action)).collect();
        assert_eq!(actions, vec![("DisasterZone", "update"), ("DisasterZone_Rooms", "same"), ("DisasterZone_Chess", "same")]);
        assert_eq!(plan.put(), vec!["DisasterZone"]);
        let out = fx.update_with(&m2, &[], 3).await.unwrap(); // (no choice: chess is on disk, so it stays)
        assert_eq!(out.folders.iter().map(|f| f.folder.as_str()).collect::<Vec<_>>(), vec!["DisasterZone"]);
        assert_eq!(fx.backup_dirs(&out.backup.unwrap()), vec!["DisasterZone"]);
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a2"));
        assert_eq!(fx.read("DisasterZone_Chess", "Chess.lua").as_deref(), Some("c1"));
        for f in ["DisasterZone_Bridge", "DisasterZone_Later"] {
            assert!(fx.layout.addons().join(f).join(format!("{f}.toc")).is_file(), "{f}");
        }
        // the one-version rule: a game that changes moves with the core
        let m3 = fx.publish("1.2", "b3", &[("Core/A.lua", "a3")], &[(ROOMS, ROOMS_FILES), ("DisasterZone_Chess", &[("Chess.lua", "c3")])]);
        let out = fx.update_with(&m3, &[], 3).await.unwrap();
        assert_eq!(fx.backup_dirs(&out.backup.unwrap()), vec!["DisasterZone", "DisasterZone_Chess"]);
        assert_eq!(fx.read("DisasterZone_Chess", "Media/board.tga"), None);
    }

    #[tokio::test]
    async fn what_is_wanted() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[], &[(ROOMS, ROOMS_FILES), ("DisasterZone_Chess", &[("C.lua", "c")]),
            ("DisasterZone_Bingo", &[("B.lua", "b")])]);
        let w = |choice: &[(&str, bool)]| {
            let c: BTreeMap<String, bool> = choice.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            wanted(&fx.layout, &m, &c)
        };
        // nothing chosen: the core and the Extra (on by default); games wait for a choice
        assert_eq!(w(&[]), vec!["DisasterZone", "DisasterZone_Rooms"]);
        assert_eq!(w(&[("rooms", false), ("chess", true), ("core", false)]), vec!["DisasterZone", "DisasterZone_Chess"]);
        // a game folder found on disk counts as chosen, unless it was turned off
        let bingo = fx.layout.addons().join("DisasterZone_Bingo");
        fs::create_dir_all(&bingo).unwrap();
        assert_eq!(w(&[]), vec!["DisasterZone", "DisasterZone_Rooms"]); // (an empty folder doesn't count)
        fs::write(bingo.join("DisasterZone_Bingo.toc"), "## X-DZ-Pack: bingo\n").unwrap();
        assert_eq!(w(&[]), vec!["DisasterZone", "DisasterZone_Rooms", "DisasterZone_Bingo"]);
        assert_eq!(w(&[("bingo", false)]), vec!["DisasterZone", "DisasterZone_Rooms"]);
        // (the plan never drops the core, whatever it is handed)
        let plan = make_plan(&fx.layout, &m, &[]).unwrap();
        assert_eq!(plan.folders[0].folder, "DisasterZone");
        assert_eq!(plan.folders[0].action, "install");
        assert_eq!(plan.dropped(), vec!["DisasterZone_Bingo"]);
    }

    #[tokio::test]
    async fn remove_and_restore_go_through_the_chain() {
        let mut fx = Fixture::new();
        let sv = fx.layout.wtf().join("Account/ME/SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join("DisasterZone.lua"), "DisasterZoneDB = {}").unwrap();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[("DisasterZone_Chess", &[("C.lua", "c1")]),
            ("DisasterZone_Bingo", &[("B.lua", "b1")])]);
        fx.update_with(&m, &[("chess", true)], 5).await.unwrap();
        // 1.1 changes the core and chess
        let m2 = fx.publish("1.1", "b2", &[("Core/A.lua", "a2")], &[("DisasterZone_Chess", &[("C.lua", "c2")]),
            ("DisasterZone_Bingo", &[("B.lua", "b1")])]);
        let b_update = fx.update_with(&m2, &[("chess", true)], 5).await.unwrap().backup.unwrap();
        // removing chess: its folder goes into a backup of its own
        let b_remove = remove(&fx.layout, "DisasterZone_Chess", 5).unwrap().unwrap();
        assert!(!fx.layout.addons().join("DisasterZone_Chess").exists());
        let info = &list_backups(&fx.layout)[0];
        assert_eq!((info.id.as_str(), info.reason.as_str()), (b_remove.as_str(), "remove"));
        assert_eq!(info.folders, vec!["DisasterZone_Chess"]);
        assert_eq!(fx.backup_dirs(&b_remove), vec!["DisasterZone_Chess", "WTF"]);
        assert!(remove(&fx.layout, "DisasterZone", 5).is_err());
        assert!(remove(&fx.layout, "DisasterZone_Bridge", 5).is_err());
        assert_eq!(remove(&fx.layout, "DisasterZone_Chess", 5).unwrap(), None); // (already gone)
        // then bingo goes in: a backup that only records it as added
        let b_bingo = fx.update_with(&m2, &[("chess", false), ("bingo", true)], 5).await.unwrap().backup.unwrap();
        assert_eq!(list_backups(&fx.layout)[0].added, vec!["DisasterZone_Bingo"]);

        // restoring the remove alone: chess 1.1 comes back, bingo goes (it came after)
        restore(&fx.layout, &b_remove, 5).unwrap();
        assert_eq!(fx.read("DisasterZone_Chess", "C.lua").as_deref(), Some("c2"));
        assert!(!fx.layout.addons().join("DisasterZone_Bingo").exists());
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a2"));

        // restoring the 1.1 update: every newer backup first, so chess and the core are both 1.0 again (not the 1.1
        // chess the remove kept), and bingo stays out
        restore(&fx.layout, &b_update, 5).unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("a1"));
        assert_eq!(fx.read("DisasterZone_Chess", "C.lua").as_deref(), Some("c1"));
        assert!(!fx.layout.addons().join("DisasterZone_Bingo").exists());
        assert_eq!(installed(&fx.layout).version.as_deref(), Some("1.0"));
        // every backup is still there
        let ids: Vec<String> = list_backups(&fx.layout).into_iter().map(|b| b.id).collect();
        for id in [&b_update, &b_remove, &b_bingo] {
            assert!(ids.contains(id), "{id}");
        }
    }

    #[tokio::test]
    async fn turning_the_rooms_off_takes_them_out_on_update() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.0", "b1", &[("Core/A.lua", "a1")], &[(ROOMS, ROOMS_FILES)]);
        fx.update_with(&m, &[], 3).await.unwrap();
        let out = fx.update_with(&m, &[("rooms", false)], 3).await.unwrap();
        assert!(!installed(&fx.layout).rooms);
        assert_eq!(out.folders.iter().map(|f| (f.folder.as_str(), f.action)).collect::<Vec<_>>(), vec![(ROOMS, "remove")]);
        assert_eq!(fx.backup_dirs(&out.backup.unwrap()), vec![ROOMS]);
        assert!(fx.update_with(&m, &[("rooms", false)], 3).await.unwrap().folders.is_empty());
    }

    #[tokio::test]
    async fn a_backup_from_launcher_0_3_restores() {
        let mut fx = Fixture::new();
        let m = fx.publish("1.5.5", "b2", &[("Core/A.lua", "new")], &[(ROOMS, ROOMS_FILES), ("DisasterZone_Chess", &[("C.lua", "c")])]);
        fx.update_with(&m, &[("chess", true)], 3).await.unwrap();
        // what 0.3 left: both folders, no folder list, an id without milliseconds (older than every 0.4 id)
        let old = fx.layout.backups().join("20261001-120000-1.5.4");
        for (folder, rel, text) in [("DisasterZone", "DisasterZone.toc", "## Version: 1.5.4\n"), ("DisasterZone", "Core/A.lua", "old"),
            ("DisasterZone", "Games/Chess.lua", "in the core"), (ROOMS, "DisasterZone_Rooms.toc", "## Version: 1.5.4\n")] {
            fs::create_dir_all(old.join(folder).join(rel).parent().unwrap()).unwrap();
            fs::write(old.join(folder).join(rel), text).unwrap();
        }
        fs::write(old.join("backup.json"), r#"{"version":"1.5.4","build":"b1","time":"20261001-120000","reason":"update","savedVariables":0}"#).unwrap();
        let all = list_backups(&fx.layout);
        assert_eq!(all.last().unwrap().folders, vec!["DisasterZone", "DisasterZone_Rooms"]);
        restore(&fx.layout, "20261001-120000-1.5.4", 3).unwrap();
        assert_eq!(fx.read("DisasterZone", "Core/A.lua").as_deref(), Some("old"));
        assert_eq!(installed(&fx.layout).version.as_deref(), Some("1.5.4"));
        // (the chess pack was installed after that backup, by an install with nothing to back up: it stays)
        assert!(fx.layout.addons().join("DisasterZone_Chess").exists());
    }

    #[tokio::test]
    async fn old_backups_are_pruned() {
        let mut fx = Fixture::new();
        for i in 0..4 {
            let m = fx.publish(&format!("1.{i}"), &format!("b{i}"), &[("Core/A.lua", &format!("a{i}"))], &[]);
            fx.update_with(&m, &[], 2).await.unwrap();
        }
        let all = list_backups(&fx.layout);
        assert_eq!(all.iter().map(|b| b.version.clone().unwrap()).collect::<Vec<_>>(), vec!["1.2", "1.1"]);
    }

    #[test]
    fn titles_and_stamps() {
        assert_eq!(clean_title("|cffff8000Disaster|rZone · Azeromon"), "Azeromon");
        assert_eq!(clean_title("|cffff8000Disaster|rZone 3D rooms"), "3D rooms");
        assert_eq!(clean_title("|cffff8000Disaster|rZone"), "DisasterZone");
        assert_eq!(clean_title("Plain |cFF00ff00green|r"), "Plain green");
        assert_eq!(toc_field("## Interface: 1\n##  x-dz-PACK : chess \n", "X-DZ-Pack").as_deref(), Some("chess"));
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
        assert_eq!(stamp_at(0), "19700101-000000");
    }
}
