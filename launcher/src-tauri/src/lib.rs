//! DisasterZone Launcher: the Tauri commands the window (ui/) calls. The work is in core/.

mod background;
pub mod core;
mod sync;

use crate::core::bridge;
use crate::core::error::msg;
use crate::core::install::{self, BackupInfo, Layout, Outcome, Plan, Progress};
use crate::core::manifest::{self, Manifest, ReleaseKey};
use crate::core::source::Source;
use crate::core::state::{self, Settings};
use crate::core::wow::{self, WowInstall};
use crate::core::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

/// One install / restore at a time (the window and the tray's background check share it).
#[derive(Default)]
pub(crate) struct Busy(tokio::sync::Mutex<()>);

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

pub(crate) fn layout(s: &Settings) -> Result<Layout> {
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
#[allow(clippy::too_many_arguments)]
fn set_settings(
    app: AppHandle,
    channel: Option<String>,
    keep_backups: Option<usize>,
    check_on_start: Option<bool>,
    source: Option<String>,
    launch_code: Option<String>,
    play_command: Option<String>,
    tray: Option<bool>,
    auto_install: Option<bool>,
    autostart: Option<bool>,
    sync_server: Option<String>,
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
    if let Some(t) = tray {
        s.tray = t;
    }
    if let Some(a) = auto_install {
        s.auto_install = a;
    }
    if let Some(a) = autostart {
        s.autostart = a;
    }
    if let Some(v) = sync_server {
        s.sync_server = v.trim().to_string();
    }
    state::save(&s)?;
    background::sync_tray(&app).map_err(|e| crate::core::Error::Msg(e.to_string()))?;
    apply_autostart(&app, &s);
    Ok(s)
}

/// Start with the computer only in tray mode (a window popping up at every login would be a nuisance).
fn apply_autostart(app: &AppHandle, s: &Settings) {
    let want = s.tray && s.autostart;
    let auto = app.autolaunch();
    if auto.is_enabled().unwrap_or(false) != want {
        let _ = if want { auto.enable() } else { auto.disable() };
    }
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
    /// every folder of the release, core first
    packs: Vec<PackView>,
    /// the release has games and the one-time picker wasn't answered yet
    needs_pick: bool,
}

/// One pack of the release as the Games tab and Home show it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackView {
    id: String,
    folder: String,
    kind: String,
    title: String,
    tagline: String,
    required: bool,
    default_on: bool,
    /// its folder is in AddOns now
    installed: bool,
    /// Update keeps it (required, chosen, or installed / on by default with no choice either way)
    wanted: bool,
    /// the user's own choice, if any
    chosen: Option<bool>,
    /// what Update does with it: "install" | "update" | "same" | "remove" | "" (not wanted, not installed)
    action: String,
    files: usize,
    /// what the release's copy takes on disk, and its full download
    bytes: u64,
    gz: u64,
    /// what Update downloads for it now
    download_bytes: u64,
    /// what the folder takes on disk now (0 when it isn't installed)
    disk_bytes: u64,
    /// games: when it was last opened in game (unix time, 0 = played at an unknown time); none = never played
    played: Option<i64>,
}

fn pack_views(lay: &Layout, m: &Manifest, s: &Settings, plan: &Plan) -> Vec<PackView> {
    let played = bridge::played_games(&lay.flavor_dir);
    m.packs
        .iter()
        .map(|p| {
            let fp = plan.folders.iter().find(|f| f.folder == p.folder);
            let (bytes, gz) = m.folder_size(&p.folder);
            let action = fp.map(|f| f.action).unwrap_or("");
            PackView {
                id: p.id.clone(),
                folder: p.folder.clone(),
                kind: p.kind.clone(),
                title: p.title.clone(),
                tagline: p.tagline.clone(),
                required: p.required,
                default_on: p.default_on,
                installed: install::on_disk(lay, &p.folder),
                wanted: !matches!(action, "remove" | ""),
                chosen: s.packs.get(&p.id).copied(),
                action: action.into(),
                files: m.folders.get(&p.folder).map_or(0, |f| f.len()),
                bytes,
                gz,
                download_bytes: fp.map_or(0, |f| f.download_bytes),
                disk_bytes: fp.map_or(0, |f| f.disk_bytes),
                played: played.get(&p.id).copied(),
            }
        })
        .collect()
}

#[tauri::command]
async fn check() -> Result<Check> {
    let s = state::load();
    let lay = layout(&s)?;
    let src = Source::parse(&s.source())?;
    let m = src.manifest(&ReleaseKey::builtin(), &s.channel).await?;
    let plan = install::make_plan(&lay, &m, &install::wanted(&lay, &m, &s.packs))?;
    Ok(Check {
        packs: pack_views(&lay, &m, &s, &plan),
        needs_pick: m.has_games() && !s.packs_picked,
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
async fn update(app: AppHandle) -> Result<Outcome> {
    run_update(&app).await
}

/// Install the newest release of the chosen channel (the Update button and the tray's install-by-itself).
pub(crate) async fn run_update(app: &AppHandle) -> Result<Outcome> {
    let busy = app.state::<Busy>();
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
    let wanted = install::wanted(&lay, &m, &s.packs);
    let out = install::update(&lay, &src, &m, &wanted, s.keep_backups, progress).await?;
    s = state::load(); // (settings may have changed during the download)
    s.seen_seq.insert(s.channel.clone(), m.seq.max(seen));
    s.notified_seq.insert(s.channel.clone(), m.seq.max(seen));
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
    let lay = layout(&s)?;
    let out = install::restore(&lay, &id, s.keep_backups)?;
    s.hold = true;
    // a pack the restore put back (a removed game) is kept from now on. One it took out keeps its choice: going back
    // to a version from before game packs mustn't lose the games at the next update.
    for p in install::installed(&lay).packs {
        s.packs.insert(p.id, true);
    }
    state::save(&s)?;
    Ok(out)
}

/// The Games tab and the picker: which packs to keep ({id: on}; Update then installs or takes them out).
/// `picked`: the one-time picker was answered.
#[tauri::command]
fn set_packs(packs: BTreeMap<String, bool>, picked: Option<bool>) -> Result<Settings> {
    let mut s = state::load();
    for (id, on) in packs {
        if !manifest::pack_id(&id) || id == "core" {
            return msg(format!("unknown pack {id}"));
        }
        s.packs.insert(id, on);
    }
    if let Some(p) = picked {
        s.packs_picked = p;
    }
    state::save(&s)?;
    Ok(s)
}

/// Games tab > Remove, and 3D rooms off: the pack's folder goes into a backup (Backups can put it back) and Update
/// leaves it out from now on. No download, so it works offline.
#[tauri::command]
async fn remove_pack(id: String, folder: Option<String>, busy: State<'_, Busy>) -> Result<Option<String>> {
    let _guard = busy.0.try_lock().map_err(|_| crate::core::Error::Msg("already working".into()))?;
    if !manifest::pack_id(&id) || id == "core" {
        return msg("unknown pack");
    }
    let s = state::load();
    let lay = layout(&s)?;
    // (by the id in its TOC; else the release's folder for it, which the window passes along)
    let folder = install::installed(&lay).packs.into_iter().find(|p| p.id == id).map(|p| p.folder).or(folder);
    let out = match folder {
        Some(f) => install::remove(&lay, &f, s.keep_backups)?,
        None => None,
    };
    let mut s = state::load();
    s.packs.insert(id, false);
    state::save(&s)?;
    Ok(out)
}

/// Open one of the launcher's folders in the OS file manager ("pack": an installed pack's folder).
#[tauri::command]
fn open_folder(kind: String, folder: Option<String>) -> Result<()> {
    let lay = layout(&state::load())?;
    let path = match kind.as_str() {
        "addons" => lay.addons(),
        "backups" => lay.backups(),
        "game" => lay.flavor_dir.clone(),
        "pack" => match folder.filter(|f| install::release_folder(f)) {
            Some(f) if lay.addons().join(&f).is_dir() => lay.addons().join(f),
            _ => return msg("that game isn't installed"),
        },
        _ => return msg("unknown folder"),
    };
    std::fs::create_dir_all(&path)?;
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| crate::core::Error::Msg(e.to_string()))
}

/// Settings > Discord: is this PC linked, when it last synced, what waits for the game.
#[tauri::command]
fn sync_status() -> sync::SyncView {
    sync::view()
}

/// Link with a code from /launcher in Discord.
#[tauri::command]
async fn sync_link(app: AppHandle, code: String) -> Result<sync::SyncView> {
    sync::link(&app, &code).await
}

#[tauri::command]
fn sync_unlink(app: AppHandle) -> Result<sync::SyncView> {
    sync::unlink(&app)
}

#[tauri::command]
async fn sync_run(app: AppHandle) -> Result<sync::SyncView> {
    sync::sync_now(&app).await
}

#[tauri::command]
fn play() -> Result<()> {
    crate::core::play::play(&state::load())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // a second start (a click on the shortcut while it sits in the tray) shows the running launcher
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| background::show_main(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Busy::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let mut s = state::load();
            background::sync_tray(&handle)?;
            apply_autostart(&handle, &s);
            // started by the login autostart, or restarted after updating itself in the background: stay in the tray
            let hidden = (std::env::args().any(|a| a == "--hidden") || s.start_hidden_once) && s.tray;
            if s.start_hidden_once {
                s.start_hidden_once = false;
                let _ = state::save(&s);
            }
            if !hidden {
                background::show_main(&handle);
            }
            sync::spawn_loop(handle.clone());
            background::spawn_loop(handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if state::load().tray {
                    api.prevent_close();
                    background::hide_to_tray(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            set_wow_path,
            set_settings,
            check,
            update,
            backups,
            restore,
            set_packs,
            remove_pack,
            open_folder,
            play,
            sync_status,
            sync_link,
            sync_unlink,
            sync_run
        ])
        .build(tauri::generate_context!())
        .expect("error while starting the DisasterZone launcher");
    app.run(|_app, _event| {
        // macOS: a click on the Dock icon while the window is hidden brings it back
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            background::show_main(_app);
        }
    });
}
