//! The engine: wires config, state, SSDP, HTTP, eventing and the player together.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::config::{Config, PlayerKind};
use crate::device::{description_xml, sink_protocol_info, SERVER_STRING};
use crate::eventing::EventRegistry;
use crate::player::{self, Player, PlayerEvent};
use crate::ssdp::SsdpServer;
use crate::state::{MediaKind, PlaybackSnapshot, RendererState, SharedState, TransportState};
use crate::util::{format_time, parse_time};

/// Shared engine state, handed to HTTP handlers and background tasks.
pub struct Engine {
    pub config: Config,
    pub state: SharedState,
    pub player: Arc<dyn Player>,
    pub events: Arc<EventRegistry>,
    pub snapshot_tx: watch::Sender<PlaybackSnapshot>,
    actual_port: AtomicU16,
    shutdown_tx: watch::Sender<bool>,
}

impl Engine {
    pub fn actual_port(&self) -> u16 {
        self.actual_port.load(Ordering::SeqCst)
    }

    pub fn set_actual_port(&self, port: u16) {
        self.actual_port.store(port, Ordering::SeqCst);
    }

    pub fn subscribe(&self) -> watch::Receiver<PlaybackSnapshot> {
        self.snapshot_tx.subscribe()
    }

    pub async fn update_snapshot(&self, f: impl FnOnce(&mut PlaybackSnapshot)) {
        self.snapshot_tx.send_modify(f);
    }

    /// Push evented state changes to GENA subscribers of `service`.
    pub async fn apply_changes(&self, service: &str, changes: Vec<(String, String)>) {
        if !changes.is_empty() {
            self.events.notify_change(service, &changes).await;
        }
    }

    /// Initial values sent to a new GENA subscriber of `service`.
    pub fn initial_event_vars(&self, service: &str) -> Vec<(String, String)> {
        // Note: uses try_read to avoid async in handler paths that hold no lock.
        let st = match self.state.try_read() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        let names: &[&str] = match service {
            "AVTransport" => &[
                "TransportState",
                "TransportStatus",
                "CurrentMediaDuration",
                "CurrentTrackDuration",
                "CurrentTrack",
                "NumberOfTracks",
            ],
            "RenderingControl" => &["Volume", "Mute"],
            "ConnectionManager" => &[
                "A_ARG_TYPE_Direction",
                "SinkProtocolInfo",
                "CurrentConnectionIDs",
            ],
            _ => return Vec::new(),
        };
        names.iter().map(|n| (n.to_string(), st.get(n))).collect()
    }

    pub fn description_xml(&self) -> String {
        description_xml(&self.config.device.udn, &self.config.device.friendly_name)
    }

    /// Media kind of the current cast, as recorded by `SetAVTransportURI`.
    pub async fn current_media_kind(&self) -> MediaKind {
        MediaKind::parse_state(&self.state.read().await.get("CurrentMediaKind"))
    }

    /// Fetch the bytes behind a media URI.
    ///
    /// Player backends read their input themselves; the UI needs this for still
    /// pictures, which it decodes and draws on its own. Supports `file://` and
    /// `http(s)://`, times out after 15s and refuses absurdly large payloads.
    pub async fn fetch_bytes(&self, uri: &str) -> anyhow::Result<Vec<u8>> {
        const MAX_BYTES: u64 = 64 * 1024 * 1024;
        if let Some(path) = crate::util::uri_to_local_path(uri) {
            let len = tokio::fs::metadata(&path).await?.len();
            anyhow::ensure!(len <= MAX_BYTES, "file too large ({len} bytes)");
            return Ok(tokio::fs::read(path).await?);
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        let response = client.get(uri).send().await?.error_for_status()?;
        anyhow::ensure!(
            response.content_length().unwrap_or(0) <= MAX_BYTES,
            "response too large"
        );
        let bytes = response.bytes().await?;
        anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "response too large");
        Ok(bytes.to_vec())
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        self.player.shutdown().await;
    }
}

/// The public entry point of the crate.
pub struct MacastCore {
    pub engine: Arc<Engine>,
    handle: tokio::runtime::Handle,
    http_task: JoinHandle<()>,
    bridge_task: JoinHandle<()>,
}

impl MacastCore {
    /// Bind the HTTP port, start SSDP, spawn the player and the event bridge.
    pub async fn start(
        config: Config,
        video_host: Option<Arc<dyn player::video_host::VideoHost>>,
    ) -> anyhow::Result<Arc<Self>> {
        let player = player::build_player(&config.player, video_host).await;
        let events = Arc::new(EventRegistry::new(SERVER_STRING));
        let (snapshot_tx, _) = watch::channel(PlaybackSnapshot {
            friendly_name: config.device.friendly_name.clone(),
            port: config.device.port,
            ..Default::default()
        });
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let state: SharedState = Arc::new(tokio::sync::RwLock::new(RendererState::new(
            &sink_protocol_info(),
        )));

        let engine = Arc::new(Engine {
            config,
            state,
            player,
            events,
            snapshot_tx,
            actual_port: AtomicU16::new(0),
            shutdown_tx,
        });

        // ---- HTTP server -------------------------------------------------
        let listener =
            match tokio::net::TcpListener::bind(("0.0.0.0", engine.config.device.port)).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(
                        "cannot bind port {} ({e}); falling back to an ephemeral port",
                        engine.config.device.port
                    );
                    tokio::net::TcpListener::bind(("0.0.0.0", 0)).await?
                }
            };
        let port = listener.local_addr()?.port();
        engine.set_actual_port(port);
        engine.update_snapshot(|s| s.port = port).await;
        tracing::info!("HTTP server listening on 0.0.0.0:{port}");

        let router = crate::http::router(engine.clone());
        let http_task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!("HTTP server error: {e}");
            }
        });

        // ---- SSDP ---------------------------------------------------------
        let ssdp = SsdpServer::new(engine.clone(), shutdown_rx);
        ssdp.start().await?;

        // ---- player events -> state + GENA --------------------------------
        let bridge_task = tokio::spawn(bridge_loop(engine.clone()));

        Ok(Arc::new(MacastCore {
            engine,
            handle: tokio::runtime::Handle::current(),
            http_task,
            bridge_task,
        }))
    }

    /// Handle of the runtime the core runs on; UI layers use this to spawn
    /// core calls from synchronous event handlers.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<PlaybackSnapshot> {
        self.engine.subscribe()
    }

    // ---- local (UI) control entry points --------------------------------
    // These go through the exact same code path as remote DLNA control, so
    // local buttons and phone controls can never disagree about state.

    pub async fn play(&self) {
        let call = crate::soap::SoapCall {
            service: "AVTransport".into(),
            action: "Play".into(),
            args: Default::default(),
        };
        let _ = crate::services::avtransport::handle(&self.engine, call).await;
    }

    pub async fn pause(&self) {
        let call = crate::soap::SoapCall {
            service: "AVTransport".into(),
            action: "Pause".into(),
            args: Default::default(),
        };
        let _ = crate::services::avtransport::handle(&self.engine, call).await;
    }

    pub async fn stop(&self) {
        let call = crate::soap::SoapCall {
            service: "AVTransport".into(),
            action: "Stop".into(),
            args: Default::default(),
        };
        let _ = crate::services::avtransport::handle(&self.engine, call).await;
    }

    pub async fn seek(&self, secs: f64) {
        let mut args = std::collections::HashMap::new();
        args.insert("Unit".to_string(), "REL_TIME".to_string());
        args.insert("Target".to_string(), crate::util::format_time(secs));
        let call = crate::soap::SoapCall {
            service: "AVTransport".into(),
            action: "Seek".into(),
            args,
        };
        let _ = crate::services::avtransport::handle(&self.engine, call).await;
    }

    /// Play a URI locally (drag & drop, "open with", tests).
    ///
    /// This is the same path a phone uses: it builds DIDL-Lite metadata so the
    /// UI title, GENA `CurrentTrackMetaData` and mpv's media title all agree.
    pub async fn play_uri(&self, uri: impl Into<String>, title: Option<String>) {
        let uri = uri.into();
        let title = title.unwrap_or_else(|| {
            uri.rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(uri.as_str())
                .to_string()
        });
        let metadata = build_didl_metadata(&uri, &title);
        let mut args = std::collections::HashMap::new();
        args.insert("InstanceID".to_string(), "0".to_string());
        args.insert("CurrentURI".to_string(), uri);
        args.insert("CurrentURIMetaData".to_string(), metadata);
        let call = crate::soap::SoapCall {
            service: "AVTransport".into(),
            action: "SetAVTransportURI".into(),
            args,
        };
        let _ = crate::services::avtransport::handle(&self.engine, call).await;
        self.play().await;
    }

    pub async fn set_volume(&self, volume: u8) {
        let mut args = std::collections::HashMap::new();
        args.insert("DesiredVolume".to_string(), volume.to_string());
        let call = crate::soap::SoapCall {
            service: "RenderingControl".into(),
            action: "SetVolume".into(),
            args,
        };
        let _ = crate::services::rendering_control::handle(&self.engine, call).await;
    }

    pub async fn set_mute(&self, mute: bool) {
        let mut args = std::collections::HashMap::new();
        args.insert(
            "DesiredMute".to_string(),
            if mute { "1" } else { "0" }.to_string(),
        );
        let call = crate::soap::SoapCall {
            service: "RenderingControl".into(),
            action: "SetMute".into(),
            args,
        };
        let _ = crate::services::rendering_control::handle(&self.engine, call).await;
    }

    /// The URL currently being played (for the "copy link" button).
    pub async fn current_uri(&self) -> String {
        self.engine.state.read().await.get("CurrentTrackURI")
    }

    /// Fetch the bytes behind a media URI (see [`Engine::fetch_bytes`]).
    pub async fn fetch_bytes(&self, uri: &str) -> anyhow::Result<Vec<u8>> {
        self.engine.fetch_bytes(uri).await
    }

    pub async fn shutdown(&self) {
        tracing::info!("shutting down MacastCore");
        self.engine.shutdown().await;
        self.http_task.abort();
        self.bridge_task.abort();
    }
}

/// Translate player events into UPnP state variables, GENA notifications and
/// UI snapshot updates.
async fn bridge_loop(engine: Arc<Engine>) {
    let mut rx = engine.player.events();
    loop {
        match rx.recv().await {
            Ok(event) => {
                // Tearing the previous media down while a picture cast arrives
                // makes the player emit `Stopped`/`Ended`. Those must not clobber
                // the state of the cast that just landed - the AVTransport
                // handlers set TransportState themselves.
                if matches!(event, PlayerEvent::Stopped | PlayerEvent::Ended)
                    && engine.current_media_kind().await == MediaKind::Image
                {
                    tracing::debug!("ignoring {event:?} during a picture cast");
                    continue;
                }
                let snap_event = event.clone();
                let mut changes_avt = Vec::new();
                let mut changes_rc = Vec::new();
                {
                    let mut st = engine.state.write().await;
                    match event {
                        PlayerEvent::Position(t) => {
                            let vars = crate::state::position_vars(t);
                            changes_avt = st.set_vars("AVTransport", &vars);
                        }
                        PlayerEvent::Duration(d) => {
                            let t = format_time(d);
                            changes_avt = st.set_vars(
                                "AVTransport",
                                &[
                                    ("CurrentTrackDuration", t.clone()),
                                    ("CurrentMediaDuration", t),
                                ],
                            );
                        }
                        PlayerEvent::Loaded { duration } => {
                            let t = format_time(duration);
                            changes_avt = st.set_vars(
                                "AVTransport",
                                &[
                                    ("CurrentTrackDuration", t.clone()),
                                    ("CurrentMediaDuration", t),
                                ],
                            );
                        }
                        PlayerEvent::Paused(true) => {
                            changes_avt = st.set_vars(
                                "AVTransport",
                                &[("TransportState", "PAUSED_PLAYBACK".into())],
                            );
                        }
                        PlayerEvent::Paused(false) | PlayerEvent::Playing => {
                            changes_avt =
                                st.set_vars("AVTransport", &[("TransportState", "PLAYING".into())]);
                        }
                        PlayerEvent::Stopped => {
                            changes_avt =
                                st.set_vars("AVTransport", &[("TransportState", "STOPPED".into())]);
                        }
                        PlayerEvent::Ended => {
                            changes_avt = st.set_vars(
                                "AVTransport",
                                &[("TransportState", "NO_MEDIA_PRESENT".into())],
                            );
                        }
                        PlayerEvent::Volume(v) => {
                            let v = v.round().clamp(0.0, 100.0) as u8;
                            changes_rc =
                                st.set_vars("RenderingControl", &[("Volume", v.to_string())]);
                        }
                        PlayerEvent::Mute(m) => {
                            let value = if m { "1" } else { "0" };
                            changes_rc = st.set_vars("RenderingControl", &[("Mute", value.into())]);
                        }
                        PlayerEvent::Speed(s) => {
                            changes_avt = st
                                .set_vars("AVTransport", &[("TransportPlaySpeed", format!("{s}"))]);
                        }
                        PlayerEvent::Error(msg) => {
                            tracing::warn!("player error: {msg}");
                            changes_avt = st.set_vars(
                                "AVTransport",
                                &[
                                    ("TransportState", "STOPPED".into()),
                                    ("TransportStatus", "ERROR_OCCURRED".into()),
                                ],
                            );
                        }
                    }
                }
                // snapshot
                match snap_event {
                    PlayerEvent::Position(t) => {
                        engine.update_snapshot(|s| s.position = t).await;
                    }
                    PlayerEvent::Duration(d) | PlayerEvent::Loaded { duration: d } => {
                        engine.update_snapshot(|s| s.duration = d).await;
                    }
                    PlayerEvent::Paused(true) => {
                        engine
                            .update_snapshot(|s| s.transport_state = TransportState::PausedPlayback)
                            .await;
                    }
                    PlayerEvent::Paused(false) | PlayerEvent::Playing => {
                        engine
                            .update_snapshot(|s| s.transport_state = TransportState::Playing)
                            .await;
                    }
                    PlayerEvent::Stopped => {
                        engine
                            .update_snapshot(|s| s.transport_state = TransportState::Stopped)
                            .await;
                    }
                    PlayerEvent::Ended => {
                        engine
                            .update_snapshot(|s| s.transport_state = TransportState::NoMediaPresent)
                            .await;
                    }
                    PlayerEvent::Volume(v) => {
                        let v = v.round().clamp(0.0, 100.0) as u8;
                        engine.update_snapshot(|s| s.volume = v).await;
                    }
                    PlayerEvent::Mute(m) => {
                        engine.update_snapshot(|s| s.mute = m).await;
                    }
                    PlayerEvent::Speed(s) => {
                        engine.update_snapshot(|snap| snap.speed = s).await;
                    }
                    PlayerEvent::Error(_) => {}
                }
                engine.apply_changes("AVTransport", changes_avt).await;
                engine.apply_changes("RenderingControl", changes_rc).await;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!("player event lagged by {n}");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Parse a `TIMEOUT: Second-1800` header value.
pub fn parse_timeout_header(value: Option<&str>) -> u32 {
    value
        .and_then(|v| v.trim().split('-').next_back())
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1800)
        .clamp(30, 86400)
}

/// Seconds helper for tests/examples.
pub fn secs_from_time_str(s: &str) -> f64 {
    parse_time(s)
}

/// Exposed for the HTTP layer's debug endpoint.
pub fn position_str(secs: f64) -> String {
    format_time(secs)
}

/// Used by `macast-app` to decide the default player kind on first run.
pub fn default_player_kind() -> PlayerKind {
    PlayerKind::default()
}

/// Build a minimal DIDL-Lite document for a URI we are about to play.
pub fn build_didl_metadata(uri: &str, title: &str) -> String {
    format!(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"><item id="0" parentID="-1" restricted="1"><dc:title>{}</dc:title><upnp:class>object.item</upnp:class><res protocolInfo="http-get:*:*:*">{}</res></item></DIDL-Lite>"#,
        crate::util::xml_escape(title),
        crate::util::xml_escape(uri),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn didl_metadata_round_trips_title() {
        let meta = build_didl_metadata("file:///C:/a%20b.mp4", "a & b");
        assert_eq!(
            crate::soap::parse_didl_title(&meta).as_deref(),
            Some("a & b")
        );
        assert!(meta.contains("file:///C:/a%20b.mp4"));
    }
}
