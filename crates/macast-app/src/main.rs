// Release builds are a GUI app: double-clicking the exe must not open a console
// window. Debug builds keep the console so `cargo run` still shows logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! gpui desktop app: a small window that shows what is being cast, transport
//! controls, volume, and the addresses the phone uses to reach us.

use gpui::*;
use macast_core::{Config, MacastCore};
use std::time::Duration;

mod views;

#[cfg(target_os = "windows")]
mod tray;

use views::RootView;

fn init_logging() {
    if cfg!(debug_assertions) {
        // Development: keep logs on the console that `cargo run` already has.
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .init();
        return;
    }

    // Release: there is no console, so write to %APPDATA%\macast-gpui\logs\.
    let dir = Config::config_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("macast-gpui.log");
    // Trivial rotation: start a fresh file once it grows past 5 MiB.
    if std::fs::metadata(&path)
        .map(|m| m.len() > 5 * 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_target(false)
                .with_ansi(false)
                // A fresh handle per event keeps the writer `Fn` and avoids
                // sharing a mutable File across threads.
                .with_writer(move || file.try_clone().expect("clone log file handle"))
                .init();
            tracing::info!("log file: {}", path.display());
            std::panic::set_hook(Box::new(|info| {
                tracing::error!("panic: {info}");
            }));
        }
        Err(e) => {
            // Last resort: without a console this goes nowhere, but the app can
            // still run (and the tray still works).
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_target(false)
                .init();
            tracing::error!("cannot open {}: {e}", path.display());
        }
    }
}

fn main() {
    init_logging();

    // The DLNA core is async (tokio); gpui owns the main thread. We create the
    // runtime up front, block on startup, then keep it alive for background
    // tasks (SSDP, HTTP, mpv IPC, GENA notifiers).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let runtime: &'static tokio::runtime::Runtime = Box::leak(Box::new(runtime));

    let core = match runtime.block_on(MacastCore::start(Config::load())) {
        Ok(core) => core,
        Err(e) => {
            tracing::error!("failed to start Macast-Gpui: {e:#}");
            std::process::exit(1);
        }
    };
    let handle = runtime.handle().clone();

    Application::new().run(move |cx| {
        cx.activate(true);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(900.0), px(560.0)), cx)),
            window_min_size: Some(size(px(760.0), px(480.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("Macast-Gpui".into()),
                // Custom titlebar: the app draws its own header/controls.
                appears_transparent: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        let window = cx
            .open_window(window_options, |window, cx| {
                let view = cx.new(|cx| RootView::new(core.clone(), cx));
                view.update(cx, |view, cx| view.watch_appearance(window, cx));
                view
            })
            .expect("failed to open the main window");

        // Windows: keep running in the system tray. Closing the window hides
        // it instead of quitting; the tray menu can restore it or exit.
        #[cfg(target_os = "windows")]
        {
            let any: AnyWindowHandle = window.into();
            match tray::start(any, cx) {
                Ok(()) => {
                    let _ = any.update(cx, |_, window, cx| {
                        window.on_window_should_close(cx, |window, _| {
                            tray::on_close_requested(window)
                        });
                    });
                }
                Err(e) => tracing::warn!("system tray unavailable, close will quit: {e}"),
            }
        }

        // Graceful shutdown: send SSDP byebye and kill mpv before exiting.
        // `on_app_quit` in gpui 0.2 expects a callback returning a future; the
        // foreground executor is smol-based, so blocking on the tokio handle
        // here is safe (we are not inside a tokio runtime context).
        let core_for_quit = core.clone();
        let quit_handle = handle.clone();
        cx.on_app_quit(move |_| {
            // `on_app_quit` takes an `FnMut`, so clone per call instead of
            // moving the captured handles into the returned future.
            let core = core_for_quit.clone();
            let quit_handle = quit_handle.clone();
            async move {
                quit_handle.block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(2), core.shutdown()).await;
                });
            }
        })
        .detach();
    });
}
