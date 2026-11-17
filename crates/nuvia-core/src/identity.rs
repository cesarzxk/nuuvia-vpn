use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, Result};

const KEY_LEN: usize = 32;

/// On-disk representation of a device identity.
#[derive(Debug, Serialize, Deserialize)]
struct IdentityFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    private_key: String,
    public_key: String,
}

/// A long lived X25519 key pair identifying this device.
///
/// The private half is zeroized on drop. This type is intentionally not
/// `Clone`: passing references around avoids accidental copies of key
/// material.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Identity {
    private_key: [u8; KEY_LEN],
    #[zeroize(skip)]
    public_key: [u8; KEY_LEN],
    #[zeroize(skip)]
    name: Option<String>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("name", &self.name)
            .field("public_key", &hex::encode(self.public_key))
            .finish_non_exhaustive()
    }
}

impl Identity {
    pub fn generate(name: Option<String>) -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self {
            private_key: secret.to_bytes(),
            public_key: public.to_bytes(),
            name,
        }
    }

    pub fn private_key(&self) -> &[u8; KEY_LEN] {
        &self.private_key
    }

    pub fn public_key(&self) -> &[u8; KEY_LEN] {
        &self.public_key
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key)
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        let file: IdentityFile = toml::from_str(&raw)?;

        let private_key = decode_key(&file.private_key, "private_key")?;
        let public_key = decode_key(&file.public_key, "public_key")?;
        let derived = PublicKey::from(&StaticSecret::from(private_key));
        if derived.to_bytes() != public_key {
            return Err(Error::config(format!(
                "identity file {} has a public key that does not match its private key",
                path.display()
            )));
        }

        Ok(Self {
            private_key,
            public_key,
            name: file.name,
        })
    }

    /// Load an identity, generating and persisting one when the file does not
    /// exist yet.
    pub fn load_or_create(path: &Path, name: Option<String>) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }

        let identity = Self::generate(name);
        identity.save(path)?;
        Ok(identity)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
                set_dir_private(parent)?;
            }
        }

        let file = IdentityFile {
            name: self.name.clone(),
            private_key: hex::encode(self.private_key),
            public_key: hex::encode(self.public_key),
        };
        let body = toml::to_string_pretty(&file)?;

        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(path)?;
        handle.write_all(body.as_bytes())?;
        handle.sync_all()?;
        set_file_private(path)?;
        Ok(())
    }
}

/// Default identity location: `$XDG_CONFIG_HOME/nuvia/identity.toml` on Unix,
/// `%APPDATA%\nuvia\identity.toml` on Windows.
pub fn default_identity_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("nuvia").join("identity.toml");
        }
    }

    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("nuvia").join("identity.toml");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("nuvia")
            .join("identity.toml");
    }
    PathBuf::from("nuvia-identity.toml")
}

fn decode_key(value: &str, field: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = hex::decode(value.trim())?;
    if bytes.len() != KEY_LEN {
        return Err(Error::config(format!(
            "{field} must be {KEY_LEN} bytes ({} hex characters), got {} bytes",
            KEY_LEN * 2,
            bytes.len()
        )));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.toml");
        let identity = Identity::generate(Some("cesar-pc".into()));
        identity.save(&path).unwrap();

        let loaded = Identity::load(&path).unwrap();
        assert_eq!(loaded.public_key(), identity.public_key());
        assert_eq!(loaded.name(), Some("cesar-pc"));
    }

    #[test]
    fn rejects_mismatched_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.toml");
        let identity = Identity::generate(None);
        identity.save(&path).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let other = Identity::generate(None);
        let bad = raw.replace(
            &hex::encode(identity.public_key()),
            &hex::encode(other.public_key()),
        );
        fs::write(&path, bad).unwrap();

        assert!(Identity::load(&path).is_err());
    }
}
