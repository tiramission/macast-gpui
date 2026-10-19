//! SOAP action dispatch, one module per UPnP service.

pub mod avtransport;
pub mod connection_manager;
pub mod rendering_control;

use std::sync::Arc;

use crate::core::Engine;
use crate::soap::{SoapCall, SoapFault};

/// Route a SOAP call to the right service handler.
pub async fn dispatch(
    engine: &Arc<Engine>,
    service: &str,
    call: SoapCall,
) -> Result<Vec<(String, String)>, SoapFault> {
    match service {
        "AVTransport" => avtransport::handle(engine, call).await,
        "RenderingControl" => rendering_control::handle(engine, call).await,
        "ConnectionManager" => connection_manager::handle(engine, call).await,
        other => {
            tracing::warn!("SOAP call for unknown service '{other}'");
            Err(SoapFault::invalid_action())
        }
    }
}
