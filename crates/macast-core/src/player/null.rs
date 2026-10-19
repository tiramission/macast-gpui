//! Null player: no media output, but it simulates playback so the DLNA stack
//! can be exercised end-to-end without mpv installed (CI, protocol testing).

use super::{Player, PlayerEvent};
use async_trait::async_trait;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

pub struct NullPlayer {
    events: broadcast::Sender<PlayerEvent>,
    started: Mutex<Option<Instant>>,
    ticker: Mutex<Option<JoinHandle<()>>>,
}

impl NullPlayer {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(64);
        NullPlayer {
            events,
            started: Mutex::new(None),
            ticker: Mutex::new(None),
        }
    }

    fn stop_ticker(&self) {
        if let Some(h) = self.ticker.lock().unwrap().take() {
            h.abort();
        }
    }
}

impl Default for NullPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Player for NullPlayer {
    async fn set_media_url(&self, url: &str, _start_secs: f64, title: &str) {
        tracing::info!("[null-player] would play '{title}' from {url}");
        self.stop_ticker();
        *self.started.lock().unwrap() = Some(Instant::now());
        let _ = self.events.send(PlayerEvent::Playing);
        let _ = self.events.send(PlayerEvent::Loaded { duration: 0.0 });

        // simulate a 1 Hz position ticker
        let tx = self.events.clone();
        let start = Instant::now();
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let _ = tx.send(PlayerEvent::Position(start.elapsed().as_secs_f64()));
            }
        });
        *self.ticker.lock().unwrap() = Some(handle);
    }

    async fn resume(&self) {
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn pause(&self) {
        let _ = self.events.send(PlayerEvent::Paused(true));
    }

    async fn stop(&self) {
        self.stop_ticker();
        let _ = self.events.send(PlayerEvent::Stopped);
    }

    async fn seek(&self, secs: f64) {
        let secs = secs.max(0.0);
        *self.started.lock().unwrap() =
            Some(Instant::now() - std::time::Duration::from_secs_f64(secs));
        let _ = self.events.send(PlayerEvent::Position(secs));
    }

    async fn set_volume(&self, volume: u8) {
        let _ = self.events.send(PlayerEvent::Volume(volume.min(100) as f64));
    }

    async fn set_mute(&self, mute: bool) {
        let _ = self.events.send(PlayerEvent::Mute(mute));
    }

    async fn set_speed(&self, speed: f64) {
        let _ = self.events.send(PlayerEvent::Speed(speed));
    }

    async fn add_subtitle(&self, url: &str, title: &str) {
        tracing::info!("[null-player] would add subtitle {title} from {url}");
    }

    async fn set_subtitle_visible(&self, visible: bool) {
        tracing::debug!("[null-player] subtitle visible = {visible}");
    }

    async fn show_text(&self, text: &str, duration_ms: u64) {
        tracing::info!("[null-player] would show text '{text}' for {duration_ms}ms");
    }

    async fn shutdown(&self) {
        self.stop_ticker();
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    fn name(&self) -> &'static str {
        "null"
    }
}
