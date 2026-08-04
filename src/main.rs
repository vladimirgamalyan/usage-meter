#![windows_subsystem = "windows"]

mod diagnose;
mod http;
mod models;
mod native_interop;
mod poller;
mod strings;
mod theme;
mod window;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, SetLastError, ERROR_ALREADY_EXISTS, WIN32_ERROR};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

use native_interop::to_wide;

fn main() {
    if std::env::args().any(|a| a == "--diagnose") {
        diagnose::init();
    }
    diagnose::log("starting");

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // Cached start value; refined per window via GetDpiForWindow later
    // (GetDpiForSystem is frozen at process start).
    let system_dpi = unsafe { GetDpiForSystem() };
    if system_dpi > 0 {
        window::set_initial_dpi(system_dpi);
    }

    // Single instance: when the named mutex already exists, surface the
    // running instance's window and exit. Clear the last error first: earlier
    // calls (e.g. recreating an existing log file) may have left
    // ERROR_ALREADY_EXISTS behind.
    let mutex_name = to_wide(strings::MUTEX_NAME);
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let _mutex = unsafe { CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        diagnose::log("another instance is already running, exiting");
        bring_existing_instance_to_front();
        return;
    }

    window::run();
}

fn bring_existing_instance_to_front() {
    let class_name = to_wide(strings::WINDOW_CLASS);
    unsafe {
        if let Ok(existing) = FindWindowW(PCWSTR(class_name.as_ptr()), None) {
            if !existing.is_invalid() {
                if IsIconic(existing).as_bool() {
                    let _ = ShowWindow(existing, SW_RESTORE);
                }
                let _ = SetForegroundWindow(existing);
            }
        }
    }
}
