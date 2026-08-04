//! Timer/message identifiers, color and wide-string helpers, `Send` HWND wrapper.

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const TIMER_POLL: usize = 1;
pub const TIMER_COUNTDOWN: usize = 2;
pub const TIMER_FAST_POLL: usize = 3;
pub const TIMER_RESET_CREDITS: usize = 4;

/// Posted by a background thread after the usage state was updated.
pub const MSG_DATA_UPDATED: u32 = WM_APP + 1;
/// Posted by a background thread after the Codex reset-credit list was updated.
pub const MSG_RESET_CREDITS_UPDATED: u32 = WM_APP + 2;

/// NUL-terminated UTF-16 for Win32 `PCWSTR` arguments.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 without the terminator, for `DrawTextW`-style slice arguments.
pub fn to_wide_no_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// `HWND` is not `Send`; background threads carry it as a raw integer.
#[derive(Clone, Copy)]
pub struct HwndHandle(isize);

unsafe impl Send for HwndHandle {}

impl HwndHandle {
    pub fn new(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    pub fn hwnd(self) -> HWND {
        HWND(self.0 as *mut core::ffi::c_void)
    }
}
