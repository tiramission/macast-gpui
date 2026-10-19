//! Built-in video backend using **libmpv** (via the `libmpv2` crate).
//!
//! libmpv is the library version of mpv — same engine, same codec support.
//! The `wid` option tells libmpv to render into our video window HWND.
//! Events are read on a dedicated thread.

use libmpv2::events::*;
use libmpv2::Mpv;
use macast_core::player::builtin::clamp_rate;
use macast_core::player::video_host::{EventSink, VideoCommand, VideoHost};
use macast_core::player::PlayerEvent;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_APP_COMMAND: u32 = WM_APP;
const DEFAULT_VIDEO_W: i32 = 960;
const DEFAULT_VIDEO_H: i32 = 540;

struct SendMpv(Mpv);
unsafe impl Send for SendMpv {}
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

struct HostShared {
    msg_hwnd: AtomicIsize,
    parent: AtomicIsize,
    sink: Mutex<Option<EventSink>>,
    mpv: Mutex<Option<SendMpv>>,
    inner: Mutex<HostInner>,
    shutdown: AtomicBool,
}

struct HostInner {
    video: Option<SendHwnd>,
    fullscreen: bool,
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
            mpv: Mutex::new(None),
            inner: Mutex::new(HostInner {
                video: None,
                fullscreen: false,
                windowed_rect: RECT::default(),
            }),
            shutdown: AtomicBool::new(false),
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

fn handle_command(shared: &Arc<HostShared>, inner: &mut HostInner, cmd: VideoCommand) {
    match cmd {
        VideoCommand::EnsureCreated => {
            if let Err(e) = ensure_mpv(shared, inner) {
                tracing::error!("builtin unavailable: {e}");
                shared.emit(PlayerEvent::Error(e));
            }
        }
        VideoCommand::Play {
            url,
            start_secs,
            title: _,
            fullscreen: _,
        } => {
            if let Err(e) = play(shared, inner, &url, start_secs) {
                tracing::error!("builtin play failed: {e}");
                shared.emit(PlayerEvent::Error(e));
            }
        }
        VideoCommand::Resume => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.set_property("pause", false);
            }
        }
        VideoCommand::Pause => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.set_property("pause", true);
            }
        }
        VideoCommand::Stop => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.command("stop", &[]);
            }
            exit_fullscreen(inner);
            hide_video_window(inner);
        }
        VideoCommand::Seek { secs } => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.command("seek", &[format!("{secs:.3}").as_str(), "absolute"]);
            }
        }
        VideoCommand::SetVolume { volume } => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.set_property("volume", f64::from(volume));
            }
        }
        VideoCommand::SetMute { mute } => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.set_property("mute", mute);
            }
        }
        VideoCommand::SetSpeed { speed } => {
            if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
                let _ = mpv.set_property("speed", clamp_rate(speed));
            }
        }
        VideoCommand::MoveSurface { .. } => {} // standalone window; not needed
        VideoCommand::ShowSurface => show_video_window(inner),
        VideoCommand::HideSurface => {
            exit_fullscreen(inner);
            hide_video_window(inner);
        }
        VideoCommand::EnterFullscreen => enter_fullscreen(inner),
        VideoCommand::Shutdown => {
            shared.shutdown.store(true, Ordering::Relaxed);
            exit_fullscreen(inner);
            hide_video_window(inner);
        }
    }
}

/// Create the mpv handle with `wid` bound to our video window, initialise.
/// Idempotent.
fn ensure_mpv(shared: &Arc<HostShared>, inner: &mut HostInner) -> std::result::Result<(), String> {
    if shared.mpv.lock().unwrap().is_some() {
        return Ok(());
    }
    let video = create_video_window(shared, inner)?;

    let mpv = Mpv::with_initializer(|init| {
        // Embed rendering into our video window.
        init.set_property("wid", video.0 as i64)?;
        init.set_property("force-window", "yes")?;
        init.set_property("keep-open", "always")?;
        init.set_property("terminal", "no")?;
        init.set_property("osc", "no")?;
        Ok(())
    })
    .map_err(|e| format!("mpv create/init failed: {e}"))?;

    *shared.mpv.lock().unwrap() = Some(SendMpv(mpv));

    // Event loop on a dedicated thread.
    let event_shared = shared.clone();
    std::thread::Builder::new()
        .name("mpv-event".into())
        .spawn(move || event_loop(&event_shared))
        .map_err(|e| format!("spawn event loop: {e}"))?;

    Ok(())
}

fn play(
    shared: &Arc<HostShared>,
    inner: &mut HostInner,
    url: &str,
    start_secs: f64,
) -> std::result::Result<(), String> {
    ensure_mpv(shared, inner)?;
    show_video_window(inner);
    if let Some(SendMpv(mpv)) = shared.mpv.lock().unwrap().as_ref() {
        if start_secs > 0.0 {
            let _ = mpv.set_property("start", format!("{start_secs:.3}"));
        }
        mpv.command("loadfile", &[url])
            .map_err(|e| format!("loadfile failed: {e}"))?;
        let _ = mpv.set_property("pause", false);
    }
    Ok(())
}

/// Create the standalone video window (hidden, centered on the parent's
/// monitor). Called by `ensure_mpv` before libmpv binds to it.
fn create_video_window(
    shared: &Arc<HostShared>,
    inner: &mut HostInner,
) -> std::result::Result<HWND, String> {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        return Ok(*video);
    }
    let parent = shared.parent_hwnd();
    let monitor = unsafe { MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut mi);
    }
    let rc = &mi.rcMonitor;
    let x = rc.left + (rc.right - rc.left - DEFAULT_VIDEO_W) / 2;
    let y = rc.top + (rc.bottom - rc.top - DEFAULT_VIDEO_H) / 2;

    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let title = windows::core::HSTRING::from("Macast-Gpui Player");
    let video = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            video_class(),
            PCWSTR::from_raw(title.as_ptr()),
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
    }
    .map_err(|e| format!("create video window: {e}"))?;
    unsafe {
        SetWindowLongPtrW(video, GWLP_USERDATA, Arc::into_raw(shared.clone()) as isize);
    }
    inner.video = Some(SendHwnd(video));
    Ok(video)
}

fn enter_fullscreen(inner: &mut HostInner) {
    if inner.fullscreen {
        return;
    }
    let Some(SendHwnd(video)) = inner.video.as_ref() else {
        return;
    };
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(*video, &mut r);
    }
    inner.windowed_rect = r;
    let monitor = unsafe { MonitorFromWindow(*video, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut mi);
    }
    if mi.rcMonitor.right > 0 {
        let rc = mi.rcMonitor;
        unsafe {
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
            let _ = SetFocus(Some(*video));
        }
        inner.fullscreen = true;
    }
}

fn exit_fullscreen(inner: &mut HostInner) {
    if !inner.fullscreen {
        return;
    }
    let Some(SendHwnd(video)) = inner.video.as_ref() else {
        return;
    };
    unsafe {
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
    }
    inner.fullscreen = false;
}

fn show_video_window(inner: &mut HostInner) {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        unsafe {
            let _ = ShowWindow(*video, SW_SHOW);
        }
    }
}

fn hide_video_window(inner: &mut HostInner) {
    if let Some(SendHwnd(video)) = inner.video.as_ref() {
        unsafe {
            let _ = ShowWindow(*video, SW_HIDE);
        }
    }
}

// ---- mpv event loop (dedicated thread) -------------------------------------

fn event_loop(shared: &Arc<HostShared>) {
    // Extract the Mpv reference outside the loop; the SendMpv wrapper keeps
    // the mpv handle alive for the process lifetime (terminate_destroy in
    // the event loop drop path is the only cleanup).
    let mpv_ref = shared
        .mpv
        .lock()
        .unwrap()
        .as_ref()
        .map(|SendMpv(m)| m as *const Mpv);

    'outer: loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            break;
        }
        let mpv = match mpv_ref {
            Some(m) => unsafe { &*m },
            None => return,
        };
        match mpv.wait_event(0.5) {
            Some(Ok(event)) => match event {
                Event::EndFile(reason) => {
                    let ended = reason == 0 || reason == 5; // EOF or ERROR
                    if ended {
                        shared.emit(PlayerEvent::Ended);
                    } else {
                        shared.emit(PlayerEvent::Stopped);
                    }
                    let mut inner = shared.inner.lock().unwrap();
                    exit_fullscreen(&mut inner);
                    hide_video_window(&mut inner);
                }
                Event::PropertyChange { name, change, .. } => match (name, change) {
                    ("time-pos", PropertyData::Double(pos)) => {
                        shared.emit(PlayerEvent::Position(pos));
                    }
                    ("duration", PropertyData::Double(dur)) => {
                        shared.emit(PlayerEvent::Duration(dur));
                    }
                    ("pause", PropertyData::Flag(paused)) => {
                        shared.emit(PlayerEvent::Paused(paused));
                    }
                    ("volume", PropertyData::Double(vol)) => {
                        shared.emit(PlayerEvent::Volume(vol));
                    }
                    ("mute", PropertyData::Flag(mute)) => {
                        shared.emit(PlayerEvent::Mute(mute));
                    }
                    ("speed", PropertyData::Double(speed)) => {
                        shared.emit(PlayerEvent::Speed(speed));
                    }
                    _ => {}
                },
                Event::Shutdown => break 'outer,
                _ => {}
            },
            Some(Err(e)) => {
                tracing::debug!("mpv event error: {e}");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            None => {} // timeout
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

use windows::core::{Result as WindowsResult, PCWSTR};
