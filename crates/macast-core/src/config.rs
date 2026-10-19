//! Persistent application configuration.
//!
//! Stored as JSON in the OS config dir:
//! - Windows: `%APPDATA%\macast-gpui\config.json`
//! - macOS:   `~/Library/Application Support/macast-gpui/config.json`
//! - Linux:   `~/.config/macast-gpui/config.json`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;
use uuid::Uuid;

/// Which player backend receives the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerKind {
    /// External mpv process, driven through its JSON IPC socket.
    #[default]
    Mpv,
    /// Built-in player: Windows Media Foundation rendering inside the app
    /// window (Windows only; needs the UI process to provide a video host).
    Builtin,
    /// No-op player (useful for headless testing of the DLNA stack).
    Null,
}

/// Window colour theme. `Auto` follows the OS appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
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
    /// Open the mpv window fullscreen when a cast starts.
    #[serde(default = "default_true")]
    pub mpv_fullscreen: bool,
    /// Window colour theme; `auto` follows the OS.
    #[serde(default)]
    pub theme: Theme,
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

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            udn: format!("{}", Uuid::new_v4()),
            friendly_name: default_friendly_name(),
            port: 5069,
            player: PlayerKind::Mpv,
            mpv_path: None,
            mpv_fullscreen: true,
            theme: Theme::Auto,
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
        Self::load_from(&Self::config_path())
    }

    /// Same as [`Config::load`] but against an explicit path (used by tests).
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => return cfg,
                Err(e) => tracing::warn!("config file is corrupt ({e}), using defaults"),
            },
            Err(e) => tracing::debug!("no config file yet: {e}"),
        }
        let cfg = Config::default();
        if let Err(e) = cfg.save_to(path) {
            tracing::warn!("could not persist initial config: {e}");
        } else {
            info!("created default config at {}", path.display());
        }
        cfg
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::config_path())
    }

    /// Write the config as pretty JSON, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let dir = std::env::temp_dir().join(format!("macast-gpui-test-{}", Uuid::new_v4()));
        let path = dir.join("config.json");

        let cfg = Config {
            friendly_name: "客厅电视".into(),
            port: 5069,
            player: PlayerKind::Null,
            ..Default::default()
        };
        cfg.save_to(&path).expect("save");

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.udn, cfg.udn);
        assert_eq!(loaded.friendly_name, "客厅电视");
        assert_eq!(loaded.player, PlayerKind::Null);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_optional_fields_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"udn":"u","friendly_name":"n","port":0}"#)
            .expect("legacy config parses");
        assert_eq!(cfg.player, PlayerKind::Mpv);
        assert!(cfg.mpv_fullscreen);
        assert_eq!(cfg.theme, Theme::Auto);
        assert_eq!(cfg.ssdp_notify_secs, 30);
        assert_eq!(cfg.max_age, 1800);
    }
}
