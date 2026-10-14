//! What the DisasterZone server bot and the members' launchers say to each other (Discord bridge, shared bot).
//!
//! The bot runs on a server. Every member's launcher syncs with it: after the game saves (login, /reload, logout) it
//! uploads what the addon wrote (DisasterZoneBridgeDB: the snapshot and the records it applied) and fetches the
//! records made in Discord, which it writes into Interface/AddOns/DisasterZone_Bridge/Inbox.lua for the next load.
//!
//! Each record is signed by the server (W-OTS + Merkle, the scheme the addon's hot patches use: [`wots`]); the addon
//! checks the signature over [`Record::message`] with the public key in Core/BridgeKey.lua, so a record can be
//! applied by whoever loads it first and relayed through the guild without trusting the member who carries it.

pub mod wots;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A change made in Discord, for the game.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// unix seconds
    pub at: u64,
    /// who made it (their Discord name)
    pub by: String,
    /// type-specific fields: strings, integers, booleans and objects only (no lists, no fractions)
    #[serde(default)]
    pub body: Value,
    /// the Discord user it came from (told if the game refuses it)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub discord: String,
    /// the server's signature over [`Record::message`], hex; empty until signed
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

/// Why a record can't be signed.
#[derive(Debug, Clone, PartialEq)]
pub struct Unsignable(pub String);

impl std::fmt::Display for Unsignable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn put_str(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("s{}:", s.len()).as_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn put_value(v: &Value, out: &mut Vec<u8>) -> Result<(), Unsignable> {
    match v {
        Value::Null => out.push(b'n'),
        Value::Bool(b) => out.push(if *b { b't' } else { b'f' }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => out.extend_from_slice(format!("i{i};").as_bytes()),
            (None, Some(u)) => out.extend_from_slice(format!("i{u};").as_bytes()),
            _ => return Err(Unsignable(format!("{n} is not a whole number"))),
        },
        Value::String(s) => put_str(s, out),
        Value::Array(_) => return Err(Unsignable("lists can't be signed (Lua numbers them its own way)".into())),
        Value::Object(m) => {
            out.push(b'{');
            // (byte order, as Lua's < on strings; a nil value is no key at all in Lua)
            let mut keys: Vec<&String> = m.iter().filter(|(_, x)| !x.is_null()).map(|(k, _)| k).collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            for k in keys {
                put_str(k, out);
                put_value(&m[k], out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

impl Record {
    /// The bytes the signature covers (addon Core/Bridge.lua builds the same from the Lua table):
    /// "DZB1" S(id) S(type) I(at) S(by) S(discord) V(body), where S(x) = "s" len ":" x, I(n) = "i" n ";", and
    /// V(body) = S / I / "t" / "f" / "n" / "{" (S(key) V(value) for each key in byte order) "}".
    pub fn message(&self) -> Result<Vec<u8>, Unsignable> {
        let mut out = b"DZB1".to_vec();
        put_str(&self.id, &mut out);
        put_str(&self.kind, &mut out);
        out.extend_from_slice(format!("i{};", self.at).as_bytes());
        put_str(&self.by, &mut out);
        put_str(&self.discord, &mut out);
        put_value(&self.body, &mut out)?;
        Ok(out)
    }
}

/// A Discord text channel the GM can post raids to (listed in the addon's Settings > Discord).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: String,
}

/// A character made in Discord: the addon shows a Discord badge after the name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub name: String,
    pub discord: String,
}

/// POST /api/sync (Authorization: Bearer <member key>), body gzip'd JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncUp {
    /// the launcher's version
    #[serde(default)]
    pub launcher: String,
    /// DisasterZoneBridgeDB as the game last saved it (snap + applied); nothing when it has not saved since the last
    /// sync (a check for new records only)
    #[serde(default)]
    pub bridge: Option<Value>,
    /// the `hash` of the last answer this launcher wrote: an unchanged answer comes back as `same` (saves traffic)
    #[serde(default)]
    pub have: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncDown {
    /// the server's clock (unix seconds)
    pub now: u64,
    /// signed records still waiting for the game
    pub records: Vec<Record>,
    pub channels: Vec<Channel>,
    pub links: Vec<Link>,
    /// the Discord name the member key belongs to
    #[serde(default)]
    pub user: String,
    /// what records, channels, links and user add up to
    #[serde(default)]
    pub hash: String,
    /// nothing changed since `have`: the lists are left out
    #[serde(default)]
    pub same: bool,
}

/// POST /api/link { code } -> { key, user }: a code from /launcher in Discord becomes the launcher's member key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkUp {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkDown {
    pub key: String,
    pub user: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(body: Value) -> Record {
        Record { id: "dc_1".into(), kind: "signup".into(), at: 1791900000, by: "Tsumi".into(), body, discord: "42".into(), sig: String::new() }
    }

    #[test]
    fn message_is_canonical() {
        let m = rec(json!({ "st": "A", "raid": "r1", "sp": 3, "late": true, "gone": null, "nest": { "b": 1, "a": "x" } })).message().unwrap();
        assert_eq!(
            String::from_utf8(m).unwrap(),
            "DZB1s4:dc_1s6:signupi1791900000;s5:Tsumis2:42{s4:latets4:nest{s1:as1:xs1:bi1;}s4:raids2:r1s2:spi3;s2:sts1:A}"
        );
        assert_eq!(String::from_utf8(rec(Value::Null).message().unwrap()).unwrap().chars().last(), Some('n'));
    }

    #[test]
    fn utf8_lengths_are_bytes() {
        let m = rec(json!({ "name": "Élise" })).message().unwrap();
        assert!(String::from_utf8(m).unwrap().ends_with("{s4:names6:Élise}"));
    }

    #[test]
    fn lists_and_fractions_are_refused() {
        assert!(rec(json!({ "l": [1, 2] })).message().is_err());
        assert!(rec(json!({ "x": 1.5 })).message().is_err());
    }
}
