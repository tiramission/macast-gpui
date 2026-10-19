//! Embed the application icon into the Windows executable.
//!
//! gpui's Windows backend loads icon resource **id 1** when it registers the
//! window class (`LoadImageW(module, PCWSTR(1), IMAGE_ICON, ...)`), so the icon
//! also becomes the taskbar/window icon, not just the file icon in Explorer.
//!
//! We call the Windows SDK's `rc.exe` directly (same approach as `package.ps1`
//! does for `fxc.exe`) to avoid pulling in another build dependency.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let res = out_dir.join("macast_app.res");
    let Some(rc) = find_rc() else {
        // Not fatal: the app still runs, it just falls back to the default icon.
        println!(
            "cargo:warning=rc.exe not found (Windows SDK missing?); building without the app icon"
        );
        return;
    };

    let status = Command::new(&rc)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("icon.rc")
        .status()
        .expect("failed to spawn rc.exe");
    assert!(status.success(), "rc.exe exited with {status}");

    println!("cargo:rustc-link-arg={}", res.display());
}

fn find_rc() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RC_EXE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let root = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| r"C:\Program Files (x86)".to_string());
    let bin = PathBuf::from(root)
        .join("Windows Kits")
        .join("10")
        .join("bin");

    let mut versions: Vec<PathBuf> = std::fs::read_dir(&bin)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();

    // Prefer the newest SDK; x64 is the target we ship.
    versions
        .iter()
        .rev()
        .map(|version| version.join("x64").join("rc.exe"))
        .find(|candidate| candidate.is_file())
}
