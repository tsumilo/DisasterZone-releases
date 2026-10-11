//! Finding World of Warcraft on Windows, macOS and Linux (Wine, Lutris, Bottles, Steam/Proton).

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Game folders inside a WoW install, in the order the launcher prefers them.
pub const FLAVORS: [(&str, &str); 4] = [
    ("_anniversary_", "Anniversary"),
    ("_classic_era_", "Classic Era"),
    ("_classic_", "Classic"),
    ("_retail_", "Retail"),
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WowInstall {
    pub root: String,
    /// flavor folder names present in this install, e.g. ["_anniversary_"]
    pub flavors: Vec<String>,
}

pub fn flavors_in(root: &Path) -> Vec<String> {
    FLAVORS
        .iter()
        .filter(|(f, _)| root.join(f).is_dir())
        .map(|(f, _)| f.to_string())
        .collect()
}

/// Whatever folder the user picked (the install, a flavor folder, Interface, AddOns, the addon folder itself):
/// the install root and the flavor it points into, when it does.
pub fn normalize(picked: &Path) -> Option<(PathBuf, Option<String>)> {
    let mut p = picked.to_path_buf();
    for _ in 0..5 {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if FLAVORS.iter().any(|(f, _)| *f == name) {
            return Some((p.parent()?.to_path_buf(), Some(name)));
        }
        if !flavors_in(&p).is_empty() {
            return Some((p, None));
        }
        p = p.parent()?.to_path_buf();
    }
    None
}

fn candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        for key in [
            r"SOFTWARE\WOW6432Node\Blizzard Entertainment\World of Warcraft",
            r"SOFTWARE\Blizzard Entertainment\World of Warcraft",
        ] {
            if let Ok(k) = hklm.open_subkey(key) {
                for value in ["InstallPath", "GamePath"] {
                    if let Ok(v) = k.get_value::<String, _>(value) {
                        out.push(PathBuf::from(v));
                    }
                }
            }
        }
        for drive in b'C'..=b'Z' {
            let d = format!("{}:\\", drive as char);
            if !Path::new(&d).exists() {
                continue;
            }
            for sub in [
                "World of Warcraft",
                r"Program Files (x86)\World of Warcraft",
                r"Program Files\World of Warcraft",
                r"Games\World of Warcraft",
                r"Battle.net\World of Warcraft",
                r"Blizzard\World of Warcraft",
            ] {
                out.push(Path::new(&d).join(sub));
            }
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from("/Applications/World of Warcraft"));
        out.push(home.join("Applications/World of Warcraft"));
    }
    #[cfg(target_os = "linux")]
    {
        let pf = "drive_c/Program Files (x86)/World of Warcraft";
        out.push(home.join(".wine").join(pf));
        out.push(home.join("Games/battlenet").join(pf));
        out.push(home.join("Games/battle-net").join(pf));
        out.push(home.join("Games/world-of-warcraft").join(pf));
        for parent in [
            home.join(".local/share/bottles/bottles"),
            home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
            home.join(".steam/steam/steamapps/compatdata"),
            home.join(".local/share/Steam/steamapps/compatdata"),
        ] {
            if let Ok(rd) = std::fs::read_dir(&parent) {
                for e in rd.flatten() {
                    out.push(e.path().join(pf));
                    out.push(e.path().join("pfx").join(pf));
                }
            }
        }
    }
    let _ = &home;
    out
}

/// Every WoW install found on this machine (deduplicated, only ones with at least one known flavor).
pub fn detect() -> Vec<WowInstall> {
    let mut out: Vec<WowInstall> = vec![];
    for c in candidates() {
        if let Some((root, _)) = normalize(&c) {
            let flavors = flavors_in(&root);
            let root_s = root.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
            if !flavors.is_empty() && !out.iter().any(|w| w.root.eq_ignore_ascii_case(&root_s)) {
                out.push(WowInstall { root: root_s, flavors });
            }
        }
    }
    out
}

/// Is a WoW client running? (Updating still works; the game only sees the new files after a restart.)
pub fn running() -> bool {
    let sys = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );
    sys.processes().values().any(|p| {
        let n = p.name().to_string_lossy().to_ascii_lowercase();
        matches!(
            n.as_str(),
            "wow.exe" | "wowclassic.exe" | "wowt.exe" | "wowb.exe" | "wow-64.exe" | "world of warcraft"
                | "world of warcraft classic"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_folder_inside_an_install_leads_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("World of Warcraft");
        let addons = root.join("_anniversary_/Interface/AddOns/DisasterZone");
        std::fs::create_dir_all(&addons).unwrap();
        std::fs::create_dir_all(root.join("_retail_")).unwrap();
        assert_eq!(normalize(&root), Some((root.clone(), None)));
        assert_eq!(normalize(&root.join("_anniversary_")), Some((root.clone(), Some("_anniversary_".into()))));
        assert_eq!(normalize(&addons), Some((root.clone(), Some("_anniversary_".into()))));
        assert_eq!(flavors_in(&root), vec!["_anniversary_", "_retail_"]);
        assert_eq!(normalize(tmp.path()), None);
    }

    #[test]
    fn detect_and_running_do_not_crash() {
        let _ = detect();
        let _ = running();
    }
}
