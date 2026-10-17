//! Tray mode (opt-in, Settings > Background): the tray icon and its menu, the check every 5 minutes, the OS
//! notifications, and installing by itself while WoW is closed (a second opt-in).

use crate::core::install;
use crate::core::manifest::ReleaseKey;
use crate::core::source::Source;
use crate::core::state;
use crate::core::wow;
use crate::core::Result;
use crate::{layout, run_update};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

pub const INTERVAL: Duration = Duration::from_secs(5 * 60);
const FIRST_CHECK: Duration = Duration::from_secs(20);
const TRAY_ID: &str = "main";

#[derive(Debug, PartialEq)]
pub enum Action {
    Nothing,
    Install,
    /// tell the user once; waits_for_wow: it installs by itself after the game closes
    Notify { waits_for_wow: bool },
    /// the first release with games: nothing installs until the user picks theirs in the window (told once)
    Pick,
}

/// What one background check does with what it found.
pub fn decide(up_to_date: bool, hold: bool, auto_install: bool, wow_running: bool, notified: bool, needs_pick: bool) -> Action {
    if up_to_date || hold {
        return Action::Nothing;
    }
    if needs_pick {
        return if notified { Action::Nothing } else { Action::Pick };
    }
    if auto_install && !wow_running {
        return Action::Install;
    }
    if notified {
        Action::Nothing
    } else {
        Action::Notify { waits_for_wow: auto_install && wow_running }
    }
}

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn set_tooltip(app: &AppHandle, text: &str) {
    if let Some(t) = app.tray_by_id(TRAY_ID) {
        let _ = t.set_tooltip(Some(text));
    }
}

/// The tray icon exists exactly while tray mode is on.
pub fn sync_tray(app: &AppHandle) -> tauri::Result<()> {
    let on = state::load().tray;
    let have = app.tray_by_id(TRAY_ID).is_some();
    if on && !have {
        let open = MenuItem::with_id(app, "open", "Open DisasterZone", true, None::<&str>)?;
        let check = MenuItem::with_id(app, "check", "Check for updates", true, None::<&str>)?;
        let sep = PredefinedMenuItem::separator(app)?;
        let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&open, &check, &sep, &quit])?;
        let mut b = TrayIconBuilder::with_id(TRAY_ID)
            .tooltip("DisasterZone")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, e| match e.id.as_ref() {
                "open" => show_main(app),
                "check" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { tick(&app, true).await });
                }
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, e| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    show_main(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            b = b.icon(icon.clone());
        }
        b.build(app)?;
    } else if !on && have {
        app.remove_tray_by_id(TRAY_ID);
    }
    Ok(())
}

/// Closing the window in tray mode hides it; the first time, say where the launcher went.
pub fn hide_to_tray(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    let mut s = state::load();
    if !s.tray_hint_shown {
        notify(app, "DisasterZone is still running", "It checks for updates in the tray. Right-click the tray icon to quit.");
        s.tray_hint_shown = true;
        let _ = state::save(&s);
    }
}

/// Checks every 5 minutes while tray mode is on (the setting is read each time, so turning it on or off needs no
/// restart).
pub fn spawn_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            if state::load().tray {
                tick(&app, false).await;
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// One check. `manual` (tray menu): always says what it found, even when nothing is new.
pub async fn tick(app: &AppHandle, manual: bool) {
    if let Err(e) = try_tick(app, manual).await {
        set_tooltip(app, "DisasterZone · could not check for updates");
        if manual {
            notify(app, "DisasterZone could not check for updates", &e.to_string());
        }
    }
    check_launcher(app).await;
}

async fn try_tick(app: &AppHandle, manual: bool) -> Result<()> {
    let mut s = state::load();
    let lay = layout(&s)?;
    let inst = install::installed(&lay);
    if inst.dev || inst.linked {
        set_tooltip(app, "DisasterZone · development copy");
        return Ok(());
    }
    let src = Source::parse(&s.source())?;
    let m = src.manifest(&ReleaseKey::builtin(), &s.channel).await?;
    if m.seq < s.seen_seq.get(&s.channel).copied().unwrap_or(0) {
        return Ok(()); // a stale copy of the release list: the next check gets the current one
    }
    let plan = install::make_plan(&lay, &m, &install::wanted(&lay, &m, &s.packs))?;
    let notified = !manual && s.notified_seq.get(&s.channel).copied().unwrap_or(0) >= m.seq;
    let needs_pick = m.has_games() && !s.packs_picked;
    match decide(plan.up_to_date, s.hold, s.auto_install, wow::running(), notified, needs_pick) {
        Action::Nothing => {
            if plan.up_to_date {
                set_tooltip(app, &format!("DisasterZone {} · up to date", m.version));
                if manual {
                    notify(app, "DisasterZone is up to date", &format!("You have the newest version, {}.", m.version));
                }
            } else if s.hold {
                set_tooltip(app, "DisasterZone · updates paused after a restore");
                if manual {
                    notify(app, &format!("DisasterZone {} is out", m.version),
                        "Updates are paused because you restored an older version. Open the launcher to update.");
                }
            } else if needs_pick {
                set_tooltip(app, "DisasterZone · pick your games to update");
            } else {
                set_tooltip(app, &format!("DisasterZone · {} is ready", m.version));
            }
        }
        Action::Install => {
            set_tooltip(app, &format!("DisasterZone · installing {}", m.version));
            let out = run_update(app).await?;
            set_tooltip(app, &format!("DisasterZone {} · up to date", out.version));
            notify(app, &format!("DisasterZone {} is installed", out.version),
                "Updated in the background. It loads the next time you start World of Warcraft.");
            let _ = app.emit("state-changed", ());
        }
        Action::Pick => {
            set_tooltip(app, "DisasterZone · pick your games to update");
            notify(app, "Pick your games to finish updating DisasterZone",
                &format!("DisasterZone {} gives every game its own download. Open the launcher to choose yours.", m.version));
            s = state::load();
            s.notified_seq.insert(s.channel.clone(), m.seq);
            state::save(&s)?;
            let _ = app.emit("state-changed", ());
        }
        Action::Notify { waits_for_wow } => {
            set_tooltip(app, &format!("DisasterZone · {} is ready", m.version));
            let body = if waits_for_wow {
                "It installs by itself once you close World of Warcraft."
            } else {
                "Open the launcher to update."
            };
            notify(app, &format!("DisasterZone {} is ready", m.version), body);
            s = state::load();
            s.notified_seq.insert(s.channel.clone(), m.seq);
            state::save(&s)?;
            let _ = app.emit("state-changed", ());
        }
    }
    Ok(())
}

/// A new launcher. With "install updates by itself" on, the window closed and nothing installing, it updates
/// itself and restarts into the tray; otherwise one notification per version (the window's bar installs it).
async fn check_launcher(app: &AppHandle) {
    let Ok(updater) = app.updater() else { return };
    let Ok(Some(u)) = updater.check().await else { return };
    let mut s = state::load();
    let window_open = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
    if s.tray && s.auto_install && !window_open {
        let busy = app.state::<crate::Busy>();
        let guard = busy.0.try_lock();
        if guard.is_ok() {
            if u.download_and_install(|_, _| {}, || {}).await.is_ok() {
                s = state::load();
                s.start_hidden_once = true;
                s.notified_launcher = u.version.clone();
                let _ = state::save(&s);
                notify(app, &format!("DisasterZone Launcher {} is installed", u.version),
                    "It updated itself and keeps running in the tray.");
                app.restart();
            }
            return; // (a failed download is tried again on the next check)
        }
    }
    if s.notified_launcher != u.version {
        notify(app, "A new DisasterZone Launcher is ready",
            &format!("Version {}: open the launcher and click Install and restart.", u.version));
        s.notified_launcher = u.version.clone();
        let _ = state::save(&s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_background_check_does() {
        // nothing new, or paused after a restore
        assert_eq!(decide(true, false, true, false, false, false), Action::Nothing);
        assert_eq!(decide(false, true, true, false, false, false), Action::Nothing);
        // a new release, install by itself on, WoW closed: install
        assert_eq!(decide(false, false, true, false, true, false), Action::Install);
        // WoW running: tell once that it installs after the game closes, then stay quiet
        assert_eq!(decide(false, false, true, true, false, false), Action::Notify { waits_for_wow: true });
        assert_eq!(decide(false, false, true, true, true, false), Action::Nothing);
        // install by itself off: tell once
        assert_eq!(decide(false, false, false, false, false, false), Action::Notify { waits_for_wow: false });
        assert_eq!(decide(false, false, false, true, true, false), Action::Nothing);
    }

    #[test]
    fn the_first_release_with_games_waits_for_the_picker() {
        // install by itself on and WoW closed: still no install, one notification, then quiet
        assert_eq!(decide(false, false, true, false, false, true), Action::Pick);
        assert_eq!(decide(false, false, true, false, true, true), Action::Nothing);
        assert_eq!(decide(false, false, false, true, false, true), Action::Pick);
        // nothing to update (the folders a zip user copied in), or paused: no nagging
        assert_eq!(decide(true, false, true, false, false, true), Action::Nothing);
        assert_eq!(decide(false, true, true, false, false, true), Action::Nothing);
    }
}
