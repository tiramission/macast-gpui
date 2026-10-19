//! gpui desktop app: a small window that shows what is being cast, transport
//! controls, volume, and the addresses the phone uses to reach us.

use gpui::*;
use macast_core::{Config, MacastCore};
use std::time::Duration;

mod views;

use views::RootView;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

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
            eprintln!("failed to start Macast-Gpui: {e:#}");
            std::process::exit(1);
        }
    };
    let handle = runtime.handle().clone();

    Application::new().run(move |cx| {
        cx.activate(true);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(780.0), px(560.0)),
                cx,
            )),
            ..Default::default()
        };

        let _ = cx.open_window(window_options, |_, cx| {
            cx.new(|cx| RootView::new(core.clone(), cx))
        });

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
                let _ = quit_handle.block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(2), core.shutdown()).await;
                });
            }
        })
        .detach();
    });
}
