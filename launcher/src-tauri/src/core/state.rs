//! The launcher's own settings (one JSON file in the OS config folder).

use super::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Where releases are published (the public releases repository). A setting can point elsewhere for testing.
pub const DEFAULT_SOURCE: &str = include_str!("../../../source.txt");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub wow_root: Option<String>,
    pub flavor: String,
    pub channel: String,
    /// highest manifest seq seen per channel: an older manifest is refused (replayed or stale)
    pub seen_seq: BTreeMap<String, u64>,
    /// after a restore: do not offer the newest release until the user asks
    pub hold: bool,
    pub keep_backups: usize,
    pub check_on_start: bool,
    /// override of DEFAULT_SOURCE (a URL or a folder)
    pub source: Option<String>,
    /// Battle.net product code for Play (e.g. "WoWC"): empty = just open Battle.net
    pub launch_code: String,
    /// Linux (or any OS): a shell command Play runs instead of Battle.net, e.g. "lutris lutris:rungame/battlenet"
    pub play_command: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            wow_root: None,
            flavor: "_anniversary_".into(),
            channel: "stable".into(),
            seen_seq: BTreeMap::new(),
            hold: false,
            keep_backups: 3,
            check_on_start: true,
            source: None,
            launch_code: String::new(),
            play_command: String::new(),
        }
    }
}

impl Settings {
    pub fn source(&self) -> String {
        self.source.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_SOURCE.trim().to_string())
    }

    pub fn flavor_dir(&self) -> Option<PathBuf> {
        self.wow_root.as_ref().map(|r| PathBuf::from(r).join(&self.flavor))
    }
}

fn path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("DisasterZoneLauncher").join("settings.json")
}

pub fn load() -> Settings {
    fs::read(path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<()> {
    let p = path();
    fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap())?;
    fs::rename(&tmp, &p)?;
    Ok(())
}
