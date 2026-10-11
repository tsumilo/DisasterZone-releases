//! The Play button: start the game through Battle.net (WoW refuses to start without it), or run the user's own
//! command (Linux: Lutris, Bottles, Steam...).

#[allow(unused_imports)]
use super::error::{msg, Error, Result};
use super::state::Settings;
use std::process::Command;

fn spawn(mut cmd: Command) -> Result<()> {
    cmd.spawn().map(|_| ()).map_err(|e| Error::Msg(format!("could not start the game: {e}")))
}

pub fn play(s: &Settings) -> Result<()> {
    let custom = s.play_command.trim();
    if !custom.is_empty() {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/C", custom]);
            return spawn(c);
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sh");
            c.args(["-c", custom]);
            return spawn(c);
        }
    }
    let code = s.launch_code.trim();
    #[cfg(windows)]
    {
        let mut found = None;
        for base in [std::env::var("ProgramFiles(x86)").ok(), std::env::var("ProgramFiles").ok()].into_iter().flatten() {
            for exe in ["Battle.net\\Battle.net Launcher.exe", "Battle.net\\Battle.net.exe"] {
                let p = std::path::Path::new(&base).join(exe);
                if p.is_file() && found.is_none() {
                    found = Some(p);
                }
            }
        }
        let Some(exe) = found else {
            return msg("Battle.net was not found: start the game from Battle.net yourself, or set a Play command in Settings");
        };
        let mut c = Command::new(exe);
        if !code.is_empty() {
            c.arg(format!("--exec=launch {code}"));
        }
        return spawn(c);
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("open");
        c.args(["-a", "Battle.net"]);
        if !code.is_empty() {
            c.args(["--args", &format!("--exec=launch {code}")]);
        }
        return spawn(c);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = code;
        msg("set a Play command in Settings (for example: lutris lutris:rungame/battlenet)")
    }
}
