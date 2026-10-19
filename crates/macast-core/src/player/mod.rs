//! Pluggable player backends.
//!
//! Mirrors Macast's `Renderer` base class: the DLNA layer only knows how to
//! call these methods, and each backend maps them onto a concrete player.
//! Events flow the other way through a broadcast channel and are translated
//! into UPnP state variable changes + GENA notifications.

pub mod alhazen;
pub mod mpv;
pub mod video_host;

use crate::config::PlayerKind;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use video_host::VideoHost;

/// Events emitted by a player backend towards the DLNA layer.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// Media loaded and (auto-)playing; duration may be unknown (0.0).
    Loaded {
        duration: f64,
    },
    /// Current playback position in seconds.
    Position(f64),
    /// Total duration in seconds (0.0 = unknown).
    Duration(f64),
    Paused(bool),
    Playing,
    Stopped,
    /// End of file reached.
    Ended,
    Volume(f64),
    Mute(bool),
    Speed(f64),
    Error(String),
}

/// A player backend. All methods are async because they may talk to an
/// external process or a network service.
#[async_trait::async_trait]
pub trait Player: Send + Sync + 'static {
    /// Load `url` and start playing. `start_secs` seeks after loading.
    async fn set_media_url(&self, url: &str, start_secs: f64, title: &str);
    async fn resume(&self);
    async fn pause(&self);
    async fn stop(&self);
    async fn seek(&self, secs: f64);
    /// 0..=100
    async fn set_volume(&self, volume: u8);
    async fn set_mute(&self, mute: bool);
    async fn set_speed(&self, speed: f64);
    async fn add_subtitle(&self, url: &str, title: &str);
    async fn set_subtitle_visible(&self, visible: bool);
    async fn show_text(&self, text: &str, duration_ms: u64);
    /// Release resources (kill child processes, close sockets).
    async fn shutdown(&self) {}
    fn events(&self) -> broadcast::Receiver<PlayerEvent>;
    fn name(&self) -> &'static str;
}

/// Build the configured player backend.
///
/// `video_host` is provided by the UI process and is required by the built-in
/// (Media Foundation) backend; without it that backend falls back to null.
pub async fn build_player(
    kind: PlayerKind,
    mpv_path: Option<PathBuf>,
    fullscreen: bool,
    video_host: Option<Arc<dyn VideoHost>>,
) -> Arc<dyn Player> {
    match kind {
        PlayerKind::Mpv => match mpv::MpvPlayer::spawn(mpv_path, fullscreen).await {
            Ok(p) => Arc::new(p),
            Err(e) => {
                tracing::error!("failed to start mpv backend ({e})");
                Arc::new(alhazen::AlhazenPlayer::new(video_host, fullscreen))
            }
        },
        PlayerKind::Alhazen => Arc::new(alhazen::AlhazenPlayer::new(video_host, fullscreen)),
    }
}
