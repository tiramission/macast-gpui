//! Pluggable player backends.
//!
//! Mirrors Macast's `Renderer` base class: the DLNA layer only knows how to
//! call these methods, and each backend maps them onto a concrete player.
//! Events flow the other way through a broadcast channel and are translated
//! into UPnP state variable changes + GENA notifications.

pub mod alhazen;
pub mod libmpv;
pub mod video_host;

use crate::config::{PlayerConfig, PlayerKind};
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

/// One decoded picture, ready to be turned into a UI image.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// BGRA rows, tightly packed: `width * height * 4` bytes.
    pub bgra: Arc<[u8]>,
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

    /// Latest picture for backends that render inside the app window
    /// (libmpv). `None` when the backend draws its own window or has no
    /// frame yet, in which case the UI keeps showing its artwork.
    fn video_frame(&self) -> Option<VideoFrame> {
        None
    }

    /// How large the video is displayed, in device pixels. Backends that
    /// render for the UI scale down to this before handing frames over.
    fn set_render_size(&self, _w: u32, _h: u32) {}
}

/// Build the configured player backend.
///
/// `video_host` is optional: the alhazen backend forwards commands to it, and
/// when it is `None` a [`NullVideoHost`](video_host::NullVideoHost) drops them
/// (in this app the UI layer drives alhazen-gpui's `VideoPlayer` directly).
pub async fn build_player(
    config: &PlayerConfig,
    video_host: Option<Arc<dyn VideoHost>>,
) -> Arc<dyn Player> {
    match config.kind {
        // mpv runs in-process via libmpv (`libmpv-2.dll`); if the DLL cannot be
        // loaded, fall back to alhazen.
        PlayerKind::Mpv => {
            match libmpv::LibmpvPlayer::new(config.libmpv_path.clone(), config.fullscreen_on_cast) {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    tracing::error!("mpv backend unavailable ({e}), using alhazen");
                    Arc::new(alhazen::AlhazenPlayer::new(
                        video_host,
                        config.fullscreen_on_cast,
                    ))
                }
            }
        }
        PlayerKind::Alhazen => Arc::new(alhazen::AlhazenPlayer::new(
            video_host,
            config.fullscreen_on_cast,
        )),
    }
}
