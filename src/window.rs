//! Window, painting, menus, timers, application state and settings.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect,
    FillRgn, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromRect,
    MonitorFromWindow, SelectClipRgn, SelectObject, SetBkMode, SetTextColor, UpdateWindow,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_MEDIUM, FW_SEMIBOLD, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, DrawMenuBar, GetClientRect, GetCursorPos, GetMenu,
    GetMessageW, GetSystemMetrics, GetWindowRect, IsIconic, KillTimer,
    LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetMenu, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HICON, HMENU, ICON_BIG, ICON_SMALL,
    IDC_ARROW, MB_ICONWARNING, MB_OK, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, TPM_RIGHTBUTTON,
    WINDOW_EX_STYLE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_MOVE, WM_PAINT, WM_SETICON, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};

use crate::diagnose;
use crate::models::{AppUsageData, CodexResetCredits};
use crate::native_interop::{
    rgb, to_wide, to_wide_no_nul, HwndHandle, MSG_DATA_UPDATED, MSG_RESET_CREDITS_UPDATED,
    TIMER_COUNTDOWN, TIMER_FAST_POLL, TIMER_POLL, TIMER_RESET_CREDITS,
};
use crate::poller::{self, CredWatchMode, PollError, Provider};
use crate::strings;
use crate::theme;

// --- Layout constants (96 DPI base, scaled through `sc`) ---------------------

const ROW_HEIGHT: i32 = 13;
/// Fill bar width at 96 DPI.
const BAR_WIDTH: i32 = 109;
/// The fill bar is a single rounded (pill-shaped) strip, slimmer than the row
/// and vertically centered in it.
const BAR_HEIGHT: i32 = 7;
const LABEL_WIDTH: i32 = 18;
const LABEL_RIGHT_MARGIN: i32 = 10;
const BAR_RIGHT_MARGIN: i32 = 4;
const TEXT_WIDTH: i32 = 86;
const CONTENT_PADDING_X: i32 = 16;
const CONTENT_PADDING_Y: i32 = 14;
const ROW_GAP: i32 = 8;
const HEADING_GAP: i32 = 5;
const BLOCK_GAP: i32 = 14;
const ROW_INDENT: i32 = 10;
const MIN_CLIENT_WIDTH: i32 = 200;

const WINDOW_STYLE: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE =
    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
        WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0,
    );

// --- Menu command identifiers ------------------------------------------------

const ID_REFRESH: usize = 1;
const ID_EXIT: usize = 2;
const ID_POLL_INTERVAL_BASE: usize = 10; // 10..13
const ID_RESET_INTERVAL_BASE: usize = 20; // 20..23
const ID_PROVIDER_BASE: usize = 60; // 60..62

const POLL_INTERVALS: [(u64, &str); 4] = [
    (60_000, strings::MENU_1_MINUTE),
    (300_000, strings::MENU_5_MINUTES),
    (900_000, strings::MENU_15_MINUTES),
    (3_600_000, strings::MENU_1_HOUR),
];
const RESET_INTERVALS: [(u64, &str); 4] = [
    (3_600_000, strings::MENU_1_HOUR),
    (21_600_000, strings::MENU_6_HOURS),
    (43_200_000, strings::MENU_12_HOURS),
    (86_400_000, strings::MENU_1_DAY),
];

const DEFAULT_POLL_INTERVAL_MS: u64 = 900_000; // 15 minutes
const DEFAULT_RESET_INTERVAL_MS: u64 = 21_600_000; // 6 hours
const FAST_POLL_MS: u32 = 5_000;
const RETRY_BASE_MS: u64 = 30_000;

// --- Settings ----------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    window_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_y: Option<i32>,
    poll_interval_ms: u64,
    reset_credit_interval_ms: u64,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window_x: None,
            window_y: None,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            reset_credit_interval_ms: DEFAULT_RESET_INTERVAL_MS,
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok().filter(|s| !s.is_empty())?;
    Some(
        PathBuf::from(appdata)
            .join("UsageMeter")
            .join("settings.json"),
    )
}

fn load_settings() -> Settings {
    let mut settings = settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default();
    // Invariant: at least one provider is always enabled.
    if !settings.show_claude_code && !settings.show_codex && !settings.show_antigravity {
        settings.show_claude_code = true;
    }
    settings
}

fn save_settings() {
    let settings = {
        let guard = state();
        let Some(s) = guard.as_ref() else { return };
        Settings {
            window_x: s.window_x,
            window_y: s.window_y,
            poll_interval_ms: s.poll_interval_ms,
            reset_credit_interval_ms: s.reset_credit_interval_ms,
            show_claude_code: s.show_claude,
            show_codex: s.show_codex,
            show_antigravity: s.show_antigravity,
        }
    };
    let Some(path) = settings_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&settings) {
        if let Err(e) = std::fs::write(&path, json) {
            diagnose::log(&format!("failed to save settings: {e}"));
        }
    }
}

// --- Application state -------------------------------------------------------

#[derive(Default, Clone)]
struct ProviderView {
    pct_5h: f64,
    text_5h: String,
    pct_7d: f64,
    text_7d: String,
}

struct AppState {
    /// The shared state carries the window handle for completeness; threads
    /// receive the handle explicitly, so it is not read back.
    #[allow(dead_code)]
    hwnd: isize,
    dark: bool,
    claude: ProviderView,
    codex: ProviderView,
    antigravity: ProviderView,
    codex_reset_lines: Vec<String>,
    show_claude: bool,
    show_codex: bool,
    show_antigravity: bool,
    last_data: Option<AppUsageData>,
    last_reset_credits: Option<CodexResetCredits>,
    last_reset_count: Option<usize>,
    poll_interval_ms: u64,
    reset_credit_interval_ms: u64,
    retry_count: u32,
    force_notify_auth_error: bool,
    auth_error_paused_polling: bool,
    cred_watch_mode: Option<CredWatchMode>,
    cred_watch_snapshot: Vec<String>,
    last_poll_ok: bool,
    window_x: Option<i32>,
    window_y: Option<i32>,
}

impl AppState {
    fn enabled_count(&self) -> usize {
        [self.show_claude, self.show_codex, self.show_antigravity]
            .iter()
            .filter(|b| **b)
            .count()
    }
}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);
static DPI: AtomicU32 = AtomicU32::new(96);

/// Seed the DPI cache with the system DPI before the window exists.
pub fn set_initial_dpi(dpi: u32) {
    DPI.store(dpi, Ordering::Relaxed);
}

/// Poison-tolerant state lock: a panic in one thread must not permanently
/// break the UI.
fn state() -> std::sync::MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn placeholder_view(text: &str) -> ProviderView {
    ProviderView {
        pct_5h: 0.0,
        text_5h: text.to_string(),
        pct_7d: 0.0,
        text_7d: text.to_string(),
    }
}

/// Set the usage texts of all enabled providers to a placeholder, keeping the
/// bar percentages as they were.
fn set_enabled_texts(s: &mut AppState, text: &str) {
    if s.show_claude {
        s.claude.text_5h = text.to_string();
        s.claude.text_7d = text.to_string();
    }
    if s.show_codex {
        s.codex.text_5h = text.to_string();
        s.codex.text_7d = text.to_string();
    }
    if s.show_antigravity {
        s.antigravity.text_5h = text.to_string();
        s.antigravity.text_7d = text.to_string();
    }
}

/// Refresh texts and bars from the last successful data. Providers that are
/// enabled but delivered nothing show `!`.
fn apply_usage_data(s: &mut AppState, data: &AppUsageData, now: SystemTime) {
    fn apply(view: &mut ProviderView, usage: Option<&crate::models::UsageData>, now: SystemTime) {
        match usage {
            Some(u) => {
                view.pct_5h = u.session.percentage;
                view.text_5h = poller::format_usage_text(&u.session, now);
                view.pct_7d = u.weekly.percentage;
                view.text_7d = poller::format_usage_text(&u.weekly, now);
            }
            None => {
                view.text_5h = strings::PLACEHOLDER_ERROR.to_string();
                view.text_7d = strings::PLACEHOLDER_ERROR.to_string();
            }
        }
    }
    if s.show_claude {
        apply(&mut s.claude, data.claude_code.as_ref(), now);
    }
    if s.show_codex {
        apply(&mut s.codex, data.codex.as_ref(), now);
    }
    if s.show_antigravity {
        apply(&mut s.antigravity, data.antigravity.as_ref(), now);
    }
}

fn recompute_reset_lines(s: &mut AppState, now: SystemTime) {
    s.codex_reset_lines = match (&s.last_reset_credits, s.show_codex) {
        (Some(credits), true) => poller::build_reset_lines(credits, now),
        _ => Vec::new(),
    };
}

// --- Palette -----------------------------------------------------------------

struct Palette {
    background: COLORREF,
    label: COLORREF,
    track: COLORREF,
    accent_claude: COLORREF,
    accent_codex: COLORREF,
    accent_antigravity: COLORREF,
    value_claude: COLORREF,
    value_codex: COLORREF,
    value_antigravity: COLORREF,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            background: rgb(0x1C, 0x1C, 0x1C),
            label: rgb(0x88, 0x88, 0x88),
            track: rgb(0x44, 0x44, 0x44),
            accent_claude: rgb(0xD9, 0x77, 0x57),
            accent_codex: rgb(0xF5, 0xF5, 0xF5),
            accent_antigravity: rgb(0x42, 0x85, 0xF4),
            value_claude: rgb(0xF0, 0x9A, 0x7A),
            value_codex: rgb(0xF5, 0xF5, 0xF5),
            value_antigravity: rgb(0x8A, 0xB4, 0xF8),
        }
    } else {
        Palette {
            background: rgb(0xF3, 0xF3, 0xF3),
            label: rgb(0x40, 0x40, 0x40),
            track: rgb(0xAA, 0xAA, 0xAA),
            accent_claude: rgb(0xD9, 0x77, 0x57),
            accent_codex: rgb(0x1F, 0x1F, 0x1F),
            accent_antigravity: rgb(0x42, 0x85, 0xF4),
            value_claude: rgb(0xA9, 0x4F, 0x32),
            value_codex: rgb(0x1F, 0x1F, 0x1F),
            value_antigravity: rgb(0x19, 0x67, 0xD2),
        }
    }
}

// --- Render model ------------------------------------------------------------

#[derive(Clone)]
struct RenderRow {
    label: &'static str,
    percent: f64,
    text: String,
    has_bar: bool,
}

#[derive(Clone)]
struct RenderBlock {
    title: &'static str,
    accent: COLORREF,
    value_color: COLORREF,
    rows: Vec<RenderRow>,
}

struct RenderModel {
    blocks: Vec<RenderBlock>,
    show_headings: bool,
    dark: bool,
}

fn build_render_model() -> RenderModel {
    let guard = state();
    let Some(s) = guard.as_ref() else {
        return RenderModel {
            blocks: Vec::new(),
            show_headings: false,
            dark: true,
        };
    };
    let pal = palette(s.dark);
    let multi = s.enabled_count() >= 2;
    let mut blocks = Vec::new();
    let usage_rows = |view: &ProviderView| {
        vec![
            RenderRow {
                label: strings::LABEL_5H,
                percent: view.pct_5h,
                text: view.text_5h.clone(),
                has_bar: true,
            },
            RenderRow {
                label: strings::LABEL_7D,
                percent: view.pct_7d,
                text: view.text_7d.clone(),
                has_bar: true,
            },
        ]
    };
    // With a single provider the window title already names it: values are
    // drawn in a neutral color and block headings are skipped entirely.
    if s.show_claude {
        blocks.push(RenderBlock {
            title: strings::PROVIDER_CLAUDE_CODE,
            accent: pal.accent_claude,
            value_color: if multi { pal.value_claude } else { pal.value_codex },
            rows: usage_rows(&s.claude),
        });
    }
    if s.show_codex {
        let mut rows = usage_rows(&s.codex);
        for line in &s.codex_reset_lines {
            rows.push(RenderRow {
                label: strings::LABEL_RESET_CREDIT,
                percent: 0.0,
                text: line.clone(),
                has_bar: false,
            });
        }
        blocks.push(RenderBlock {
            title: strings::PROVIDER_CODEX,
            accent: pal.accent_codex,
            value_color: pal.value_codex,
            rows,
        });
    }
    if s.show_antigravity {
        blocks.push(RenderBlock {
            title: strings::PROVIDER_ANTIGRAVITY,
            accent: pal.accent_antigravity,
            value_color: if multi {
                pal.value_antigravity
            } else {
                pal.value_codex
            },
            rows: usage_rows(&s.antigravity),
        });
    }
    RenderModel {
        blocks,
        show_headings: multi,
        dark: s.dark,
    }
}

// --- Metrics -----------------------------------------------------------------

fn sc(px: i32, dpi: u32) -> i32 {
    ((px as f64) * (dpi as f64) / 96.0).round() as i32
}

struct Metrics {
    row_h: i32,
    bar_w: i32,
    bar_h: i32,
    label_w: i32,
    label_margin: i32,
    bar_margin: i32,
    text_w: i32,
    pad_x: i32,
    pad_y: i32,
    row_gap: i32,
    heading_gap: i32,
    block_gap: i32,
    row_indent: i32,
    min_client_w: i32,
}

impl Metrics {
    fn new(dpi: u32) -> Self {
        Self {
            row_h: sc(ROW_HEIGHT, dpi),
            bar_w: sc(BAR_WIDTH, dpi),
            bar_h: sc(BAR_HEIGHT, dpi),
            label_w: sc(LABEL_WIDTH, dpi),
            label_margin: sc(LABEL_RIGHT_MARGIN, dpi),
            bar_margin: sc(BAR_RIGHT_MARGIN, dpi),
            text_w: sc(TEXT_WIDTH, dpi),
            pad_x: sc(CONTENT_PADDING_X, dpi),
            pad_y: sc(CONTENT_PADDING_Y, dpi),
            row_gap: sc(ROW_GAP, dpi),
            heading_gap: sc(HEADING_GAP, dpi),
            block_gap: sc(BLOCK_GAP, dpi),
            row_indent: sc(ROW_INDENT, dpi),
            min_client_w: sc(MIN_CLIENT_WIDTH, dpi),
        }
    }

    fn bar_width(&self) -> i32 {
        self.bar_w
    }

    fn content_width(&self) -> i32 {
        2 * self.pad_x
            + self.row_indent
            + self.label_w
            + self.label_margin
            + self.bar_width()
            + self.bar_margin
            + self.text_w
    }

    fn client_width(&self) -> i32 {
        self.content_width().max(self.min_client_w)
    }

    fn block_height(&self, rows: usize, heading: bool) -> i32 {
        let rows = rows.max(1) as i32;
        let heading_h = if heading { self.row_h + self.heading_gap } else { 0 };
        heading_h + rows * self.row_h + (rows - 1) * self.row_gap
    }

    fn content_height(&self, model: &RenderModel) -> i32 {
        let blocks: i32 = model
            .blocks
            .iter()
            .map(|b| self.block_height(b.rows.len(), model.show_headings))
            .sum();
        let gaps = self.block_gap * (model.blocks.len().saturating_sub(1)) as i32;
        2 * self.pad_y + blocks + gaps
    }
}

// --- Window sizing and placement ---------------------------------------------

fn current_dpi() -> u32 {
    DPI.load(Ordering::Relaxed)
}

fn refresh_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi > 0 {
        DPI.store(dpi, Ordering::Relaxed);
        dpi
    } else {
        current_dpi()
    }
}

/// Outer window size for the given client size, accounting for the menu bar.
fn window_size_for_client(client_w: i32, client_h: i32, dpi: u32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    unsafe {
        // bmenu = true: without it the menu bar eats into the content.
        let _ = AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE, true, WINDOW_EX_STYLE(0), dpi);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn desired_window_size(dpi: u32) -> (i32, i32) {
    let model = build_render_model();
    let metrics = Metrics::new(dpi);
    let client_w = metrics.client_width();
    let client_h = metrics.content_height(&model);
    window_size_for_client(client_w, client_h, dpi)
}

/// Resize the window to fit its content. When the window grows past the work
/// area it is pulled up/left, but never moved when the user has deliberately
/// dragged it past the top or left edge.
fn resize_window_to_content(hwnd: HWND) {
    let dpi = refresh_dpi(hwnd);
    let (new_w, new_h) = desired_window_size(dpi);
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        let (cur_w, cur_h) = (rect.right - rect.left, rect.bottom - rect.top);
        let mut x = rect.left;
        let mut y = rect.top;
        if cur_w == new_w && cur_h == new_h {
            return;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            if x + new_w > work.right && x > work.left {
                x = (work.right - new_w).max(work.left);
            }
            if y + new_h > work.bottom && y > work.top {
                y = (work.bottom - new_h).max(work.top);
            }
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            new_w,
            new_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// A saved position is only used when the window rectangle would land on some
/// monitor; otherwise the window is centered on the current monitor.
fn validate_saved_position(x: i32, y: i32, w: i32, h: i32) -> bool {
    let rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    unsafe { !MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_invalid() }
}

fn center_on_current_monitor(w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            (
                work.left + (work.right - work.left - w) / 2,
                work.top + (work.bottom - work.top - h) / 2,
            )
        } else {
            (
                (GetSystemMetrics(SM_CXSCREEN) - w) / 2,
                (GetSystemMetrics(SM_CYSCREEN) - h) / 2,
            )
        }
    }
}

fn update_window_title(hwnd: HWND) {
    let title = {
        let guard = state();
        let Some(s) = guard.as_ref() else { return };
        title_for(s.show_claude, s.show_codex, s.show_antigravity)
    };
    let wide = to_wide(title);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

fn title_for(show_claude: bool, show_codex: bool, show_antigravity: bool) -> &'static str {
    match (show_claude, show_codex, show_antigravity) {
        (false, true, false) => strings::TITLE_CODEX_ONLY,
        (false, false, true) => strings::TITLE_ANTIGRAVITY_ONLY,
        _ => strings::TITLE_DEFAULT,
    }
}

fn apply_dark_titlebar(hwnd: HWND, dark: bool) {
    let value: i32 = if dark { 1 } else { 0 };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

/// Re-read the system theme; repaint and recolor the title bar on change.
fn check_theme(hwnd: HWND) {
    let dark = theme::is_dark_theme();
    let changed = {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        if s.dark != dark {
            s.dark = dark;
            true
        } else {
            false
        }
    };
    if changed {
        apply_dark_titlebar(hwnd, dark);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn set_window_icons(hwnd: HWND) {
    unsafe {
        let mut path = vec![0u16; 32_768];
        let len = windows::Win32::System::LibraryLoader::GetModuleFileNameW(None, &mut path);
        if len == 0 {
            return;
        }
        let mut big = HICON::default();
        let mut small = HICON::default();
        // The icon comes from the executable itself.
        ExtractIconExW(
            PCWSTR(path.as_ptr()),
            0,
            Some(&mut big),
            Some(&mut small),
            1,
        );
        if !big.is_invalid() {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(big.0 as isize)),
            );
        }
        if !small.is_invalid() {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(small.0 as isize)),
            );
        }
    }
}

// --- Menu --------------------------------------------------------------------

unsafe fn append_string(menu: HMENU, id: usize, text: &str, checked: bool) {
    let wide = to_wide(text);
    let flags = if checked {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
}

unsafe fn append_submenu(menu: HMENU, submenu: HMENU, text: &str) {
    let wide = to_wide(text);
    let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR(wide.as_ptr()));
}

/// One builder for both the menu bar dropdown and the context menu, so they
/// can never drift apart.
unsafe fn build_settings_popup() -> Option<HMENU> {
    let guard = state();
    let s = guard.as_ref()?;
    let menu = CreatePopupMenu().ok()?;
    append_string(menu, ID_REFRESH, strings::MENU_REFRESH, false);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

    let freq = CreatePopupMenu().ok()?;
    for (i, (ms, label)) in POLL_INTERVALS.iter().enumerate() {
        append_string(
            freq,
            ID_POLL_INTERVAL_BASE + i,
            label,
            s.poll_interval_ms == *ms,
        );
    }
    append_submenu(menu, freq, strings::MENU_UPDATE_FREQUENCY);

    if s.show_codex {
        let reset = CreatePopupMenu().ok()?;
        for (i, (ms, label)) in RESET_INTERVALS.iter().enumerate() {
            append_string(
                reset,
                ID_RESET_INTERVAL_BASE + i,
                label,
                s.reset_credit_interval_ms == *ms,
            );
        }
        append_submenu(menu, reset, strings::MENU_RESET_CREDIT_FREQUENCY);
    }

    let models = CreatePopupMenu().ok()?;
    append_string(
        models,
        ID_PROVIDER_BASE,
        strings::PROVIDER_CLAUDE_CODE,
        s.show_claude,
    );
    append_string(
        models,
        ID_PROVIDER_BASE + 1,
        strings::PROVIDER_CODEX,
        s.show_codex,
    );
    append_string(
        models,
        ID_PROVIDER_BASE + 2,
        strings::PROVIDER_ANTIGRAVITY,
        s.show_antigravity,
    );
    append_submenu(menu, models, strings::MENU_MODELS);

    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    append_string(menu, ID_EXIT, strings::MENU_EXIT, false);
    Some(menu)
}

/// Checked items cannot be updated in place: the menu is rebuilt from scratch
/// and the old handle destroyed after `SetMenu`.
fn rebuild_menu(hwnd: HWND) {
    unsafe {
        let Ok(bar) = CreateMenu() else { return };
        let Some(popup) = build_settings_popup() else {
            return;
        };
        append_submenu(bar, popup, strings::MENU_SETTINGS);
        let old = GetMenu(hwnd);
        if SetMenu(hwnd, Some(bar)).is_ok() {
            if !old.is_invalid() {
                let _ = DestroyMenu(old);
            }
            let _ = DrawMenuBar(hwnd);
        }
    }
}

fn show_context_menu(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let Some(popup) = build_settings_popup() else {
            return;
        };
        let (mut x, mut y) = (x, y);
        if x == -1 && y == -1 {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok() {
                x = rect.left + (rect.right - rect.left) / 2;
                y = rect.top + (rect.bottom - rect.top) / 2;
            }
        }
        // Without foreground activation the popup does not close on an
        // outside click.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(popup, TPM_RIGHTBUTTON, x, y, None, hwnd, None);
        let _ = DestroyMenu(popup);
    }
}

// --- Polling threads ---------------------------------------------------------

fn warning_text(provider: Provider, error: PollError) -> &'static str {
    match (provider, error) {
        (Provider::ClaudeCode, PollError::NoCredentials) => strings::WARN_CLAUDE_NO_CREDENTIALS,
        (Provider::ClaudeCode, _) => strings::WARN_CLAUDE_AUTH,
        (Provider::Codex, PollError::NoCredentials) => strings::WARN_CODEX_NO_CREDENTIALS,
        (Provider::Codex, _) => strings::WARN_CODEX_AUTH,
        (Provider::Antigravity, PollError::NoCredentials) => {
            strings::WARN_ANTIGRAVITY_NO_CREDENTIALS
        }
        (Provider::Antigravity, _) => strings::WARN_ANTIGRAVITY_AUTH,
    }
}

fn post_to_window(handle: HwndHandle, message: u32) {
    unsafe {
        let _ = PostMessageW(Some(handle.hwnd()), message, WPARAM(0), LPARAM(0));
    }
}

fn spawn_poll(hwnd: HWND) {
    let handle = HwndHandle::new(hwnd);
    std::thread::spawn(move || poll_body(handle));
}

/// One poll round: network in this background thread, state update under the
/// lock, then a posted message; painting happens only in the UI thread.
fn poll_body(handle: HwndHandle) {
    let Some((show_claude, show_codex, show_antigravity)) = ({
        let guard = state();
        guard
            .as_ref()
            .map(|s| (s.show_claude, s.show_codex, s.show_antigravity))
    }) else {
        return;
    };
    let result = poller::poll(show_claude, show_codex, show_antigravity);
    let now = SystemTime::now();

    let mut notify: Option<(Provider, PollError)> = None;
    let mut snapshot_mode: Option<CredWatchMode> = None;
    let mut refetch_credits = false;
    {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        match result {
            Ok(data) => {
                s.retry_count = 0;
                s.last_poll_ok = true;
                s.auth_error_paused_polling = false;
                s.cred_watch_mode = None;
                s.cred_watch_snapshot.clear();
                s.force_notify_auth_error = false;
                apply_usage_data(s, &data, now);
                recompute_reset_lines(s, now);
                if s.show_codex {
                    // The count piggybacked on the usage response is a free
                    // change signal for the full credit list.
                    if let Some(count) = data.codex_reset_credits_available {
                        refetch_credits = match s.last_reset_count {
                            Some(previous) => previous != count,
                            None => s.last_reset_credits.is_none(),
                        };
                        s.last_reset_count = Some(count);
                    }
                }
                s.last_data = Some(data);
            }
            Err((provider, error)) => {
                s.last_poll_ok = false;
                if error.is_auth_error() {
                    // Hammering the endpoint is pointless until the user signs
                    // in again: pause polling and watch the credentials.
                    set_enabled_texts(s, strings::PLACEHOLDER_ERROR);
                    let was_paused = s.auth_error_paused_polling;
                    s.auth_error_paused_polling = true;
                    let antigravity_only =
                        s.show_antigravity && !s.show_claude && !s.show_codex;
                    let mode = if antigravity_only {
                        CredWatchMode::Antigravity
                    } else if error == PollError::NoCredentials {
                        CredWatchMode::AllSources
                    } else {
                        CredWatchMode::ActiveSource
                    };
                    s.cred_watch_mode = Some(mode);
                    snapshot_mode = Some(mode);
                    if !was_paused || s.force_notify_auth_error {
                        notify = Some((provider, error));
                    }
                    s.force_notify_auth_error = false;
                } else {
                    set_enabled_texts(s, strings::PLACEHOLDER_LOADING);
                    s.retry_count = s.retry_count.saturating_add(1);
                }
            }
        }
    }

    post_to_window(handle, MSG_DATA_UPDATED);

    // The snapshot may probe WSL (seconds): collected outside the lock.
    if let Some(mode) = snapshot_mode {
        let snapshot = poller::credential_snapshot(mode, show_codex, show_antigravity);
        let mut guard = state();
        if let Some(s) = guard.as_mut() {
            if s.auth_error_paused_polling {
                s.cred_watch_snapshot = snapshot;
            }
        }
    }

    if let Some((provider, error)) = notify {
        // Modal warning from the background thread, owned by the main window.
        let text = to_wide(warning_text(provider, error));
        let caption = to_wide(strings::TITLE_DEFAULT);
        unsafe {
            MessageBoxW(
                Some(handle.hwnd()),
                PCWSTR(text.as_ptr()),
                PCWSTR(caption.as_ptr()),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    if refetch_credits {
        reset_credits_body(handle);
    }
}

fn spawn_reset_credits_poll(hwnd: HWND) {
    let handle = HwndHandle::new(hwnd);
    std::thread::spawn(move || reset_credits_body(handle));
}

fn reset_credits_body(handle: HwndHandle) {
    match poller::poll_codex_reset_credits() {
        Ok(credits) => {
            let now = SystemTime::now();
            {
                let mut guard = state();
                let Some(s) = guard.as_mut() else { return };
                if !s.show_codex {
                    return;
                }
                s.last_reset_credits = Some(credits);
                recompute_reset_lines(s, now);
            }
            post_to_window(handle, MSG_RESET_CREDITS_UPDATED);
        }
        Err(e) => {
            // Reset credits change over weeks: a failed request must not wipe
            // the last known list.
            diagnose::log(&format!("codex reset credits poll failed: {e:?}"));
        }
    }
}

/// While polling is paused on an auth error the poll timer only compares
/// credential snapshots; polling resumes as soon as the snapshot changes
/// (i.e. the user signed in again).
fn snapshot_check_body(handle: HwndHandle) {
    let Some((mode, old_snapshot, show_codex, show_antigravity)) = ({
        let guard = state();
        guard.as_ref().and_then(|s| {
            s.cred_watch_mode.map(|m| {
                (
                    m,
                    s.cred_watch_snapshot.clone(),
                    s.show_codex,
                    s.show_antigravity,
                )
            })
        })
    }) else {
        return;
    };
    let new_snapshot = poller::credential_snapshot(mode, show_codex, show_antigravity);
    if new_snapshot == old_snapshot {
        return;
    }
    diagnose::log("credential change detected, resuming polling");
    {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        s.auth_error_paused_polling = false;
        s.cred_watch_mode = None;
        s.cred_watch_snapshot.clear();
        s.retry_count = 0;
        s.last_poll_ok = false;
        set_enabled_texts(s, strings::PLACEHOLDER_LOADING);
    }
    post_to_window(handle, MSG_DATA_UPDATED);
    poll_body(handle);
}

fn spawn_snapshot_check(hwnd: HWND) {
    let handle = HwndHandle::new(hwnd);
    std::thread::spawn(move || snapshot_check_body(handle));
}

// --- Timer scheduling --------------------------------------------------------

fn set_timer(hwnd: HWND, id: usize, ms: u64) {
    unsafe {
        SetTimer(Some(hwnd), id, ms.min(u32::MAX as u64) as u32, None);
    }
}

fn kill_timer(hwnd: HWND, id: usize) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
}

fn retry_delay_ms(retry_count: u32, poll_interval_ms: u64) -> u64 {
    if retry_count == 0 {
        return poll_interval_ms;
    }
    // 30s * 2^(n-1), capped at the regular poll interval.
    let shift = (retry_count - 1).min(20);
    RETRY_BASE_MS
        .saturating_mul(1u64 << shift)
        .min(poll_interval_ms)
}

/// True when any visible usage window has a reset time in the past.
fn any_window_overdue(s: &AppState, now: SystemTime) -> bool {
    let Some(data) = &s.last_data else {
        return false;
    };
    let mut sections = Vec::new();
    if s.show_claude {
        if let Some(u) = &data.claude_code {
            sections.push(u.session.resets_at);
            sections.push(u.weekly.resets_at);
        }
    }
    if s.show_codex {
        if let Some(u) = &data.codex {
            sections.push(u.session.resets_at);
            sections.push(u.weekly.resets_at);
        }
    }
    if s.show_antigravity {
        if let Some(u) = &data.antigravity {
            sections.push(u.session.resets_at);
            sections.push(u.weekly.resets_at);
        }
    }
    sections.into_iter().flatten().any(|t| t <= now)
}

/// Schedule the countdown timer for the earliest upcoming text change across
/// all visible values (six usage values plus the reset-credit lines).
fn reschedule_countdown(hwnd: HWND) {
    let now = SystemTime::now();
    let mut min_secs: Option<u64> = None;
    {
        let guard = state();
        let Some(s) = guard.as_ref() else { return };
        let mut consider = |t: Option<SystemTime>| {
            if let Some(next) = t.and_then(|t| poller::seconds_until_text_change(t, now)) {
                min_secs = Some(min_secs.map_or(next, |m: u64| m.min(next)));
            }
        };
        if let Some(data) = &s.last_data {
            if s.show_claude {
                if let Some(u) = &data.claude_code {
                    consider(u.session.resets_at);
                    consider(u.weekly.resets_at);
                }
            }
            if s.show_codex {
                if let Some(u) = &data.codex {
                    consider(u.session.resets_at);
                    consider(u.weekly.resets_at);
                }
            }
            if s.show_antigravity {
                if let Some(u) = &data.antigravity {
                    consider(u.session.resets_at);
                    consider(u.weekly.resets_at);
                }
            }
        }
        if s.show_codex {
            if let Some(credits) = &s.last_reset_credits {
                for expiry in credits.expiries.iter().flatten() {
                    consider(Some(*expiry));
                }
            }
        }
    }
    // Nothing to count: 60 seconds; never below 1 second so the timer does not
    // spin idle.
    let secs = min_secs.unwrap_or(60).max(1);
    set_timer(hwnd, TIMER_COUNTDOWN, secs * 1000);
}

/// React to freshly polled data: theme check, repaint, timer replanning.
fn on_data_updated(hwnd: HWND) {
    check_theme(hwnd);
    let now = SystemTime::now();
    let (paused, ok, retry, interval, overdue) = {
        let guard = state();
        let Some(s) = guard.as_ref() else { return };
        (
            s.auth_error_paused_polling,
            s.last_poll_ok,
            s.retry_count,
            s.poll_interval_ms,
            any_window_overdue(s, now),
        )
    };
    if paused {
        // The poll timer keeps ticking, but only to compare credential
        // snapshots; countdown and fast-poll timers stop.
        kill_timer(hwnd, TIMER_COUNTDOWN);
        kill_timer(hwnd, TIMER_FAST_POLL);
        set_timer(hwnd, TIMER_POLL, interval);
    } else if ok {
        set_timer(hwnd, TIMER_POLL, interval);
        if overdue {
            // The server usually updates the numbers with a delay after a
            // window resets: poll frequently until nothing is overdue.
            set_timer(hwnd, TIMER_FAST_POLL, FAST_POLL_MS as u64);
        } else {
            kill_timer(hwnd, TIMER_FAST_POLL);
        }
        reschedule_countdown(hwnd);
    } else {
        // Transient failure: exponential backoff, capped at the poll interval.
        set_timer(hwnd, TIMER_POLL, retry_delay_ms(retry, interval));
        kill_timer(hwnd, TIMER_FAST_POLL);
        kill_timer(hwnd, TIMER_COUNTDOWN);
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn on_countdown_tick(hwnd: HWND) {
    let now = SystemTime::now();
    {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        // Only recompute from real data; placeholders stay as they are.
        if s.last_poll_ok {
            if let Some(data) = s.last_data.clone() {
                apply_usage_data(s, &data, now);
            }
        }
        recompute_reset_lines(s, now);
    }
    // An expired reset credit removes its line: the window may shrink.
    resize_window_to_content(hwnd);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    reschedule_countdown(hwnd);
}

// --- Command handling --------------------------------------------------------

fn on_refresh(hwnd: HWND) {
    {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        set_enabled_texts(s, strings::PLACEHOLDER_LOADING);
        s.force_notify_auth_error = true;
        s.retry_count = 0;
        s.last_poll_ok = false;
        s.auth_error_paused_polling = false;
        s.cred_watch_mode = None;
        s.cred_watch_snapshot.clear();
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    let codex_enabled = {
        let guard = state();
        guard.as_ref().map(|s| s.show_codex).unwrap_or(false)
    };
    spawn_poll(hwnd);
    if codex_enabled {
        spawn_reset_credits_poll(hwnd);
    }
}

fn on_poll_interval_selected(hwnd: HWND, index: usize) {
    let Some((ms, _)) = POLL_INTERVALS.get(index) else {
        return;
    };
    {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        s.poll_interval_ms = *ms;
    }
    save_settings();
    rebuild_menu(hwnd);
    set_timer(hwnd, TIMER_POLL, *ms);
}

fn on_reset_interval_selected(hwnd: HWND, index: usize) {
    let Some((ms, _)) = RESET_INTERVALS.get(index) else {
        return;
    };
    let codex_enabled = {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        s.reset_credit_interval_ms = *ms;
        s.show_codex
    };
    save_settings();
    rebuild_menu(hwnd);
    if codex_enabled {
        set_timer(hwnd, TIMER_RESET_CREDITS, *ms);
    }
}

fn on_provider_toggled(hwnd: HWND, index: usize) {
    let (codex_on_now, codex_was_on, reset_interval) = {
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        let currently = match index {
            0 => s.show_claude,
            1 => s.show_codex,
            2 => s.show_antigravity,
            _ => return,
        };
        // The last enabled provider cannot be switched off.
        if currently && s.enabled_count() == 1 {
            return;
        }
        let codex_was_on = s.show_codex;
        match index {
            0 => s.show_claude = !s.show_claude,
            1 => s.show_codex = !s.show_codex,
            2 => s.show_antigravity = !s.show_antigravity,
            _ => {}
        }
        set_enabled_texts(s, strings::PLACEHOLDER_LOADING);
        s.last_poll_ok = false;
        s.retry_count = 0;
        s.auth_error_paused_polling = false;
        s.cred_watch_mode = None;
        s.cred_watch_snapshot.clear();
        if codex_was_on && !s.show_codex {
            // Disabling Codex clears the credit list, its lines and the count.
            s.last_reset_credits = None;
            s.last_reset_count = None;
            s.codex_reset_lines.clear();
        }
        (s.show_codex, codex_was_on, s.reset_credit_interval_ms)
    };
    update_window_title(hwnd);
    save_settings();
    if codex_on_now {
        set_timer(hwnd, TIMER_RESET_CREDITS, reset_interval);
    } else {
        kill_timer(hwnd, TIMER_RESET_CREDITS);
    }
    rebuild_menu(hwnd);
    resize_window_to_content(hwnd);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    spawn_poll(hwnd);
    if codex_on_now && !codex_was_on {
        spawn_reset_credits_poll(hwnd);
    }
}

fn on_command(hwnd: HWND, id: usize) {
    match id {
        ID_REFRESH => on_refresh(hwnd),
        ID_EXIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ if (ID_POLL_INTERVAL_BASE..ID_POLL_INTERVAL_BASE + POLL_INTERVALS.len()).contains(&id) => {
            on_poll_interval_selected(hwnd, id - ID_POLL_INTERVAL_BASE);
        }
        _ if (ID_RESET_INTERVAL_BASE..ID_RESET_INTERVAL_BASE + RESET_INTERVALS.len())
            .contains(&id) =>
        {
            on_reset_interval_selected(hwnd, id - ID_RESET_INTERVAL_BASE);
        }
        _ if (ID_PROVIDER_BASE..ID_PROVIDER_BASE + 3).contains(&id) => {
            on_provider_toggled(hwnd, id - ID_PROVIDER_BASE);
        }
        _ => {}
    }
}

// --- Painting ----------------------------------------------------------------

unsafe fn create_font(dpi: u32, weight: i32) -> windows::Win32::Graphics::Gdi::HFONT {
    let face = to_wide("Segoe UI");
    CreateFontW(
        -sc(12, dpi),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
        PCWSTR(face.as_ptr()),
    )
}

unsafe fn draw_text_in(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    rect: RECT,
    color: COLORREF,
    align_right: bool,
) {
    SetTextColor(hdc, color);
    let mut wide = to_wide_no_nul(text);
    let mut rect = rect;
    let format = DT_SINGLELINE
        | DT_VCENTER
        | DT_NOPREFIX
        | if align_right { DT_RIGHT } else { DT_LEFT };
    DrawTextW(hdc, &mut wide, &mut rect, format);
}

/// Draw one fill bar: a single pill-shaped track, slimmer than the row and
/// vertically centered in it, filled from the left. The fill is floored to
/// whole pixels and skipped at zero. GDI regions exclude the right/bottom
/// edge, hence the +1 when creating them.
unsafe fn draw_bar(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    m: &Metrics,
    x: i32,
    row_y: i32,
    percent: f64,
    accent: windows::Win32::Graphics::Gdi::HBRUSH,
    track: windows::Win32::Graphics::Gdi::HBRUSH,
) {
    let y = row_y + (m.row_h - m.bar_h) / 2;
    // The rounding ellipse equals the bar height: fully rounded ends.
    let region = CreateRoundRectRgn(x, y, x + m.bar_w + 1, y + m.bar_h + 1, m.bar_h, m.bar_h);
    let _ = FillRgn(hdc, region, track);
    let fill_px = (m.bar_w as f64 * (percent / 100.0).clamp(0.0, 1.0)).floor() as i32;
    if fill_px > 0 {
        SelectClipRgn(hdc, Some(region));
        let rect = RECT {
            left: x,
            top: y,
            right: x + fill_px,
            bottom: y + m.bar_h + 1,
        };
        FillRect(hdc, &rect, accent);
        SelectClipRgn(hdc, None);
    }
    let _ = DeleteObject(region.into());
}

unsafe fn on_paint(hwnd: HWND) {
    let dpi = refresh_dpi(hwnd);
    let model = build_render_model();
    let pal = palette(model.dark);
    let m = Metrics::new(dpi);

    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let (cw, ch) = (client.right - client.left, client.bottom - client.top);
    if cw <= 0 || ch <= 0 {
        let _ = EndPaint(hwnd, &ps);
        return;
    }

    // Double buffering: draw into a compatible bitmap, then one BitBlt.
    let mem = CreateCompatibleDC(Some(hdc));
    let bitmap = CreateCompatibleBitmap(hdc, cw, ch);
    let old_bitmap = SelectObject(mem, bitmap.into());

    let bg_brush = CreateSolidBrush(pal.background);
    FillRect(mem, &client, bg_brush);

    let track_brush = CreateSolidBrush(pal.track);
    let font = create_font(dpi, FW_MEDIUM.0 as i32);
    let font_bold = create_font(dpi, FW_SEMIBOLD.0 as i32);
    let old_font = SelectObject(mem, font.into());
    SetBkMode(mem, TRANSPARENT);

    // Content is centered inside the client area: the window can be wider
    // because of MIN_CLIENT_WIDTH.
    let content_w = m.content_width();
    let content_h = m.content_height(&model);
    let origin_x = (cw - content_w) / 2;
    let mut y = (ch - content_h) / 2 + m.pad_y;
    let content_x = origin_x + m.pad_x;
    let row_x = content_x + m.row_indent;
    let bar_x = row_x + m.label_w + m.label_margin;
    let text_x = bar_x + m.bar_width() + m.bar_margin;

    for (block_index, block) in model.blocks.iter().enumerate() {
        if block_index > 0 {
            y += m.block_gap;
        }
        if model.show_headings {
            SelectObject(mem, font_bold.into());
            draw_text_in(
                mem,
                block.title,
                RECT {
                    left: content_x,
                    top: y,
                    right: content_x + content_w - 2 * m.pad_x,
                    bottom: y + m.row_h,
                },
                block.value_color,
                false,
            );
            SelectObject(mem, font.into());
            y += m.row_h + m.heading_gap;
        }
        let accent_brush = CreateSolidBrush(block.accent);
        for (row_index, row) in block.rows.iter().enumerate() {
            if row_index > 0 {
                y += m.row_gap;
            }
            draw_text_in(
                mem,
                row.label,
                RECT {
                    left: row_x,
                    top: y,
                    right: row_x + m.label_w,
                    bottom: y + m.row_h,
                },
                pal.label,
                true,
            );
            if row.has_bar {
                draw_bar(mem, &m, bar_x, y, row.percent, accent_brush, track_brush);
            }
            // Reset-credit lines align their text with the percent column.
            draw_text_in(
                mem,
                &row.text,
                RECT {
                    left: text_x,
                    top: y,
                    right: text_x + m.text_w,
                    bottom: y + m.row_h,
                },
                block.value_color,
                false,
            );
            y += m.row_h;
        }
        let _ = DeleteObject(accent_brush.into());
    }

    let _ = BitBlt(hdc, 0, 0, cw, ch, Some(mem), 0, 0, SRCCOPY);

    // GDI objects leak by the hour if not deleted: the window repaints often.
    SelectObject(mem, old_font);
    SelectObject(mem, old_bitmap);
    let _ = DeleteObject(font.into());
    let _ = DeleteObject(font_bold.into());
    let _ = DeleteObject(bg_brush.into());
    let _ = DeleteObject(track_brush.into());
    let _ = DeleteObject(bitmap.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

// --- Window procedure --------------------------------------------------------

extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            unsafe { on_paint(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            // The background is painted by us; returning 1 avoids flicker.
            LRESULT(1)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_POLL => {
                    let paused = {
                        let guard = state();
                        guard
                            .as_ref()
                            .map(|s| s.auth_error_paused_polling)
                            .unwrap_or(false)
                    };
                    if paused {
                        spawn_snapshot_check(hwnd);
                    } else {
                        spawn_poll(hwnd);
                    }
                }
                TIMER_COUNTDOWN => on_countdown_tick(hwnd),
                TIMER_FAST_POLL => spawn_poll(hwnd),
                TIMER_RESET_CREDITS => {
                    let codex = {
                        let guard = state();
                        guard.as_ref().map(|s| s.show_codex).unwrap_or(false)
                    };
                    if codex {
                        spawn_reset_credits_poll(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        MSG_DATA_UPDATED => {
            on_data_updated(hwnd);
            LRESULT(0)
        }
        MSG_RESET_CREDITS_UPDATED => {
            resize_window_to_content(hwnd);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            reschedule_countdown(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            on_command(hwnd, (wparam.0 & 0xFFFF) as usize);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            // Only inside the client area; the title bar keeps its system menu.
            // (-1, -1) means the keyboard menu key.
            let in_client = (x == -1 && y == -1) || unsafe {
                let mut point = POINT { x, y };
                let mut client = RECT::default();
                windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point).as_bool()
                    && GetClientRect(hwnd, &mut client).is_ok()
                    && point.x >= client.left
                    && point.x < client.right
                    && point.y >= client.top
                    && point.y < client.bottom
            };
            if in_client {
                show_context_menu(hwnd, x, y);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_MOVE => {
            // A minimized window reports meaningless coordinates.
            unsafe {
                if !IsIconic(hwnd).as_bool() {
                    let mut rect = RECT::default();
                    if GetWindowRect(hwnd, &mut rect).is_ok() {
                        let mut guard = state();
                        if let Some(s) = guard.as_mut() {
                            s.window_x = Some(rect.left);
                            s.window_y = Some(rect.top);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xFFFF) as u32;
            if new_dpi > 0 {
                DPI.store(new_dpi, Ordering::Relaxed);
            }
            unsafe {
                // Apply the system-suggested rectangle first, then refit the
                // window to its content.
                let suggested = lparam.0 as *const RECT;
                if !suggested.is_null() {
                    let r = *suggested;
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            resize_window_to_content(hwnd);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            resize_window_to_content(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            check_theme(hwnd);
            resize_window_to_content(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            save_settings();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

// --- Startup -----------------------------------------------------------------

pub fn run() {
    let settings = load_settings();
    let dark = theme::is_dark_theme();

    let class_name = to_wide(strings::WINDOW_CLASS);
    let hinstance = unsafe {
        windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|h| h.into())
            .unwrap_or_default()
    };
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: PCWSTR(class_name.as_ptr()),
        // Empty background brush: the paint handler fills everything itself.
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        diagnose::log("RegisterClassW failed");
        return;
    }

    // Initial sizing uses the system DPI; the window is refit to its
    // per-monitor DPI right after creation.
    let dpi = current_dpi();
    let metrics = Metrics::new(dpi);
    let initial_model = initial_render_model(&settings);
    let client_w = metrics.client_width();
    let client_h = metrics.content_height(&initial_model);
    let (win_w, win_h) = window_size_for_client(client_w, client_h, dpi);

    // CW_USEDEFAULT would let the shell's STARTUPINFO override the position on
    // the first ShowWindow: coordinates are always passed explicitly.
    let saved_position = match (settings.window_x, settings.window_y) {
        (Some(x), Some(y)) if validate_saved_position(x, y, win_w, win_h) => Some((x, y)),
        _ => None,
    };
    let (x, y) = saved_position.unwrap_or((0, 0));

    let title = to_wide(title_for(
        settings.show_claude_code,
        settings.show_codex,
        settings.show_antigravity,
    ));
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            diagnose::log(&format!("CreateWindowExW failed: {e}"));
            return;
        }
    };
    diagnose::log("window created");

    {
        let mut guard = state();
        *guard = Some(AppState {
            hwnd: hwnd.0 as isize,
            dark,
            claude: placeholder_view(strings::PLACEHOLDER_NONE),
            codex: placeholder_view(strings::PLACEHOLDER_NONE),
            antigravity: placeholder_view(strings::PLACEHOLDER_NONE),
            codex_reset_lines: Vec::new(),
            show_claude: settings.show_claude_code,
            show_codex: settings.show_codex,
            show_antigravity: settings.show_antigravity,
            last_data: None,
            last_reset_credits: None,
            last_reset_count: None,
            poll_interval_ms: settings.poll_interval_ms,
            reset_credit_interval_ms: settings.reset_credit_interval_ms,
            retry_count: 0,
            force_notify_auth_error: false,
            auth_error_paused_polling: false,
            cred_watch_mode: None,
            cred_watch_snapshot: Vec::new(),
            last_poll_ok: false,
            window_x: settings.window_x,
            window_y: settings.window_y,
        });
    }

    // Center on the current monitor before showing when there is no usable
    // saved position.
    if saved_position.is_none() {
        let (cx, cy) = center_on_current_monitor(win_w, win_h);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                cx,
                cy,
                win_w,
                win_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    rebuild_menu(hwnd);
    apply_dark_titlebar(hwnd, dark);
    set_window_icons(hwnd);
    // Refit under the real per-monitor DPI (the menu bar can also wrap).
    resize_window_to_content(hwnd);

    // Remember the actual initial position.
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            let mut guard = state();
            if let Some(s) = guard.as_mut() {
                s.window_x = Some(rect.left);
                s.window_y = Some(rect.top);
            }
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
    }

    let (poll_interval, codex_enabled, reset_interval) = {
        let guard = state();
        let s = guard.as_ref().expect("state initialized above");
        (
            s.poll_interval_ms,
            s.show_codex,
            s.reset_credit_interval_ms,
        )
    };
    set_timer(hwnd, TIMER_POLL, poll_interval);
    spawn_poll(hwnd);
    if codex_enabled {
        set_timer(hwnd, TIMER_RESET_CREDITS, reset_interval);
        spawn_reset_credits_poll(hwnd);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Render model used before the app state exists, for initial window sizing.
fn initial_render_model(settings: &Settings) -> RenderModel {
    let empty_rows = || {
        vec![
            RenderRow {
                label: strings::LABEL_5H,
                percent: 0.0,
                text: String::new(),
                has_bar: true,
            },
            RenderRow {
                label: strings::LABEL_7D,
                percent: 0.0,
                text: String::new(),
                has_bar: true,
            },
        ]
    };
    let pal = palette(true);
    let mut blocks = Vec::new();
    for enabled in [
        settings.show_claude_code,
        settings.show_codex,
        settings.show_antigravity,
    ] {
        if enabled {
            blocks.push(RenderBlock {
                title: "",
                accent: pal.accent_claude,
                value_color: pal.value_claude,
                rows: empty_rows(),
            });
        }
    }
    let show_headings = blocks.len() >= 2;
    RenderModel {
        blocks,
        show_headings,
        dark: true,
    }
}
