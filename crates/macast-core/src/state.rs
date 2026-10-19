//! Renderer state: UPnP state variables + a snapshot broadcast to the UI.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

use crate::util::{format_time, parse_time};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SERVER_INFO: &str = "Linux/5.0 UPnP/1.1 Macast-Gpui/0.1";

/// UPnP AVTransport `TransportState` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    NoMediaPresent,
    Stopped,
    Playing,
    PausedPlayback,
}

impl TransportState {
    pub fn as_str(self) -> &'static str {
        match self {
            TransportState::NoMediaPresent => "NO_MEDIA_PRESENT",
            TransportState::Stopped => "STOPPED",
            TransportState::Playing => "PLAYING",
            TransportState::PausedPlayback => "PAUSED_PLAYBACK",
        }
    }
    pub fn parse_state(s: &str) -> Self {
        match s {
            "PLAYING" => TransportState::Playing,
            "PAUSED_PLAYBACK" => TransportState::PausedPlayback,
            "NO_MEDIA_PRESENT" => TransportState::NoMediaPresent,
            _ => TransportState::Stopped,
        }
    }
}

/// A cheap, UI-friendly projection of the renderer state.
#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub transport_state: TransportState,
    pub transport_status: String,
    pub title: String,
    pub uri: String,
    pub metadata: String,
    pub position: f64,
    pub duration: f64,
    pub volume: u8,
    pub mute: bool,
    pub speed: f64,
    /// Actual bound HTTP port (may differ from the configured one).
    pub port: u16,
    /// Non-loopback IPv4 addresses we are reachable on.
    pub ips: Vec<String>,
    pub friendly_name: String,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        PlaybackSnapshot {
            transport_state: TransportState::NoMediaPresent,
            transport_status: "OK".into(),
            title: String::new(),
            uri: String::new(),
            metadata: String::new(),
            position: 0.0,
            duration: 0.0,
            volume: 80,
            mute: false,
            speed: 1.0,
            port: 0,
            ips: Vec::new(),
            friendly_name: String::new(),
        }
    }
}

/// State variables that are `sendEvents="yes"` and therefore trigger GENA
/// notifications (mirrors Macast's `SERVICE_STATE_OBSERVED`).
pub fn is_evented(service: &str, name: &str) -> bool {
    match service {
        "AVTransport" => matches!(
            name,
            "TransportState"
                | "TransportStatus"
                | "CurrentMediaDuration"
                | "CurrentTrackDuration"
                | "CurrentTrack"
                | "NumberOfTracks"
                | "RelativeTimePosition"
                | "AbsoluteTimePosition"
                | "CurrentTrackURI"
                | "CurrentTrackMetaData"
                | "TransportPlaySpeed"
        ),
        "RenderingControl" => matches!(name, "Volume" | "Mute"),
        "ConnectionManager" => matches!(
            name,
            "A_ARG_TYPE_Direction" | "SinkProtocolInfo" | "CurrentConnectionIDs"
        ),
        _ => false,
    }
}

/// All mutable renderer state, guarded by one lock (contention is negligible:
/// a handful of DLNA calls per user action).
pub struct RendererState {
    pub vars: HashMap<String, String>,
    /// Monotonic counter used to build GENA `LastChange` deltas.
    pub last_change: String,
}

pub type SharedState = Arc<RwLock<RendererState>>;

impl RendererState {
    pub fn new(sink_protocol_info: &str) -> Self {
        let mut vars = HashMap::new();
        // Defaults mirroring Macast's DLNAProtocol.init_state()
        vars.insert("CurrentPlayMode".into(), "NORMAL".into());
        vars.insert("TransportPlaySpeed".into(), "1".into());
        vars.insert("TransportStatus".into(), "OK".into());
        vars.insert("RelativeCounterPosition".into(), "2147483647".into());
        vars.insert("AbsoluteCounterPosition".into(), "2147483647".into());
        vars.insert("A_ARG_TYPE_Direction".into(), "Output".into());
        vars.insert("CurrentConnectionIDs".into(), "0".into());
        vars.insert("PlaybackStorageMedium".into(), "None".into());
        vars.insert("SinkProtocolInfo".into(), sink_protocol_info.to_string());
        vars.insert("SourceProtocolInfo".into(), String::new());
        vars.insert("TransportState".into(), "NO_MEDIA_PRESENT".into());
        vars.insert("CurrentTrackURI".into(), String::new());
        vars.insert("CurrentTrackMetaData".into(), String::new());
        vars.insert("CurrentTrackTitle".into(), String::new());
        vars.insert("RelativeTimePosition".into(), "0:00:00".into());
        vars.insert("AbsoluteTimePosition".into(), "0:00:00".into());
        vars.insert("CurrentTrackDuration".into(), "0:00:00".into());
        vars.insert("CurrentMediaDuration".into(), "0:00:00".into());
        vars.insert("CurrentTrack".into(), "1".into());
        vars.insert("NumberOfTracks".into(), "1".into());
        vars.insert("Volume".into(), "80".into());
        vars.insert("Mute".into(), "0".into());

        RendererState {
            vars,
            last_change: String::new(),
        }
    }

    // ---- typed getters -------------------------------------------------

    pub fn get(&self, name: &str) -> String {
        self.vars.get(name).cloned().unwrap_or_default()
    }

    pub fn get_f64(&self, name: &str) -> f64 {
        self.vars.get(name).map(|s| parse_time(s)).unwrap_or(0.0)
    }

    pub fn get_bool(&self, name: &str) -> bool {
        matches!(
            self.vars.get(name).map(String::as_str),
            Some("1") | Some("true") | Some("yes")
        )
    }

    // ---- state variable updates ----------------------------------------

    /// Set a state variable. Returns `Some((name, value))` when the change
    /// should be pushed to GENA subscribers of `service`.
    pub fn set_var(
        &mut self,
        service: &str,
        name: &str,
        value: impl Into<String>,
    ) -> Option<(String, String)> {
        let value = value.into();
        let changed = self.vars.get(name).map(String::as_str) != Some(value.as_str());
        self.vars.insert(name.to_string(), value.clone());
        if changed && is_evented(service, name) {
            debug!("state change: {service}::{name} = {value}");
            Some((name.to_string(), value))
        } else {
            None
        }
    }

    /// Convenience: update several vars at once, returning the evented deltas.
    pub fn set_vars(&mut self, service: &str, updates: &[(&str, String)]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, value) in updates {
            if let Some(delta) = self.set_var(service, name, value.clone()) {
                out.push(delta);
            }
        }
        out
    }
}

/// Map an mpv-reported position (seconds) into the two UPnP position vars.
pub fn position_vars(pos: f64) -> [(&'static str, String); 2] {
    let t = format_time(pos);
    [
        ("RelativeTimePosition", t.clone()),
        ("AbsoluteTimePosition", t),
    ]
}
