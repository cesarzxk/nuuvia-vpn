use std::io;

/// Errors returned by the nuvia core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("noise: {0}")]
    Noise(#[from] snow::Error),

    #[error("crypto: {0}")]
    Crypto(String),

    #[error("argon2: {0}")]
    Argon2(String),

    #[error("hex: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),

    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("handshake: {0}")]
    Handshake(String),

    #[error("http: {0}")]
    Http(String),

    #[error("timed out: {0}")]
    Timeout(String),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    pub fn handshake(msg: impl Into<String>) -> Self {
        Self::Handshake(msg.into())
    }
}

impl From<chacha20poly1305::Error> for Error {
    fn from(value: chacha20poly1305::Error) -> Self {
        // aead::Error does not implement std::error::Error, so thiserror
        // cannot derive the conversion.
        Self::Crypto(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
