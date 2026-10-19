//! macast-core: a DLNA/UPnP **MediaRenderer** (receiver / "cast target") implemented in Rust.
//!
//! The crate is UI-agnostic. It exposes [`MacastCore`], which:
//!
//! 1. announces a MediaRenderer device on the LAN via SSDP (`ssdp`),
//! 2. serves the device description + SCPD documents and answers SOAP control
//!    actions (AVTransport / RenderingControl / ConnectionManager) over HTTP (`http`, `services`),
//! 3. keeps DLNA state variables, pushes GENA event notifications to subscribers (`eventing`),
//! 4. drives a pluggable player backend (`player`), defaulting to an external `mpv`
//!    process controlled through its JSON IPC socket.
//!
//! The UI layer subscribes to a `tokio::sync::watch` channel of [`PlaybackSnapshot`].

pub mod config;
pub mod core;
pub mod device;
pub mod eventing;
pub mod http;
pub mod player;
pub mod services;
pub mod soap;
pub mod ssdp;
pub mod state;
pub mod util;

pub use config::{Config, PlayerKind};
pub use core::MacastCore;
pub use state::{PlaybackSnapshot, TransportState};
