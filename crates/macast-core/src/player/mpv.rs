//! mpv backend: an external `mpv` process driven through its JSON IPC socket.
//!
//! This mirrors what Macast does (spawn mpv with `--input-ipc-server` and talk
//! newline-delimited JSON over the socket), so all of mpv's decoding, subtitles,
//! hardware acceleration and OSC controls come for free.

use super::{Player, PlayerEvent};
use crate::util::parse_time;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinHandle;

/// Windows named pipe path, or a Unix domain socket path, for this run.
fn ipc_socket_path() -> String {
    let n: u32 = {
        use rand::Rng;
        rand::rng().random_range(0..100_000)
    };
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\macast_gpui_mpv_{n}")
    }
    #[cfg(not(windows))]
    {
        format!("/tmp/macast_gpui_mpv_{n}.sock")
    }
}

fn mpv_executable(mpv_path: Option<PathBuf>) -> String {
    mpv_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "mpv".to_string())
}

/// mpv command line (mirrors Macast's `macast_renderer/mpv.py`).
fn mpv_args(ipc_path: &str) -> Vec<String> {
    vec![
        format!("--input-ipc-server={ipc_path}"),
        "--idle=yes".into(),
        "--no-terminal".into(),
        // No window while idle: mpv is spawned at app start (`--idle=yes`) and
        // an empty player window on startup is unwanted. `force-window` is
        // switched on per media below so audio/images still get a window.
        "--force-window=no".into(),
        "--image-display-duration=inf".into(),
        "--hwdec=auto".into(),
        "--save-position-on-quit=yes".into(),
        "--script-opts=osc-timetotal=yes,osc-layout=bottombar,osc-title=${title},osc-showwindowed=yes,osc-seekbarstyle=bar,osc-visibility=auto".into(),
    ]
}

/// IPC commands for one cast. Split out so the sequence is unit-testable
/// without spawning mpv.
fn media_commands(url: &str, start_secs: f64, title: &str, fullscreen: bool) -> Vec<Vec<Value>> {
    // Show the player window only once something is actually being cast.
    // Video opens a window on its own; this also covers audio and images.
    let mut commands = vec![vec![
        "set_property".into(),
        "force-window".into(),
        "yes".into(),
    ]];
    // Applied per cast, so leaving fullscreen with Esc does not disable it for
    // the next video.
    if fullscreen {
        commands.push(vec![
            "set_property".into(),
            "fullscreen".into(),
            true.into(),
        ]);
    }
    commands.push(vec!["loadfile".into(), url.into(), "replace".into()]);
    if start_secs > 0.0 {
        commands.push(vec!["seek".into(), start_secs.into(), "absolute".into()]);
    }
    if !title.is_empty() {
        commands.push(vec![
            "set_property".into(),
            "force-media-title".into(),
            title.into(),
        ]);
    }
    commands
}

/// Parse one line of mpv's JSON IPC output into a [`PlayerEvent`].
fn parse_ipc_event(line: &str) -> Option<PlayerEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let event = v.get("event")?.as_str()?;
    match event {
        "property-change" => {
            let name = v.get("name")?.as_str()?;
            let data = v.get("data");
            match name {
                "time-pos" => data?.as_f64().map(PlayerEvent::Position),
                "duration" => data?.as_f64().map(PlayerEvent::Duration),
                "pause" => data?.as_bool().map(PlayerEvent::Paused),
                "volume" => data?.as_f64().map(PlayerEvent::Volume),
                "mute" => data?.as_bool().map(PlayerEvent::Mute),
                "speed" => data?.as_f64().map(PlayerEvent::Speed),
                _ => None,
            }
        }
        "end-file" => Some(PlayerEvent::Ended),
        "playback-restart" => Some(PlayerEvent::Playing),
        "idle" => Some(PlayerEvent::Stopped),
        "shutdown" => Some(PlayerEvent::Stopped),
        _ => None,
    }
}

#[cfg(windows)]
type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(not(windows))]
type IpcStream = tokio::net::UnixStream;

async fn connect_ipc(path: &str, timeout: Duration) -> anyhow::Result<IpcStream> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let res = {
            #[cfg(windows)]
            {
                tokio::net::windows::named_pipe::ClientOptions::new().open(path)
            }
            #[cfg(not(windows))]
            {
                tokio::net::UnixStream::connect(path).await
            }
        };
        match res {
            Ok(s) => return Ok(s),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(e) => anyhow::bail!("cannot connect to mpv IPC at {path}: {e}"),
        }
    }
}

pub struct MpvPlayer {
    events: broadcast::Sender<PlayerEvent>,
    cmd_tx: mpsc::UnboundedSender<Value>,
    shutdown_tx: broadcast::Sender<()>,
    child: Arc<Mutex<Option<Child>>>,
    _tasks: Vec<JoinHandle<()>>,
    ipc_path: String,
    fullscreen: bool,
}

impl MpvPlayer {
    /// Spawn mpv and connect to its IPC socket. Returns once connected.
    pub async fn spawn(mpv_path: Option<PathBuf>, fullscreen: bool) -> anyhow::Result<Self> {
        let ipc_path = ipc_socket_path();
        let exe = mpv_executable(mpv_path);

        let mut cmd = Command::new(&exe);
        cmd.args(mpv_args(&ipc_path))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: don't flash a console window next to mpv's.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to launch mpv ('{exe}'): {e}. Install mpv or set mpv_path in config."
            )
        })?;

        let stream = match connect_ipc(&ipc_path, Duration::from_secs(20)).await {
            Ok(s) => s,
            Err(e) => {
                // best effort cleanup
                let mut guard = child;
                let _ = guard.kill().await;
                return Err(e);
            }
        };

        let (event_tx, _) = broadcast::channel(256);
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Value>();
        let (shutdown_tx, _) = broadcast::channel::<()>(4);

        let (reader, writer) = tokio::io::split(stream);

        // writer task: serializes commands onto the IPC socket
        let mut shutdown_rx_w = shutdown_tx.subscribe();
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            loop {
                tokio::select! {
                    _ = shutdown_rx_w.recv() => break,
                    Some(v) = cmd_rx.recv() => {
                        let line = format!("{v}\n");
                        if writer.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // reader task: mpv events -> broadcast
        let ev_tx = event_tx.clone();
        let mut shutdown_rx_r = shutdown_tx.subscribe();
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                tokio::select! {
                    _ = shutdown_rx_r.recv() => break,
                    line = lines.next_line() => {
                        match line {
                            Ok(Some(l)) => {
                                if let Some(ev) = parse_ipc_event(&l) {
                                    let _ = ev_tx.send(ev);
                                }
                            }
                            _ => {
                                let _ = ev_tx.send(PlayerEvent::Stopped);
                                break;
                            }
                        }
                    }
                }
            }
        });

        let player = Self {
            events: event_tx,
            cmd_tx,
            shutdown_tx,
            child: Arc::new(Mutex::new(Some(child))),
            _tasks: vec![writer_task, reader_task],
            ipc_path,
            fullscreen,
        };

        // Observe the properties we translate into UPnP state.
        player.observe(1, "time-pos");
        player.observe(2, "duration");
        player.observe(3, "pause");
        player.observe(4, "volume");
        player.observe(5, "mute");
        player.observe(6, "speed");

        tracing::info!("mpv backend ready (ipc: {})", player.ipc_path);
        Ok(player)
    }

    fn send(&self, command: Value) {
        let _ = self.cmd_tx.send(command);
    }

    fn observe(&self, id: u64, property: &str) {
        self.send(json!({ "command": ["observe_property", id, property] }));
    }

    fn cmd(&self, args: Vec<Value>) {
        self.send(json!({ "command": args }));
    }
}

#[async_trait]
impl Player for MpvPlayer {
    async fn set_media_url(&self, url: &str, start_secs: f64, title: &str) {
        for command in media_commands(url, start_secs, title, self.fullscreen) {
            self.cmd(command);
        }
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn resume(&self) {
        self.cmd(vec!["set_property".into(), "pause".into(), false.into()]);
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn pause(&self) {
        self.cmd(vec!["set_property".into(), "pause".into(), true.into()]);
        let _ = self.events.send(PlayerEvent::Paused(true));
    }

    async fn stop(&self) {
        // Drop the forced window first so stopping leaves mpv idle and invisible.
        self.cmd(vec![
            "set_property".into(),
            "force-window".into(),
            "no".into(),
        ]);
        self.cmd(vec!["stop".into()]);
        let _ = self.events.send(PlayerEvent::Stopped);
    }

    async fn seek(&self, secs: f64) {
        self.cmd(vec!["seek".into(), secs.into(), "absolute".into()]);
    }

    async fn set_volume(&self, volume: u8) {
        self.cmd(vec![
            "set_property".into(),
            "volume".into(),
            (volume.min(100) as f64).into(),
        ]);
    }

    async fn set_mute(&self, mute: bool) {
        self.cmd(vec!["set_property".into(), "mute".into(), mute.into()]);
    }

    async fn set_speed(&self, speed: f64) {
        self.cmd(vec!["set_property".into(), "speed".into(), speed.into()]);
    }

    async fn add_subtitle(&self, url: &str, title: &str) {
        self.cmd(vec![
            "sub-add".into(),
            url.into(),
            "select".into(),
            title.into(),
        ]);
    }

    async fn set_subtitle_visible(&self, visible: bool) {
        self.cmd(vec![
            "set_property".into(),
            "sub-visibility".into(),
            visible.into(),
        ]);
    }

    async fn show_text(&self, text: &str, duration_ms: u64) {
        self.cmd(vec!["show-text".into(), text.into(), duration_ms.into()]);
    }

    async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
        let pid = {
            let child = self.child.lock().await;
            child.as_ref().and_then(|child| child.id())
        };
        #[cfg(windows)]
        if let Some(pid) = pid {
            // mpv is commonly launched through a shim (mpv.com) that starts the
            // real mpv.exe; killing only the shim leaves an orphan behind, so
            // take down the whole process tree.
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(windows))]
        let _ = pid;
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = child.kill().await;
        }
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    fn name(&self) -> &'static str {
        "mpv"
    }
}

/// Parse a UPnP `H:MM:SS` target into seconds (used by Seek).
pub fn seek_target_to_secs(target: &str) -> f64 {
    parse_time(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_without_an_idle_window() {
        let args = mpv_args(r"\\.\pipe\macast_test");
        assert!(
            args.iter().any(|arg| arg == "--force-window=no"),
            "mpv must start idle without a window"
        );
        assert!(
            !args.iter().any(|arg| arg.contains("immediate")),
            "--force-window=immediate would pop an empty player window at startup"
        );
    }

    #[test]
    fn parses_seek_targets() {
        assert_eq!(seek_target_to_secs("0:01:15"), 75.0);
        assert_eq!(seek_target_to_secs("12.5"), 12.5);
    }

    #[test]
    fn fullscreen_is_applied_on_every_cast() {
        let commands = media_commands("http://host/v.mp4", 0.0, "", true);
        assert!(commands.iter().any(|command| {
            command.get(1).and_then(|v| v.as_str()) == Some("fullscreen")
                && command.get(2).and_then(|v| v.as_bool()) == Some(true)
        }));
        // The command must precede loadfile so the window opens fullscreen.
        let fullscreen_at = commands
            .iter()
            .position(|command| command.get(1).and_then(|v| v.as_str()) == Some("fullscreen"))
            .unwrap();
        let loadfile_at = commands
            .iter()
            .position(|command| command.first().and_then(|v| v.as_str()) == Some("loadfile"))
            .unwrap();
        assert!(fullscreen_at < loadfile_at);

        let commands = media_commands("http://host/v.mp4", 0.0, "", false);
        assert!(!commands
            .iter()
            .any(|command| command.get(1).and_then(|v| v.as_str()) == Some("fullscreen")));
    }
}
