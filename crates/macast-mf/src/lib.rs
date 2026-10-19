//! UI-side video host for the built-in (Media Foundation) backend.
//!
//! Owns a **message-only window** on the main thread. The core-side
//! `BuiltinPlayer` posts [`VideoCommand`]s to it; the wndproc executes them
//! on the gpui main thread, serialised with the rest of the UI. mfplay
//! requires the creating thread to pump messages.
//!
//! The video plays in a **standalone top-level window** (same model as the
//! mpv backend): the window is created on demand when a cast starts and
//! hidden when the cast stops. Fullscreen covers the monitor; Esc exits.

use macast_core::player::builtin::{
    map_mfp_event, ns100_to_secs, secs_to_100ns, MfpEventDetail, MfpEventKind,
};
use macast_core::player::video_host::{EventSink, VideoCommand, VideoHost};
use macast_core::player::PlayerEvent;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_APP_COMMAND: u32 = WM_APP;
/// Timer id for the 1 Hz position poll.
const POSITION_TIMER_ID: usize = 1;
/// Default video window size.
const DEFAULT_VIDEO_W: i32 = 960;
const DEFAULT_VIDEO_H: i32 = 540;

/// COM interface/window raw wrappers are `!Send` only because the bindings are
/// conservative. Everything below stays in shared state and is only ever
/// touched from the main thread, which makes the wrappers sound.
struct SendPlayer(IMFPMediaPlayer);
unsafe impl Send for SendPlayer {}
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}
#[allow(dead_code)]
struct SendCallback(IMFPMediaPlayerCallback);
unsafe impl Send for SendCallback {}

struct HostShared {
    msg_hwnd: AtomicIsize,
    parent: AtomicIsize,
    sink: Mutex<Option<EventSink>>,
    mf: Mutex<Option<SendPlayer>>,
    inner: Mutex<HostInner>,
}

struct HostInner {
    /// Standalone video window (created on demand).
    video: Option<SendHwnd>,
    callback: Option<SendCallback>,
    start_secs: f64,
    fullscreen: bool,
    /// Windowed rect to restore when leaving fullscreen.
    windowed_rect: RECT,
}

impl HostShared {
    fn emit(&self, event: PlayerEvent) {
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            sink(event);
        }
    }

    fn parent_hwnd(&self) -> HWND {
        HWND(self.parent.load(Ordering::Relaxed) as *mut _)
    }

    fn msg_hwnd(&self) -> HWND {
        HWND(self.msg_hwnd.load(Ordering::Relaxed) as *mut _)
    }

    fn player(&self) -> Option<IMFPMediaPlayer> {
        self.mf.lock().unwrap().as_ref().map(|p| p.0.clone())
    }

    fn post_cmd(&self, cmd: VideoCommand) {
        let hwnd = self.msg_hwnd();
        if hwnd.is_invalid() {
            return;
        }
        let boxed = Box::into_raw(Box::new(cmd));
        unsafe {
            if PostMessageW(
                Some(hwnd),
                WM_APP_COMMAND,
                WPARAM(0),
                LPARAM(boxed as isize),
            )
            .is_err()
            {
                drop(Box::from_raw(boxed));
            }
        }
    }
}

pub struct MfVideoHost {
    shared: Arc<HostShared>,
}

impl MfVideoHost {
    pub fn new() -> windows::core::Result<Self> {
        let shared = Arc::new(HostShared {
            msg_hwnd: AtomicIsize::new(0),
            parent: AtomicIsize::new(0),
            sink: Mutex::new(None),
            mf: Mutex::new(None),
            inner: Mutex::new(HostInner {
                video: None,
                callback: None,
                start_secs: 0.0,
                fullscreen: false,
                windowed_rect: RECT::default(),
            }),
        });

        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(video_host_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: host_class(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let wc_video = WNDCLASSW {
                lpfnWndProc: Some(video_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: video_class(),
                hbrBackground: CreateSolidBrush(COLORREF(0)),
                ..Default::default()
            };
            RegisterClassW(&wc_video);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                host_class(),
                host_class(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance.into()),
                None,
            )?;
            shared.msg_hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::into_raw(shared.clone()) as isize);
        }

        Ok(Self { shared })
    }

    pub fn attach_parent(&self, parent_hwnd: isize) {
        self.shared.parent.store(parent_hwnd, Ordering::Relaxed);
    }
}

impl VideoHost for MfVideoHost {
    fn post(&self, cmd: VideoCommand) {
        self.shared.post_cmd(cmd);
    }

    fn set_event_sink(&self, sink: EventSink) {
        *self.shared.sink.lock().unwrap() = Some(sink);
    }

    fn is_fullscreen(&self) -> bool {
        self.shared.inner.lock().unwrap().fullscreen
    }
}

// ---- wndprocs -------------------------------------------------------------

unsafe extern "system" fn video_host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const HostShared;
    match msg {
        WM_APP_COMMAND => {
            if ptr.is_null() {
                return LRESULT(0);
            }
            let shared = ManuallyDrop::new(Arc::from_raw(ptr));
            let boxed = lparam.0 as *mut VideoCommand;
            if !boxed.is_null() {
                let cmd = Box::from_raw(boxed);
                let mut inner = shared.inner.lock().unwrap();
                handle_command(&shared, &mut inner, *cmd);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == POSITION_TIMER_ID => {
            if !ptr.is_null() {
                let shared = ManuallyDrop::new(Arc::from_raw(ptr));
                poll_position(&shared);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if !ptr.is_null() {
                drop(Arc::from_raw(ptr));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn video_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN if wparam.0 as u32 == u32::from(VK_ESCAPE.0) => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const HostShared;
            if !ptr.is_null() {
                let shared = ManuallyDrop::new(Arc::from_raw(ptr));
                let mut inner = shared.inner.lock().unwrap();
                exit_fullscreen(&mut inner);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const HostShared;
            if !ptr.is_null() {
                drop(Arc::from_raw(ptr));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---- command execution (main thread) --------------------------------------

unsafe fn handle_command(shared: &Arc<HostShared>, inner: &mut HostInner, cmd: VideoCommand) {
    match cmd {
        VideoCommand::EnsureCreated => unsafe {
            if let Err(e) = ensure_mf(shared, inner) {
                tracing::error!("builtin backend unavailable: {e}");
                shared.emit(PlayerEvent::Error(e));
            }
        },
        VideoCommand::Play {
            url,
            start_secs,
            title: _,
            fullscreen: _,
        } => unsafe { play(shared, inner, &url, start_secs) },
        VideoCommand::Resume => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.Play();
                }
            }
        }
        VideoCommand::Pause => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.Pause();
                }
            }
        }
        VideoCommand::Stop => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.Stop();
                }
            }
            // mpv behaviour: force-window=no on stop → hide the video window.
            exit_fullscreen(inner);
            hide_video_window(inner);
        }
        VideoCommand::Seek { secs } => {
            if let Some(mf) = shared.player() {
                let pv = prop_i64(secs_to_100ns(secs));
                unsafe {
                    let _ = mf.SetPosition(&MFP_POSITIONTYPE_100NS, &pv);
                }
            }
        }
        VideoCommand::SetVolume { volume } => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.SetVolume(f32::from(volume) / 100.0);
                }
            }
        }
        VideoCommand::SetMute { mute } => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.SetMute(mute);
                }
            }
        }
        VideoCommand::SetSpeed { speed } => {
            if let Some(mf) = shared.player() {
                unsafe {
                    let _ = mf.SetRate(speed as f32);
                }
            }
        }
        // Not meaningful for a standalone window; the host manages its own
        // video window lifecycle.
        VideoCommand::MoveSurface { .. } => {}
        VideoCommand::ShowSurface => {
            show_video_window(inner);
        }
        VideoCommand::HideSurface => unsafe {
            exit_fullscreen(inner);
            hide_video_window(inner);
        },
        VideoCommand::EnterFullscreen => unsafe { enter_fullscreen(inner) },
        VideoCommand::Shutdown => unsafe { shutdown_host(shared, inner) },
    }
}

/// Create the standalone video window (shown, centered on the parent's
/// Create MFPlay + video window, then start playing `url`. Idempotent for MF.
unsafe fn play(shared: &Arc<HostShared>, inner: &mut HostInner, url: &str, start_secs: f64) {
    if let Err(e) = ensure_mf(shared, inner) {
        tracing::error!("builtin backend unavailable: {e}");
        shared.emit(PlayerEvent::Error(e));
        return;
    }
    let Some(mf) = shared.player() else {
        return;
    };
    show_video_window(inner);
    inner.start_secs = start_secs;
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    if let Err(e) = mf.CreateMediaItemFromURL(PCWSTR::from_raw(wide.as_ptr()), false, 0, None) {
        shared.emit(PlayerEvent::Error(format!("create media item failed: {e}")));
    }
}

/// Create MFPlay (without showing the video window). Idempotent.
unsafe fn ensure_mf(
    shared: &Arc<HostShared>,
    inner: &mut HostInner,
) -> std::result::Result<(), String> {
    if shared.mf.lock().unwrap().is_some() {
        return Ok(());
    }
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

    // Create the real video window (hidden) so MFPlay binds to the correct
    // HWND from the start - EVR renders into whichever HWND it was given.
    let video = create_video_window(shared, inner)?;
    let callback: IMFPMediaPlayerCallback = MfCallback {
        shared: shared.clone(),
    }
    .into();
    let mut mf: Option<IMFPMediaPlayer> = None;
    MFPCreateMediaPlayer(
        PCWSTR::null(),
        false,
        MFP_CREATION_OPTIONS(0),
        &callback,
        Some(video),
        Some(&mut mf),
    )
    .map_err(|e| format!("MFPCreateMediaPlayer failed: {e}"))?;
    let mf = mf.ok_or_else(|| "MFPCreateMediaPlayer returned no player".to_string())?;
    *shared.mf.lock().unwrap() = Some(SendPlayer(mf.clone()));
    inner.callback = Some(SendCallback(callback));

    SetTimer(Some(shared.msg_hwnd()), POSITION_TIMER_ID, 1000, None);
    Ok(())
}

/// Create the standalone video window (hidden, centered on the parent's
/// monitor). Called by `ensure_mf` before MFPlay binds to it.
unsafe fn create_video_window(
    shared: &Arc<HostShared>,
    inner: &mut HostInner,
) -> std::result::Result<HWND, String> {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        return Ok(*video);
    }
    let parent = shared.parent_hwnd();
    let monitor = MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(monitor, &mut mi);
    let rc = &mi.rcMonitor;
    let x = rc.left + (rc.right - rc.left - DEFAULT_VIDEO_W) / 2;
    let y = rc.top + (rc.bottom - rc.top - DEFAULT_VIDEO_H) / 2;

    let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
    let video = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        video_class(),
        windows::core::w!("Macast-Gpui Player"),
        WS_OVERLAPPEDWINDOW, // hidden (no WS_VISIBLE); shown on Play
        x,
        y,
        DEFAULT_VIDEO_W,
        DEFAULT_VIDEO_H,
        None,
        None,
        Some(hinstance.into()),
        None,
    )
    .map_err(|e| format!("create video window failed: {e}"))?;
    SetWindowLongPtrW(video, GWLP_USERDATA, Arc::into_raw(shared.clone()) as isize);
    inner.video = Some(SendHwnd(video));
    Ok(video)
}

unsafe fn enter_fullscreen(inner: &mut HostInner) {
    if inner.fullscreen {
        return;
    }
    let Some(SendHwnd(video)) = inner.video.as_ref() else {
        return;
    };
    let mut r = RECT::default();
    let _ = GetWindowRect(*video, &mut r);
    inner.windowed_rect = r;
    let monitor = MonitorFromWindow(*video, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut mi).as_bool() {
        let rc = mi.rcMonitor;
        let _ = SetWindowLongPtrW(*video, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
        let _ = SetWindowPos(
            *video,
            Some(HWND_TOPMOST),
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOACTIVATE,
        );
        inner.fullscreen = true;
        let _ = SetFocus(Some(*video));
    }
}

unsafe fn exit_fullscreen(inner: &mut HostInner) {
    if !inner.fullscreen {
        return;
    }
    let Some(SendHwnd(video)) = inner.video.as_ref() else {
        return;
    };
    let _ = SetWindowLongPtrW(
        *video,
        GWL_STYLE,
        (WS_OVERLAPPEDWINDOW | WS_VISIBLE).0 as isize,
    );
    let r = inner.windowed_rect;
    let _ = SetWindowPos(
        *video,
        None,
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    inner.fullscreen = false;
}

unsafe fn show_video_window(inner: &mut HostInner) {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        let _ = ShowWindow(*video, SW_SHOW);
    }
}

unsafe fn hide_video_window(inner: &mut HostInner) {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        let _ = ShowWindow(*video, SW_HIDE);
    }
}

unsafe fn shutdown_host(shared: &Arc<HostShared>, inner: &mut HostInner) {
    exit_fullscreen(inner);
    if let Some(SendPlayer(mf)) = shared.mf.lock().unwrap().take() {
        let _ = mf.Shutdown();
    }
    inner.callback = None;
    if let Some(SendHwnd(video)) = inner.video.take() {
        let _ = DestroyWindow(video);
    }
}

unsafe fn poll_position(shared: &Arc<HostShared>) {
    let Some(mf) = shared.player() else {
        return;
    };
    if let Ok(pv) = mf.GetPosition(&MFP_POSITIONTYPE_100NS) {
        shared.emit(PlayerEvent::Position(prop_as_secs(&pv)));
    }
    if let Ok(pv) = mf.GetDuration(&MFP_POSITIONTYPE_100NS) {
        shared.emit(PlayerEvent::Duration(prop_as_secs(&pv)));
    }
}

// ---- mfplay callback -------------------------------------------------------

#[implement(IMFPMediaPlayerCallback)]
struct MfCallback {
    shared: Arc<HostShared>,
}

impl IMFPMediaPlayerCallback_Impl for MfCallback_Impl {
    fn OnMediaPlayerEvent(&self, peventheader: *const MFP_EVENT_HEADER) {
        unsafe {
            let header = &*peventheader;
            let kind = match header.eEventType {
                MFP_EVENT_TYPE_PLAY => MfpEventKind::Play,
                MFP_EVENT_TYPE_PAUSE => MfpEventKind::Pause,
                MFP_EVENT_TYPE_STOP => MfpEventKind::Stop,
                MFP_EVENT_TYPE_POSITION_SET => MfpEventKind::PositionSet,
                MFP_EVENT_TYPE_RATE_SET => MfpEventKind::RateSet,
                MFP_EVENT_TYPE_MEDIAITEM_SET => MfpEventKind::MediaItemSet,
                MFP_EVENT_TYPE_ERROR => MfpEventKind::Error,
                MFP_EVENT_TYPE_PLAYBACK_ENDED => MfpEventKind::PlaybackEnded,
                _ => return,
            };

            let mut detail = MfpEventDetail::default();
            let mut media_item: Option<IMFPMediaItem> = None;
            let mut set_media_item = false;
            if kind == MfpEventKind::MediaItemSet {
                let e = &*(peventheader as *const MFP_MEDIAITEM_SET_EVENT);
                if let Some(item) = e.pMediaItem.as_ref() {
                    media_item = Some(item.clone());
                    if let Ok(pv) = item.GetDuration(&MFP_POSITIONTYPE_100NS) {
                        detail.duration_secs = prop_as_secs(&pv);
                    }
                    let start = {
                        let mut inner = self.shared.inner.lock().unwrap();
                        let start = inner.start_secs;
                        inner.start_secs = 0.0;
                        start
                    };
                    if start > 0.0 {
                        let pv = prop_i64(secs_to_100ns(start));
                        let _ = item.SetStartStopPosition(
                            Some(&MFP_POSITIONTYPE_100NS),
                            Some(&pv),
                            None,
                            None,
                        );
                    }
                    set_media_item = true;
                }
            } else if kind == MfpEventKind::RateSet {
                let e = &*(peventheader as *const MFP_RATE_SET_EVENT);
                detail.rate = e.flRate;
            } else if kind == MfpEventKind::Error {
                detail.message = format!("media foundation error {:?}", header.hrEvent);
            }

            let action = map_mfp_event(kind, &detail);

            if set_media_item {
                if let Some(mf) = self.shared.player() {
                    if let Some(item) = media_item.as_ref() {
                        let _ = mf.SetMediaItem(item);
                    }
                }
            }

            for event in action.events {
                self.shared.emit(event);
            }

            // mpv behaviour: hide the video window when playback ends.
            if kind == MfpEventKind::PlaybackEnded {
                let mut inner = self.shared.inner.lock().unwrap();
                exit_fullscreen(&mut inner);
                hide_video_window(&mut inner);
            }
        }
    }
}

// ---- PROPVARIANT helpers ---------------------------------------------------

fn prop_i64(v: i64) -> PROPVARIANT {
    let mut pv = PROPVARIANT::default();
    unsafe {
        let slot = &mut *pv.Anonymous.Anonymous;
        slot.vt = VT_I8;
        slot.Anonymous.hVal = v;
    }
    pv
}

fn prop_as_secs(pv: &PROPVARIANT) -> f64 {
    unsafe {
        if pv.Anonymous.Anonymous.vt == VT_I8 {
            ns100_to_secs(pv.Anonymous.Anonymous.Anonymous.hVal)
        } else {
            0.0
        }
    }
}

// ---- window class helpers --------------------------------------------------

fn host_class() -> PCWSTR {
    leaked_wide("MacastGpuiVideoHost")
}

fn video_class() -> PCWSTR {
    leaked_wide("MacastGpuiVideoPlayer")
}

fn leaked_wide(s: &str) -> PCWSTR {
    let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    PCWSTR::from_raw(Box::leak(wide.into_boxed_slice()).as_ptr())
}
