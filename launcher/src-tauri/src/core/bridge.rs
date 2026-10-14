//! The Discord bridge's files on a member's PC (the launcher's sync with the guild's bot server: sync.rs).
//!
//!   inbox  <flavor>/Interface/AddOns/DisasterZone_Bridge/Inbox.lua   DZBridge = { v, written, user, records, channels, links }
//!   answer <flavor>/WTF/Account/<acct>/SavedVariables/DisasterZone.lua   DisasterZoneBridgeDB (snap + applied)
//! The records are signed by the bot's server; the addon checks them (Core/Bridge.lua), so this file can't be used to
//! fake a change. The folder is made by the launcher, not shipped in releases.

use super::error::Result;
use dz_proto::SyncDown;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const FOLDER: &str = "DisasterZone_Bridge";
const SV_NAME: &str = "DisasterZoneBridgeDB";

// ------------------------------------------------------------------------------------------------------------------
// Inbox.lua (+ the folder's TOC)
// ------------------------------------------------------------------------------------------------------------------

fn lua_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn lua_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("nil"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => lua_str(s, out),
        Value::Array(a) => {
            out.push('{');
            for x in a {
                lua_value(x, out);
                out.push_str(", ");
            }
            out.push('}');
        }
        Value::Object(m) => {
            out.push('{');
            for (k, x) in m {
                out.push('[');
                lua_str(k, out);
                out.push_str("] = ");
                lua_value(x, out);
                out.push_str(", ");
            }
            out.push('}');
        }
    }
}

/// The inbox for what the server sent. (DZBridge, not 1.4's DZBridgeInbox: an addon that can't check signatures
/// must not take these records and mark them refused.)
pub fn inbox_lua(down: &SyncDown, written: u64) -> String {
    let mut s = String::from("-- Written by the DisasterZone launcher (Discord sync). Do not edit: it is rewritten, and the addon checks every\n-- record's signature.\n");
    s.push_str(&format!("DZBridge = {{ v = 2, written = {written}, user = "));
    lua_str(&down.user, &mut s);
    s.push_str(", records = {\n");
    for r in &down.records {
        s.push_str("  ");
        lua_value(&serde_json::to_value(r).unwrap(), &mut s);
        s.push_str(",\n");
    }
    s.push_str("}, channels = ");
    lua_value(&serde_json::to_value(&down.channels).unwrap(), &mut s);
    s.push_str(", links = ");
    lua_value(&serde_json::to_value(&down.links).unwrap(), &mut s);
    s.push_str(" }\n");
    s
}

/// The `## Interface:` line of DisasterZone's TOC, so the bridge folder never shows as out of date.
fn interface_of(addons: &Path) -> String {
    fs::read_to_string(addons.join("DisasterZone").join("DisasterZone.toc"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("## Interface:").map(|v| v.trim().to_string())))
        .unwrap_or_else(|| "20505".into())
}

fn write_atomic(p: &Path, text: &str) -> Result<()> {
    if fs::read_to_string(p).ok().as_deref() == Some(text) {
        return Ok(());
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, p)?;
    Ok(())
}

/// Write the bridge folder (the game reads it at its next login or /reload). Only when the addon is installed.
pub fn write_inbox(flavor_dir: &Path, down: &SyncDown) -> Result<()> {
    let addons = flavor_dir.join("Interface").join("AddOns");
    if !addons.join("DisasterZone").is_dir() {
        return Ok(());
    }
    let dir = addons.join(FOLDER);
    fs::create_dir_all(&dir)?;
    let toc = format!(
        "## Interface: {}\n## Title: |cffff8000Disaster|rZone Discord bridge\n## Notes: Made by the DisasterZone launcher: what the guild's Discord bot hands to the game.\n## Author: Casual Disaster\n## Version: 2\n\nInbox.lua\n",
        interface_of(&addons)
    );
    write_atomic(&dir.join(format!("{FOLDER}.toc")), &toc)?;
    // (the written stamp only changes with the content: an unchanged sync doesn't rewrite the file)
    let old = fs::read_to_string(dir.join("Inbox.lua")).unwrap_or_default();
    let same = inbox_lua(down, 0);
    let written = old
        .lines()
        .nth(2)
        .and_then(|l| l.split("written = ").nth(1))
        .and_then(|r| r.split(',').next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .filter(|w| old.replace(&format!("written = {w},"), "written = 0,") == same)
        .unwrap_or(down.now);
    write_atomic(&dir.join("Inbox.lua"), &inbox_lua(down, written))
}

/// Unlinked: no records any more (channels and badges go too).
pub fn clear_inbox(flavor_dir: &Path) -> Result<()> {
    let p = flavor_dir.join("Interface").join("AddOns").join(FOLDER).join("Inbox.lua");
    if p.exists() {
        write_atomic(&p, &inbox_lua(&SyncDown::default(), 0))?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------
// Reading SavedVariables: a small reader for the Lua WoW writes (tables, strings, numbers, booleans, nil, comments)
// ------------------------------------------------------------------------------------------------------------------

struct Lua<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lua<'a> {
    fn skip(&mut self) {
        loop {
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.b[self.i..].starts_with(b"--") {
                while self.i < self.b.len() && self.b[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                return;
            }
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn string(&mut self) -> Option<String> {
        let q = self.b[self.i];
        self.i += 1;
        let mut out = Vec::new();
        while let Some(&c) = self.b.get(self.i) {
            self.i += 1;
            if c == q {
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            if c != b'\\' {
                out.push(c);
                continue;
            }
            let e = *self.b.get(self.i)?;
            self.i += 1;
            match e {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'0'..=b'9' => {
                    let mut n = (e - b'0') as u32;
                    for _ in 0..2 {
                        match self.b.get(self.i) {
                            Some(d @ b'0'..=b'9') => {
                                n = n * 10 + (d - b'0') as u32;
                                self.i += 1;
                            }
                            _ => break,
                        }
                    }
                    out.push(n as u8);
                }
                b'\n' => out.push(b'\n'),
                other => out.push(other),
            }
        }
        None
    }

    fn value(&mut self) -> Option<Value> {
        self.skip();
        let c = *self.b.get(self.i)?;
        match c {
            b'{' => self.table(),
            b'"' | b'\'' => self.string().map(Value::String),
            _ => {
                let start = self.i;
                while self.i < self.b.len()
                    && (self.b[self.i].is_ascii_alphanumeric() || b"-+._".contains(&self.b[self.i]))
                {
                    self.i += 1;
                }
                if self.i == start {
                    return None; // (not a value: stop instead of looping)
                }
                let w = std::str::from_utf8(&self.b[start..self.i]).ok()?;
                match w {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    "nil" => Some(Value::Null),
                    _ => {
                        if let Ok(n) = w.parse::<i64>() {
                            Some(Value::from(n))
                        } else {
                            // (inf / nan have no JSON form: null)
                            Some(w.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number))
                        }
                    }
                }
            }
        }
    }

    /// `name =` (a key without brackets): the name, past the `=`; otherwise nothing is consumed.
    fn name_key(&mut self) -> Option<String> {
        let start = self.i;
        let mut j = start;
        while j < self.b.len() && (self.b[j].is_ascii_alphanumeric() || self.b[j] == b'_') {
            j += 1;
        }
        if j == start || self.b[start].is_ascii_digit() {
            return None;
        }
        let name = std::str::from_utf8(&self.b[start..j]).ok()?.to_string();
        if ["true", "false", "nil"].contains(&name.as_str()) {
            return None;
        }
        self.i = j;
        if self.eat(b'=') && self.b.get(self.i) != Some(&b'=') {
            Some(name)
        } else {
            self.i = start;
            None
        }
    }

    /// A table becomes a JSON object; array entries get keys "1", "2", ... (Lua's own numbering).
    fn table(&mut self) -> Option<Value> {
        self.i += 1;
        let mut m = Map::new();
        let mut n = 0;
        loop {
            if self.eat(b'}') {
                return Some(Value::Object(m));
            }
            self.skip();
            let key = if self.b.get(self.i) == Some(&b'[') {
                self.i += 1;
                let k = match self.value()? {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                if !self.eat(b']') || !self.eat(b'=') {
                    return None;
                }
                k
            } else if let Some(k) = self.name_key() {
                k
            } else {
                n += 1;
                n.to_string()
            };
            let v = self.value()?;
            m.insert(key, v);
            self.eat(b',');
            self.eat(b';');
        }
    }
}

/// The value assigned to the global `name` in a SavedVariables file (WoW writes `Name = value` at line start).
pub fn read_global(text: &str, name: &str) -> Option<Value> {
    let head = format!("{name} = ");
    let start = if text.starts_with(&head) { 0 } else { text.find(&format!("\n{head}"))? + 1 };
    let mut p = Lua { b: text.as_bytes(), i: start + head.len() };
    p.value()
}

/// The SavedVariables files under a game folder, newest first (one per account).
fn saved_variables(flavor_dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(accounts) = fs::read_dir(flavor_dir.join("WTF").join("Account")) else { return Vec::new() };
    let mut out: Vec<(SystemTime, PathBuf)> = accounts
        .flatten()
        .map(|a| a.path().join("SavedVariables").join("DisasterZone.lua"))
        .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// When the game last saved (the newest account played).
pub fn newest_saved_variables(flavor_dir: &Path) -> Option<SystemTime> {
    saved_variables(flavor_dir).first().map(|(t, _)| *t)
}

/// DisasterZoneBridgeDB from the newest SavedVariables under a game folder.
pub fn read_bridge_db(flavor_dir: &Path) -> Option<Value> {
    let (_, path) = saved_variables(flavor_dir).into_iter().next()?;
    let text = fs::read(&path).map(|d| String::from_utf8_lossy(&d).into_owned()).ok()?;
    read_global(&text, SV_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dz_proto::{Channel, Link, Record};
    use serde_json::json;

    const SV: &str = r#"
DisasterZoneDB = {
	["big"] = { 1, 2, 3, },
}
DisasterZoneBridgeDB = {
	["applied"] = {
		["dc_1"] = {
			["t"] = 1791900000,
			["ok"] = true,
		},
		["dc_2"] = {
			["why"] = "unknown type \"x\"\nline\124two",
			["ok"] = false,
			["t"] = 1.5e3,
		},
	},
	["loaded"] = 1791900001,
	["list"] = {
		"a", -- [1]
		'b', -- [2]
	},
}
"#;

    #[test]
    fn reads_what_wow_writes() {
        let v = read_global(SV, "DisasterZoneBridgeDB").unwrap();
        assert_eq!(v["applied"]["dc_1"]["ok"], json!(true));
        assert_eq!(v["applied"]["dc_2"]["why"], json!("unknown type \"x\"\nline|two"));
        assert_eq!(v["applied"]["dc_2"]["t"], json!(1500.0));
        assert_eq!(v["list"]["2"], json!("b"));
        assert_eq!(read_global(SV, "DisasterZoneDB").unwrap()["big"]["3"], json!(3));
        assert!(read_global(SV, "Nope").is_none());
    }

    fn down(text: &str) -> SyncDown {
        SyncDown {
            now: 7,
            records: vec![Record {
                id: "dc_1".into(),
                kind: "test".into(),
                at: 1,
                by: "Tsumi".into(),
                body: json!({ "text": text }),
                discord: "42".into(),
                sig: "00ff".into(),
            }],
            channels: vec![Channel { id: "1".into(), name: "raids".into() }],
            links: vec![Link { name: "Lee".into(), discord: "Tsumi".into() }],
            user: "Tsumi".into(),
            ..Default::default()
        }
    }

    #[test]
    fn inbox_round_trips_through_the_reader() {
        let lua = inbox_lua(&down("hi \"there\" | \\ \n"), 7);
        let v = read_global(&lua, "DZBridge").unwrap();
        assert_eq!(v["written"], json!(7));
        assert_eq!(v["user"], json!("Tsumi"));
        assert_eq!(v["channels"]["1"]["name"], json!("raids"));
        assert_eq!(v["links"]["1"]["discord"], json!("Tsumi"));
        assert_eq!(v["records"]["1"]["id"], json!("dc_1"));
        assert_eq!(v["records"]["1"]["sig"], json!("00ff"));
        assert_eq!(v["records"]["1"]["body"]["text"], json!("hi \"there\" | \\ \n"));
    }

    #[test]
    fn inbox_is_written_only_next_to_the_addon_and_keeps_its_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let flavor = dir.path().join("_anniversary_");
        write_inbox(&flavor, &down("a")).unwrap();
        assert!(!flavor.join("Interface/AddOns/DisasterZone_Bridge").exists()); // (no addon: nothing)
        let addon = flavor.join("Interface/AddOns/DisasterZone");
        fs::create_dir_all(&addon).unwrap();
        fs::write(addon.join("DisasterZone.toc"), "## Interface: 20504\n").unwrap();
        write_inbox(&flavor, &down("a")).unwrap();
        let toc = fs::read_to_string(flavor.join("Interface/AddOns/DisasterZone_Bridge/DisasterZone_Bridge.toc")).unwrap();
        assert!(toc.contains("## Interface: 20504"));
        let inbox = flavor.join("Interface/AddOns/DisasterZone_Bridge/Inbox.lua");
        // the same records later: the written stamp stays
        let mut later = down("a");
        later.now = 99;
        write_inbox(&flavor, &later).unwrap();
        assert!(fs::read_to_string(&inbox).unwrap().contains("written = 7,"));
        write_inbox(&flavor, &{ let mut d = down("b"); d.now = 100; d }).unwrap();
        assert!(fs::read_to_string(&inbox).unwrap().contains("written = 100,"));
        clear_inbox(&flavor).unwrap();
        assert!(!fs::read_to_string(&inbox).unwrap().contains("dc_1"));
    }

    #[test]
    fn newest_account_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        for acct in ["A", "B"] {
            let sv = dir.path().join("WTF/Account").join(acct).join("SavedVariables");
            fs::create_dir_all(&sv).unwrap();
            fs::write(sv.join("DisasterZone.lua"), SV).unwrap();
        }
        assert!(newest_saved_variables(dir.path()).is_some());
        assert!(read_bridge_db(dir.path()).is_some());
        assert!(newest_saved_variables(&dir.path().join("nope")).is_none());
    }
}
