//! Windows system tray integration.
//!
//! gpui has no tray API, so we use `tray-icon` (the library Tauri uses). On
//! Windows it creates a hidden message window and relies on the thread's
//! message loop; gpui's `Application::run` pumps messages with `GetMessageW` +
//! `DispatchMessageW`, so events are delivered correctly.
//!
//! The tray owns three behaviours:
//! - left click / double click -> restore and focus the main window
//! - menu "显示主窗口" -> same
//! - menu "退出" -> graceful shutdown (the close handler stops intercepting)

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, Window};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW};

/// Set once the tray icon exists. The close handler only hides the window when
/// the tray can bring it back.
static TRAY_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set by the tray's "退出" item so the close handler stops vetoing.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Whether closing the window should hide it instead of quitting.
pub fn on_close_requested(window: &mut Window) -> bool {
    if !TRAY_ACTIVE.load(Ordering::SeqCst) || QUITTING.load(Ordering::SeqCst) {
        return true;
    }
    if let Some(hwnd) = window_hwnd(window) {
        unsafe {
            let _ = ShowWindow(HWND(hwnd as *mut core::ffi::c_void), SW_HIDE);
        }
    }
    // Veto the close: the window is hidden, the process keeps running.
    false
}

/// Create the icon and start polling its menu/click events on gpui's executor.
pub fn start(window: AnyWindowHandle, cx: &mut App) -> anyhow::Result<()> {
    let menu = Menu::new();
    let show_item = MenuItem::new("显示主窗口", true, None);
    let quit_item = MenuItem::new("退出 Macast-Gpui", true, None);
    menu.append(&show_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Macast-Gpui · DLNA 接收端")
        .with_icon(build_icon()?)
        // Left click restores the window; right click opens the menu.
        .with_menu_on_left_click(false)
        .build()?;

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    TRAY_ACTIVE.store(true, Ordering::SeqCst);
    tracing::info!("tray icon created");

    cx.spawn(async move |cx: &mut AsyncApp| loop {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == show_id {
                show_window(window, cx);
            } else if event.id == quit_id {
                QUITTING.store(true, Ordering::SeqCst);
                let _ = cx.update(|cx| cx.quit());
                return;
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => show_window(window, cx),
                _ => {}
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(120))
            .await;
    })
    .detach();

    // The icon must outlive `start()`; it is one small allocation for the whole
    // process lifetime, so leaking it is simpler than threading a Send guard
    // through gpui's main-thread-only app state.
    std::mem::forget(tray);
    Ok(())
}

fn show_window(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let _ = window.update(cx, |_, window, _| {
        if let Some(hwnd) = window_hwnd(window) {
            unsafe {
                let _ = ShowWindow(HWND(hwnd as *mut core::ffi::c_void), SW_SHOW);
                let _ = SetForegroundWindow(HWND(hwnd as *mut core::ffi::c_void));
            }
        }
        window.activate_window();
    });
}

fn window_hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // `Window` also has an inherent `window_handle()` returning gpui's own
    // handle type, so call the trait method explicitly.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// A 32×32 RGBA icon: blue disc with a white play triangle.
fn build_icon() -> anyhow::Result<Icon> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            if dx * dx + dy * dy > 15.0 * 15.0 {
                continue;
            }
            let x_i = x as i32;
            let y_i = y as i32;
            let in_triangle = (11..=23).contains(&x_i) && (y_i - 16).abs() <= (23 - x_i) * 8 / 12;
            let (r, g, b) = if in_triangle {
                (255u8, 255u8, 255u8)
            } else {
                (47u8, 111u8, 235u8)
            };
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(Icon::from_rgba(rgba, SIZE, SIZE)?)
}
