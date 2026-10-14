//! Discord sync: every member's launcher carries the game's saves to the guild's bot server and brings the changes
//! made in Discord back (dz_proto documents the server's side). After the game saves (login, /reload, logout) the
//! launcher uploads DisasterZoneBridgeDB within seconds; once a minute it asks for new records anyway, so they are in
//! the inbox before the next load. Linking: /launcher in Discord gives a code; Settings > Discord trades it for this
//! PC's member key, kept in the OS credential store.

use crate::core::bridge;
use crate::core::error::msg;
use crate::core::state;
use crate::core::Result;
use dz_proto::{LinkDown, LinkUp, SyncDown, SyncUp};
use serde::Serialize;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};

const KEY_SERVICE: &str = "DisasterZoneLauncher";
const KEY_USER: &str = "discord-sync-key";
/// The guild's bot server (a setting can point elsewhere, e.g. http://127.0.0.1:8787 while testing at home).
pub const DEFAULT_SERVER: &str = include_str!("../../server.txt");
/// While the game runs, ask for new records this often even when it hasn't saved (none while it is closed: the next
/// start asks at once). The bot's free server has little traffic to spare.
const POLL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    pub linked: bool,
    /// the Discord name the key belongs to
    pub user: String,
    pub server: String,
    /// unix seconds of the last good sync
    pub last: Option<u64>,
    /// records waiting for the game
    pub waiting: usize,
    pub error: Option<String>,
}

static VIEW: Mutex<Option<SyncView>> = Mutex::new(None);
/// The hash of the answer the inbox was last written from (an unchanged one comes back as "same").
static HAVE: Mutex<String> = Mutex::new(String::new());
/// When the game last saved, as uploaded (a check for records alone doesn't upload the same save again).
static SENT: Mutex<Option<SystemTime>> = Mutex::new(None);
/// (one sync at a time: the loop and the window's buttons)
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEY_SERVICE, KEY_USER).map_err(|e| crate::core::Error::Msg(format!("credential store: {e}")))
}

fn key() -> Option<String> {
    entry().ok()?.get_password().ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

pub fn server() -> String {
    let s = state::load();
    let url = if s.sync_server.trim().is_empty() { DEFAULT_SERVER.trim() } else { s.sync_server.trim() };
    url.trim_end_matches('/').to_string()
}

pub fn view() -> SyncView {
    let mut v = VIEW.lock().unwrap().clone().unwrap_or_default();
    v.linked = key().is_some();
    v.server = server();
    if v.user.is_empty() {
        v.user = state::load().sync_user;
    }
    v
}

fn set_view(app: &AppHandle, f: impl FnOnce(&mut SyncView)) {
    {
        let mut g = VIEW.lock().unwrap();
        f(g.get_or_insert_with(SyncView::default));
    }
    // (the whole view, with linked / server / user filled in: the stored one leaves them to view())
    let _ = app.emit("sync-status", view());
}

fn now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("DisasterZoneLauncher/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| crate::core::Error::Net(e.to_string()))
}

/// The server's answer as text: its own message for a refusal, a plain line otherwise.
async fn refusal(r: reqwest::Response) -> String {
    let code = r.status();
    let text = r.text().await.unwrap_or_default();
    if !text.trim().is_empty() && text.len() < 300 {
        text
    } else {
        format!("the bot's server answered {code}")
    }
}

/// Trade a /launcher code for this PC's member key, then sync.
pub async fn link(app: &AppHandle, code: &str) -> Result<SyncView> {
    let base = server();
    if base.is_empty() {
        return msg("no bot server is set (Settings > Discord > Server)");
    }
    let r = client()?
        .post(format!("{base}/api/link"))
        .json(&LinkUp { code: code.trim().to_string() })
        .send()
        .await
        .map_err(|e| crate::core::Error::Net(format!("can't reach the bot's server: {e}")))?;
    if !r.status().is_success() {
        return msg(refusal(r).await);
    }
    let down: LinkDown = r.json().await.map_err(|e| crate::core::Error::Net(e.to_string()))?;
    entry()?.set_password(&down.key).map_err(|e| crate::core::Error::Msg(format!("could not save the key: {e}")))?;
    let mut s = state::load();
    s.sync_user = down.user.clone();
    state::save(&s)?;
    set_view(app, |v| {
        v.user = down.user;
        v.error = None;
    });
    let _ = sync_now(app).await;
    Ok(view())
}

/// Forget this PC's key; the inbox empties (no more records, badges or channels from Discord).
pub fn unlink(app: &AppHandle) -> Result<SyncView> {
    if let Ok(e) = entry() {
        let _ = e.delete_credential();
    }
    let mut s = state::load();
    s.sync_user.clear();
    state::save(&s)?;
    if let Some(dir) = s.flavor_dir() {
        let _ = bridge::clear_inbox(&dir);
    }
    set_view(app, |v| *v = SyncView::default());
    Ok(view())
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = gz.write_all(data);
    gz.finish().unwrap_or_default()
}

/// Upload the game's last save, write the inbox from the answer.
pub async fn sync_now(app: &AppHandle) -> Result<SyncView> {
    let _g = RUNNING.lock().await;
    let Some(k) = key() else { return msg("this launcher is not linked: type /launcher in Discord for a code") };
    let s = state::load();
    let Some(dir) = s.flavor_dir().filter(|d| d.is_dir()) else { return msg("pick your World of Warcraft folder first") };
    let saved = bridge::newest_saved_variables(&dir);
    let fresh = saved.is_some() && *SENT.lock().unwrap() != saved;
    let have = if fresh { String::new() } else { HAVE.lock().unwrap().clone() };
    let up = SyncUp {
        launcher: env!("CARGO_PKG_VERSION").into(),
        bridge: if fresh { bridge::read_bridge_db(&dir) } else { None },
        have,
    };
    let body = gzip(&serde_json::to_vec(&up).unwrap_or_default());
    let result: Result<SyncDown> = async {
        let r = client()?
            .post(format!("{}/api/sync", server()))
            .bearer_auth(&k)
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(body)
            .send()
            .await
            .map_err(|e| crate::core::Error::Net(format!("can't reach the bot's server: {e}")))?;
        if !r.status().is_success() {
            return msg(refusal(r).await);
        }
        r.json::<SyncDown>().await.map_err(|e| crate::core::Error::Net(e.to_string()))
    }
    .await;
    match result {
        Ok(down) => {
            *SENT.lock().unwrap() = saved;
            if !down.same {
                bridge::write_inbox(&dir, &down)?;
                *HAVE.lock().unwrap() = down.hash.clone();
            }
            set_view(app, |v| {
                v.last = Some(now());
                if !down.same {
                    v.waiting = down.records.len();
                }
                v.user = down.user.clone();
                v.error = None;
            });
            Ok(view())
        }
        Err(e) => {
            let text = e.to_string();
            set_view(app, |v| v.error = Some(text.clone()));
            Err(e)
        }
    }
}

/// The loop: a sync soon after every game save, and once a minute anyway. Quiet while unlinked.
pub fn spawn_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut seen: Option<SystemTime> = None;
        let mut last = Instant::now() - POLL;
        let mut wait = POLL;
        let mut was_running = false;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if key().is_none() {
                continue;
            }
            let Some(dir) = state::load().flavor_dir() else { continue };
            let saved = bridge::newest_saved_variables(&dir);
            let running = crate::core::wow::running();
            // (a save is sent at once; records are asked for while the game runs, and once when it starts)
            let due = saved != seen || (running && (last.elapsed() >= wait || !was_running));
            was_running = running;
            if !due {
                continue;
            }
            seen = saved;
            last = Instant::now();
            // (the server unreachable: try less often, up to every 10 minutes)
            wait = match sync_now(&app).await {
                Ok(_) => POLL,
                Err(_) => (wait * 2).min(Duration::from_secs(600)),
            };
        }
    });
}
