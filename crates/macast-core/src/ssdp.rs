//! SSDP server: device discovery (M-SEARCH) + presence announcements (NOTIFY).
//!
//! Faithful to the UPnP Device Architecture v1.1 and to Macast's behaviour:
//! - one receive socket bound to 0.0.0.0:1900 joined to the multicast group on
//!   every non-loopback interface (with SO_REUSEADDR so other SSDP services can
//!   coexist on Windows),
//! - one send socket per interface (IP_MULTICAST_IF + bound to the interface IP)
//!   so responses/announcements leave through the right NIC,
//! - M-SEARCH responses are sent unicast from the interface whose subnet
//!   contains the requester, after a random jitter of 0..MX seconds,
//! - `ssdp:alive` announcements on start (sent twice, per spec) and then every
//!   `ssdp_notify_secs`; `ssdp:byebye` on shutdown.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::Arc;
use std::time::Duration;

use if_addrs::IfAddr;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::core::Engine;
use crate::device::location_url;
use crate::util::http_date_now;

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;

#[derive(Debug, Clone, Copy)]
pub struct Iface {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
}

/// Enumerate usable (non-loopback, non-link-local) IPv4 interfaces.
pub fn interfaces() -> anyhow::Result<Vec<Iface>> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        let (ip, mask) = match &iface.addr {
            IfAddr::V4(v4) => (v4.ip, v4.netmask),
            _ => continue,
        };
        if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
            continue;
        }
        out.push(Iface { ip, mask });
    }
    Ok(out)
}

fn same_subnet(a: Ipv4Addr, mask: Ipv4Addr, b: Ipv4Addr) -> bool {
    let (ao, bo, mo) = (a.octets(), b.octets(), mask.octets());
    (0..4).all(|i| ao[i] & mo[i] == bo[i] & mo[i])
}

fn make_recv_socket(ifaces: &[Iface]) -> anyhow::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Windows: SO_REUSEADDR is required to share port 1900 with other SSDP
    // listeners (e.g. other cast receivers). Harmless elsewhere.
    sock.set_reuse_address(true)?;
    sock.bind(&SocketAddr::from(([0, 0, 0, 0], SSDP_PORT)).into())?;
    for i in ifaces {
        sock.join_multicast_v4(&SSDP_ADDR, &i.ip)?;
    }
    // don't receive our own multicast back
    sock.set_multicast_loop_v4(false)?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(StdUdpSocket::from(sock))?)
}

fn make_send_socket(iface: &Iface) -> anyhow::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // send through this interface
    sock.set_multicast_if_v4(&iface.ip)?;
    // bind to the interface IP so the UDP source address is correct
    sock.bind(&SocketAddr::from((iface.ip, 0u16)).into())?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(StdUdpSocket::from(sock))?)
}

struct SearchReq {
    st: String,
    mx: u32,
}

fn parse_search(data: &[u8]) -> Option<SearchReq> {
    let text = String::from_utf8_lossy(data);
    if !text.starts_with("M-SEARCH") {
        return None;
    }
    let mut st = None;
    let mut mx = None;
    for line in text.split("\r\n").skip(1) {
        let (k, v) = match line.split_once(':') {
            Some(kv) => kv,
            None => continue,
        };
        match k.trim().to_uppercase().as_str() {
            "ST" => st = Some(v.trim().to_string()),
            "MX" => mx = v.trim().parse::<u32>().ok(),
            _ => {}
        }
    }
    let st = st?;
    Some(SearchReq {
        st,
        mx: mx.unwrap_or(1).clamp(1, 5),
    })
}

/// (USN, ST) pairs we advertise. Same set as Macast.
fn usn_pairs(udn: &str) -> Vec<(String, String)> {
    let u = |suffix: &str| format!("uuid:{udn}::{suffix}");
    vec![
        (u("upnp:rootdevice"), "upnp:rootdevice".to_string()),
        (format!("uuid:{udn}"), format!("uuid:{udn}")),
        (
            u("urn:schemas-upnp-org:device:MediaRenderer:1"),
            "urn:schemas-upnp-org:device:MediaRenderer:1".to_string(),
        ),
        (
            u("urn:schemas-upnp-org:service:AVTransport:1"),
            "urn:schemas-upnp-org:service:AVTransport:1".to_string(),
        ),
        (
            u("urn:schemas-upnp-org:service:RenderingControl:1"),
            "urn:schemas-upnp-org:service:RenderingControl:1".to_string(),
        ),
        (
            u("urn:schemas-upnp-org:service:ConnectionManager:1"),
            "urn:schemas-upnp-org:service:ConnectionManager:1".to_string(),
        ),
    ]
}

pub struct SsdpServer {
    engine: Arc<Engine>,
    shutdown: watch::Receiver<bool>,
}

impl SsdpServer {
    pub fn new(engine: Arc<Engine>, shutdown: watch::Receiver<bool>) -> Arc<Self> {
        Arc::new(SsdpServer { engine, shutdown })
    }

    /// Bind sockets and spawn the listener + notifier tasks.
    pub async fn start(self: Arc<Self>) -> anyhow::Result<()> {
        let ifaces = interfaces()?;
        if ifaces.is_empty() {
            anyhow::bail!("SSDP: no usable network interface found");
        }
        let rx = make_recv_socket(&ifaces)?;
        let send_socks: Vec<(Iface, Arc<UdpSocket>)> = {
            let mut v = Vec::new();
            for i in &ifaces {
                match make_send_socket(i) {
                    Ok(s) => v.push((*i, Arc::new(s))),
                    Err(e) => tracing::warn!("SSDP: cannot create send socket on {}: {e}", i.ip),
                }
            }
            v
        };
        tracing::info!(
            "SSDP: listening on {} interface(s): {}",
            ifaces.len(),
            ifaces
                .iter()
                .map(|i| i.ip.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );

        let ifaces_for_engine = ifaces.clone();
        let port = self.engine.actual_port();
        let ips: Vec<String> = ifaces.iter().map(|i| i.ip.to_string()).collect();
        self.engine
            .update_snapshot(|s| {
                s.ips = ips;
                s.port = port;
                s.friendly_name = self.engine.config.device.friendly_name.clone();
            })
            .await;
        let _ = ifaces_for_engine;

        tokio::spawn(Self::listener(
            self.clone(),
            rx,
            send_socks.clone(),
            ifaces.clone(),
        ));
        tokio::spawn(Self::notifier(self.clone(), send_socks));

        Ok(())
    }

    fn search_response(&self, iface: &Iface, st: &str, usn: &str) -> String {
        let port = self.engine.actual_port();
        let loc = location_url(&iface.ip.to_string(), port);
        format!(
            "HTTP/1.1 200 OK\r\n\
             CACHE-CONTROL: max-age={max_age}\r\n\
             DATE: {date}\r\n\
             EXT:\r\n\
             LOCATION: {loc}\r\n\
             SERVER: {server}\r\n\
             ST: {st}\r\n\
             USN: {usn}\r\n\
             \r\n",
            max_age = self.engine.config.network.max_age,
            date = http_date_now(),
            server = crate::device::SERVER_STRING,
        )
    }

    fn notify_msg(&self, iface: &Iface, st: &str, usn: &str, nts: &str) -> String {
        let port = self.engine.actual_port();
        let loc = location_url(&iface.ip.to_string(), port);
        if nts == "ssdp:byebye" {
            format!(
                "NOTIFY * HTTP/1.1\r\n\
                 HOST: {SSDP_ADDR}:{SSDP_PORT}\r\n\
                 NT: {st}\r\n\
                 NTS: {nts}\r\n\
                 SERVER: {server}\r\n\
                 USN: {usn}\r\n\
                 \r\n",
                server = crate::device::SERVER_STRING,
            )
        } else {
            format!(
                "NOTIFY * HTTP/1.1\r\n\
                 HOST: {SSDP_ADDR}:{SSDP_PORT}\r\n\
                 CACHE-CONTROL: max-age={max_age}\r\n\
                 LOCATION: {loc}\r\n\
                 NT: {st}\r\n\
                 NTS: {nts}\r\n\
                 SERVER: {server}\r\n\
                 USN: {usn}\r\n\
                 \r\n",
                max_age = self.engine.config.network.max_age,
                server = crate::device::SERVER_STRING,
            )
        }
    }

    async fn listener(
        self: Arc<Self>,
        rx: UdpSocket,
        send_socks: Vec<(Iface, Arc<UdpSocket>)>,
        ifaces: Vec<Iface>,
    ) {
        let mut shutdown = self.shutdown.clone();
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, from) = tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    tracing::debug!("SSDP listener shutting down");
                    break;
                }
                res = rx.recv_from(&mut buf) => match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("SSDP recv error: {e}");
                        continue;
                    }
                },
            };
            let req = match parse_search(&buf[..len]) {
                Some(r) => r,
                None => continue,
            };
            let from_ip = match from.ip() {
                std::net::IpAddr::V4(v4) => v4,
                _ => continue,
            };
            tracing::debug!("SSDP M-SEARCH ST={} MX={} from {from_ip}", req.st, req.mx);

            // Choose the interface that can reach the requester.
            let chosen: Vec<(Iface, Arc<UdpSocket>)> = if req.st == "ssdp:all" {
                send_socks.clone()
            } else {
                send_socks
                    .iter()
                    .filter(|(i, _)| same_subnet(i.ip, i.mask, from_ip))
                    .cloned()
                    .collect()
            };
            if chosen.is_empty() {
                tracing::debug!("SSDP: no interface on the requester's subnet ({from_ip})");
                continue;
            }

            let pairs: Vec<(String, String)> = usn_pairs(&self.engine.config.device.udn)
                .into_iter()
                .filter(|(_, st)| req.st == "ssdp:all" || *st == req.st)
                .collect();

            for (iface, sock) in chosen {
                for (usn, st) in &pairs {
                    let msg = self.search_response(&iface, st, usn);
                    let sock = sock.clone();
                    let dest = SocketAddr::from((from_ip, from.port()));
                    // random jitter 0..MX to avoid response storms
                    let jitter_ms = {
                        use rand::Rng;
                        rand::rng().random_range(0..=req.mx.saturating_mul(1000))
                    };
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(jitter_ms as u64)).await;
                        if let Err(e) = sock.send_to(msg.as_bytes(), dest).await {
                            tracing::debug!("SSDP response send failed: {e}");
                        }
                    });
                }
            }
            let _ = ifaces;
        }
    }

    async fn notifier(self: Arc<Self>, send_socks: Vec<(Iface, Arc<UdpSocket>)>) {
        let mut shutdown = self.shutdown.clone();
        let pairs = usn_pairs(&self.engine.config.device.udn);
        let interval = Duration::from_secs(self.engine.config.network.ssdp_notify_secs.max(1));

        // Announce immediately (twice, as recommended by the spec).
        for round in 0..2 {
            if round == 1 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            self.announce(&send_socks, &pairs, "ssdp:alive").await;
        }

        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    self.announce(&send_socks, &pairs, "ssdp:byebye").await;
                    tracing::info!("SSDP: sent byebye, notifier stopped");
                    return;
                }
                _ = tokio::time::sleep(interval) => {
                    self.announce(&send_socks, &pairs, "ssdp:alive").await;
                }
            }
        }
    }

    async fn announce(
        &self,
        send_socks: &[(Iface, Arc<UdpSocket>)],
        pairs: &[(String, String)],
        nts: &str,
    ) {
        for (iface, sock) in send_socks {
            for (usn, st) in pairs {
                let msg = self.notify_msg(iface, st, usn, nts);
                if let Err(e) = sock
                    .send_to(msg.as_bytes(), SocketAddr::from((SSDP_ADDR, SSDP_PORT)))
                    .await
                {
                    tracing::debug!("SSDP NOTIFY send failed on {}: {e}", iface.ip);
                }
            }
        }
    }
}
