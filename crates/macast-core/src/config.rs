//! Persistent application configuration.
//!
//! Stored as JSON in the OS config dir:
//! - Windows: `%APPDATA%\macast-gpui\config.json`
//! - macOS:   `~/Library/Application Support/macast-gpui/config.json`
//! - Linux:   `~/.config/macast-gpui/config.json`
//!
//! The file is grouped (`device` / `player` / `ui` / `network`) so related
//! settings live together; every group has defaults, so a config file only
//! needs to spell out what differs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;
use uuid::Uuid;

/// Which player engine receives the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerKind {
    /// mpv loaded in-process (`libmpv-2.dll`), rendered inside the app window.
    Mpv,
    /// Pure-Rust media player engine (alhazen-core) - default.
    #[default]
    Alhazen,
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

/// How this renderer presents itself on the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Stable device UUID (persisted so phones remember the device).
    pub udn: String,
    /// Name shown in the phone's cast list.
    pub friendly_name: String,
    /// Preferred HTTP port. `0` = let the OS pick (the real port is shown in the UI).
    pub port: u16,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            udn: format!("{}", Uuid::new_v4()),
            friendly_name: default_friendly_name(),
            port: 5069,
        }
    }
}

/// Which engine plays, and how the picture is shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerConfig {
    /// Engine to use.
    #[serde(default)]
    pub kind: PlayerKind,
    /// Enter fullscreen when a cast starts (Esc leaves fullscreen again).
    #[serde(default = "default_true")]
    pub fullscreen_on_cast: bool,
    /// Path to `libmpv-2.dll` (mpv backend). `None` = probe the usual
    /// places (env `MACAST_LIBMPV`, the exe directory, `PATH`, Scoop).
    #[serde(default)]
    pub libmpv_path: Option<PathBuf>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            kind: PlayerKind::Alhazen,
            fullscreen_on_cast: true,
            libmpv_path: None,
        }
    }
}

/// Window appearance and shell integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Colour theme; `auto` follows the OS.
    #[serde(default)]
    pub theme: Theme,
    /// Show the system tray icon; closing the window hides to tray instead of
    /// quitting. When `false`, closing the window exits the app.
    #[serde(default = "default_true")]
    pub tray_enabled: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: Theme::Auto,
            tray_enabled: true,
        }
    }
}

/// SSDP announcement tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Seconds between SSDP `ssdp:alive` announcements.
    #[serde(default = "default_notify_secs")]
    pub ssdp_notify_secs: u64,
    /// `CACHE-CONTROL: max-age` advertised in SSDP announcements.
    #[serde(default = "default_max_age")]
    pub max_age: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            ssdp_notify_secs: default_notify_secs(),
            max_age: default_max_age(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub device: DeviceConfig,
    #[serde(default)]
    pub player: PlayerConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub network: NetworkConfig,
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

        let mut cfg = Config::default();
        cfg.device.friendly_name = "客厅电视".into();
        cfg.player.kind = PlayerKind::Mpv;
        cfg.player.libmpv_path = Some(PathBuf::from("C:/lib/libmpv-2.dll"));
        cfg.save_to(&path).expect("save");

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.device.udn, cfg.device.udn);
        assert_eq!(loaded.device.friendly_name, "客厅电视");
        assert_eq!(loaded.player.kind, PlayerKind::Mpv);
        assert_eq!(
            loaded.player.libmpv_path.as_deref(),
            Some(Path::new("C:/lib/libmpv-2.dll"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_object_falls_back_to_defaults() {
        let cfg: Config = serde_json::from_str("{}").expect("empty config parses");
        assert_eq!(cfg.player.kind, PlayerKind::Alhazen);
        assert!(cfg.player.fullscreen_on_cast);
        assert!(cfg.player.libmpv_path.is_none());
        assert_eq!(cfg.ui.theme, Theme::Auto);
        assert!(cfg.ui.tray_enabled);
        assert_eq!(cfg.network.ssdp_notify_secs, 30);
        assert_eq!(cfg.network.max_age, 1800);
        assert_eq!(cfg.device.port, 5069);
    }

    #[test]
    fn two_defaults_get_distinct_udns() {
        assert_ne!(Config::default().device.udn, Config::default().device.udn);
    }
}
