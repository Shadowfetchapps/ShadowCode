//! `remote.json` in the profile's config folder: remote access switches,
//! paired devices and phone notification settings.
//!
//! The file is written with mode 0600 and holds only SHA-256 digests of
//! device tokens, never the tokens. It is separate from `config.yaml` so the
//! general settings routes (which remote clients may use) can never change
//! it; only `/api/remote…`, which remote clients cannot reach, does.
use crate::paths::{self, AppPaths};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const FILE: &str = "remote.json";
pub const DEFAULT_ADDRESS: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 7390;
/// Paired devices kept at once.
pub const MAX_DEVICES: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Start the web server whenever this profile's engine runs in the
    /// desktop or `shadowcode serve`.
    pub enabled: bool,
    /// IP address to listen on. Loopback unless the user chose another.
    pub address: String,
    pub port: u16,
    /// An address to put in pairing links and notification links instead of
    /// the bound one (for example `https://box.tailnet.ts.net` behind
    /// `tailscale serve`).
    pub public_url: String,
    /// Interactive terminals and direct commands over remote access.
    pub allow_terminals: bool,
    pub devices: Vec<Device>,
    pub ntfy: Ntfy,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            address: DEFAULT_ADDRESS.into(),
            port: DEFAULT_PORT,
            public_url: String::new(),
            allow_terminals: false,
            devices: Vec::new(),
            ntfy: Ntfy::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Hex SHA-256 of the device's access token.
    pub digest: String,
    pub created_at: f64,
    #[serde(default)]
    pub last_seen: Option<f64>,
    /// Came back with a restore (Settings › Your data) and may be a device
    /// removed since that backup: it cannot connect until the user turns
    /// remote access on in Settings, having checked the list.
    #[serde(default)]
    pub restored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Ntfy {
    /// Empty until the user enters one: nothing is ever sent by default.
    pub server: String,
    pub topic: String,
    /// Include the approval request or task summary in the message.
    pub details: bool,
    pub events: NtfyEvents,
    /// Came back with a restore, and the topic may be one a lost phone
    /// still follows: nothing is sent until the user saves the server and
    /// topic again or turns remote access on.
    pub restored: bool,
}
impl Ntfy {
    pub fn configured(&self) -> bool {
        !self.server.is_empty() && !self.topic.is_empty()
    }
}

impl Settings {
    /// The user turned remote access on after a restore: the devices and
    /// phone notifications that came back with it work again.
    pub fn confirm_restored(&mut self) {
        for device in &mut self.devices {
            device.restored = false;
        }
        self.ntfy.restored = false;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NtfyEvents {
    pub approval: bool,
    pub finished: bool,
    pub failed: bool,
    pub limit: bool,
}
impl Default for NtfyEvents {
    fn default() -> Self {
        Self {
            approval: true,
            finished: true,
            failed: true,
            limit: true,
        }
    }
}

pub fn path(paths: &AppPaths) -> PathBuf {
    paths.config.join(FILE)
}

/// Read the saved settings; a missing file reads as the defaults.
pub fn load(paths: &AppPaths) -> Result<Settings> {
    let file = path(paths);
    match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("Could not read {}", file.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", file.display())),
    }
}

/// Save atomically with owner-only permissions (0600).
pub fn save(paths: &AppPaths, settings: &Settings) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(settings)?;
    paths::atomic_write(&path(paths), &bytes, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_privately_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(dir.path()).unwrap();
        assert_eq!(load(&paths).unwrap(), Settings::default());
        let mut settings = Settings::default();
        settings.devices.push(Device {
            id: "d1".into(),
            name: "Phone".into(),
            digest: "00".repeat(32),
            created_at: 1.0,
            last_seen: None,
            restored: false,
        });
        save(&paths, &settings).unwrap();
        assert_eq!(load(&paths).unwrap(), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(&paths))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Old or partial files fill in defaults.
        std::fs::write(path(&paths), r#"{"enabled":true}"#).unwrap();
        let partial = load(&paths).unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.port, DEFAULT_PORT);
        assert!(partial.ntfy.events.approval && !partial.ntfy.configured());
    }
}
