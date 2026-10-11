//! Where releases come from: the public releases repository over HTTPS, or a local folder (tests, a local try of
//! `release.py publish --out DIR`).

use super::error::{Error, Result};
use super::manifest::{verify_and_parse, Manifest, ReleaseKey};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub enum Source {
    Http { base: String, client: reqwest::Client },
    Dir(PathBuf),
}

impl Source {
    pub fn parse(s: &str) -> Result<Source> {
        if s.starts_with("https://") || s.starts_with("http://") {
            let base = if s.ends_with('/') { s.to_string() } else { format!("{s}/") };
            let client = reqwest::Client::builder()
                .user_agent(concat!("DisasterZone-Launcher/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(Source::Http { base, client })
        } else {
            Ok(Source::Dir(PathBuf::from(s)))
        }
    }

    /// One file of the release tree. `fresh` asks the CDN for a current copy (manifests change, objects never do).
    pub async fn get(&self, rel: &str, fresh: bool) -> Result<Vec<u8>> {
        match self {
            Source::Http { base, client } => {
                let mut url = format!("{base}{rel}");
                if fresh {
                    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    url.push_str(&format!("?t={t}"));
                }
                let r = client.get(&url).send().await.map_err(|e| Error::Net(e.to_string()))?;
                if !r.status().is_success() {
                    return Err(Error::Net(format!("{rel}: HTTP {}", r.status())));
                }
                Ok(r.bytes().await.map_err(|e| Error::Net(e.to_string()))?.to_vec())
            }
            Source::Dir(d) => tokio::fs::read(d.join(rel)).await.map_err(|e| Error::Net(format!("{rel}: {e}"))),
        }
    }

    pub async fn manifest(&self, key: &ReleaseKey, channel: &str) -> Result<Manifest> {
        let data = self.get(&format!("channels/{channel}.json"), true).await?;
        let sig = self.get(&format!("channels/{channel}.json.sig"), true).await?;
        let m = verify_and_parse(key, &data, &sig)?;
        if m.channel != channel {
            return Err(Error::BadManifest(format!("asked for {channel}, got {}", m.channel)));
        }
        Ok(m)
    }
}
