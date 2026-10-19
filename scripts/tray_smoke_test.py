"""Smoke-test the Windows tray integration without touching the mouse.

Start the app first (target/debug/macast-app.exe), then run:

    python scripts/tray_smoke_test.py          # hide + restore checks
    python scripts/tray_smoke_test.py --quit   # also select "退出" from the menu

The tray icon lives in a hidden window created by `tray-icon` with the class
name "tray_icon_app"; its Shell_NotifyIcon callback is WM_USER + 6002. Sending
that message with WM_LBUTTONUP is exactly what a left click produces.

Menu command ids are handed out sequentially starting at 1000, so the second
item ("退出", after a separator) is 1001. If you reorder the menu, update
QUIT_COMMAND_ID below.
"""

import argparse
import ctypes
import sys
import time

WM_CLOSE = 0x0010
WM_COMMAND = 0x0111
WM_LBUTTONUP = 0x0202
WM_USER_TRAYICON = 6002
QUIT_COMMAND_ID = 1001

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.FindWindowW.restype = ctypes.c_void_p
user32.FindWindowW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p]
user32.IsWindowVisible.restype = ctypes.c_bool
user32.IsWindowVisible.argtypes = [ctypes.c_void_p]
user32.PostMessageW.restype = ctypes.c_bool
user32.PostMessageW.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_void_p, ctypes.c_void_p]


def fail(message: str) -> None:
    print(f"FAIL: {message}")
    sys.exit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--quit", action="store_true", help="also trigger the 退出 menu item")
    args = parser.parse_args()

    tray = user32.FindWindowW("tray_icon_app", None)
    main_window = user32.FindWindowW(None, "Macast-Gpui")
    if not tray:
        fail("tray window not found - is the app running?")
    if not main_window:
        fail("main window not found (title should be Macast-Gpui)")
    if not user32.IsWindowVisible(main_window):
        fail("main window starts hidden")

    user32.PostMessageW(main_window, WM_CLOSE, None, None)
    time.sleep(0.6)
    if user32.IsWindowVisible(main_window):
        fail("WM_CLOSE did not hide the window (close-to-tray broken)")

    user32.PostMessageW(tray, WM_USER_TRAYICON, None, WM_LBUTTONUP)
    time.sleep(0.8)
    if not user32.IsWindowVisible(main_window):
        fail("tray left click did not restore the window")

    print("PASS: close hides the window, tray click restores it")

    if args.quit:
        user32.PostMessageW(tray, WM_COMMAND, ctypes.c_void_p(QUIT_COMMAND_ID), None)
        time.sleep(1.5)
        if user32.FindWindowW("tray_icon_app", None):
            fail("quit menu item did not exit the app")
        print("PASS: menu 退出 shut the app down")


if __name__ == "__main__":
    main()
