use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("download failed: {0}")]
    Net(String),
    #[error("the release could not be verified (bad signature). If a release was published a moment ago, try again in a few minutes.")]
    BadSignature,
    #[error("the release list is damaged: {0}")]
    BadManifest(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn msg<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Msg(text.into()))
}

// Tauri commands return errors to the page as plain text.
impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
