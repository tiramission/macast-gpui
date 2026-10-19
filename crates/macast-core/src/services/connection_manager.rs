//! ConnectionManager:1 — protocol negotiation (which mime types we accept).

use std::sync::Arc;

use crate::core::Engine;
use crate::device::sink_protocol_info;
use crate::soap::{SoapCall, SoapFault};

pub async fn handle(
    _engine: &Arc<Engine>,
    call: SoapCall,
) -> Result<Vec<(String, String)>, SoapFault> {
    match call.action.as_str() {
        "GetProtocolInfo" => Ok(vec![
            ("Source".into(), String::new()),
            ("Sink".into(), sink_protocol_info()),
        ]),
        "GetCurrentConnectionIDs" => Ok(vec![("ConnectionIDs".into(), "0".into())]),
        "GetCurrentConnectionInfo" => Ok(vec![
            ("ConnectionID".into(), "0".into()),
            ("RcsID".into(), "0".into()),
            ("AVTransportID".into(), "0".into()),
            ("ProtocolInfo".into(), String::new()),
            ("PeerConnectionManager".into(), String::new()),
            ("PeerConnectionID".into(), "-1".into()),
            ("Direction".into(), "Output".into()),
            ("Status".into(), "OK".into()),
        ]),
        other => {
            tracing::debug!("ConnectionManager: unimplemented action '{other}'");
            Err(SoapFault::invalid_action())
        }
    }
}
