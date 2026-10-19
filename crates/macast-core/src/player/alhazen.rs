//! Built-in backend: forwards commands to the UI-side video host, which plays
//! media through Windows Media Foundation (mfplay).
//!
//! This module is deliberately free of Win32 code so the core stays UI- and
//! platform-agnostic — without a [`VideoHost`] the backend simply cannot be
//! constructed and `build_player` falls back to the null player.

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

/// Seconds → 100-nanosecond units (Media Foundation's time base), clamped to a
/// valid non-negative range.
pub fn secs_to_100ns(secs: f64) -> i64 {
    if !(secs.is_finite() && secs > 0.0) {
        return 0;
    }
    let scaled = secs * 10_000_000.0;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else {
        scaled as i64
    }
}

/// 100-nanosecond units → seconds.
pub fn ns100_to_secs(v: i64) -> f64 {
    (v.max(0) as f64) / 10_000_000.0
}

/// mfplay accepts rates in [1/16, 16].
pub fn clamp_rate(speed: f64) -> f64 {
    if !speed.is_finite() {
        return 1.0;
    }
    speed.clamp(0.0625, 16.0)
}

/// The subset of `MFP_EVENT_TYPE` values the backend reacts to, decoupled from
/// the windows crate so the mapping is testable without COM types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MfpEventKind {
    Play,
    Pause,
    Stop,
    PositionSet,
    RateSet,
    MediaItemSet,
    Error,
    PlaybackEnded,
}

/// Payload extracted from an mfplay event, decoupled from the windows crate.
#[derive(Debug, Clone, Default)]
pub struct MfpEventDetail {
    pub position_secs: f64,
    pub rate: f32,
    pub duration_secs: f64,
    pub has_video: bool,
    pub message: String,
}

/// What to do with a mapped mfplay event: the [`PlayerEvent`]s to emit and
/// whether the video surface should be shown/hidden.
#[derive(Debug, Clone, Default)]
pub struct MfpEventAction {
    pub events: Vec<PlayerEvent>,
    pub show_surface: Option<bool>,
}

pub fn map_mfp_event(kind: MfpEventKind, detail: &MfpEventDetail) -> MfpEventAction {
    let mut action = MfpEventAction::default();
    match kind {
        MfpEventKind::Play => {
            action.events.push(PlayerEvent::Playing);
            action.show_surface = Some(true);
        }
        MfpEventKind::Pause => {
            action.events.push(PlayerEvent::Paused(true));
        }
        MfpEventKind::Stop => {
            action.events.push(PlayerEvent::Stopped);
            action.show_surface = Some(false);
        }
        MfpEventKind::PositionSet => {
            action
                .events
                .push(PlayerEvent::Position(detail.position_secs));
        }
        MfpEventKind::RateSet => {
            action
                .events
                .push(PlayerEvent::Speed(f64::from(detail.rate)));
        }
        MfpEventKind::MediaItemSet => {
            action.events.push(PlayerEvent::Loaded {
                duration: detail.duration_secs,
            });
            action
                .events
                .push(PlayerEvent::Duration(detail.duration_secs));
            action.show_surface = Some(detail.has_video);
        }
        MfpEventKind::Error => {
            action
                .events
                .push(PlayerEvent::Error(detail.message.clone()));
            action.show_surface = Some(false);
        }
        MfpEventKind::PlaybackEnded => {
            action.events.push(PlayerEvent::Ended);
            action.show_surface = Some(false);
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_seconds_to_100ns() {
        assert_eq!(secs_to_100ns(0.0), 0);
        assert_eq!(secs_to_100ns(-1.0), 0);
        assert_eq!(secs_to_100ns(f64::NAN), 0);
        assert_eq!(secs_to_100ns(1.0), 10_000_000);
        assert_eq!(secs_to_100ns(0.5), 5_000_000);
        assert_eq!(secs_to_100ns(f64::MAX), i64::MAX);
    }

    #[test]
    fn converts_100ns_to_seconds() {
        assert_eq!(ns100_to_secs(0), 0.0);
        assert_eq!(ns100_to_secs(-5), 0.0);
        assert_eq!(ns100_to_secs(10_000_000), 1.0);
    }

    #[test]
    fn round_trips_time() {
        for secs in [0.001, 1.0, 12.5, 3600.0] {
            assert!((ns100_to_secs(secs_to_100ns(secs)) - secs).abs() < 1e-7);
        }
    }

    #[test]
    fn clamps_rate() {
        assert_eq!(clamp_rate(1.0), 1.0);
        assert_eq!(clamp_rate(0.001), 0.0625);
        assert_eq!(clamp_rate(100.0), 16.0);
        assert_eq!(clamp_rate(f64::NAN), 1.0);
    }

    #[test]
    fn maps_play_to_playing_and_show() {
        let action = map_mfp_event(MfpEventKind::Play, &MfpEventDetail::default());
        assert_eq!(action.events, vec![PlayerEvent::Playing]);
        assert_eq!(action.show_surface, Some(true));
    }

    #[test]
    fn maps_media_item_set_with_duration_and_visibility() {
        let detail = MfpEventDetail {
            duration_secs: 30.0,
            has_video: true,
            ..Default::default()
        };
        let action = map_mfp_event(MfpEventKind::MediaItemSet, &detail);
        assert_eq!(
            action.events,
            vec![
                PlayerEvent::Loaded { duration: 30.0 },
                PlayerEvent::Duration(30.0)
            ]
        );
        assert_eq!(action.show_surface, Some(true));

        let detail = MfpEventDetail {
            duration_secs: 180.0,
            has_video: false, // audio-only cast keeps the artwork visible
            ..Default::default()
        };
        let action = map_mfp_event(MfpEventKind::MediaItemSet, &detail);
        assert_eq!(action.show_surface, Some(false));
    }

    #[test]
    fn maps_stop_ended_error_to_hidden() {
        for kind in [MfpEventKind::Stop, MfpEventKind::PlaybackEnded] {
            let action = map_mfp_event(kind, &MfpEventDetail::default());
            assert_eq!(action.show_surface, Some(false));
        }
        let detail = MfpEventDetail {
            message: "boom".into(),
            ..Default::default()
        };
        let action = map_mfp_event(MfpEventKind::Error, &detail);
        assert_eq!(action.events, vec![PlayerEvent::Error("boom".into())]);
        assert_eq!(action.show_surface, Some(false));
    }

    #[test]
    fn maps_position_and_rate() {
        let detail = MfpEventDetail {
            position_secs: 12.0,
            ..Default::default()
        };
        let action = map_mfp_event(MfpEventKind::PositionSet, &detail);
        assert_eq!(action.events, vec![PlayerEvent::Position(12.0)]);

        let detail = MfpEventDetail {
            rate: 1.5,
            ..Default::default()
        };
        let action = map_mfp_event(MfpEventKind::RateSet, &detail);
        assert_eq!(action.events, vec![PlayerEvent::Speed(1.5)]);
    }
}
