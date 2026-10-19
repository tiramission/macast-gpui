//! Persistent application configuration.
//!
//! Stored as JSON in the OS config dir:
//! - Windows: `%APPDATA%\macast-gpui\config.json`
//! - macOS:   `~/Library/Application Support/macast-gpui/config.json`
//! - Linux:   `~/.config/macast-gpui/config.json`

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;
use uuid::Uuid;

/// Which player backend receives the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerKind {
    /// External mpv process, driven through its JSON IPC socket.
    Mpv,
    /// No-op player (useful for headless testing of the DLNA stack).
    Null,
}

impl Default for PlayerKind {
    fn default() -> Self {
        PlayerKind::Mpv
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Stable device UUID (persisted so phones remember the device across restarts).
    pub udn: String,
    /// Name shown in the phone's cast list.
    pub friendly_name: String,
    /// Preferred HTTP port. `0` = let the OS pick (the real port is reported in the UI).
    pub port: u16,
    #[serde(default)]
    pub player: PlayerKind,
    /// Path to the mpv executable. `None` = look it up on `PATH`.
    #[serde(default)]
    pub mpv_path: Option<PathBuf>,
    /// Seconds between SSDP `ssdp:alive` announcements.
    #[serde(default = "default_notify_secs")]
    pub ssdp_notify_secs: u64,
    /// `CACHE-CONTROL: max-age` advertised in SSDP announcements.
    #[serde(default = "default_max_age")]
    pub max_age: u32,
}

fn default_notify_secs() -> u64 {
    30
}

fn default_max_age() -> u32 {
    1800
}

impl Default for Config {
    fn default() -> Self {
        Config {
            udn: format!("{}", Uuid::new_v4()),
            friendly_name: default_friendly_name(),
            port: 5069,
            player: PlayerKind::Mpv,
            mpv_path: None,
            ssdp_notify_secs: default_notify_secs(),
            max_age: default_max_age(),
        }
    }
}

fn default_friendly_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "PC".to_string());
    format!("Macast-Gpui({host})")
}

impl Config {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("macast-gpui")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    /// Load config from disk, creating it (with a fresh UDN) on first run.
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => return cfg,
                Err(e) => tracing::warn!("config file is corrupt ({e}), using defaults"),
            },
            Err(e) => tracing::debug!("no config file yet: {e}"),
        }
        let cfg = Config::default();
        if let Err(e) = cfg.save() {
            tracing::warn!("could not persist initial config: {e}");
        } else {
            info!("created default config at {}", path.display());
        }
        cfg
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(Self::config_path(), text)?;
        Ok(())
    }
}
