//! GENA event subscription registry (UPnP "eventing").
//!
//! Control points `SUBSCRIBE` to `/<Service>/event` with a callback URL; we
//! reply with a SID and then push `NOTIFY` requests whenever an evented state
//! variable changes. AVTransport/RenderingControl changes are wrapped in the
//! `LastChange` XML document; ConnectionManager uses plain properties.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::util::xml_escape;

#[derive(Debug, Clone)]
pub struct Subscriber {
    pub callback: String,
    pub service: String,
    pub sid: String,
    pub timeout: u32,
    pub expires_at: Instant,
    pub seq: u32,
}

pub struct EventRegistry {
    subs: Mutex<HashMap<String, Subscriber>>,
    client: reqwest::Client,
    server: String,
}

impl EventRegistry {
    pub fn new(server: impl Into<String>) -> Self {
        EventRegistry {
            subs: Mutex::new(HashMap::new()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            server: server.into(),
        }
    }

    /// Register a new subscription (or refresh an identical existing one).
    /// Returns `(SID, timeout)`.
    pub fn subscribe(&self, service: &str, callback: &str, timeout: u32) -> (String, u32) {
        let mut subs = self.subs.lock().unwrap();
        // A control point re-subscribing with the same callback gets the old SID.
        if let Some(existing) = subs
            .values_mut()
            .find(|s| s.service == service && s.callback == callback)
        {
            existing.expires_at = Instant::now() + Duration::from_secs(timeout as u64);
            existing.timeout = timeout;
            return (existing.sid.clone(), timeout);
        }
        let sid = format!("uuid:{}", Uuid::new_v4());
        subs.insert(
            sid.clone(),
            Subscriber {
                callback: callback.to_string(),
                service: service.to_string(),
                sid: sid.clone(),
                timeout,
                expires_at: Instant::now() + Duration::from_secs(timeout as u64),
                seq: 0,
            },
        );
        tracing::info!("GENA subscribe: {service} -> {callback} (sid {sid})");
        (sid, timeout)
    }

    pub fn renew(&self, sid: &str, timeout: u32) -> bool {
        let mut subs = self.subs.lock().unwrap();
        if let Some(s) = subs.get_mut(sid) {
            s.expires_at = Instant::now() + Duration::from_secs(timeout as u64);
            s.timeout = timeout;
            true
        } else {
            false
        }
    }

    pub fn remove(&self, sid: &str) -> bool {
        let removed = self.subs.lock().unwrap().remove(sid).is_some();
        if removed {
            tracing::info!("GENA unsubscribe: {sid}");
        }
        removed
    }

    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().unwrap().len()
    }

    /// Send the initial full state of a service to a freshly subscribed client.
    pub async fn send_initial(&self, service: &str, sid: &str, vars: Vec<(String, String)>) {
        let sub = {
            let subs = self.subs.lock().unwrap();
            subs.get(sid).cloned()
        };
        if let Some(sub) = sub {
            let body = self.build_propertyset(service, &vars);
            self.deliver(sid, &sub, &body).await;
        }
    }

    /// Push a state change to every subscriber of `service`.
    pub async fn notify_change(&self, service: &str, changes: &[(String, String)]) {
        if changes.is_empty() {
            return;
        }
        let targets: Vec<Subscriber> = {
            let subs = self.subs.lock().unwrap();
            subs.values()
                .filter(|s| s.service == service && s.expires_at > Instant::now())
                .cloned()
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let body = self.build_propertyset(service, changes);
        for sub in &targets {
            if !self.deliver(&sub.sid, sub, &body).await {
                // delivery failed -> drop the subscriber; a well-behaved
                // control point re-subscribes on its next event cycle.
                let mut subs = self.subs.lock().unwrap();
                subs.remove(&sub.sid);
            }
        }
    }

    /// POST a GENA NOTIFY. Returns false on failure.
    ///
    /// `SEQ` must increment for every message of a subscription (starting at 0
    /// for the initial push), so the counter is read from — and bumped in —
    /// the registry atomically. A failed delivery still consumes its SEQ: that
    /// is exactly how a control point detects a missed event.
    async fn deliver(&self, sid: &str, sub: &Subscriber, body: &str) -> bool {
        let seq = {
            let mut subs = self.subs.lock().unwrap();
            match subs.get_mut(sid) {
                Some(s) => {
                    let q = s.seq;
                    s.seq = q.wrapping_add(1);
                    q
                }
                None => sub.seq,
            }
        };
        let method = match reqwest::Method::from_bytes(b"NOTIFY") {
            Ok(m) => m,
            Err(_) => return false,
        };
        let res = self
            .client
            .request(method, &sub.callback)
            .header("NT", "upnp:event")
            .header("NTS", "upnp:propchange")
            .header("CONTENT-TYPE", "text/xml; charset=\"utf-8\"")
            .header("SERVER", &self.server)
            .header("SID", &sub.sid)
            .header("SEQ", seq.to_string())
            .header("TIMEOUT", format!("Second-{}", sub.timeout))
            .body(body.to_string())
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!("GENA NOTIFY to {} failed: HTTP {}", sub.callback, r.status());
                false
            }
            Err(e) => {
                tracing::warn!("GENA NOTIFY to {} failed: {e}", sub.callback);
                false
            }
        }
    }

    fn build_propertyset(&self, service: &str, changes: &[(String, String)]) -> String {
        let ns = "urn:schemas-upnp-org:event-1-0";
        if service == "ConnectionManager" {
            // ConnectionManager events use plain state variable properties.
            let mut props = String::new();
            for (k, v) in changes {
                props.push_str(&format!("<{k}>{}</{k}>", xml_escape(v)));
            }
            return format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<e:propertyset xmlns:e="{ns}">{props}</e:propertyset>"#
            );
        }

        // AVTransport / RenderingControl wrap everything in <LastChange>.
        let avt_ns = if service == "AVTransport" {
            "urn:schemas-upnp-org:metadata-1-0/AVT/"
        } else {
            "urn:schemas-upnp-org:metadata-1-0/RCS/"
        };
        let mut vars = String::new();
        for (k, v) in changes {
            vars.push_str(&format!("<{k} val=\"{}\"/>", xml_escape(v)));
        }
        let event = format!(
            "<Event xmlns=\"{avt_ns}\"><InstanceID val=\"0\">{vars}</InstanceID></Event>"
        );
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<e:propertyset xmlns:e="{ns}"><e:property><LastChange>{}</LastChange></e:property></e:propertyset>"#,
            xml_escape(&event)
        )
    }
}
