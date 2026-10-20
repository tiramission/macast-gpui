//! alhazen backend (the default): forwards commands to a UI-side [`VideoHost`]
//! and mirrors them back as [`PlayerEvent`]s. The actual decoding and in-window
//! rendering are done by alhazen-gpui's `VideoPlayer` entity in the UI layer.
//!
//! This module is deliberately free of Win32 code so the core stays UI- and
//! platform-agnostic; without a host it uses [`NullVideoHost`](super::video_host::NullVideoHost),
//! which accepts commands and drops them.

use super::video_host::{VideoCommand, VideoHost};
use super::{Player, PlayerEvent};
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct AlhazenPlayer {
    events: broadcast::Sender<PlayerEvent>,
    host: Arc<dyn VideoHost>,
    fullscreen: bool,
}

impl AlhazenPlayer {
    pub fn new(host: Option<Arc<dyn VideoHost>>, fullscreen: bool) -> Self {
        let (events, _) = broadcast::channel(256);
        let host: Arc<dyn VideoHost> = match host {
            Some(h) => h,
            None => Arc::new(super::video_host::NullVideoHost),
        };
        host.set_event_sink({
            let events = events.clone();
            Arc::new(move |event| {
                let _ = events.send(event);
            })
        });
        Self {
            events,
            host,
            fullscreen,
        }
    }
}

#[async_trait::async_trait]
impl Player for AlhazenPlayer {
    async fn set_media_url(&self, url: &str, start_secs: f64, title: &str) {
        self.host.post(VideoCommand::Play {
            url: url.to_string(),
            start_secs,
            title: title.to_string(),
            fullscreen: self.fullscreen,
        });
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn resume(&self) {
        self.host.post(VideoCommand::Resume);
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn pause(&self) {
        self.host.post(VideoCommand::Pause);
        let _ = self.events.send(PlayerEvent::Paused(true));
    }

    async fn stop(&self) {
        self.host.post(VideoCommand::Stop);
        let _ = self.events.send(PlayerEvent::Stopped);
    }

    async fn seek(&self, secs: f64) {
        self.host.post(VideoCommand::Seek { secs });
    }

    async fn set_volume(&self, volume: u8) {
        self.host.post(VideoCommand::SetVolume { volume });
        let _ = self.events.send(PlayerEvent::Volume(f64::from(volume)));
    }

    async fn set_mute(&self, mute: bool) {
        self.host.post(VideoCommand::SetMute { mute });
        let _ = self.events.send(PlayerEvent::Mute(mute));
    }

    async fn set_speed(&self, speed: f64) {
        let clamped = clamp_rate(speed);
        self.host.post(VideoCommand::SetSpeed { speed: clamped });
        let _ = self.events.send(PlayerEvent::Speed(clamped));
    }

    async fn add_subtitle(&self, _url: &str, _title: &str) {
        tracing::debug!("[alhazen] subtitles are not supported yet");
    }

    async fn set_subtitle_visible(&self, _visible: bool) {
        tracing::debug!("[alhazen] subtitles are not supported yet");
    }

    async fn show_text(&self, _text: &str, _duration_ms: u64) {
        tracing::debug!("[alhazen] on-screen text is not supported yet");
    }

    async fn shutdown(&self) {
        self.host.post(VideoCommand::Shutdown);
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    fn name(&self) -> &'static str {
        "alhazen"
    }
}

// ---- pure helpers (unit-tested) ------------------------------------------

/// Playback rates are clamped to a sane range (0.0625× – 16×).
pub fn clamp_rate(speed: f64) -> f64 {
    if !speed.is_finite() {
        return 1.0;
    }
    speed.clamp(0.0625, 16.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_rate() {
        assert_eq!(clamp_rate(1.0), 1.0);
        assert_eq!(clamp_rate(0.001), 0.0625);
        assert_eq!(clamp_rate(100.0), 16.0);
        assert_eq!(clamp_rate(f64::NAN), 1.0);
    }
}
