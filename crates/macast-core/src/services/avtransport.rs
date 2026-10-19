//! AVTransport:1 — the service phones use to push media and control playback.

use std::sync::Arc;

use crate::core::Engine;
use crate::soap::{parse_didl_title, SoapCall, SoapFault};
use crate::state::{position_vars, TransportState};
use crate::util::{format_time, parse_time};

const NO_MEDIA_DURATION: &str = "0:00:00";

pub async fn handle(
    engine: &Arc<Engine>,
    call: SoapCall,
) -> Result<Vec<(String, String)>, SoapFault> {
    match call.action.as_str() {
        "SetAVTransportURI" => set_av_transport_uri(engine, &call).await,
        "SetNextAVTransportURI" => {
            let uri = call.args.get("NextURI").cloned().unwrap_or_default();
            let meta = call.args.get("NextURIMetaData").cloned().unwrap_or_default();
            let mut st = engine.state.write().await;
            let changes = st.set_vars(
                "AVTransport",
                &[
                    ("NextAVTransportURI", uri),
                    ("NextAVTransportURIMetaData", meta),
                ],
            );
            drop(st);
            engine.apply_changes("AVTransport", changes).await;
            Ok(vec![])
        }
        "GetMediaInfo" => get_media_info(engine).await,
        "GetTransportInfo" => {
            let st = engine.state.read().await;
            Ok(vec![
                ("CurrentTransportState".into(), st.get("TransportState")),
                ("CurrentTransportStatus".into(), st.get("TransportStatus")),
                (
                    "CurrentSpeed".into(),
                    st.get("TransportPlaySpeed"),
                ),
            ])
        }
        "GetPositionInfo" => get_position_info(engine).await,
        "GetDeviceCapabilities" => Ok(vec![
            ("PlayMedia".into(), "NETWORK,UNKNOWN".into()),
            ("RecMedia".into(), "NONE".into()),
            ("RecQualityModes".into(), "NONE".into()),
        ]),
        "GetTransportSettings" => {
            let st = engine.state.read().await;
            Ok(vec![
                ("PlayMode".into(), st.get("CurrentPlayMode")),
                ("RecQualityMode".into(), "NOT_IMPLEMENTED".into()),
            ])
        }
        "GetCurrentTransportActions" => Ok(vec![(
            "Actions".into(),
            "Play,Pause,Stop,Seek,Next,Previous".into(),
        )]),
        "Play" => {
            engine.player.resume().await;
            let mut st = engine.state.write().await;
            let changes =
                st.set_vars("AVTransport", &[("TransportState", "PLAYING".into()), ("TransportStatus", "OK".into())]);
            drop(st);
            engine.apply_changes("AVTransport", changes).await;
            engine
                .update_snapshot(|s| s.transport_state = TransportState::Playing)
                .await;
            Ok(vec![])
        }
        "Pause" => {
            engine.player.pause().await;
            let mut st = engine.state.write().await;
            let changes = st.set_vars(
                "AVTransport",
                &[("TransportState", "PAUSED_PLAYBACK".into()), ("TransportStatus", "OK".into())],
            );
            drop(st);
            engine.apply_changes("AVTransport", changes).await;
            engine
                .update_snapshot(|s| s.transport_state = TransportState::PausedPlayback)
                .await;
            Ok(vec![])
        }
        "Stop" => {
            engine.player.stop().await;
            let mut st = engine.state.write().await;
            let changes =
                st.set_vars("AVTransport", &[("TransportState", "STOPPED".into())]);
            drop(st);
            engine.apply_changes("AVTransport", changes).await;
            engine
                .update_snapshot(|s| s.transport_state = TransportState::Stopped)
                .await;
            Ok(vec![])
        }
        "Seek" => {
            let unit = call.args.get("Unit").cloned().unwrap_or_else(|| "REL_TIME".into());
            let target = call.args.get("Target").cloned().unwrap_or_default();
            let secs = parse_time(&target);
            // Only relative-time seeking is meaningful for network streams.
            let _ = unit;
            engine.player.seek(secs).await;
            let vars = position_vars(secs);
            let mut st = engine.state.write().await;
            let changes = st.set_vars("AVTransport", &vars);
            drop(st);
            engine.apply_changes("AVTransport", changes).await;
            engine.update_snapshot(|s| s.position = secs).await;
            Ok(vec![])
        }
        other => {
            tracing::debug!("AVTransport: unimplemented action '{other}'");
            Err(SoapFault::invalid_action())
        }
    }
}

async fn set_av_transport_uri(
    engine: &Arc<Engine>,
    call: &SoapCall,
) -> Result<Vec<(String, String)>, SoapFault> {
    let uri = call
        .args
        .get("CurrentURI")
        .cloned()
        .ok_or_else(SoapFault::invalid_args)?;
    if uri.trim().is_empty() {
        return Err(SoapFault::invalid_args());
    }
    let metadata = call
        .args
        .get("CurrentURIMetaData")
        .cloned()
        .unwrap_or_default();
    // Phones usually embed the title in DIDL-Lite metadata; fall back to the URI.
    let title = parse_didl_title(&metadata)
        .or_else(|| uri.rsplit('/').next().map(|s| s.to_string()))
        .unwrap_or_else(|| uri.clone());

    engine
        .player
        .set_media_url(&uri, 0.0, &title)
        .await;

    let mut updates: Vec<(&str, String)> = vec![
        ("CurrentTrackURI", uri.clone()),
        ("CurrentTrackMetaData", metadata.clone()),
        ("CurrentTrackTitle", title.clone()),
        // The phone will typically send Play() right after; mpv usually
        // starts auto-playing, which updates this via player events.
        ("TransportState", "PAUSED_PLAYBACK".into()),
        ("TransportStatus", "OK".into()),
        ("CurrentTrack", "1".into()),
        ("NumberOfTracks", "1".into()),
    ];
    updates.extend(position_vars(0.0));
    let mut st = engine.state.write().await;
    let changes = st.set_vars("AVTransport", &updates);
    drop(st);
    tracing::info!("SetAVTransportURI: {uri}");
    engine.apply_changes("AVTransport", changes).await;
    engine
        .update_snapshot(|s| {
            s.uri = uri;
            s.metadata = metadata;
            s.title = title;
            s.position = 0.0;
            s.duration = 0.0;
            s.transport_state = TransportState::PausedPlayback;
        })
        .await;

    Ok(vec![])
}

async fn get_media_info(engine: &Arc<Engine>) -> Result<Vec<(String, String)>, SoapFault> {
    let st = engine.state.read().await;
    let uri = st.get("CurrentTrackURI");
    let duration = if uri.is_empty() {
        NO_MEDIA_DURATION.to_string()
    } else {
        st.get("CurrentMediaDuration")
    };
    let play_medium = if uri.is_empty() { "NONE" } else { "NETWORK" };
    Ok(vec![
        ("NrTracks".into(), "1".into()),
        ("MediaDuration".into(), duration),
        ("CurrentURI".into(), uri),
        ("CurrentURIMetaData".into(), st.get("CurrentTrackMetaData")),
        ("NextURI".into(), "NOT_IMPLEMENTED".into()),
        ("NextURIMetaData".into(), "NOT_IMPLEMENTED".into()),
        ("PlayMedium".into(), play_medium.into()),
        ("RecordMedium".into(), "NONE".into()),
        ("WriteStatus".into(), "NOT_WRITABLE".into()),
    ])
}

async fn get_position_info(engine: &Arc<Engine>) -> Result<Vec<(String, String)>, SoapFault> {
    let st = engine.state.read().await;
    let uri = st.get("CurrentTrackURI");
    let duration = if uri.is_empty() {
        NO_MEDIA_DURATION.to_string()
    } else {
        st.get("CurrentMediaDuration")
    };
    let position = if uri.is_empty() {
        NO_MEDIA_DURATION.to_string()
    } else {
        st.get("RelativeTimePosition")
    };
    Ok(vec![
        ("Track".into(), "1".into()),
        ("TrackDuration".into(), duration),
        ("TrackMetaData".into(), st.get("CurrentTrackMetaData")),
        ("TrackURI".into(), uri),
        ("RelTime".into(), position.clone()),
        ("AbsTime".into(), position),
        ("RelCount".into(), "2147483647".into()),
        ("AbsCount".into(), "2147483647".into()),
    ])
}

/// Format a duration in seconds the way AVTransport reports it.
pub fn format_duration(secs: f64) -> String {
    format_time(secs)
}
