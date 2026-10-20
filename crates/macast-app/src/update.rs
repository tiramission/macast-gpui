//! In-app update check / install, on top of `cargo-packager-updater`.
//!
//! The updater crate is blocking (`reqwest::blocking`), so all network work runs
//! on the core's tokio runtime and reports back over a channel - the UI only
//! ever sees [`UpdateEvent`]s and never touches the network itself.
//!
//! Installing means: download the NSIS installer this release was packaged with,
//! verify its minisign signature against the release public key, then let the
//! installer take over. It is started silently with `/R` (restart when done) and
//! the app exits first, because the installer has to replace the very files we
//! are running from.

pub use cargo_packager_updater::Update;
use macast_core::Config;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

/// Manifest published alongside every release. `releases/latest/download`
/// always resolves to the newest one, so one URL covers all versions.
const DEFAULT_ENDPOINT: &str =
    "https://github.com/tiramission/macast-gpui/releases/latest/download/latest.json";

/// Public half of the release signing key, from `cargo packager signer generate`
/// (base64 of a minisign public key). `update.pubkey` in config.json overrides
/// it, which is how a local test server can be trusted instead.
pub const DEFAULT_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQ3QUU2QTM5MTM3OTY2QTgKUldTb1pua1RPV3F1MTZ3bWwxRzN2TE1Yb3RodUllQngxTGxDYW9pSzZTOXhVbkIrSEIzYzRoNDEK";

/// What a worker thread reports back to the UI.
pub enum UpdateEvent {
    /// The manifest announces nothing newer than what we run.
    UpToDate,
    /// A newer release is available; its release notes live in `Update::body`.
    Available(Box<Update>),
    /// Download progress; `total` is `None` when the server sends no length.
    Progress { received: u64, total: Option<u64> },
    /// Downloaded and signature-checked; ready to install.
    Downloaded { update: Box<Update>, bytes: Vec<u8> },
    /// Something went wrong; the message is meant for the user.
    Failed(String),
}

/// What the settings panel shows on its 更新 rows.
pub enum UpdateStage {
    /// Not checked yet in this run.
    Idle,
    Checking,
    UpToDate,
    Available(Box<Update>),
    Downloading {
        version: String,
        received: u64,
        total: Option<u64>,
    },
    /// The installer is running; the app is on its way out.
    Installing(String),
    Failed(String),
}

/// Ask the release manifest for an update (blocking, on the runtime's pool).
pub fn check(
    runtime: &tokio::runtime::Handle,
    config: &Config,
    tx: mpsc::UnboundedSender<UpdateEvent>,
) {
    let updater_config = match updater_config(config) {
        Ok(config) => config,
        Err(e) => {
            let _ = tx.send(UpdateEvent::Failed(e));
            return;
        }
    };
    let current = match cargo_packager_updater::semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) => version,
        Err(e) => {
            let _ = tx.send(UpdateEvent::Failed(format!("版本号无法解析：{e}")));
            return;
        }
    };

    runtime.spawn_blocking(move || {
        match cargo_packager_updater::check_update(current, updater_config) {
            Ok(Some(update)) => {
                tracing::info!("update available: v{}", update.version);
                let _ = tx.send(UpdateEvent::Available(Box::new(update)));
            }
            Ok(None) => {
                tracing::info!("no update available");
                let _ = tx.send(UpdateEvent::UpToDate);
            }
            Err(e) => {
                tracing::warn!("update check failed: {e}");
                let _ = tx.send(UpdateEvent::Failed(format!("检查更新失败：{e}")));
            }
        }
    });
}

/// Download `update` and verify its signature (blocking, on the runtime's pool).
pub fn download(
    runtime: &tokio::runtime::Handle,
    update: Update,
    tx: mpsc::UnboundedSender<UpdateEvent>,
) {
    runtime.spawn_blocking(move || {
        // The callback reports each chunk, not a running total.
        let received = AtomicU64::new(0);
        let result = update.download_extended(
            |chunk, total| {
                let so_far = received.fetch_add(chunk as u64, Ordering::Relaxed) + chunk as u64;
                let _ = tx.send(UpdateEvent::Progress {
                    received: so_far,
                    total,
                });
            },
            || {},
        );

        match result {
            Ok(bytes) => {
                tracing::info!(
                    "downloaded v{} ({} bytes), signature ok",
                    update.version,
                    bytes.len()
                );
                let _ = tx.send(UpdateEvent::Downloaded {
                    update: Box::new(update),
                    bytes,
                });
            }
            Err(e) => {
                tracing::warn!("update download failed: {e}");
                let _ = tx.send(UpdateEvent::Failed(format!("下载更新失败：{e}")));
            }
        }
    });
}

/// Hand `bytes` (an already verified installer) to the OS installer, which
/// replaces the app and restarts it. Exits the process on success, so anything
/// that still needs to happen has to be done before this call.
///
/// Note this does *not* call `Update::install`: that one pipes the installer
/// through PowerShell with its arguments joined into a single `"/S, /R"` token,
/// which NSIS does not parse - it opens its wizard and waits for a human.
pub fn install(
    runtime: &tokio::runtime::Handle,
    update: Update,
    bytes: Vec<u8>,
    tx: mpsc::UnboundedSender<UpdateEvent>,
) {
    runtime.spawn_blocking(move || {
        tracing::info!("handing over to the v{} installer", update.version);
        match run_installer(&update.version, &bytes) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                tracing::warn!("update install failed: {e}");
                let _ = tx.send(UpdateEvent::Failed(format!("安装更新失败：{e}")));
            }
        }
    });
}

/// Start the installer from a temp file, silently, asking it to relaunch the
/// app once it is done.
///
/// `/S` and `/R` are passed as two separate arguments. `/S` also keeps the
/// installer's own language dialog and wizard out of the way - the app already
/// shows download and install progress, and a wizard nobody clicks would leave
/// the update half applied.
#[cfg(windows)]
fn run_installer(version: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let dir = std::env::temp_dir();
    // Installers left behind by earlier updates are dead weight (and the one we
    // are about to write may still exist), so clear them out first.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("macast-gpui-update") && name.ends_with(".exe") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let path = dir.join(format!("macast-gpui-update-{version}.exe"));
    std::fs::write(&path, bytes)?;
    std::process::Command::new(&path)
        .args(["/S", "/R"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn run_installer(_version: &str, _bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "只有 Windows 安装包支持应用内更新",
    ))
}

/// The version compiled into this binary.
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Bytes as shown in the progress line ("12.3 MB", "820 KB").
pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

fn updater_config(config: &Config) -> Result<cargo_packager_updater::Config, String> {
    let endpoint = config
        .update
        .endpoint
        .as_deref()
        .unwrap_or(DEFAULT_ENDPOINT);
    let pubkey = config.update.pubkey.as_deref().unwrap_or(DEFAULT_PUBKEY);
    let endpoint = cargo_packager_updater::url::Url::parse(endpoint)
        .map_err(|e| format!("更新地址无效：{e}"))?;

    Ok(cargo_packager_updater::Config {
        endpoints: vec![endpoint],
        pubkey: pubkey.to_string(),
        // Installing is done by [`run_installer`], so the crate's own Windows
        // launch settings are not used.
        windows: None,
    })
}
