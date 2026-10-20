//! Bridge between the alhazen backend (this crate) and the UI process
//! (macast-app), which owns the window the video renders into.
//!
//! The core stays UI- and platform-agnostic: [`AlhazenPlayer`](super::alhazen::AlhazenPlayer)
//! only forwards [`VideoCommand`]s to a [`VideoHost`] implementation provided
//! by the app, and receives [`PlayerEvent`]s back through an [`EventSink`].
//!
//! In this app the picture is decoded and rendered by alhazen-gpui's
//! `VideoPlayer` entity, so the host is [`NullVideoHost`] and the window
//! commands are accepted and dropped.

use super::PlayerEvent;
use std::sync::Arc;

/// Commands the alhazen player sends to the UI-side video host.
///
/// The host executes them on the UI main thread; ordering is preserved.
#[derive(Debug, Clone)]
pub enum VideoCommand {
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
}

/// A no-op [`VideoHost`] that accepts commands but does nothing.
///
/// Used when the video rendering is handled entirely by the UI layer
/// (e.g. alhazen-gpui's `VideoPlayer` entity) rather than by a
/// window-managing host.
pub struct NullVideoHost;

impl VideoHost for NullVideoHost {
    fn post(&self, _cmd: VideoCommand) {}
    fn set_event_sink(&self, _sink: EventSink) {}
}
