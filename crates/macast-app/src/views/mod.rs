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

use gpui::prelude::FluentBuilder;
use gpui::*;
use macast_core::{Config, MacastCore, PlaybackSnapshot, PlayerKind, TransportState};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

// ---- design tokens -------------------------------------------------------
// One dark surface ramp, one accent. Semantic green is reserved for "playing";
// volume and progress both use the accent so colour always means one thing.

const BG: u32 = 0x0A0D13;
const SURFACE: u32 = 0x121722;
const SURFACE_HI: u32 = 0x171E2C;
const TEXT: u32 = 0xEDF0F7;
const MUTED: u32 = 0x98A2B3;
const DIM: u32 = 0x7C8698;
const ACCENT: u32 = 0x6C8CFF;
const SUCCESS: u32 = 0x3DDC97;
const WARN: u32 = 0xF5B84D;
const ON_ACCENT: u32 = 0x0A0D13;

const BORDER: u32 = 0xFFFFFF14; // white @ 8%
const BORDER_STRONG: u32 = 0xFFFFFF24; // white @ 14%
const HOVER: u32 = 0xFFFFFF0F; // white @ 6%

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
    /// Timestamp of the last titlebar click, for double-click to maximize.
    last_title_click: Option<std::time::Instant>,
}

impl RootView {
    pub fn new(core: Arc<MacastCore>, cx: &mut Context<Self>) -> Self {
        let mut rx = core.subscribe();
        let initial = rx.borrow_and_update().clone();

        // Watch channel -> view updates. `tokio::sync::watch` futures are
        // runtime-agnostic, so they play nicely with gpui's own executor.
        // gpui 0.2 (edition 2024) wants an async closure here.
        cx.spawn(async move |view, cx| {
            while rx.changed().await.is_ok() {
                let snap = rx.borrow_and_update().clone();
                if view
                    .update(&mut *cx, |this, cx| {
                        this.snapshot = snap;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

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
            last_title_click: None,
        }
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
        if self.config.player != kind {
            self.config.player = kind;
            self.save_config("播放后端", cx);
        }
    }

    fn toggle_fullscreen(&mut self, cx: &mut Context<Self>) {
        self.config.mpv_fullscreen = !self.config.mpv_fullscreen;
        self.save_config("投屏全屏", cx);
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
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = view.update(&mut *cx, |this, cx| {
                if this.notice_seq == seq {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.snapshot.clone();
        let playing = s.transport_state == TransportState::Playing;
        let has_media = !s.uri.is_empty();

        let mut root = div()
            .id("app-root")
            .relative()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
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
                        .child(footer(&s, &self.core)),
                ),
            );

        if self.show_settings {
            root = root.child(backdrop(cx)).child(settings_panel(
                &self.config,
                s.port,
                &s.ips,
                self.autostart,
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
                        .text_color(rgb(DIM))
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
        .bg(rgb(BG))
        .border_b_1()
        .border_color(rgba(BORDER))
        .child(drag_region)
        .child(status_inline(s))
        .child(div().ml_2().child(icon_button(
            "settings",
            Glyph::Sliders,
            32.0,
            if show_settings {
                rgb(ACCENT)
            } else {
                rgb(MUTED)
            },
            if show_settings {
                rgba(0x6C8CFF29)
            } else {
                rgba(HOVER)
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
            rgb(MUTED),
            rgba(HOVER),
            |_, window, _| window.minimize_window(),
        ))
        .child(icon_button(
            "win-max",
            Glyph::Maximize,
            30.0,
            rgb(MUTED),
            rgba(HOVER),
            |_, window, _| toggle_maximize(window),
        ))
        .child(icon_button(
            "win-close",
            Glyph::Close,
            30.0,
            rgb(MUTED),
            rgba(0xF0444429),
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
fn window_handle_of(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
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
            linear_color_stop(rgb(0x7D9AFF), 0.0),
            linear_color_stop(rgb(0x3E5BD6), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph_icon(Glyph::Play, 13.0, rgb(0x0A0D13)))
}

fn hero_card(
    s: &PlaybackSnapshot,
    playing: bool,
    has_media: bool,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    cx: &mut Context<RootView>,
) -> Div {
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
                    window.paint_quad(fill(track, rgba(0xFFFFFF14)));
                    if frac > 0.0 {
                        let filled = Bounds::new(
                            track.origin,
                            size(track.size.width * frac, track.size.height),
                        );
                        window.paint_quad(fill(filled, rgb(ACCENT)));
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
                .text_color(rgb(MUTED))
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
                .text_color(rgb(DIM))
                .child(fmt_time(s.position))
                .child(format!("-{}", fmt_time((s.duration - s.position).max(0.0)))),
        );
    } else if has_media {
        info = info.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(DIM))
                .child(format!("已播放 {}", fmt_time(s.position))),
        );
    } else {
        info = info.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(DIM))
                .child("① 手机与电脑连同一个 Wi-Fi   ② 视频 App 里点投屏   ③ 选择本机"),
        );
    }

    div()
        .flex_1()
        .min_h(px(240.0))
        .max_h(px(440.0))
        .flex()
        .items_center()
        .gap_6()
        .p_6()
        .rounded_2xl()
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(SURFACE_HI), 0.0),
            linear_color_stop(rgb(0x0F141E), 1.0),
        ))
        .border_1()
        .border_color(rgba(BORDER))
        .child(artwork(s, playing))
        .child(info)
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
            linear_color_stop(rgb(0x33478F), 0.0),
            linear_color_stop(rgb(0x141A27), 1.0),
        ))
        .border_1()
        .border_color(rgba(BORDER_STRONG))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .size(px(86.0))
                .rounded_full()
                .border_1()
                .border_color(rgba(0xFFFFFF0F)),
        )
        .child(
            div()
                .absolute()
                .size(px(60.0))
                .rounded_full()
                .border_1()
                .border_color(rgba(0xFFFFFF0A)),
        )
        .child(glyph_icon(glyph, 34.0, rgba(0xFFFFFFFF)))
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
                    window.paint_quad(fill(track, rgba(0xFFFFFF14)));
                    if vol_frac > 0.0 {
                        let filled = Bounds::new(
                            track.origin,
                            size(track.size.width * vol_frac, track.size.height),
                        );
                        window.paint_quad(fill(filled, rgb(ACCENT)));
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
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgba(BORDER))
        .child(seek_step_button("seek-back", Direction::Back, cx))
        .child(
            div()
                .id("play-pause")
                .size(px(46.0))
                .rounded_full()
                .bg(if has_media {
                    rgb(ACCENT)
                } else {
                    rgb(SURFACE_HI)
                })
                .when(has_media, |this| {
                    this.hover(|s| s.bg(rgb(0x7D9AFF)))
                        .active(|s| s.bg(rgb(0x5B7BEE)))
                })
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(glyph_icon(
                    if playing { Glyph::Pause } else { Glyph::Play },
                    18.0,
                    if has_media {
                        rgb(ON_ACCENT)
                    } else {
                        rgb(MUTED)
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
            rgb(MUTED),
            rgba(HOVER),
            cx.listener(|this, _, _, _| this.stop()),
        ))
        .child(divider(px(22.0)))
        .child(icon_button(
            "mute",
            if s.mute { Glyph::Mute } else { Glyph::Volume },
            38.0,
            if s.mute { rgb(WARN) } else { rgb(MUTED) },
            rgba(HOVER),
            cx.listener(|this, _, _, _| this.toggle_mute()),
        ))
        .child(volume_bar)
        .child(
            div()
                .w(px(38.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
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

fn footer(s: &PlaybackSnapshot, core: &MacastCore) -> Div {
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
        .text_color(rgb(DIM))
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
                .flex_shrink_0()
                .child(format!("{} 后端", core.engine.player.name())),
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
        .bg(rgba(0x05070C99))
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
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let mpv_path = config
        .mpv_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "自动（PATH 中的 mpv）".to_string());

    let body = div()
        .id("settings-scroll")
        .flex_1()
        .overflow_y_scroll()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(section(
            "设备",
            vec![
                setting_row("名称", div().child(config.friendly_name.clone())),
                setting_row(
                    "端口",
                    div().child(if config.port == actual_port {
                        format!("{actual_port}")
                    } else {
                        format!("{} → {}", config.port, actual_port)
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
        .child(section(
            "播放",
            vec![
                setting_row("后端", segmented_player(config.player, cx)),
                setting_row(
                    "mpv 路径",
                    div().truncate().text_color(rgb(MUTED)).child(mpv_path),
                ),
                setting_row(
                    "投屏时全屏",
                    switch(
                        "switch-fullscreen",
                        config.mpv_fullscreen,
                        cx.listener(|this, _, _, cx| this.toggle_fullscreen(cx)),
                    ),
                ),
            ],
        ))
        .child(section(
            "系统",
            vec![
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
        .child(
            div()
                .pt_2()
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(rgb(DIM))
                .child("名称、端口、mpv 路径请编辑 config.json 后重启应用。"),
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
            linear_color_stop(rgb(0x121824), 0.0),
            linear_color_stop(rgb(0x0D121A), 1.0),
        ))
        .border_l_1()
        .border_color(rgba(BORDER))
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
                .border_color(rgba(BORDER))
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
                    rgb(MUTED),
                    rgba(HOVER),
                    cx.listener(|this, _, _, cx| {
                        this.show_settings = false;
                        cx.notify();
                    }),
                )),
        )
        .child(body)
}

fn section(label: &'static str, rows: Vec<Div>) -> Div {
    let mut column = div().flex().flex_col().child(
        div()
            .pb_2()
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(DIM))
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
        .border_color(rgba(0xFFFFFF0A))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .text_size(px(12.5))
                .text_color(rgb(TEXT))
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
                .bg(rgb(0x1B2333))
                .border_1()
                .border_color(rgba(0x6C8CFF40))
                .text_size(px(12.0))
                .text_color(rgb(TEXT))
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
        TransportState::Playing => SUCCESS,
        TransportState::PausedPlayback => WARN,
        _ => DIM,
    };
    div().size(px(6.0)).rounded_full().bg(rgb(color))
}

fn status_inline(s: &PlaybackSnapshot) -> Div {
    let (label, color) = match s.transport_state {
        TransportState::Playing => ("正在播放", SUCCESS),
        TransportState::PausedPlayback => ("已暂停", WARN),
        TransportState::Stopped => ("已停止", MUTED),
        TransportState::NoMediaPresent => ("待机", MUTED),
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
        TransportState::Playing => ("正在播放", SUCCESS),
        TransportState::PausedPlayback => ("已暂停", WARN),
        TransportState::Stopped => ("已停止", MUTED),
        TransportState::NoMediaPresent => ("待机", MUTED),
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
    div().w(px(1.0)).h(height).bg(rgba(0xFFFFFF12))
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
            .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
            .when(active, |this| this.bg(rgb(0x2A3550)))
            .when(!active, |this| this.hover(|s| s.bg(rgba(HOVER))))
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
        .bg(rgb(0x0B0F17))
        .border_1()
        .border_color(rgba(BORDER))
        .child(option(
            "player-mpv",
            "mpv",
            current == PlayerKind::Mpv,
            PlayerKind::Mpv,
            cx,
        ))
        .child(option(
            "player-null",
            "null",
            current == PlayerKind::Null,
            PlayerKind::Null,
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
        .bg(if on { rgb(ACCENT) } else { rgb(0x2B3446) })
        .flex()
        .items_center()
        .when(on, |this| this.justify_end())
        .when(!on, |this| this.justify_start())
        .cursor_pointer()
        .hover(|this| this.opacity(0.9))
        .child(div().size(px(18.0)).rounded_full().bg(if on {
            rgb(0xFFFFFF)
        } else {
            rgb(0x8B94A7)
        }))
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
        .hover(|this| this.bg(rgba(HOVER)))
        .active(|this| this.bg(rgba(0xFFFFFF1A)))
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(glyph_icon(glyph, 12.0, rgb(MUTED)))
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
        .text_color(rgb(MUTED))
        .hover(|this| this.bg(rgba(HOVER)).text_color(rgb(TEXT)))
        .active(|this| this.bg(rgba(0xFFFFFF1A)))
        .on_click(on_click);
    if let Some(glyph) = glyph {
        button = button.child(glyph_icon(glyph, 13.0, rgb(MUTED)));
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
        .active(move |this| this.bg(rgba(0xFFFFFF1F)))
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
