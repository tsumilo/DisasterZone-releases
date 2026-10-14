//! DisasterZone Launcher: the Tauri commands the window (ui/) calls. The work is in core/.

mod background;
pub mod core;
mod sync;

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
    let out = install::update(&lay, &src, &m, s.keep_backups, progress).await?;
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
