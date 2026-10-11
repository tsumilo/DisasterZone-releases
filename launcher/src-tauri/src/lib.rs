//! DisasterZone Launcher: the Tauri commands the window (ui/) calls. The work is in core/.

pub mod core;

use crate::core::error::msg;
use crate::core::install::{self, BackupInfo, Layout, Outcome, Progress};
use crate::core::manifest::ReleaseKey;
use crate::core::source::Source;
use crate::core::state::{self, Settings};
use crate::core::wow::{self, WowInstall};
use crate::core::Result;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[derive(Default)]
struct Busy(tokio::sync::Mutex<()>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    settings: Settings,
    installs: Vec<WowInstall>,
    /// None when no usable game folder is set
    installed: Option<install::Installed>,
    flavor_dir: Option<String>,
    wow_running: bool,
    launcher_version: &'static str,
    source: String,
}

fn layout(s: &Settings) -> Result<Layout> {
    match s.flavor_dir() {
        Some(d) if d.is_dir() => Ok(Layout::new(d)),
        Some(d) => msg(format!("{} does not exist: pick your World of Warcraft folder in Settings", d.display())),
        None => msg("pick your World of Warcraft folder first"),
    }
}

/// First run: take the first WoW install found that has the wanted flavor (or any flavor).
fn autodetect(s: &mut Settings, installs: &[WowInstall]) {
    if s.wow_root.is_some() {
        return;
    }
    let pick = installs.iter().find(|w| w.flavors.contains(&s.flavor)).or_else(|| installs.first());
    if let Some(w) = pick {
        s.wow_root = Some(w.root.clone());
        if !w.flavors.contains(&s.flavor) {
            s.flavor = w.flavors[0].clone();
        }
        let _ = state::save(s);
    }
}

#[tauri::command]
fn status() -> Status {
    let mut s = state::load();
    let installs = wow::detect();
    autodetect(&mut s, &installs);
    let lay = layout(&s).ok();
    Status {
        installed: lay.as_ref().map(install::installed),
        flavor_dir: lay.map(|l| l.flavor_dir.to_string_lossy().to_string()),
        installs,
        wow_running: wow::running(),
        launcher_version: env!("CARGO_PKG_VERSION"),
        source: s.source(),
        settings: s,
    }
}

#[tauri::command]
fn set_wow_path(path: String, flavor: Option<String>) -> Result<Settings> {
    let (root, picked_flavor) = match wow::normalize(&PathBuf::from(&path)) {
        Some(x) => x,
        None => {
            return msg(
                "that folder is not a World of Warcraft install (no _anniversary_, _classic_ or _retail_ folder in it)",
            )
        }
    };
    let flavors = wow::flavors_in(&root);
    let mut s = state::load();
    let want = flavor.or(picked_flavor).unwrap_or_else(|| s.flavor.clone());
    s.flavor = if flavors.contains(&want) { want } else { flavors[0].clone() };
    s.wow_root = Some(root.to_string_lossy().to_string());
    state::save(&s)?;
    Ok(s)
}

#[tauri::command]
fn set_settings(
    channel: Option<String>,
    keep_backups: Option<usize>,
    check_on_start: Option<bool>,
    source: Option<String>,
    launch_code: Option<String>,
    play_command: Option<String>,
) -> Result<Settings> {
    let mut s = state::load();
    if let Some(c) = channel {
        if !["stable", "beta"].contains(&c.as_str()) {
            return msg("unknown channel");
        }
        s.channel = c;
    }
    if let Some(k) = keep_backups {
        s.keep_backups = k.clamp(1, 10);
    }
    if let Some(c) = check_on_start {
        s.check_on_start = c;
    }
    if let Some(src) = source {
        s.source = Some(src).filter(|x| !x.trim().is_empty());
    }
    if let Some(c) = launch_code {
        s.launch_code = c.trim().to_string();
    }
    if let Some(c) = play_command {
        s.play_command = c.trim().to_string();
    }
    state::save(&s)?;
    Ok(s)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    version: String,
    build: String,
    seq: u64,
    channel: String,
    asset_pack: u32,
    published: String,
    notes: serde_json::Value,
    files: usize,
    changed: usize,
    removed: usize,
    download_bytes: u64,
    up_to_date: bool,
    /// the manifest is older than one this launcher already saw (stale CDN copy or a replay)
    older: bool,
    hold: bool,
    zip_url: Option<String>,
}

#[tauri::command]
async fn check() -> Result<Check> {
    let s = state::load();
    let lay = layout(&s)?;
    let src = Source::parse(&s.source())?;
    let m = src.manifest(&ReleaseKey::builtin(), &s.channel).await?;
    let plan = install::make_plan(&lay, &m)?;
    Ok(Check {
        older: m.seq < s.seen_seq.get(&s.channel).copied().unwrap_or(0),
        hold: s.hold,
        files: m.file_count(),
        changed: plan.fetch.len(),
        removed: plan.remove.len(),
        download_bytes: plan.download_bytes,
        up_to_date: plan.up_to_date,
        zip_url: m.zip.as_ref().and_then(|z| z.url.clone()),
        version: m.version,
        build: m.build,
        seq: m.seq,
        channel: m.channel,
        asset_pack: m.asset_pack,
        published: m.published,
        notes: m.notes,
    })
}

#[tauri::command]
async fn update(app: AppHandle, busy: State<'_, Busy>) -> Result<Outcome> {
    let _guard = busy.0.try_lock().map_err(|_| crate::core::Error::Msg("already working".into()))?;
    let mut s = state::load();
    let lay = layout(&s)?;
    let src = Source::parse(&s.source())?;
    let m = src.manifest(&ReleaseKey::builtin(), &s.channel).await?;
    let seen = s.seen_seq.get(&s.channel).copied().unwrap_or(0);
    if m.seq < seen {
        return msg(
            "the release list is older than one this launcher already installed from: try again in a few minutes",
        );
    }
    let emitter = app.clone();
    let progress: install::ProgressFn = Arc::new(move |p: Progress| {
        let _ = emitter.emit("progress", p);
    });
    let out = install::update(&lay, &src, &m, s.keep_backups, progress).await?;
    s.seen_seq.insert(s.channel.clone(), m.seq.max(seen));
    s.hold = false;
    state::save(&s)?;
    Ok(out)
}

#[tauri::command]
fn backups() -> Result<Vec<BackupInfo>> {
    Ok(install::list_backups(&layout(&state::load())?))
}

#[tauri::command]
async fn restore(id: String, busy: State<'_, Busy>) -> Result<Option<String>> {
    let _guard = busy.0.try_lock().map_err(|_| crate::core::Error::Msg("already working".into()))?;
    let mut s = state::load();
    let out = install::restore(&layout(&s)?, &id, s.keep_backups)?;
    s.hold = true;
    state::save(&s)?;
    Ok(out)
}

/// Open one of the launcher's folders in the OS file manager.
#[tauri::command]
fn open_folder(kind: String) -> Result<()> {
    let lay = layout(&state::load())?;
    let path = match kind.as_str() {
        "addons" => lay.addons(),
        "backups" => lay.backups(),
        "game" => lay.flavor_dir.clone(),
        _ => return msg("unknown folder"),
    };
    std::fs::create_dir_all(&path)?;
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| crate::core::Error::Msg(e.to_string()))
}

#[tauri::command]
fn play() -> Result<()> {
    crate::core::play::play(&state::load())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Busy::default())
        .invoke_handler(tauri::generate_handler![
            status,
            set_wow_path,
            set_settings,
            check,
            update,
            backups,
            restore,
            open_folder,
            play
        ])
        .run(tauri::generate_context!())
        .expect("error while running the DisasterZone launcher");
}
