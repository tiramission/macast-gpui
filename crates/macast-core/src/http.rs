//! HTTP layer (axum): device description, SCPD documents, SOAP control endpoint
//! and GENA event subscriptions.

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use std::sync::Arc;

use crate::core::{parse_timeout_header, Engine};
use crate::device;
use crate::soap::{self, SoapFault};
use crate::state::PlaybackSnapshot;

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        // debug / status page
        .route("/", get(status_page))
        // root device description (referenced by SSDP LOCATION)
        .route("/description.xml", get(description))
        // service descriptions (SCPD). axum 0.8 rejects a path segment that
        // mixes a param with a literal suffix ("{service}.xml"), and the SCPD
        // set is fixed anyway — register the three services explicitly.
        .route("/dlna/AVTransport.xml", get(scpd_avtransport))
        .route("/dlna/RenderingControl.xml", get(scpd_rendering_control))
        .route("/dlna/ConnectionManager.xml", get(scpd_connection_manager))
        // SOAP control endpoints
        .route("/{service}/action", any(control))
        // GENA event subscriptions
        .route("/{service}/event", any(event_sub))
        .with_state(engine)
}

/// GET /description.xml
async fn description(State(engine): State<Arc<Engine>>) -> Response {
    let xml = engine.description_xml();
    xml_response(xml)
}

/// GET /dlna/AVTransport.xml etc. — the SCPD documents.
async fn scpd_avtransport() -> Response {
    xml_response(device::SCPD_AVTRANSPORT.to_string())
}

async fn scpd_rendering_control() -> Response {
    xml_response(device::SCPD_RENDERING_CONTROL.to_string())
}

async fn scpd_connection_manager() -> Response {
    xml_response(device::SCPD_CONNECTION_MANAGER.to_string())
}

/// POST /{Service}/action — the SOAP control endpoint.
async fn control(
    State(engine): State<Arc<Engine>>,
    Path(service): Path<String>,
    method: Method,
    body: axum::body::Bytes,
) -> Response {
    if method != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "SOAP control requires POST").into_response();
    }
    let body = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return fault_response(&SoapFault::new(400, "request body is not UTF-8")),
    };
    let call = match soap::parse_call(body) {
        Ok(c) => c,
        Err(f) => return fault_response(&f),
    };
    if !call.service.eq_ignore_ascii_case(&service) {
        // Some control points post to a generic URL; trust the SOAP namespace.
        tracing::debug!(
            "control URL service '{service}' != SOAP namespace service '{}'",
            call.service
        );
    }

    match crate::services::dispatch(&engine, &call.service, call.clone()).await {
        Ok(out_args) => {
            let body = soap::build_response(&call.service, &call.action, &out_args);
            let mut resp = xml_response(body);
            resp.headers_mut().insert(
                header::HeaderName::from_static("ext"),
                header::HeaderValue::from_static(""),
            );
            resp
        }
        Err(f) => fault_response(&f),
    }
}

/// SUBSCRIBE / UNSUBSCRIBE /{Service}/event — GENA eventing.
async fn event_sub(
    State(engine): State<Arc<Engine>>,
    Path(service): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    match method.as_str() {
        "SUBSCRIBE" => {
            let timeout = parse_timeout_header(header_str("TIMEOUT"));
            if let Some(sid) = header_str("SID") {
                // renew
                if engine.events.renew(sid, timeout) {
                    let mut resp = (StatusCode::OK, "").into_response();
                    resp.headers_mut().insert(
                        header::HeaderName::from_static("sid"),
                        header::HeaderValue::from_str(sid).unwrap(),
                    );
                    resp.headers_mut().insert(
                        header::HeaderName::from_static("timeout"),
                        header::HeaderValue::from_str(&format!("Second-{timeout}")).unwrap(),
                    );
                    return resp;
                }
                return (StatusCode::PRECONDITION_FAILED, "no such SID").into_response();
            }
            let callback = match header_str("CALLBACK") {
                Some(c) => c,
                None => {
                    return (StatusCode::PRECONDITION_FAILED, "missing CALLBACK").into_response()
                }
            };
            // CALLBACK: <http://192.168.1.5:1234/cb>
            let url = callback
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            if !url.starts_with("http") {
                return (StatusCode::PRECONDITION_FAILED, "bad CALLBACK").into_response();
            }
            let (sid, timeout) = engine.events.subscribe(&service, &url, timeout);
            // send the initial state immediately
            let vars = engine.initial_event_vars(&service);
            engine.events.send_initial(&service, &sid, vars).await;

            let mut resp = (StatusCode::OK, "").into_response();
            resp.headers_mut().insert(
                header::HeaderName::from_static("sid"),
                header::HeaderValue::from_str(&sid).unwrap(),
            );
            resp.headers_mut().insert(
                header::HeaderName::from_static("timeout"),
                header::HeaderValue::from_str(&format!("Second-{timeout}")).unwrap(),
            );
            resp
        }
        "UNSUBSCRIBE" => match header_str("SID") {
            Some(sid) if engine.events.remove(sid) => (StatusCode::OK, "").into_response(),
            _ => (StatusCode::PRECONDITION_FAILED, "no such SID").into_response(),
        },
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            "expected SUBSCRIBE/UNSUBSCRIBE",
        )
            .into_response(),
    }
}

/// GET / — tiny status page (handy in a browser; the real UI is the gpui app).
async fn status_page(State(engine): State<Arc<Engine>>) -> Response {
    let snap: PlaybackSnapshot = engine.snapshot_tx.borrow().clone();
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "friendly_name": snap.friendly_name,
        "udn": engine.config.udn,
        "port": snap.port,
        "ips": snap.ips,
        "player": engine.player.name(),
        "transport_state": snap.transport_state.as_str(),
        "title": snap.title,
        "uri": snap.uri,
        "position": snap.position,
        "duration": snap.duration,
        "volume": snap.volume,
        "mute": snap.mute,
        "subscribers": engine.events.subscriber_count(),
    }))
    .unwrap_or_default();
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Macast-Gpui</title></head>\
         <body style=\"font-family:system-ui;padding:2rem\">\
         <h2>Macast-Gpui</h2><pre>{json}</pre></body></html>"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn xml_response(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
        body,
    )
        .into_response()
}

fn fault_response(fault: &SoapFault) -> Response {
    let body = soap::build_fault(fault);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
        body,
    )
        .into_response()
}
