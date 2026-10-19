//! RenderingControl:1 — volume / mute.

use std::sync::Arc;

use crate::core::Engine;
use crate::soap::{SoapCall, SoapFault};

fn parse_bool(s: &str) -> bool {
    matches!(s.trim(), "1" | "true" | "TRUE" | "True" | "yes")
}

fn parse_volume(s: &str) -> Result<u8, SoapFault> {
    s.trim()
        .parse::<i64>()
        .map(|v| v.clamp(0, 100) as u8)
        .map_err(|_| SoapFault::invalid_args())
}

pub async fn handle(
    engine: &Arc<Engine>,
    call: SoapCall,
) -> Result<Vec<(String, String)>, SoapFault> {
    match call.action.as_str() {
        "GetVolume" => {
            let st = engine.state.read().await;
            Ok(vec![("CurrentVolume".into(), st.get("Volume"))])
        }
        "GetMute" => {
            let st = engine.state.read().await;
            let mute = if st.get_bool("Mute") { "1" } else { "0" };
            Ok(vec![("CurrentMute".into(), mute.into())])
        }
        "SetVolume" => {
            let raw = call
                .args
                .get("DesiredVolume")
                .cloned()
                .ok_or_else(SoapFault::invalid_args)?;
            let volume = parse_volume(&raw)?;
            engine.player.set_volume(volume).await;
            let mut st = engine.state.write().await;
            let changes = st.set_vars("RenderingControl", &[("Volume", volume.to_string())]);
            drop(st);
            engine.apply_changes("RenderingControl", changes).await;
            engine.update_snapshot(|s| s.volume = volume).await;
            Ok(vec![])
        }
        "SetMute" => {
            let raw = call
                .args
                .get("DesiredMute")
                .cloned()
                .ok_or_else(SoapFault::invalid_args)?;
            let mute = parse_bool(&raw);
            engine.player.set_mute(mute).await;
            let mut st = engine.state.write().await;
            let value = if mute { "1" } else { "0" };
            let changes = st.set_vars("RenderingControl", &[("Mute", value.into())]);
            drop(st);
            engine.apply_changes("RenderingControl", changes).await;
            engine.update_snapshot(|s| s.mute = mute).await;
            Ok(vec![])
        }
        other => {
            tracing::debug!("RenderingControl: unimplemented action '{other}'");
            Err(SoapFault::invalid_action())
        }
    }
}
