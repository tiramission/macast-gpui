//! Device description document and SCPD service descriptions.

use crate::state::VERSION;
use crate::util::xml_escape;

pub const SCPD_AVTRANSPORT: &str = include_str!("../assets/AVTransport.xml");
pub const SCPD_RENDERING_CONTROL: &str = include_str!("../assets/RenderingControl.xml");
pub const SCPD_CONNECTION_MANAGER: &str = include_str!("../assets/ConnectionManager.xml");
/// The Sink protocol info table (mime types we accept), one entry per line.
pub const SINK_PROTOCOL_INFO: &str = include_str!("../assets/SinkProtocolInfo.csv");

pub fn sink_protocol_info() -> String {
    SINK_PROTOCOL_INFO.trim().to_string()
}

pub const SERVER_STRING: &str =
    concat!("Linux/5.0 UPnP/1.1 Macast-Gpui/", env!("CARGO_PKG_VERSION"));

/// The three UPnP services we expose, with their control/event URLs
/// (relative, exactly like Macast's description).
pub struct ServiceUrls {
    pub name: &'static str,
    pub control_url: &'static str,
    pub event_sub_url: &'static str,
    pub scpd_url: &'static str,
}

pub const SERVICES: [ServiceUrls; 3] = [
    ServiceUrls {
        name: "AVTransport",
        control_url: "AVTransport/action",
        event_sub_url: "AVTransport/event",
        scpd_url: "dlna/AVTransport.xml",
    },
    ServiceUrls {
        name: "RenderingControl",
        control_url: "RenderingControl/action",
        event_sub_url: "RenderingControl/event",
        scpd_url: "dlna/RenderingControl.xml",
    },
    ServiceUrls {
        name: "ConnectionManager",
        control_url: "ConnectionManager/action",
        event_sub_url: "ConnectionManager/event",
        scpd_url: "dlna/ConnectionManager.xml",
    },
];

/// UPnP service type namespace for a short service name.
pub fn service_namespace(service: &str) -> String {
    format!("urn:schemas-upnp-org:service:{service}:1")
}

/// Build `/description.xml` (the root device description a control point
/// fetches from the LOCATION header advertised via SSDP).
///
/// All URLs in the document are relative, so the same bytes work for every
/// interface/port combination; only the SSDP `LOCATION` header is absolute.
pub fn description_xml(udn: &str, friendly_name: &str) -> String {
    let mut services = String::new();
    for s in SERVICES.iter() {
        services.push_str(&format!(
            r#"        <service>
            <serviceType>urn:schemas-upnp-org:service:{name}:1</serviceType>
            <serviceId>urn:upnp-org:serviceId:{name}</serviceId>
            <controlURL>{control}</controlURL>
            <eventSubURL>{event}</eventSubURL>
            <SCPDURL>{scpd}</SCPDURL>
        </service>
"#,
            name = s.name,
            control = s.control_url,
            event = s.event_sub_url,
            scpd = s.scpd_url,
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<root xmlns:dlna="urn:schemas-dlna-org:device-1-0" xmlns="urn:schemas-upnp-org:device-1-0">
    <specVersion>
        <major>1</major>
        <minor>0</minor>
    </specVersion>
    <device>
        <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
        <UDN>uuid:{udn}</UDN>
        <friendlyName>{friendly}</friendlyName>
        <manufacturer>macast-gpui</manufacturer>
        <manufacturerURL>https://github.com/xfangfang/Macast</manufacturerURL>
        <modelDescription>AVTransport Media Renderer</modelDescription>
        <modelName>Macast-Gpui</modelName>
        <modelNumber>{version}</modelNumber>
        <modelURL>https://github.com/zed-industries/zed</modelURL>
        <serialNumber>1024</serialNumber>
        <dlna:X_DLNADOC xmlns:dlna="urn:schemas-dlna-org:device-1-0">DMR-1.50</dlna:X_DLNADOC>
        <serviceList>
{services}        </serviceList>
    </device>
</root>
"#,
        udn = xml_escape(udn),
        friendly = xml_escape(friendly_name),
        version = VERSION,
        services = services,
    )
}

/// Full LOCATION URL for the description document.
pub fn location_url(ip: &str, port: u16) -> String {
    format!("http://{ip}:{port}/description.xml")
}
