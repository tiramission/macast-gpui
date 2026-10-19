//! Bridge between the built-in video backend (this crate) and the UI process
//! (macast-app), which owns the windows the video renders into.
//!
//! The core stays UI- and platform-agnostic: [`BuiltinPlayer`](super::builtin::BuiltinPlayer)
//! only forwards [`VideoCommand`]s to a [`VideoHost`] implementation provided
//! by the app, and receives [`PlayerEvent`]s back through an [`EventSink`].
//! The app-side host executes every command on the UI main thread.

use super::PlayerEvent;
use std::sync::Arc;

/// Commands the built-in player sends to the UI-side video host.
///
/// The host executes them on the UI main thread; ordering is preserved.
#[derive(Debug, Clone)]
pub enum VideoCommand {
    /// Idempotent: create the video surface (child window) if not yet created.
    EnsureCreated,
    /// Replace the current media and start playback.
    Play {
        url: String,
        start_secs: f64,
        title: String,
        fullscreen: bool,
    },
    Resume,
    Pause,
    Stop,
    Seek {
        secs: f64,
    },
    SetVolume {
        volume: u8,
    },
    SetMute {
        mute: bool,
    },
    SetSpeed {
        speed: f64,
    },
    /// Move/resize the video surface. Physical pixels, relative to the host
    /// window's client area. Ignored while fullscreen.
    MoveSurface {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    ShowSurface,
    HideSurface,
    /// Cover the whole monitor with the video surface (cast fullscreen).
    EnterFullscreen,
    /// Tear everything down (app shutdown).
    Shutdown,
}

/// Receives [`PlayerEvent`]s from the video host on the UI side.
pub type EventSink = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

/// Implemented by the UI process; executes video commands on the main thread.
pub trait VideoHost: Send + Sync + 'static {
    /// Non-blocking: queues the command for execution on the main thread.
    fn post(&self, cmd: VideoCommand);
    /// Registers the event sink (called once when the player is built).
    fn set_event_sink(&self, sink: EventSink);
    /// Whether the video surface currently covers the whole monitor.
    fn is_fullscreen(&self) -> bool {
        false
    }
}
