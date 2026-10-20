//! The main window view.
//!
//! Design: a compact "cinema console" — dark surface, one accent colour, a
//! single hero card for what is playing, a floating transport dock, and
//! settings in a slide-over panel instead of pushing the layout around.
//!
//! gpui 0.2.2 has no built-in icon set, buttons or toggles, so the small
//! component kit at the bottom of this file draws glyphs with `canvas` and
//! composes controls from `div`s. Text inputs are still file-based (no IME in
//! this gpui release): the settings panel shows values and offers shortcuts.

use alhazen_gpui::{Source, VideoPlayer};
use gpui::prelude::FluentBuilder;
use gpui::*;
use macast_core::player::{Player, VideoFrame};
use macast_core::{
    Config, MacastCore, MediaKind, PlaybackSnapshot, PlayerKind, Theme, TransportState,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};

use crate::update::{self, Update, UpdateEvent, UpdateStage};

// ---- design tokens -------------------------------------------------------
// Two palettes share the same semantic roles: one accent, status colours only
// for playback state, three text levels. `Auto` resolves against the OS
// appearance every frame, so switching themes does not need a restart.

#[derive(Clone, Copy)]
struct Palette {
    bg: u32,
    surface: u32,
    surface_hi: u32,
    text: u32,
    muted: u32,
    dim: u32,
    accent: u32,
    accent_hover: u32,
    accent_pressed: u32,
    on_accent: u32,
    success: u32,
    warn: u32,
    border: u32,
    border_strong: u32,
    hover: u32,
    active: u32,
    divider: u32,
    accent_soft: u32,
    accent_border: u32,
    close_hover: u32,
    backdrop: u32,
    hero_from: u32,
    hero_to: u32,
    panel_from: u32,
    panel_to: u32,
    art_from: u32,
    art_to: u32,
    brand_from: u32,
    brand_to: u32,
    on_art: u32,
    toast_bg: u32,
    inset: u32,
    segment_active: u32,
    switch_off: u32,
    knob: u32,
}

impl Palette {
    const DARK: Palette = Palette {
        bg: 0x0A0D13,
        surface: 0x121722,
        surface_hi: 0x171E2C,
        text: 0xEDF0F7,
        muted: 0x98A2B3,
        dim: 0x7C8698,
        accent: 0x6C8CFF,
        accent_hover: 0x7D9AFF,
        accent_pressed: 0x5B7BEE,
        on_accent: 0x0A0D13,
        success: 0x3DDC97,
        warn: 0xF5B84D,
        border: 0xFFFFFF14,
        border_strong: 0xFFFFFF24,
        hover: 0xFFFFFF0F,
        active: 0xFFFFFF1F,
        divider: 0xFFFFFF12,
        accent_soft: 0x6C8CFF29,
        accent_border: 0x6C8CFF40,
        close_hover: 0xF0444429,
        backdrop: 0x05070C99,
        hero_from: 0x171E2C,
        hero_to: 0x0F141E,
        panel_from: 0x121824,
        panel_to: 0x0D121A,
        art_from: 0x33478F,
        art_to: 0x141A27,
        brand_from: 0x7D9AFF,
        brand_to: 0x3E5BD6,
        on_art: 0xFFFFFF,
        toast_bg: 0x1B2333,
        inset: 0x0B0F17,
        segment_active: 0x2A3550,
        switch_off: 0x2B3446,
        knob: 0xFFFFFF,
    };

    const LIGHT: Palette = Palette {
        bg: 0xF3F5F9,
        surface: 0xFFFFFF,
        surface_hi: 0xFFFFFF,
        text: 0x111827,
        muted: 0x5B6472,
        dim: 0x5F6878,
        accent: 0x3D5AF1,
        accent_hover: 0x2F49D9,
        accent_pressed: 0x2A41C4,
        on_accent: 0xFFFFFF,
        success: 0x047857,
        warn: 0xB45309,
        border: 0x00000014,
        border_strong: 0x00000024,
        hover: 0x0000000A,
        active: 0x00000014,
        divider: 0x0000000F,
        accent_soft: 0x3D5AF11F,
        accent_border: 0x3D5AF140,
        close_hover: 0xDC26261F,
        backdrop: 0x0F172A59,
        hero_from: 0xFFFFFF,
        hero_to: 0xF6F8FF,
        panel_from: 0xFFFFFF,
        panel_to: 0xF7F9FD,
        art_from: 0x6C8CFF,
        art_to: 0x3D5AF1,
        brand_from: 0x6C8CFF,
        brand_to: 0x3D5AF1,
        on_art: 0xFFFFFF,
        toast_bg: 0xFFFFFF,
        inset: 0xF1F4FA,
        segment_active: 0xE4E9FF,
        switch_off: 0xD7DDE9,
        knob: 0xFFFFFF,
    };
}

thread_local! {
    static PALETTE: Cell<Palette> = const { Cell::new(Palette::DARK) };
}

fn set_palette(palette: Palette) {
    PALETTE.with(|cell| cell.set(palette));
}

fn p() -> Palette {
    PALETTE.with(|cell| cell.get())
}

pub struct RootView {
    core: Arc<MacastCore>,
    /// Config as last loaded/saved by the UI. `core.engine.config` is frozen at
    /// startup, so edits are written to disk and applied on the next launch.
    config: Config,
    snapshot: PlaybackSnapshot,
    notice: Option<String>,
    /// Incremented whenever a notice is set, so stale timers do not clear it.
    notice_seq: u64,
    show_settings: bool,
    /// Whether the app is registered to start with the OS session.
    autostart: bool,
    /// Bounds of the seek bar, captured while painting it. Click coordinates are
    /// window-relative, so we need the bar's own rectangle to compute a fraction.
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Same trick for the volume slider.
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Alhazen video player entity (only when alhazen backend is active).
    alhazen_player: Option<gpui::Entity<VideoPlayer>>,
    /// Whether the alhazen video is in fullscreen mode.
    alhazen_fullscreen: bool,
    /// A cast asked for fullscreen while the window was hidden (closed to the
    /// tray) or minimized. gpui 0.2.2 cannot toggle fullscreen in either state,
    /// so the window is brought back first and the toggle runs once it really
    /// is on screen again.
    pending_fullscreen: bool,
    /// A fullscreen toggle that has been issued but not yet reflected by
    /// `Window::is_fullscreen()` (gpui applies it on its executor). Another
    /// toggle in that window would cancel it, so only one may be in flight.
    fullscreen_pending: Option<bool>,
    /// Esc key state from the previous frame, for edge detection.
    esc_was_down: bool,
    /// Last mpv frame image, so the next one can drop it from the atlas.
    mpv_slot: Rc<Cell<Option<Arc<RenderImage>>>>,
    /// Decoded still picture of the current image cast, drawn by this view
    /// (neither backend decodes pictures).
    image: Option<Arc<RenderImage>>,
    /// `cast_seq` the picture in `image` belongs to (0 = none).
    image_seq: u64,
    /// `cast_seq` of the cast the alhazen player was built for. Bumped by
    /// the core on every SetAVTransportURI, so re-casting the same uri still
    /// counts as a new cast.
    last_cast_seq: u64,
    /// Timestamp of the last titlebar click, for double-click to maximize.
    last_title_click: Option<std::time::Instant>,
    /// Update check / download / install progress, shown in the settings panel.
    update_stage: UpdateStage,
    /// Our window; async state updates go through it instead of
    /// `update_entity` - see [`RootView::init`]. `None` until `init`.
    window_handle: Option<AnyWindowHandle>,
    /// Keeps the OS appearance observer alive (auto theme).
    _appearance_sub: Option<Subscription>,
}

impl RootView {
    pub fn new(core: Arc<MacastCore>) -> Self {
        let mut rx = core.subscribe();
        let initial = rx.borrow_and_update().clone();
        drop(rx);

        let config = core.engine.config.clone();
        let autostart = is_autostart_enabled();

        Self {
            core,
            config,
            snapshot: initial,
            notice: None,
            notice_seq: 0,
            show_settings: false,
            autostart,
            seek_bounds: Rc::new(Cell::new(Bounds::default())),
            volume_bounds: Rc::new(Cell::new(Bounds::default())),
            alhazen_player: None,
            alhazen_fullscreen: false,
            pending_fullscreen: false,
            fullscreen_pending: None,
            esc_was_down: false,
            mpv_slot: Rc::new(Cell::new(None)),
            image: None,
            image_seq: 0,
            last_cast_seq: 0,
            last_title_click: None,
            update_stage: UpdateStage::Idle,
            window_handle: None,
            _appearance_sub: None,
        }
    }

    /// Called once with the real window: remember the handle, track OS theme
    /// changes, and start the state watcher.
    ///
    /// The watcher deliberately updates the view through
    /// [`AppContext::update_window`], not `update_entity`. On Windows gpui
    /// pumps messages from inside frame presents and window-style changes,
    /// which can run a queued foreground task while the app is still
    /// borrowed; `update_entity` panics on that ("RefCell already borrowed")
    /// and takes the app down, while `update_window` uses `try_borrow_mut`
    /// and simply skips a beat.
    pub fn init(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_handle = Some(window.window_handle());
        self._appearance_sub = Some(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));

        let handle = self.window_handle;
        let mut rx = self.core.subscribe();
        self.snapshot = rx.borrow_and_update().clone();
        // `tokio::sync::watch` futures are runtime-agnostic, so they play
        // nicely with gpui's own executor. gpui 0.2 (edition 2024) wants an
        // async closure here.
        cx.spawn(async move |view, cx| {
            while rx.changed().await.is_ok() {
                let snap = rx.borrow_and_update().clone();
                if view.upgrade().is_none() {
                    break;
                }
                if let Some(handle) = handle {
                    let _ = cx.update_window(handle, |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            // A cast that lands while the window is closed to
                            // the tray (or minimized) has to bring the window
                            // back: gpui cannot toggle fullscreen from either
                            // state, and a hidden window may not be rendering
                            // at all, so this cannot wait for `render`.
                            let cast_arrived =
                                snap.cast_seq != this.snapshot.cast_seq && !snap.uri.is_empty();
                            this.snapshot = snap;
                            if cast_arrived
                                && this.config.player.fullscreen_on_cast
                                && window_needs_reveal(window)
                            {
                                tracing::info!(
                                    "cast arrived while the window is hidden/minimized - restoring it"
                                );
                                reveal_window(window);
                                this.pending_fullscreen = true;
                            }
                            cx.notify();
                        });
                    });
                }
            }
        })
        .detach();

        // Look for a newer release once per run. It stays quiet on failure: a
        // machine without internet (or before the first signed release) should
        // not be nagged by a check the user never asked for.
        if self.config.update.check_on_startup {
            self.check_update(true, cx);
        }
    }

    // ---- update check / install -----------------------------------------
    // See [`crate::update`]: all network work happens on the core's runtime,
    // results come back as [`UpdateEvent`]s and drive `self.update_stage`.

    /// Start a check. `quiet` keeps failures off the toast (used at startup).
    fn check_update(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if matches!(
            self.update_stage,
            UpdateStage::Checking | UpdateStage::Downloading { .. } | UpdateStage::Installing(_)
        ) {
            return;
        }
        self.update_stage = UpdateStage::Checking;
        cx.notify();
        let config = self.config.clone();
        self.run_update_worker(quiet, cx, move |runtime, tx| {
            update::check(&runtime, &config, tx)
        });
    }

    /// Download the release the last check found, then install it.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStage::Available(update) = &self.update_stage else {
            return;
        };
        let update = update.clone();
        self.update_stage = UpdateStage::Downloading {
            version: update.version.clone(),
            received: 0,
            total: None,
        };
        cx.notify();
        self.run_update_worker(false, cx, move |runtime, tx| {
            update::download(&runtime, *update, tx)
        });
    }

    fn toggle_update_check_on_startup(&mut self, cx: &mut Context<Self>) {
        self.config.update.check_on_startup = !self.config.update.check_on_startup;
        let label = if self.config.update.check_on_startup {
            "开启"
        } else {
            "关闭"
        };
        self.save_config(&format!("启动时检查更新（{label}）"), cx);
    }

    /// Run `worker` on the core's runtime and pump its events into the stage.
    fn run_update_worker<F>(&self, quiet: bool, cx: &mut Context<Self>, worker: F)
    where
        F: FnOnce(tokio::runtime::Handle, tokio::sync::mpsc::UnboundedSender<UpdateEvent>),
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UpdateEvent>();
        worker(self.core.handle(), tx);
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            while let Some(event) = rx.recv().await {
                let Some(handle) = handle else {
                    return;
                };
                let delivered = cx.update_window(handle, |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.apply_update_event(event, quiet, cx));
                });
                if delivered.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn apply_update_event(&mut self, event: UpdateEvent, quiet: bool, cx: &mut Context<Self>) {
        match event {
            UpdateEvent::UpToDate => self.update_stage = UpdateStage::UpToDate,
            UpdateEvent::Available(update) => {
                self.set_notice(
                    format!("发现新版本 v{}，可在设置里更新", update.version),
                    cx,
                );
                self.update_stage = UpdateStage::Available(update);
            }
            UpdateEvent::Progress { received, total } => {
                if let UpdateStage::Downloading { version, .. } = &self.update_stage {
                    self.update_stage = UpdateStage::Downloading {
                        version: version.clone(),
                        received,
                        total,
                    };
                }
            }
            UpdateEvent::Downloaded { update, bytes } => self.start_install(*update, bytes, cx),
            UpdateEvent::Failed(e) => {
                tracing::warn!("update: {e}");
                if !quiet {
                    self.set_notice(e.clone(), cx);
                }
                self.update_stage = UpdateStage::Failed(e);
            }
        }
        cx.notify();
    }

    /// Shut the DLNA side down the way the normal quit path does (SSDP byebye,
    /// mpv teardown), then let the installer replace us and restart the app.
    /// On success `Update::install` exits the process and never comes back.
    fn start_install(&mut self, update: Update, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.update_stage = UpdateStage::Installing(update.version.clone());
        cx.notify();

        let core = self.core.clone();
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            // `tokio::time::timeout` needs the runtime context, which gpui's
            // executor does not provide, so the shutdown (with its timeout) runs
            // as a task on the core's runtime and this task only joins it.
            let runtime = core.handle();
            let shutdown = runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(2), core.shutdown()).await;
            });
            let _ = shutdown.await;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UpdateEvent>();
            update::install(&runtime, update, bytes, tx);
            // Only reached when the installer could not even be started.
            while let Some(UpdateEvent::Failed(e)) = rx.recv().await {
                let Some(handle) = handle else {
                    return;
                };
                let _ = cx.update_window(handle, |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.update_stage = UpdateStage::Failed(e);
                        cx.notify();
                    });
                });
            }
        })
        .detach();
    }

    // ---- local control helpers ------------------------------------------
    // They spawn onto the core's tokio runtime; the core routes them through
    // the exact same handlers as remote DLNA control.

    fn spawn_core<F>(&self, f: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.core.handle().spawn(f);
    }

    fn play(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.play().await });
    }

    fn pause(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.pause().await });
    }

    fn stop(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.stop().await });
    }

    fn set_volume(&self, volume: u8) {
        let core = self.core.clone();
        self.spawn_core(async move { core.set_volume(volume).await });
    }

    fn toggle_mute(&self) {
        let mute = !self.snapshot.mute;
        let core = self.core.clone();
        self.spawn_core(async move { core.set_mute(mute).await });
    }

    /// Seek by a fraction (0..=1) of the current media duration.
    fn seek_to(&self, fraction: f64) {
        if self.snapshot.duration <= 0.0 {
            return;
        }
        let target = (fraction * self.snapshot.duration).clamp(0.0, self.snapshot.duration);
        let core = self.core.clone();
        self.spawn_core(async move { core.seek(target).await });
    }

    fn nudge_position(&self, delta: f64) {
        if self.snapshot.duration <= 0.0 && self.snapshot.position <= 0.0 {
            return;
        }
        let target = (self.snapshot.position + delta).max(0.0);
        let target = if self.snapshot.duration > 0.0 {
            target.min(self.snapshot.duration)
        } else {
            target
        };
        let core = self.core.clone();
        self.spawn_core(async move { core.seek(target).await });
    }

    /// Play a local file dropped onto the window.
    fn play_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        match url::Url::from_file_path(&path) {
            Ok(url) => {
                let title = path.file_stem().map(|s| s.to_string_lossy().to_string());
                let core = self.core.clone();
                self.spawn_core(async move { core.play_uri(url.to_string(), title).await });
                self.set_notice(format!("正在播放：{label}"), cx);
            }
            Err(_) => self.set_notice(format!("无法把路径转成 URI：{}", path.display()), cx),
        }
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        let uri = self
            .core
            .engine
            .state
            .try_read()
            .map(|g| g.get("CurrentTrackURI"))
            .unwrap_or_default();
        let message = if uri.is_empty() {
            "当前没有可复制的链接".to_string()
        } else {
            // arboard needs a thread with a message loop; a quick blocking call
            // on the UI thread is fine for a one-shot clipboard write.
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(uri)) {
                Ok(()) => "链接已复制到剪贴板".to_string(),
                Err(e) => format!("复制失败: {e}"),
            }
        };
        self.set_notice(message, cx);
    }

    /// Persist `self.config`, then tell the user a restart is required.
    fn save_config(&mut self, what: &str, cx: &mut Context<Self>) {
        match self.config.save() {
            Ok(()) => self.set_notice(format!("{what}已保存，重启应用后生效"), cx),
            Err(e) => self.set_notice(format!("保存配置失败：{e}"), cx),
        }
    }

    fn set_player(&mut self, kind: PlayerKind, cx: &mut Context<Self>) {
        if self.config.player.kind != kind {
            self.config.player.kind = kind;
            self.save_config("播放后端", cx);
        }
    }

    fn toggle_fullscreen(&mut self, cx: &mut Context<Self>) {
        self.config.player.fullscreen_on_cast = !self.config.player.fullscreen_on_cast;
        self.save_config("投屏全屏", cx);
    }

    /// Toggle the window's fullscreen state, one request at a time.
    ///
    /// gpui applies the toggle on its executor, so `is_fullscreen()` keeps
    /// reporting the old state for a frame or two. Issuing a second toggle in
    /// that window cancels the first one - which is how a "leave fullscreen"
    /// turned into a per-frame ping-pong (and why Stop sometimes left the
    /// window fullscreen).
    fn set_fullscreen(&mut self, window: &mut Window, want: bool) {
        if self.fullscreen_pending.is_some() || window.is_fullscreen() == want {
            return;
        }
        tracing::info!("fullscreen: {}", if want { "enter" } else { "leave" });
        self.fullscreen_pending = Some(want);
        window.toggle_fullscreen();
    }

    /// Make the window fullscreen for a cast that just arrived.
    ///
    /// gpui 0.2.2's `toggle_fullscreen` is a no-op on a window that is not
    /// visible (it only records a deferred placement the window has already
    /// consumed), and it repositions with `SWP_NOACTIVATE`, so a minimized
    /// window stays minimized. Both are exactly the "cast while the window is
    /// closed/minimized" case, so bring the window back first and defer the
    /// toggle to a later frame - `reveal_window` is asynchronous.
    fn enter_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window_needs_reveal(window) {
            tracing::info!("cast arrived while the window is hidden/minimized - restoring it");
            reveal_window(window);
            self.pending_fullscreen = true;
            // Keep drawing: the deferred toggle happens on a later frame.
            cx.notify();
            return;
        }
        self.alhazen_fullscreen = true;
        self.set_fullscreen(window, true);
    }

    fn toggle_tray(&mut self, cx: &mut Context<Self>) {
        self.config.ui.tray_enabled = !self.config.ui.tray_enabled;
        match self.config.save() {
            Ok(()) => {
                let label = if self.config.ui.tray_enabled {
                    "开启"
                } else {
                    "关闭"
                };
                self.set_notice(format!("系统托盘：{label}（重启应用后生效）"), cx);
            }
            Err(e) => self.set_notice(format!("保存配置失败：{e}"), cx),
        }
        cx.notify();
    }

    fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.config.ui.theme == theme {
            return;
        }
        self.config.ui.theme = theme;
        match self.config.save() {
            Ok(()) => {
                let label = match theme {
                    Theme::Auto => "跟随系统",
                    Theme::Light => "亮色",
                    Theme::Dark => "暗色",
                };
                // Theme is applied per frame, so no restart is needed.
                self.set_notice(format!("主题：{label}"), cx);
            }
            Err(e) => self.set_notice(format!("保存配置失败：{e}"), cx),
        }
        cx.notify();
    }

    fn open_config(&mut self, cx: &mut Context<Self>) {
        // Make sure the file exists before handing it to the OS.
        if let Err(e) = self.config.save() {
            self.set_notice(format!("保存配置失败：{e}"), cx);
            return;
        }
        match open_in_default_app(&Config::config_path()) {
            Ok(()) => self.set_notice("已用系统默认程序打开 config.json", cx),
            Err(e) => self.set_notice(format!("无法打开配置文件：{e}"), cx),
        }
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let enable = !self.autostart;
        match set_autostart(enable) {
            Ok(()) => {
                self.autostart = enable;
                self.set_notice(
                    if enable {
                        "已开启开机自启"
                    } else {
                        "已关闭开机自启"
                    },
                    cx,
                );
            }
            Err(e) => self.set_notice(format!("设置开机自启失败：{e}"), cx),
        }
    }

    /// Show a transient message; it clears itself after a few seconds.
    fn set_notice(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(message.into());
        self.notice_seq = self.notice_seq.wrapping_add(1);
        let seq = self.notice_seq;
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            if view.upgrade().is_none() {
                return;
            }
            // Same graceful update path as the state watcher - see
            // [`RootView::init`].
            if let Some(handle) = handle {
                let _ = cx.update_window(handle, |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if this.notice_seq == seq {
                            this.notice = None;
                            cx.notify();
                        }
                    });
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Download and decode the picture of an image cast, then install it.
    ///
    /// The fetch needs the tokio runtime (reqwest) and decoding is CPU work, so
    /// both run on the core runtime / blocking pool; the pixels come back over a
    /// oneshot channel and become a `RenderImage` inside `update_window` - the
    /// re-entrancy-safe path the state watcher uses.
    fn start_image_load(&self, uri: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let handle = self.window_handle;
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<DecodedImage, String>>();
        core.handle().spawn(async move {
            let _ = tx.send(load_image(&core, &uri).await);
        });
        cx.spawn(async move |view, cx| {
            let Ok(result) = rx.await else {
                return;
            };
            let Some(handle) = handle else {
                return;
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    match result {
                        Ok(decoded) => {
                            if let Some(old) = this.image.take() {
                                let _ = window.drop_image(old);
                            }
                            this.image = Some(Arc::new(RenderImage::new([image::Frame::new(
                                image::RgbaImage::from_raw(
                                    decoded.width,
                                    decoded.height,
                                    decoded.bgra,
                                )
                                .expect("decoded buffer matches its dimensions"),
                            )])));
                        }
                        Err(e) => tracing::warn!("picture cast failed: {e}"),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = match self.config.ui.theme {
            Theme::Light => Palette::LIGHT,
            Theme::Dark => Palette::DARK,
            Theme::Auto => match window.appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Palette::LIGHT,
                _ => Palette::DARK,
            },
        };
        set_palette(palette);

        let runtime_backend = self.core.engine.player.name();

        let s = self.snapshot.clone();
        let playing = s.transport_state == TransportState::Playing;
        let has_media = !s.uri.is_empty();

        // Let a toggle that was already issued settle before deciding
        // anything else: gpui applies it asynchronously, so the window state
        // lags by a frame or two. A pending toggle is stale once the window
        // disappears (gpui drops it for invisible windows).
        if let Some(target) = self.fullscreen_pending {
            if window.is_fullscreen() == target || window_needs_reveal(window) {
                self.fullscreen_pending = None;
            }
        }

        // Leave fullscreen when the flag is cleared (Esc) or playback ended.
        // Only the alhazen cast flow puts the window into fullscreen, so any
        // fullscreen window without the flag should exit it.
        if !self.alhazen_fullscreen && window.is_fullscreen() {
            self.set_fullscreen(window, false);
        }

        // Esc leaves the alhazen fullscreen mode - and ONLY that: nothing
        // here touches the process or the window's lifetime. gpui 0.2.2's
        // key dispatch needs a focus system this UI does not have (events
        // are dropped without a focused node), so poll the global key state
        // instead, with edge detection so a held key fires once. Polling is
        // active only while fullscreen, where the window covers the screen
        // and is therefore necessarily the foreground window.
        if self.alhazen_fullscreen {
            let down = (unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000) != 0;
            if down && !self.esc_was_down {
                self.alhazen_fullscreen = false;
            }
            self.esc_was_down = down;
        } else {
            self.esc_was_down = false;
        }
        let is_alhazen =
            runtime_backend == "alhazen" || self.config.player.kind == PlayerKind::Alhazen;
        // Both in-window backends: alhazen draws from its entity, mpv from a
        // frame rendered by libmpv.
        let is_inline = matches!(runtime_backend, "alhazen" | "mpv");
        // A picture cast has nothing for a player to do: this view fetches the
        // still, decodes it and draws it itself. Stop/Ended goes back to idle.
        let is_image = has_media
            && s.media_kind == MediaKind::Image
            && !matches!(
                s.transport_state,
                TransportState::Stopped | TransportState::NoMediaPresent
            );
        let mpv_paint: Option<MpvPaint> = (runtime_backend == "mpv" && !is_image)
            .then(|| {
                let player: Arc<dyn Player> = self.core.engine.player.clone();
                let frame = player.video_frame()?;
                Some(MpvPaint {
                    frame,
                    slot: self.mpv_slot.clone(),
                    player,
                })
            })
            .flatten();

        // Fetch (or drop) the picture of the current cast. The download and
        // decode run on the core runtime; `cast_seq` says when to load a new one.
        if is_image {
            if s.cast_seq != self.image_seq {
                self.image_seq = s.cast_seq;
                if let Some(old) = self.image.take() {
                    let _ = window.drop_image(old);
                }
                self.start_image_load(s.uri.clone(), cx);
            }
        } else if self.image_seq != 0 {
            self.image_seq = 0;
            if let Some(old) = self.image.take() {
                let _ = window.drop_image(old);
            }
        }

        // Alhazen backend: create a VideoPlayer entity for the current cast.
        // When fullscreen is enabled, make the gpui window fullscreen too.
        if is_inline && self.config.player.fullscreen_on_cast {
            // A cast that arrived while the window was hidden or minimized
            // queued its fullscreen here: `reveal_window` posts the command to
            // Windows, so the toggle only works once it has been processed.
            if self.pending_fullscreen {
                if !has_media {
                    // The cast ended before the window ever came back.
                    self.pending_fullscreen = false;
                } else if !window_needs_reveal(window) {
                    self.pending_fullscreen = false;
                    self.alhazen_fullscreen = true;
                    self.set_fullscreen(window, true);
                }
            } else if has_media && s.cast_seq != self.last_cast_seq {
                // A new cast arrived while the window was already on screen.
                if window.is_fullscreen() {
                    // Already fullscreen from an earlier cast: the window
                    // itself only has to come back on screen.
                    reveal_window(window);
                } else {
                    tracing::info!(
                        "enter fullscreen: cast_seq {} -> {}",
                        self.last_cast_seq,
                        s.cast_seq
                    );
                    self.enter_fullscreen(window, cx);
                }
            }
        }
        if is_image {
            // Nothing to decode: tear down any video player, and keep the
            // "last cast" marker current so the picture does not re-enter
            // fullscreen on every frame.
            self.alhazen_player = None;
            self.last_cast_seq = s.cast_seq;
        } else if is_alhazen {
            // A stopped/ended cast clears the "last cast" marker so that
            // re-casting the SAME uri counts as a new cast (and re-enters
            // fullscreen), not just a different uri.
            let stopped = !has_media
                || matches!(
                    s.transport_state,
                    TransportState::Stopped | TransportState::NoMediaPresent
                );
            if stopped {
                self.alhazen_player = None;
                self.alhazen_fullscreen = false;
                self.set_fullscreen(window, false);
            } else if s.cast_seq != self.last_cast_seq {
                self.last_cast_seq = s.cast_seq;
                let source = Source::parse(&s.uri)
                    .unwrap_or_else(|_| Source::parse("file:///dev/null").unwrap());
                let config = alhazen_gpui::PlayerConfig {
                    autoplay: true,
                    // GPU (DXVA) decoding for codecs both the platform and the
                    // native decoders handle; without this the software path
                    // (or ffmpeg-cli) takes 4K streams and cannot keep up.
                    prefer_hardware: true,
                    // Never silently switch to ffmpeg-cli (software) when the
                    // hardware decoder falls behind - it only looks worse.
                    auto_fallback: false,
                    // The MF decoder emits frames in bursts; a deeper queue
                    // keeps the UI fed between bursts instead of dropping.
                    frame_queue_len: 16,
                    ..Default::default()
                };
                self.alhazen_player = Some(cx.new(|cx| VideoPlayer::new(source, config, cx)));
                if let Some(player) = &self.alhazen_player {
                    player.update(cx, |p, inner_cx| p.play(inner_cx));
                    // Auto-exit fullscreen and clear player on playback end.
                    cx.subscribe(
                        player,
                        move |this, _, event: &alhazen_gpui::PlayerEvent, cx| match event {
                            alhazen_gpui::PlayerEvent::StateChanged(
                                alhazen_gpui::PlayerState::Ended,
                            ) => {
                                this.alhazen_player = None;
                                this.alhazen_fullscreen = false;
                                this.stop();
                                cx.notify();
                            }
                            alhazen_gpui::PlayerEvent::Error(e) => {
                                tracing::error!("alhazen error: {e}");
                                this.alhazen_player = None;
                                this.alhazen_fullscreen = false;
                                this.stop();
                                cx.notify();
                            }
                            alhazen_gpui::PlayerEvent::StateChanged(state) => {
                                tracing::debug!("alhazen state: {state:?}");
                            }
                            _ => {}
                        },
                    )
                    .detach();
                }
            }
        } else if is_inline {
            // mpv backend: the worker owns the video lifecycle; here we only
            // track cast boundaries and mirror fullscreen transitions. The
            // cast_seq bookkeeping matters as much as in the alhazen branch —
            // otherwise every frame re-triggers "new cast" and fights Esc.
            if s.cast_seq != self.last_cast_seq {
                self.last_cast_seq = s.cast_seq;
            }
            let stopped = !has_media
                || matches!(
                    s.transport_state,
                    TransportState::Stopped | TransportState::NoMediaPresent
                );
            if stopped {
                self.alhazen_fullscreen = false;
                self.set_fullscreen(window, false);
            }
        } else {
            self.alhazen_player = None;
            if self.alhazen_fullscreen {
                self.alhazen_fullscreen = false;
                self.set_fullscreen(window, false);
            }
        }

        // Alhazen fullscreen: render video_view covering the entire window.
        if is_alhazen && self.alhazen_fullscreen {
            if let Some(player) = &self.alhazen_player {
                return div()
                    .id("video-fullscreen")
                    .size_full()
                    .bg(rgb(0x000000))
                    .child(alhazen_gpui::video_view(player.clone()).size_full());
            }
        }
        // mpv fullscreen: same idea, drawing the libmpv frame edge to edge.
        if is_inline && !is_alhazen && self.alhazen_fullscreen {
            if let Some(mpv) = mpv_paint.clone() {
                return div()
                    .id("video-fullscreen")
                    .size_full()
                    .bg(rgb(0x000000))
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |bounds, _, window, _| {
                                paint_mpv_frame(window, bounds, &mpv);
                            },
                        )
                        .size_full(),
                    );
            }
        }

        // Picture fullscreen: the still covers the window over black.
        if is_image && self.alhazen_fullscreen {
            let img = self.image.clone();
            return div()
                .id("image-fullscreen")
                .size_full()
                .bg(rgb(0x000000))
                .child(
                    canvas(
                        move |_, _, _| {},
                        move |bounds, _, window, _| {
                            if let Some(img) = &img {
                                paint_card_image(window, bounds, img, true);
                            }
                        },
                    )
                    .size_full(),
                );
        }

        let mut root = div()
            .id("app-root")
            .relative()
            .size_full()
            .bg(rgb(p().bg))
            .text_color(rgb(p().text))
            .flex()
            .flex_col()
            // Dropping a local media file plays it directly. gpui maps OS file
            // drops onto its internal drag payload (`ExternalPaths`).
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                if let Some(path) = paths.paths().first() {
                    this.play_path(path.clone(), cx);
                }
            }))
            .child(title_bar(&s, self.show_settings, cx))
            .child(
                div().flex_1().flex().flex_col().items_center().p_6().child(
                    div()
                        .w_full()
                        .max_w(px(1100.0))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap_5()
                        .child(hero_card(
                            &s,
                            playing,
                            has_media,
                            HeroPicture {
                                alhazen: self.alhazen_player.clone(),
                                mpv: mpv_paint,
                                image: self.image.clone(),
                            },
                            self.seek_bounds.clone(),
                            cx,
                        ))
                        .child(transport_dock(
                            &s,
                            playing,
                            has_media,
                            self.volume_bounds.clone(),
                            cx,
                        ))
                        .child(footer(&s, runtime_backend)),
                ),
            );

        if self.show_settings {
            root = root.child(backdrop(cx)).child(settings_panel(
                &self.config,
                s.port,
                &s.ips,
                self.autostart,
                runtime_backend,
                &self.update_stage,
                cx,
            ));
        }

        if let Some(notice) = &self.notice {
            root = root.child(toast(notice.clone(), cx));
        }

        root
    }
}

// ---- layout pieces -------------------------------------------------------

fn title_bar(s: &PlaybackSnapshot, show_settings: bool, cx: &mut Context<RootView>) -> Div {
    let device = if s.friendly_name.is_empty() {
        "…".to_string()
    } else {
        s.friendly_name.clone()
    };

    // Only the left region starts a window move; buttons must stay clickable.
    let drag_region = div()
        .flex()
        .items_center()
        .flex_1()
        .h_full()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, _| {
                let now = std::time::Instant::now();
                let double = this
                    .last_title_click
                    .map(|last| now.duration_since(last) < Duration::from_millis(400))
                    .unwrap_or(false);
                this.last_title_click = if double { None } else { Some(now) };
                if double {
                    toggle_maximize(window);
                } else {
                    begin_window_drag(window);
                }
            }),
        )
        .child(brand_mark())
        .child(
            div()
                .ml_2()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Macast"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(rgb(p().dim))
                        .truncate()
                        .child(device),
                ),
        )
        .child(div().flex_1());

    div()
        .h(px(46.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .pl_4()
        .pr_2()
        .bg(rgb(p().bg))
        .border_b_1()
        .border_color(rgba(p().border))
        .child(drag_region)
        .child(status_inline(s))
        .child(div().ml_2().child(icon_button(
            "settings",
            Glyph::Sliders,
            32.0,
            if show_settings {
                rgb(p().accent)
            } else {
                rgb(p().muted)
            },
            if show_settings {
                rgba(p().accent_soft)
            } else {
                rgba(p().hover)
            },
            cx.listener(|this, _, _, cx| {
                this.show_settings = !this.show_settings;
                cx.notify();
            }),
        )))
        .child(divider(px(18.0)))
        .child(icon_button(
            "win-min",
            Glyph::Minimize,
            30.0,
            rgb(p().muted),
            rgba(p().hover),
            |_, window, _| window.minimize_window(),
        ))
        .child(icon_button(
            "win-max",
            Glyph::Maximize,
            30.0,
            rgb(p().muted),
            rgba(p().hover),
            |_, window, _| toggle_maximize(window),
        ))
        .child(icon_button(
            "win-close",
            Glyph::Close,
            30.0,
            rgb(p().muted),
            rgba(p().close_hover),
            |_, window, _| {
                #[cfg(target_os = "windows")]
                if crate::tray::hide_to_tray(window) {
                    return;
                }
                window.remove_window();
            },
        ))
}

#[cfg(target_os = "windows")]
pub(crate) fn window_handle_of(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(handle) = handle.as_raw() {
            return Some(windows::Win32::Foundation::HWND(
                handle.hwnd.get() as *mut core::ffi::c_void
            ));
        }
    }
    None
}

/// Whether the window is in a state gpui 0.2.2 cannot make fullscreen: hidden
/// (closed to the tray) or minimized. `WindowsWindow::toggle_fullscreen` bails
/// out when `IsWindowVisible` is false, and it positions the window with
/// `SWP_NOACTIVATE`, so a minimized window keeps sitting in the taskbar.
#[cfg(target_os = "windows")]
fn window_needs_reveal(window: &Window) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
    match window_handle_of(window) {
        Some(hwnd) => unsafe { IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() },
        None => false,
    }
}

#[cfg(not(target_os = "windows"))]
fn window_needs_reveal(_window: &Window) -> bool {
    false
}

/// Ask Windows to bring a hidden/minimized window back onto the screen.
///
/// Asynchronous on purpose: the synchronous `ShowWindow` re-enters gpui's
/// message handler while it still holds state borrows (RefCell panic) and the
/// surface never resizes. Both commands are posted in order, so a window that
/// is hidden *and* minimized ends up shown and restored.
#[cfg(target_os = "windows")]
fn reveal_window(window: &Window) {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, ShowWindowAsync, SW_RESTORE, SW_SHOW,
    };
    let Some(hwnd) = window_handle_of(window) else {
        return;
    };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOW);
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn reveal_window(_window: &Window) {}

fn toggle_maximize(window: &mut Window) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            IsZoomed, ShowWindowAsync, SW_MAXIMIZE, SW_RESTORE,
        };
        if let Some(hwnd) = window_handle_of(window) {
            // ShowWindowAsync: the synchronous variant re-enters gpui's message
            // handler while it still holds state borrows (RefCell panic) and
            // the surface never resizes.
            let command = if unsafe { IsZoomed(hwnd).as_bool() } {
                SW_RESTORE
            } else {
                SW_MAXIMIZE
            };
            unsafe {
                let _ = ShowWindowAsync(hwnd, command);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        window.zoom_window();
    }
}

#[cfg(target_os = "windows")]
fn begin_window_drag(window: &mut Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, HTCAPTION, WM_NCLBUTTONDOWN};

    // gpui 0.2.2's Windows backend does not implement `start_window_move`, so
    // hand the drag to DefWindowProc exactly like a native caption would.
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(handle) = handle.as_raw() {
            unsafe {
                let _ = ReleaseCapture();
                SendMessageW(
                    HWND(handle.hwnd.get() as *mut core::ffi::c_void),
                    WM_NCLBUTTONDOWN,
                    Some(WPARAM(HTCAPTION as usize)),
                    Some(LPARAM(0)),
                );
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn begin_window_drag(window: &mut Window) {
    window.start_window_move();
}

fn brand_mark() -> Div {
    div()
        .size(px(24.0))
        .rounded_lg()
        .bg(linear_gradient(
            140.0,
            linear_color_stop(rgb(p().brand_from), 0.0),
            linear_color_stop(rgb(p().brand_to), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph_icon(Glyph::Play, 13.0, rgb(p().on_accent)))
}

/// The three ways a picture can reach the hero card: an alhazen `VideoPlayer`
/// entity, a libmpv frame, or a still decoded by this view. At most one is
/// ever `Some` - the backend and the media kind decide which.
struct HeroPicture {
    alhazen: Option<gpui::Entity<VideoPlayer>>,
    mpv: Option<MpvPaint>,
    image: Option<Arc<RenderImage>>,
}

fn hero_card(
    s: &PlaybackSnapshot,
    playing: bool,
    has_media: bool,
    picture: HeroPicture,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    cx: &mut Context<RootView>,
) -> Div {
    let HeroPicture {
        alhazen: alhazen_player,
        mpv: mpv_paint,
        image,
    } = picture;
    let frac = if s.duration > 0.0 {
        (s.position / s.duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let cell = seek_bounds.clone();

    let title = if has_media {
        s.title.clone()
    } else {
        "等待投屏".to_string()
    };
    let subtitle = if has_media {
        s.uri.clone()
    } else {
        "在手机上点“投屏 / Cast”，或把本地视频拖进这个窗口".to_string()
    };

    let progress = div()
        .id("seek-bar")
        .h(px(18.0))
        .w_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .on_click(cx.listener(|this, event: &ClickEvent, _, _| {
            let bounds = this.seek_bounds.get();
            if bounds.size.width > px(1.0) {
                let fraction = ((event.position().x - bounds.origin.x) / bounds.size.width)
                    .clamp(0.0, 1.0) as f64;
                this.seek_to(fraction);
            }
        }))
        .child(
            canvas(
                move |bounds, _, _| {
                    cell.set(bounds);
                },
                move |bounds, _, window, _| {
                    let track_h = px(6.0);
                    let track = Bounds::new(
                        point(
                            bounds.origin.x,
                            bounds.origin.y + (bounds.size.height - track_h) / 2.0,
                        ),
                        size(bounds.size.width, track_h),
                    );
                    window.paint_quad(fill(track, rgba(p().border)));
                    if frac > 0.0 {
                        let filled = Bounds::new(
                            track.origin,
                            size(track.size.width * frac, track.size.height),
                        );
                        window.paint_quad(fill(filled, rgb(p().accent)));
                    }
                },
            )
            .w_full()
            .h_full(),
        );

    let mut info = div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap_2()
        .overflow_hidden()
        .child(div().flex().items_center().child(status_chip(s)))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(rgb(p().muted))
                .truncate()
                .child(subtitle),
        )
        .when(s.duration > 0.0, |this| this.child(progress));

    if s.duration > 0.0 {
        info = info.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(11.0))
                .text_color(rgb(p().dim))
                .child(fmt_time(s.position))
                .child(format!("-{}", fmt_time((s.duration - s.position).max(0.0)))),
        );
    } else if has_media {
        info = info.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(p().dim))
                .child(format!("已播放 {}", fmt_time(s.position))),
        );
    } else {
        info = info.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(p().dim))
                .child("① 手机与电脑连同一个 Wi-Fi   ② 视频 App 里点投屏   ③ 选择本机"),
        );
    }

    // While a picture is on screen it owns the whole hero card: no leftover
    // idle artwork/metadata stacked over the video. Idle keeps the old layout.
    //
    // The card's own background is the idle gradient, not black: both backends
    // paint only the picture area and leave the letterbox pixels untouched
    // (alhazen paints just the fitted rect, libmpv leaves them transparent), so
    // the bars show the card colour instead of hard black edges.
    let card = div()
        .flex_1()
        .min_h(px(240.0))
        .max_h(px(440.0))
        .overflow_hidden()
        .rounded_2xl()
        .border_1()
        .border_color(rgba(p().border))
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(p().hero_from), 0.0),
            linear_color_stop(rgb(p().hero_to), 1.0),
        ));

    // Picture cast: the decoded still owns the card, bars show the card colour.
    if let Some(img) = image {
        return card.flex().items_center().justify_center().child(
            canvas(
                move |_, _, _| {},
                move |bounds, _, window, _| {
                    // A window left fullscreen while the flag is already cleared
                    // still has to be noticed, so keep the frames coming then.
                    let keep_repainting = window.is_fullscreen();
                    paint_card_image(window, bounds, &img, keep_repainting);
                },
            )
            .size_full(),
        );
    }
    if let Some(player) = alhazen_player {
        return card
            .flex()
            .items_center()
            .justify_center()
            .child(alhazen_gpui::video_view(player).size_full());
    }
    // mpv backend: the picture arrives as a raw BGRA frame from libmpv.
    if let Some(mpv) = mpv_paint {
        return card.flex().items_center().justify_center().child(
            canvas(
                move |_, _, _| {},
                move |bounds, _, window, _| {
                    paint_mpv_frame(window, bounds, &mpv);
                },
            )
            .size_full(),
        );
    }

    card.flex()
        .items_center()
        .gap_6()
        .p_6()
        .child(artwork(s, playing))
        .child(info)
}

/// A frame from the libmpv worker plus the bookkeeping needed to paint it.
#[derive(Clone)]
struct MpvPaint {
    frame: VideoFrame,
    slot: Rc<Cell<Option<Arc<RenderImage>>>>,
    player: Arc<dyn Player>,
}

/// Draws one libmpv frame over `bounds`, retiring the previous image from the
/// sprite atlas and asking for the next animation frame.
fn paint_mpv_frame(window: &mut Window, bounds: Bounds<Pixels>, mpv: &MpvPaint) {
    use std::sync::atomic::{AtomicU64, Ordering};
    let MpvPaint {
        frame,
        slot,
        player,
    } = mpv;
    let scale = window.scale_factor();
    let w = (f32::from(bounds.size.width) * scale).round().max(2.0) as u32;
    let h = (f32::from(bounds.size.height) * scale).round().max(2.0) as u32;
    // Log when the target changes (window resize, fullscreen toggle) - the
    // frame the worker hands over follows one step behind, which shows up
    // here as a temporary w/h mismatch.
    static LAST: AtomicU64 = AtomicU64::new(0);
    let packed = (u64::from(w) << 32) | u64::from(h);
    if LAST.swap(packed, Ordering::Relaxed) != packed {
        tracing::info!(
            "mpv paint: target {w}x{h}, frame {}x{}",
            frame.width,
            frame.height
        );
    }
    // Let mpv render at the size we actually show.
    player.set_render_size(w, h);
    let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra.to_vec())
    else {
        return;
    };
    let img = Arc::new(RenderImage::new([image::Frame::new(buffer)]));
    // Drop the previous one so the sprite atlas does not grow per frame.
    if let Some(old) = slot.replace(Some(img.clone())) {
        let _ = window.drop_image(old);
    }
    let _ = window.paint_image(bounds, Corners::default(), img, 0, false);
    window.request_animation_frame();
}

/// Draws a decoded picture inside `bounds`, preserving its aspect ratio; the
/// card background shows through where the picture does not reach.
///
/// `keep_repainting` matters because Esc-to-windowed is polled from `render`:
/// a still never changes, so unless we ask for another frame the repaint loop
/// stops - and a window that is still fullscreen would then never be noticed.
fn paint_card_image(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    img: &Arc<RenderImage>,
    keep_repainting: bool,
) {
    let target = ObjectFit::Contain.get_bounds(bounds, img.size(0));
    let _ = window.paint_image(target, Corners::default(), img.clone(), 0, false);
    if keep_repainting {
        window.request_animation_frame();
    }
}

/// A decoded picture, in the byte order gpui's sprite atlas expects (BGRA).
struct DecodedImage {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

/// Longest side we hand to gpui: the sprite atlas caps a texture at 16384px, and
/// an uncapped 24MP photo would also waste memory and upload time.
const MAX_IMAGE_SIDE: u32 = 4096;

/// Fetch and decode a picture for an image cast.
async fn load_image(core: &Arc<MacastCore>, uri: &str) -> Result<DecodedImage, String> {
    let bytes = core.fetch_bytes(uri).await.map_err(|e| format!("{e:#}"))?;
    tokio::task::spawn_blocking(move || decode_image(&bytes))
        .await
        .map_err(|e| format!("decode task failed: {e}"))?
}

/// Decode a picture and convert it to the byte order gpui's atlas wants.
///
/// gpui's sprite atlas is BGRA on every platform (`B8G8R8A8` / `BGRA8`), so -
/// exactly like gpui's own image loader - swap R and B before handing the frame
/// over; otherwise red and blue come out exchanged.
fn decode_image(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut img =
        image::load_from_memory(bytes).map_err(|e| format!("unsupported picture: {e}"))?;
    if img.width().max(img.height()) > MAX_IMAGE_SIDE {
        img = img.resize(
            MAX_IMAGE_SIDE,
            MAX_IMAGE_SIDE,
            image::imageops::FilterType::Triangle,
        );
    }
    let (width, height) = (img.width(), img.height());
    let mut bgra = img.into_rgba8().into_raw();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(DecodedImage {
        width,
        height,
        bgra,
    })
}

fn artwork(s: &PlaybackSnapshot, playing: bool) -> Div {
    let glyph = if playing {
        Glyph::Equalizer
    } else if s.transport_state == TransportState::PausedPlayback {
        Glyph::Play
    } else {
        Glyph::Cast
    };

    div()
        .relative()
        .size(px(112.0))
        .flex_shrink_0()
        .rounded_2xl()
        .overflow_hidden()
        .bg(linear_gradient(
            150.0,
            linear_color_stop(rgb(p().art_from), 0.0),
            linear_color_stop(rgb(p().art_to), 1.0),
        ))
        .border_1()
        .border_color(rgba(p().border_strong))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .size(px(86.0))
                .rounded_full()
                .border_1()
                .border_color(rgba(p().hover)),
        )
        .child(
            div()
                .absolute()
                .size(px(60.0))
                .rounded_full()
                .border_1()
                .border_color(rgba(p().divider)),
        )
        .child(glyph_icon(glyph, 34.0, rgb(p().on_art)))
}

fn transport_dock(
    s: &PlaybackSnapshot,
    playing: bool,
    has_media: bool,
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    cx: &mut Context<RootView>,
) -> Div {
    let vol_frac = (s.volume as f32 / 100.0).clamp(0.0, 1.0);

    let volume_cell = volume_bounds.clone();
    let volume_bar = div()
        .id("volume-bar")
        .w(px(112.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .on_click(cx.listener(|this, event: &ClickEvent, _, _| {
            let bounds = this.volume_bounds.get();
            if bounds.size.width > px(1.0) {
                let fraction =
                    ((event.position().x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
                this.set_volume((fraction * 100.0).round() as u8);
            }
        }))
        .child(
            canvas(
                move |bounds, _, _| {
                    volume_cell.set(bounds);
                },
                move |bounds, _, window, _| {
                    let track_h = px(4.0);
                    let track = Bounds::new(
                        point(
                            bounds.origin.x,
                            bounds.origin.y + (bounds.size.height - track_h) / 2.0,
                        ),
                        size(bounds.size.width, track_h),
                    );
                    window.paint_quad(fill(track, rgba(p().border)));
                    if vol_frac > 0.0 {
                        let filled = Bounds::new(
                            track.origin,
                            size(track.size.width * vol_frac, track.size.height),
                        );
                        window.paint_quad(fill(filled, rgb(p().accent)));
                    }
                },
            )
            .w_full()
            .h_full(),
        );

    div()
        .h(px(64.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .rounded_2xl()
        .bg(rgb(p().surface))
        .border_1()
        .border_color(rgba(p().border))
        .child(seek_step_button("seek-back", Direction::Back, cx))
        .child(
            div()
                .id("play-pause")
                .size(px(46.0))
                .rounded_full()
                .bg(if has_media {
                    rgb(p().accent)
                } else {
                    rgb(p().surface_hi)
                })
                .when(has_media, |this| {
                    this.hover(|s| s.bg(rgb(p().accent_hover)))
                        .active(|s| s.bg(rgb(p().accent_pressed)))
                })
                .when(!has_media, |this| {
                    this.border_1().border_color(rgba(p().border_strong))
                })
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(glyph_icon(
                    if playing { Glyph::Pause } else { Glyph::Play },
                    18.0,
                    if has_media {
                        rgb(p().on_accent)
                    } else {
                        rgb(p().muted)
                    },
                ))
                .on_click(cx.listener(|this, _, _, _| {
                    if this.snapshot.transport_state == TransportState::Playing {
                        this.pause();
                    } else {
                        this.play();
                    }
                })),
        )
        .child(seek_step_button("seek-fwd", Direction::Forward, cx))
        .child(icon_button(
            "stop",
            Glyph::Stop,
            38.0,
            rgb(p().muted),
            rgba(p().hover),
            cx.listener(|this, _, _, _| this.stop()),
        ))
        .child(divider(px(22.0)))
        .child(icon_button(
            "mute",
            if s.mute { Glyph::Mute } else { Glyph::Volume },
            38.0,
            if s.mute {
                rgb(p().warn)
            } else {
                rgb(p().muted)
            },
            rgba(p().hover),
            cx.listener(|this, _, _, _| this.toggle_mute()),
        ))
        .child(volume_bar)
        .child(
            div()
                .w(px(38.0))
                .text_size(px(11.0))
                .text_color(rgb(p().muted))
                .child(format!("{}%", s.volume)),
        )
        .child(div().flex_1())
        .child(text_button(
            "copy-link",
            "复制链接",
            Some(Glyph::Copy),
            cx.listener(|this, _, _, cx| this.copy_link(cx)),
        ))
}

fn footer(s: &PlaybackSnapshot, runtime_backend: &str) -> Div {
    let addresses = if s.ips.is_empty() {
        "未找到可用网卡".to_string()
    } else {
        s.ips.join("  ·  ")
    };
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_1()
        .text_size(px(11.0))
        .text_color(rgb(p().dim))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .truncate()
                .child(status_dot(s.transport_state))
                .child(format!("端口 {}  ·  {}", s.port, addresses)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .child(format!("{runtime_backend} 后端")),
        )
}

fn backdrop(cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("settings-backdrop")
        .absolute()
        .top(px(46.0))
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .bg(rgba(p().backdrop))
        .on_click(cx.listener(|this, _, _, cx| {
            this.show_settings = false;
            cx.notify();
        }))
}

fn settings_panel(
    config: &Config,
    actual_port: u16,
    ips: &[String],
    autostart: bool,
    runtime_backend: &str,
    update_stage: &UpdateStage,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    // mpv is loaded in-process (libmpv) and rendered inside the window; the
    // DLL has to be reachable as `libmpv-2.dll` (next to the exe or on PATH).
    let libmpv_path = config
        .player
        .libmpv_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "自动（PATH 中的 libmpv-2.dll）".to_string());

    // 播放 section: mpv 相关行只在 mpv 后端显示。
    let play_rows: Vec<Div> = {
        let mut rows = vec![
            setting_row("当前运行", div().child(runtime_backend.to_string())),
            setting_row("启动后端", segmented_player(config.player.kind, cx)),
        ];
        match config.player.kind {
            PlayerKind::Alhazen => rows.push(
                div()
                    .text_size(px(10.5))
                    .line_height(px(16.0))
                    .text_color(rgb(p().dim))
                    .child("进程内解码，画面直接画在窗口内；H.264/AAC 开箱可用，HEVC 需系统「HEVC 视频扩展」。"),
            ),
            PlayerKind::Mpv => {
                rows.push(setting_row(
                    "libmpv",
                    div()
                        .w(px(190.0))
                        .truncate()
                        .text_color(rgb(p().muted))
                        .child(libmpv_path),
                ));
                rows.push(
                    div()
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(rgb(p().dim))
                        .child("需要 libmpv-2.dll 在 exe 同目录或 PATH 中；画面直接画在窗口内。"),
                );
            }
        }
        rows.push(setting_row(
            "投屏时全屏",
            switch(
                "switch-fullscreen",
                config.player.fullscreen_on_cast,
                cx.listener(|this, _, _, cx| this.toggle_fullscreen(cx)),
            ),
        ));
        rows
    };

    let body = div()
        .id("settings-scroll")
        .flex_1()
        .overflow_y_scroll()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(section("播放", play_rows))
        .child(section(
            "外观",
            vec![setting_row("主题", segmented_theme(config.ui.theme, cx))],
        ))
        .child(section(
            "系统",
            vec![
                setting_row(
                    "系统托盘",
                    switch(
                        "switch-tray",
                        config.ui.tray_enabled,
                        cx.listener(|this, _, _, cx| this.toggle_tray(cx)),
                    ),
                ),
                setting_row(
                    "开机自启",
                    switch(
                        "switch-autostart",
                        autostart,
                        cx.listener(|this, _, _, cx| this.toggle_autostart(cx)),
                    ),
                ),
                setting_row(
                    "配置文件",
                    text_button(
                        "open-config",
                        "打开",
                        None,
                        cx.listener(|this, _, _, cx| this.open_config(cx)),
                    ),
                ),
            ],
        ))
        .child(section(
            "更新",
            update_rows(config.update.check_on_startup, update_stage, cx),
        ))
        .child(section(
            "设备",
            vec![
                setting_row("名称", div().child(config.device.friendly_name.clone())),
                setting_row(
                    "端口",
                    div().child(if config.device.port == actual_port {
                        format!("{actual_port}")
                    } else {
                        format!("{} → {}", config.device.port, actual_port)
                    }),
                ),
                setting_row(
                    "地址",
                    div().truncate().child(if ips.is_empty() {
                        "—".to_string()
                    } else {
                        ips.join("  ·  ")
                    }),
                ),
            ],
        ))
        .child(
            div()
                .pt_2()
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(rgb(p().dim))
                .child("名称、端口、mpv 路径请编辑 config.json 后重启应用；后端切换重启后生效。"),
        );

    div()
        .id("settings-panel")
        .absolute()
        .top(px(46.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .w(px(348.0))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(p().panel_from), 0.0),
            linear_color_stop(rgb(p().panel_to), 1.0),
        ))
        .border_l_1()
        .border_color(rgba(p().border))
        .flex()
        .flex_col()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .h(px(56.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_between()
                .px_6()
                .border_b_1()
                .border_color(rgba(p().border))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("设置"),
                )
                .child(icon_button(
                    "close-settings",
                    Glyph::Close,
                    30.0,
                    rgb(p().muted),
                    rgba(p().hover),
                    cx.listener(|this, _, _, cx| {
                        this.show_settings = false;
                        cx.notify();
                    }),
                )),
        )
        .child(body)
}

/// Rows of the 更新 section: what we run, when we look for updates, and the
/// result of the last check / download.
fn update_rows(
    check_on_startup: bool,
    stage: &UpdateStage,
    cx: &mut Context<RootView>,
) -> Vec<Div> {
    let busy = matches!(
        stage,
        UpdateStage::Checking | UpdateStage::Downloading { .. } | UpdateStage::Installing(_)
    );

    let check: AnyElement = if busy {
        div()
            .text_size(px(11.5))
            .text_color(rgb(p().dim))
            .child("…")
            .into_any_element()
    } else {
        text_button(
            "check-update",
            "检查",
            None,
            cx.listener(|this, _, _, cx| this.check_update(false, cx)),
        )
        .into_any_element()
    };

    vec![
        setting_row(
            "当前版本",
            div().child(format!("v{}", update::current_version())),
        ),
        setting_row(
            "启动时检查",
            switch(
                "switch-check-updates",
                check_on_startup,
                cx.listener(|this, _, _, cx| this.toggle_update_check_on_startup(cx)),
            ),
        ),
        setting_row("检查更新", check),
        update_status(stage, cx),
    ]
}

/// One line under the check button: the result, the download progress, or the
/// release notes plus the install button.
fn update_status(stage: &UpdateStage, cx: &mut Context<RootView>) -> Div {
    let line = |text: String, color: u32| {
        div()
            .text_size(px(11.0))
            .line_height(px(17.0))
            .text_color(rgb(color))
            .child(text)
    };

    match stage {
        UpdateStage::Idle => line("尚未检查更新".to_string(), p().dim),
        UpdateStage::Checking => line("正在检查…".to_string(), p().dim),
        UpdateStage::UpToDate => line("已是最新版本".to_string(), p().success),
        UpdateStage::Failed(e) => line(e.clone(), p().warn),
        UpdateStage::Installing(version) => {
            line(format!("正在安装 v{version}，应用即将重启"), p().accent)
        }
        UpdateStage::Downloading {
            received, total, ..
        } => {
            let text = match total {
                Some(total) if *total > 0 => format!(
                    "下载中 {:.0}%（{} / {}）",
                    *received as f64 * 100.0 / *total as f64,
                    update::human_size(*received),
                    update::human_size(*total)
                ),
                _ => format!("下载中 {}…", update::human_size(*received)),
            };
            line(text, p().accent)
        }
        UpdateStage::Available(update) => {
            let mut column = div().flex().flex_col().gap_2().child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(p().accent))
                    .child(format!("发现新版本 v{}", update.version)),
            );
            if let Some(notes) = update
                .body
                .as_deref()
                .map(str::trim)
                .filter(|notes| !notes.is_empty())
            {
                column = column.child(
                    div()
                        .max_h(px(88.0))
                        .overflow_hidden()
                        .text_size(px(11.0))
                        .line_height(px(17.0))
                        .text_color(rgb(p().dim))
                        .child(notes.chars().take(240).collect::<String>()),
                );
            }
            column.child(text_button(
                "install-update",
                "下载并安装",
                None,
                cx.listener(|this, _, _, cx| this.install_update(cx)),
            ))
        }
    }
    .pt_1()
}

fn section(label: &'static str, rows: Vec<Div>) -> Div {
    let mut column = div().flex().flex_col().child(
        div()
            .pb_2()
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(p().dim))
            .child(label),
    );
    for row in rows {
        column = column.child(row);
    }
    column
}

fn setting_row(label: &'static str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .min_h(px(46.0))
        .border_b_1()
        .border_color(rgba(p().divider))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(12.0))
                .text_color(rgb(p().muted))
                .child(label),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_size(px(12.5))
                .text_color(rgb(p().text))
                .child(value),
        )
}

fn toast(message: String, cx: &mut Context<RootView>) -> Div {
    div()
        .absolute()
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(18.0))
        .flex()
        .justify_center()
        .child(
            div()
                .id("toast")
                .px_4()
                .py_2()
                .rounded_full()
                .bg(rgb(p().toast_bg))
                .border_1()
                .border_color(rgba(p().accent_border))
                .text_size(px(12.0))
                .text_color(rgb(p().text))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.notice = None;
                    cx.notify();
                }))
                .child(message),
        )
}

// ---- small components ----------------------------------------------------

fn status_dot(state: TransportState) -> Div {
    let color = match state {
        TransportState::Playing => p().success,
        TransportState::PausedPlayback => p().warn,
        _ => p().dim,
    };
    div().size(px(6.0)).rounded_full().bg(rgb(color))
}

fn status_inline(s: &PlaybackSnapshot) -> Div {
    let (label, color) = match s.transport_state {
        TransportState::Playing => ("正在播放", p().success),
        TransportState::PausedPlayback => ("已暂停", p().warn),
        TransportState::Stopped => ("已停止", p().muted),
        TransportState::NoMediaPresent => ("待机", p().muted),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(status_dot(s.transport_state))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(color))
                .child(label),
        )
}

fn status_chip(s: &PlaybackSnapshot) -> Div {
    let (label, color) = match s.transport_state {
        TransportState::Playing => ("正在播放", p().success),
        TransportState::PausedPlayback => ("已暂停", p().warn),
        TransportState::Stopped => ("已停止", p().muted),
        TransportState::NoMediaPresent => ("待机", p().muted),
    };
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(24.0))
        .px_2p5()
        .rounded_full()
        .bg(rgba((color << 8) | 0x1F))
        .text_size(px(11.0))
        .text_color(rgb(color))
        .child(status_dot(s.transport_state))
        .child(label)
}

fn divider(height: Pixels) -> Div {
    div().w(px(1.0)).h(height).bg(rgba(p().divider))
}

fn segmented_player(current: PlayerKind, cx: &mut Context<RootView>) -> Div {
    let option = |id: &'static str,
                  label: &'static str,
                  active: bool,
                  kind: PlayerKind,
                  cx: &mut Context<RootView>| {
        let mut button = div()
            .id(id)
            .px(px(10.0))
            .h(px(24.0))
            .rounded_md()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(11.5))
            .text_color(if active {
                rgb(p().text)
            } else {
                rgb(p().muted)
            })
            .when(active, |this| this.bg(rgb(p().segment_active)))
            .when(!active, |this| this.hover(|s| s.bg(rgba(p().hover))))
            .child(label);
        if !active {
            button = button.on_click(cx.listener(move |this, _, _, cx| {
                this.set_player(kind, cx);
            }));
        }
        button
    };

    div()
        .flex()
        .items_center()
        .gap_1()
        .p(px(2.0))
        .rounded_lg()
        .bg(rgb(p().inset))
        .border_1()
        .border_color(rgba(p().border))
        .child(option(
            "player-mpv",
            "mpv",
            current == PlayerKind::Mpv,
            PlayerKind::Mpv,
            cx,
        ))
        .child(option(
            "player-alhazen",
            "alhazen",
            current == PlayerKind::Alhazen,
            PlayerKind::Alhazen,
            cx,
        ))
}

fn segmented_theme(current: Theme, cx: &mut Context<RootView>) -> Div {
    let option = |id: &'static str,
                  label: &'static str,
                  active: bool,
                  theme: Theme,
                  cx: &mut Context<RootView>| {
        let mut button = div()
            .id(id)
            .px(px(9.0))
            .h(px(24.0))
            .rounded_md()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(11.5))
            .text_color(if active {
                rgb(p().text)
            } else {
                rgb(p().muted)
            })
            .when(active, |this| this.bg(rgb(p().segment_active)))
            .when(!active, |this| this.hover(|s| s.bg(rgba(p().hover))))
            .child(label);
        if !active {
            button = button.on_click(cx.listener(move |this, _, _, cx| {
                this.set_theme(theme, cx);
            }));
        }
        button
    };

    div()
        .flex()
        .items_center()
        .gap_1()
        .p(px(2.0))
        .rounded_lg()
        .bg(rgb(p().inset))
        .border_1()
        .border_color(rgba(p().border))
        .child(option(
            "theme-auto",
            "自动",
            current == Theme::Auto,
            Theme::Auto,
            cx,
        ))
        .child(option(
            "theme-light",
            "亮色",
            current == Theme::Light,
            Theme::Light,
            cx,
        ))
        .child(option(
            "theme-dark",
            "暗色",
            current == Theme::Dark,
            Theme::Dark,
            cx,
        ))
}

fn switch(
    id: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(38.0))
        .h(px(22.0))
        .p(px(2.0))
        .rounded_full()
        .bg(if on {
            rgb(p().accent)
        } else {
            rgb(p().switch_off)
        })
        .flex()
        .items_center()
        .when(on, |this| this.justify_end())
        .when(!on, |this| this.justify_start())
        .cursor_pointer()
        .hover(|this| this.opacity(0.9))
        .child(
            div()
                .size(px(18.0))
                .rounded_full()
                .bg(rgb(p().knob))
                .border_1()
                .border_color(rgba(p().border)),
        )
        .on_click(on_click)
}

enum Direction {
    Back,
    Forward,
}

fn seek_step_button(
    id: &'static str,
    direction: Direction,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let (glyph, delta) = match direction {
        Direction::Back => (Glyph::ChevronLeft, -10.0),
        Direction::Forward => (Glyph::ChevronRight, 10.0),
    };
    div()
        .id(id)
        .h(px(38.0))
        .px_2()
        .rounded_lg()
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .hover(|this| this.bg(rgba(p().hover)))
        .active(|this| this.bg(rgba(p().active)))
        .text_size(px(11.0))
        .text_color(rgb(p().muted))
        .child(glyph_icon(glyph, 12.0, rgb(p().muted)))
        .child("10s")
        .on_click(cx.listener(move |this, _, _, _| this.nudge_position(delta)))
}

fn text_button(
    id: &'static str,
    label: impl IntoElement + 'static,
    glyph: Option<Glyph>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .h(px(32.0))
        .px_3()
        .rounded_lg()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(rgb(p().muted))
        .hover(|this| this.bg(rgba(p().hover)).text_color(rgb(p().text)))
        .active(|this| this.bg(rgba(p().active)))
        .on_click(on_click);
    if let Some(glyph) = glyph {
        button = button.child(glyph_icon(glyph, 13.0, rgb(p().muted)));
    }
    button.child(label)
}

fn icon_button(
    id: &'static str,
    glyph: Glyph,
    size: f32,
    icon_color: impl Into<Hsla> + Copy,
    hover_bg: impl Into<Hsla> + Copy,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let icon_color: Hsla = icon_color.into();
    let hover_bg: Hsla = hover_bg.into();
    div()
        .id(id)
        .size(px(size))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |this| this.bg(hover_bg))
        .active(move |this| this.bg(rgba(p().active)))
        .child(glyph_icon(glyph, size * 0.46, icon_color))
        .on_click(on_click)
}

// ---- vector glyphs (gpui has no icon set) -------------------------------

#[derive(Clone, Copy)]
enum Glyph {
    Play,
    Pause,
    Stop,
    Volume,
    Mute,
    Sliders,
    Close,
    Minimize,
    Maximize,
    ChevronLeft,
    ChevronRight,
    Copy,
    Cast,
    Equalizer,
}

fn glyph_icon(glyph: Glyph, size: f32, color: impl Into<Hsla> + Copy) -> impl IntoElement {
    let color = color.into();
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| paint_glyph(window, bounds, glyph, color),
    )
    .size(px(size))
}

fn paint_path_result<E>(window: &mut Window, path: Result<gpui::Path<Pixels>, E>, color: Hsla) {
    if let Ok(path) = path {
        window.paint_path(path, color);
    }
}

fn paint_glyph(window: &mut Window, bounds: Bounds<Pixels>, glyph: Glyph, color: Hsla) {
    let scale: f32 = bounds.size.width.into();
    let scale = scale.min(f32::from(bounds.size.height));
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let p = |x: f32, y: f32| point(px(ox + x * scale), px(oy + y * scale));
    let quad = |x: f32, y: f32, w: f32, h: f32| {
        fill(
            Bounds::new(p(x, y), size(px(w * scale), px(h * scale))),
            color,
        )
    };
    let stroke = (scale * 0.1).clamp(1.2, 2.2);

    match glyph {
        Glyph::Play => {
            let points = [p(0.30, 0.18), p(0.82, 0.50), p(0.30, 0.82)];
            let mut path = PathBuilder::fill();
            path.add_polygon(&points, true);
            paint_path_result(window, path.build(), color);
        }
        Glyph::Pause => {
            window.paint_quad(quad(0.28, 0.20, 0.16, 0.60));
            window.paint_quad(quad(0.56, 0.20, 0.16, 0.60));
        }
        Glyph::Stop => {
            window.paint_quad(quad(0.26, 0.26, 0.48, 0.48));
        }
        Glyph::Volume => {
            let points = [
                p(0.10, 0.40),
                p(0.27, 0.40),
                p(0.45, 0.24),
                p(0.45, 0.76),
                p(0.27, 0.60),
                p(0.10, 0.60),
            ];
            let mut body = PathBuilder::fill();
            body.add_polygon(&points, true);
            paint_path_result(window, body.build(), color);

            let mut waves = PathBuilder::stroke(px(stroke));
            waves.move_to(p(0.55, 0.38));
            waves.curve_to(p(0.55, 0.62), p(0.66, 0.50));
            waves.move_to(p(0.66, 0.28));
            waves.curve_to(p(0.66, 0.72), p(0.82, 0.50));
            paint_path_result(window, waves.build(), color);
        }
        Glyph::Mute => {
            let points = [
                p(0.10, 0.40),
                p(0.27, 0.40),
                p(0.45, 0.24),
                p(0.45, 0.76),
                p(0.27, 0.60),
                p(0.10, 0.60),
            ];
            let mut body = PathBuilder::fill();
            body.add_polygon(&points, true);
            paint_path_result(window, body.build(), color);

            let mut cross = PathBuilder::stroke(px(stroke));
            cross.move_to(p(0.58, 0.38));
            cross.line_to(p(0.82, 0.62));
            cross.move_to(p(0.82, 0.38));
            cross.line_to(p(0.58, 0.62));
            paint_path_result(window, cross.build(), color);
        }
        Glyph::Sliders => {
            let mut path = PathBuilder::stroke(px(stroke));
            for (y, a, b, knob) in [
                (0.30, 0.16, 0.84, 0.60),
                (0.50, 0.16, 0.84, 0.38),
                (0.70, 0.16, 0.84, 0.54),
            ] {
                path.move_to(p(a, y));
                path.line_to(p(knob - 0.08, y));
                path.move_to(p(knob + 0.08, y));
                path.line_to(p(b, y));
            }
            paint_path_result(window, path.build(), color);
            window.paint_quad(quad(0.55, 0.24, 0.10, 0.12));
            window.paint_quad(quad(0.33, 0.44, 0.10, 0.12));
            window.paint_quad(quad(0.49, 0.64, 0.10, 0.12));
        }
        Glyph::Close => {
            let mut path = PathBuilder::stroke(px(stroke));
            path.move_to(p(0.28, 0.28));
            path.line_to(p(0.72, 0.72));
            path.move_to(p(0.72, 0.28));
            path.line_to(p(0.28, 0.72));
            paint_path_result(window, path.build(), color);
        }
        Glyph::Minimize => {
            let mut path = PathBuilder::stroke(px(stroke));
            path.move_to(p(0.24, 0.50));
            path.line_to(p(0.76, 0.50));
            paint_path_result(window, path.build(), color);
        }
        Glyph::Maximize => {
            let mut path = PathBuilder::stroke(px(stroke));
            path.move_to(p(0.26, 0.26));
            path.line_to(p(0.74, 0.26));
            path.line_to(p(0.74, 0.74));
            path.line_to(p(0.26, 0.74));
            path.close();
            paint_path_result(window, path.build(), color);
        }
        Glyph::ChevronLeft => {
            let mut path = PathBuilder::stroke(px(stroke));
            path.move_to(p(0.60, 0.24));
            path.line_to(p(0.38, 0.50));
            path.line_to(p(0.60, 0.76));
            paint_path_result(window, path.build(), color);
        }
        Glyph::ChevronRight => {
            let mut path = PathBuilder::stroke(px(stroke));
            path.move_to(p(0.40, 0.24));
            path.line_to(p(0.62, 0.50));
            path.line_to(p(0.40, 0.76));
            paint_path_result(window, path.build(), color);
        }
        Glyph::Copy => {
            let mut path = PathBuilder::stroke(px(stroke * 0.9));
            path.move_to(p(0.24, 0.32));
            path.line_to(p(0.62, 0.32));
            path.line_to(p(0.62, 0.74));
            path.line_to(p(0.24, 0.74));
            path.close();
            path.move_to(p(0.38, 0.24));
            path.line_to(p(0.76, 0.24));
            path.line_to(p(0.76, 0.62));
            paint_path_result(window, path.build(), color);
        }
        Glyph::Cast => {
            let mut path = PathBuilder::stroke(px(stroke * 0.9));
            path.move_to(p(0.14, 0.26));
            path.line_to(p(0.86, 0.26));
            path.line_to(p(0.86, 0.74));
            path.line_to(p(0.14, 0.74));
            path.close();
            paint_path_result(window, path.build(), color);
            let points = [p(0.42, 0.38), p(0.62, 0.50), p(0.42, 0.62)];
            let mut play = PathBuilder::fill();
            play.add_polygon(&points, true);
            paint_path_result(window, play.build(), color);
        }
        Glyph::Equalizer => {
            for (i, (x, h)) in [(0.18, 0.28), (0.38, 0.52), (0.56, 0.38), (0.74, 0.62)]
                .into_iter()
                .enumerate()
            {
                let _ = i;
                window.paint_quad(quad(x, 0.80 - h, 0.10, h));
            }
        }
    }
}

// ---- helpers -------------------------------------------------------------

fn fmt_time(secs: f64) -> String {
    let t = secs.round().max(0.0) as u64;
    format!("{:02}:{:02}", t / 60, t % 60)
}

/// Open a file with the OS default application.
fn open_in_default_app(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
}

// ---- start-at-login (Windows registry Run key) ---------------------------

#[cfg(target_os = "windows")]
const AUTOSTART_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const AUTOSTART_VALUE: &str = "MacastGpui";

fn is_autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        // Read the registry directly instead of spawning `reg.exe`: spawning a
        // child process here used to block the first frame for ~6 seconds on a
        // cold start (security software scanning the child).
        windows_registry::CURRENT_USER
            .open(AUTOSTART_KEY)
            .and_then(|key| key.get_string(AUTOSTART_VALUE))
            .is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn set_autostart(enable: bool) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let key = windows_registry::CURRENT_USER
            .open(AUTOSTART_KEY)
            .map_err(|e| std::io::Error::other(format!("{e}")))?;
        if enable {
            let exe = std::env::current_exe()?;
            let quoted = format!("\"{}\"", exe.display());
            key.set_string(AUTOSTART_VALUE, quoted)
                .map_err(|e| std::io::Error::other(format!("{e}")))?;
        } else if key.get_string(AUTOSTART_VALUE).is_ok() {
            key.remove_value(AUTOSTART_VALUE)
                .map_err(|e| std::io::Error::other(format!("{e}")))?;
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enable;
        Err(std::io::Error::other(
            "开机自启目前仅支持 Windows".to_string(),
        ))
    }
}
