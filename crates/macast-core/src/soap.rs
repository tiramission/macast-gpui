//! SOAP 1.1 envelope parsing / building for UPnP control messages.

use crate::util::xml_escape;
use std::collections::HashMap;

/// A parsed control request: `<u:Action xmlns:u="urn:...service:AVTransport:1">…</u:Action>`.
#[derive(Debug, Clone)]
pub struct SoapCall {
    /// Short service name, e.g. `AVTransport`.
    pub service: String,
    /// Action name, e.g. `SetAVTransportURI`.
    pub action: String,
    /// In-arguments, keyed by argument name.
    pub args: HashMap<String, String>,
}

/// A UPnP SOAP fault (UPnP Device Architecture error codes).
#[derive(Debug, Clone)]
pub struct SoapFault {
    pub error_code: u32,
    pub error_description: String,
}

impl SoapFault {
    pub fn new(code: u32, desc: impl Into<String>) -> Self {
        SoapFault {
            error_code: code,
            error_description: desc.into(),
        }
    }
    pub fn invalid_action() -> Self {
        SoapFault::new(401, "Invalid Action")
    }
    pub fn invalid_args() -> Self {
        SoapFault::new(402, "Invalid Args")
    }
    pub fn action_failed(desc: impl Into<String>) -> Self {
        SoapFault::new(501, desc)
    }
}

/// Parse a SOAP request body. Returns a fault for anything malformed.
pub fn parse_call(body: &str) -> Result<SoapCall, SoapFault> {
    let doc = roxmltree::Document::parse(body)
        .map_err(|e| SoapFault::new(400, format!("Malformed XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Envelope" {
        return Err(SoapFault::new(400, "Missing SOAP Envelope"));
    }
    let body_el = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "Body")
        .ok_or_else(|| SoapFault::new(400, "Missing SOAP Body"))?;

    let action_el = body_el
        .children()
        .find(|n| n.is_element())
        .ok_or_else(|| SoapFault::new(400, "Missing action element"))?;

    let action = action_el.tag_name().name().to_string();
    let ns = action_el
        .tag_name()
        .namespace()
        .unwrap_or("")
        .to_string();
    // urn:schemas-upnp-org:service:AVTransport:1 -> AVTransport
    let service = ns
        .split(':')
        .nth(3)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SoapFault::new(400, "Unknown service namespace"))?
        .to_string();

    let mut args = HashMap::new();
    for child in action_el.children().filter(|n| n.is_element()) {
        let name = child.tag_name().name().to_string();
        let value = child.text().unwrap_or("").to_string();
        args.insert(name, value);
    }

    Ok(SoapCall { service, action, args })
}

/// Extract the `<dc:title>` from DIDL-Lite metadata (used by SetAVTransportURI).
pub fn parse_didl_title(metadata: &str) -> Option<String> {
    if metadata.trim().is_empty() {
        return None;
    }
    let doc = roxmltree::Document::parse(metadata).ok()?;
    // dc namespace: http://purl.org/dc/elements/1.1/
    doc.descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some("http://purl.org/dc/elements/1.1/")
                && n.tag_name().name() == "title"
        })
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Build a successful action response envelope.
pub fn build_response(service: &str, action: &str, out_args: &[(String, String)]) -> String {
    let ns = crate::device::service_namespace(service);
    let mut args = String::new();
    for (k, v) in out_args {
        args.push_str(&format!("<{k}>{}</{k}>", xml_escape(v)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<u:{action}Response xmlns:u="{ns}">{args}</u:{action}Response>
</s:Body>
</s:Envelope>"#
    )
}

/// Build a SOAP fault envelope. Control points expect HTTP 500 with this body.
pub fn build_fault(fault: &SoapFault) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<s:Fault>
<faultcode>s:Client</faultcode>
<faultstring>UPnPError</faultstring>
<detail>
<UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
<errorCode>{code}</errorCode>
<errorDescription>{desc}</errorDescription>
</UPnPError>
</detail>
</s:Fault>
</s:Body>
</s:Envelope>"#,
        code = fault.error_code,
        desc = xml_escape(&fault.error_description),
    )
}
