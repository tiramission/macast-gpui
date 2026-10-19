//! mpv backend, rendered **inside the app window**.
//!
//! Playback control goes through the [`libmpv2`] crate; the pictures come from
//! mpv's software render API (`MPV_RENDER_API_TYPE_SW`), which libmpv2's
//! high-level wrapper does not expose, so those few calls use `libmpv2_sys`
//! directly against `Mpv::ctx`.
//!
//! mpv is only ever touched from the worker thread that owns it: commands
//! arrive through a queue, and that thread also renders and pumps events.

use super::{Player, PlayerEvent, VideoFrame};
use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use tokio::sync::broadcast;

/// Work for the mpv thread.
enum Cmd {
    Load(String, f64),
    Pause(bool),
    Stop,
    Seek(f64),
    Volume(f64),
    Mute(bool),
    Speed(f64),
    SubAdd(String),
    Quit,
}

pub struct LibmpvPlayer {
    events: broadcast::Sender<PlayerEvent>,
    tx: Mutex<Option<mpsc::Sender<Cmd>>>,
    /// Latest rendered picture, shared with the UI.
    frame: Arc<Mutex<Option<VideoFrame>>>,
    /// Size (device pixels) we ask mpv to render at.
    render_size: Arc<Mutex<(u32, u32)>>,
    _quit: Arc<AtomicBool>,
}

impl LibmpvPlayer {
    /// Create the mpv instance and start its worker thread.
    ///
    /// `libmpv_path` is only used to log/report where the DLL was expected:
    /// libmpv is a link-time dependency, so the loader must find
    /// `libmpv-2.dll` next to the exe or on `PATH`.
    pub fn new(libmpv_path: Option<std::path::PathBuf>, _fullscreen: bool) -> Result<Self, String> {
        if let Some(p) = &libmpv_path {
            tracing::debug!("configured libmpv path: {}", p.display());
        }
        let (events, _) = broadcast::channel(256);
        let (tx, rx) = mpsc::channel::<Cmd>();
        let frame: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(None));
        let render_size = Arc::new(Mutex::new((960u32, 540u32)));
        let quit = Arc::new(AtomicBool::new(false));

        // The Mpv instance is created on the worker thread: mpv wants to be
        // driven from one thread, and that is the one.
        let worker = Worker {
            rx,
            events: events.clone(),
            frame: frame.clone(),
            render_size: render_size.clone(),
            quit: quit.clone(),
        };
        std::thread::Builder::new()
            .name("libmpv".into())
            .spawn(move || {
                if let Err(e) = worker.run() {
                    tracing::error!("mpv worker failed: {e}");
                }
            })
            .map_err(|e| format!("spawn mpv thread: {e}"))?;

        Ok(Self {
            events,
            tx: Mutex::new(Some(tx)),
            frame,
            render_size,
            _quit: quit,
        })
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

struct Worker {
    rx: mpsc::Receiver<Cmd>,
    events: broadcast::Sender<PlayerEvent>,
    frame: Arc<Mutex<Option<VideoFrame>>>,
    render_size: Arc<Mutex<(u32, u32)>>,
    quit: Arc<AtomicBool>,
}

impl Worker {
    fn run(self) -> Result<(), String> {
        let Worker {
            rx,
            events,
            frame,
            render_size,
            quit,
        } = self;
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("vo", "libmpv")?;
            init.set_option("keep-open", "always")?;
            init.set_option("idle", "yes")?;
            init.set_option("terminal", "no")?;
            init.set_option("osc", "no")?;
            init.set_option("config", "no")?;
            init.set_option("force-window", "no")?;
            init.set_option("hwdec", "auto")?;
            Ok(())
        })
        .map_err(|e| format!("mpv init: {e}"))?;
        tracing::info!("mpv backend ready ({})", mpv_version(&mpv));

        for (id, name, format) in [
            (1u64, "time-pos", Format::Double),
            (2u64, "duration", Format::Double),
            (3u64, "pause", Format::Flag),
            (4u64, "volume", Format::Double),
            (5u64, "mute", Format::Flag),
            (6u64, "speed", Format::Double),
        ] {
            let _ = mpv.observe_property(name, format, id);
        }

        // Software render context: mpv draws into a buffer we hand it.
        let mut ctx: *mut libmpv2_sys::mpv_render_context = std::ptr::null_mut();
        let api_type = CString::new("sw").map_err(|_| "CString".to_string())?;
        let mut create_params = [
            libmpv2_sys::mpv_render_param {
                type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: api_type.as_ptr() as *mut _,
            },
            libmpv2_sys::mpv_render_param {
                type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let rc = unsafe {
            libmpv2_sys::mpv_render_context_create(
                &mut ctx,
                mpv.ctx.as_ptr(),
                create_params.as_mut_ptr(),
            )
        };
        if rc != 0 || ctx.is_null() {
            return Err(format!("mpv_render_context_create failed ({rc})"));
        }
        let format = CString::new("bgra").map_err(|_| "CString".to_string())?;
        let mut pixels: Vec<u8> = Vec::new();
        // Target size of the frame currently in the shared slot. When the UI
        // resizes (or toggles fullscreen) we re-render at the new size even if
        // mpv has no fresh frame, so a paused picture still rescales.
        let mut last_size = (0u32, 0u32);

        while !quit.load(Ordering::Relaxed) {
            // Commands first: they decide what the next frame should be.
            while let Ok(cmd) = rx.try_recv() {
                let is_quit = matches!(cmd, Cmd::Quit);
                apply(&mpv, &events, cmd);
                if is_quit {
                    quit.store(true, Ordering::Relaxed);
                }
            }
            if quit.load(Ordering::Relaxed) {
                break;
            }

            // Render when mpv has a new frame, or when only the size changed.
            let update = unsafe { libmpv2_sys::mpv_render_context_update(ctx) };
            let (w, h) = *render_size.lock().unwrap();
            let has_frame = frame.lock().unwrap().is_some();
            let new_frame =
                update & libmpv2_sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64 != 0;
            let resized = has_frame && (w, h) != last_size;
            if (new_frame || resized) && w > 0 && h > 0 {
                let need = (w as usize) * (h as usize) * 4;
                if pixels.len() != need {
                    pixels = vec![0u8; need];
                }
                let mut size = [w as i32, h as i32];
                let mut stride = (w * 4) as i32;
                let mut params = [
                    libmpv2_sys::mpv_render_param {
                        type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE,
                        data: size.as_mut_ptr() as *mut _,
                    },
                    libmpv2_sys::mpv_render_param {
                        type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT,
                        data: format.as_ptr() as *mut _,
                    },
                    libmpv2_sys::mpv_render_param {
                        type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE,
                        data: &mut stride as *mut _ as *mut _,
                    },
                    libmpv2_sys::mpv_render_param {
                        type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER,
                        data: pixels.as_mut_ptr() as *mut _,
                    },
                    libmpv2_sys::mpv_render_param {
                        type_: libmpv2_sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                        data: std::ptr::null_mut(),
                    },
                ];
                let rc =
                    unsafe { libmpv2_sys::mpv_render_context_render(ctx, params.as_mut_ptr()) };
                if rc == 0 {
                    unsafe { libmpv2_sys::mpv_render_context_report_swap(ctx) };
                    last_size = (w, h);
                    *frame.lock().unwrap() = Some(VideoFrame {
                        width: w,
                        height: h,
                        bgra: Arc::from(pixels.as_slice()),
                    });
                }
            }

            // Events.
            match mpv.wait_event(0.02) {
                Some(Ok(Event::Shutdown)) => break,
                Some(Ok(Event::FileLoaded | Event::PlaybackRestart | Event::StartFile)) => {
                    let _ = events.send(PlayerEvent::Playing);
                }
                Some(Ok(Event::EndFile(reason))) => {
                    // MPV_END_FILE_REASON_EOF == 0 (libmpv2 keeps the alias private).
                    let ended = reason == 0;
                    let _ = events.send(if ended {
                        PlayerEvent::Ended
                    } else {
                        PlayerEvent::Stopped
                    });
                    *frame.lock().unwrap() = None;
                }
                Some(Ok(Event::PropertyChange { name, change, .. })) => {
                    let value = match (name, change) {
                        ("time-pos", PropertyData::Double(v)) => Some(PlayerEvent::Position(v)),
                        ("duration", PropertyData::Double(v)) => Some(PlayerEvent::Duration(v)),
                        ("pause", PropertyData::Flag(v)) => Some(PlayerEvent::Paused(v)),
                        ("volume", PropertyData::Double(v)) => Some(PlayerEvent::Volume(v)),
                        ("mute", PropertyData::Flag(v)) => Some(PlayerEvent::Mute(v)),
                        ("speed", PropertyData::Double(v)) => Some(PlayerEvent::Speed(v)),
                        _ => None,
                    };
                    if let Some(v) = value {
                        let _ = events.send(v);
                    }
                }
                Some(Err(e)) => tracing::debug!("mpv event error: {e}"),
                Some(Ok(_)) => {}
                None => {}
            }
        }

        unsafe { libmpv2_sys::mpv_render_context_free(ctx) };
        Ok(())
    }
}

fn mpv_version(mpv: &Mpv) -> String {
    mpv.get_property::<String>("mpv-version")
        .unwrap_or_else(|_| "unknown version".into())
}

/// Runs one command on the mpv thread.
fn apply(mpv: &Mpv, events: &broadcast::Sender<PlayerEvent>, cmd: Cmd) {
    let run = |name: &str, args: &[&str]| {
        if let Err(e) = mpv.command(name, args) {
            tracing::debug!("mpv {name} failed: {e}");
        }
    };
    match cmd {
        Cmd::Load(url, start) => {
            run("loadfile", &[&url]);
            if start > 0.0 {
                run("seek", &[&format!("{start:.3}"), "absolute"]);
            }
            run("set", &["pause", "no"]);
            let _ = events.send(PlayerEvent::Playing);
        }
        Cmd::Pause(paused) => run("set", &["pause", if paused { "yes" } else { "no" }]),
        Cmd::Stop => {
            run("stop", &[]);
            let _ = events.send(PlayerEvent::Stopped);
        }
        Cmd::Seek(secs) => run("seek", &[&format!("{secs:.3}"), "absolute"]),
        Cmd::Volume(v) => {
            if let Err(e) = mpv.set_property("volume", v) {
                tracing::debug!("set volume: {e}");
            }
        }
        Cmd::Mute(m) => {
            if let Err(e) = mpv.set_property("mute", m) {
                tracing::debug!("set mute: {e}");
            }
        }
        Cmd::Speed(s) => run("set", &["speed", &format!("{s:.3}")]),
        Cmd::SubAdd(url) => run("sub-add", &[&url]),
        Cmd::Quit => {}
    }
}

#[async_trait::async_trait]
impl Player for LibmpvPlayer {
    async fn set_media_url(&self, url: &str, start_secs: f64, _title: &str) {
        self.send(Cmd::Load(url.to_string(), start_secs));
    }

    async fn resume(&self) {
        self.send(Cmd::Pause(false));
        let _ = self.events.send(PlayerEvent::Playing);
    }

    async fn pause(&self) {
        self.send(Cmd::Pause(true));
        let _ = self.events.send(PlayerEvent::Paused(true));
    }

    async fn stop(&self) {
        self.send(Cmd::Stop);
    }

    async fn seek(&self, secs: f64) {
        self.send(Cmd::Seek(secs));
    }

    async fn set_volume(&self, volume: u8) {
        self.send(Cmd::Volume(f64::from(volume)));
    }

    async fn set_mute(&self, mute: bool) {
        self.send(Cmd::Mute(mute));
    }

    async fn set_speed(&self, speed: f64) {
        self.send(Cmd::Speed(speed.clamp(0.1, 8.0)));
    }

    async fn add_subtitle(&self, url: &str, _title: &str) {
        self.send(Cmd::SubAdd(url.to_string()));
    }

    async fn set_subtitle_visible(&self, _visible: bool) {}

    async fn show_text(&self, _text: &str, _duration_ms: u64) {}

    async fn shutdown(&self) {
        self.send(Cmd::Quit);
        *self.tx.lock().unwrap() = None;
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    fn name(&self) -> &'static str {
        "mpv"
    }

    fn video_frame(&self) -> Option<VideoFrame> {
        self.frame.lock().unwrap().clone()
    }

    fn set_render_size(&self, w: u32, h: u32) {
        *self.render_size.lock().unwrap() = (w.max(2), h.max(2));
    }
}
