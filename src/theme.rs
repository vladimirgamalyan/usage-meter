//! Windows light/dark theme detection.

use windows::core::w;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

/// True when the system uses the dark theme.
///
/// Reads `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`
/// `SystemUsesLightTheme`; the value `1` means light. Any read error is
/// treated as dark.
pub fn is_dark_theme() -> bool {
    unsafe {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        );
        !(status == ERROR_SUCCESS && value == 1)
    }
}
