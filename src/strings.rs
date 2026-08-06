//! All user-visible strings (English only).

pub const WINDOW_CLASS: &str = "UsageMeter";
pub const MUTEX_NAME: &str = "Global\\UsageMeter";

pub const TITLE_DEFAULT: &str = "Usage Meter";
pub const TITLE_CODEX_ONLY: &str = "Codex Meter";
pub const TITLE_ANTIGRAVITY_ONLY: &str = "Antigravity Meter";

pub const PROVIDER_CLAUDE_CODE: &str = "Claude Code";
pub const PROVIDER_CODEX: &str = "Codex";
pub const PROVIDER_ANTIGRAVITY: &str = "Antigravity";

pub const LABEL_5H: &str = "5h";
pub const LABEL_7D: &str = "7d";
pub const LABEL_RESET_CREDIT: &str = "reset";

pub const PLACEHOLDER_NONE: &str = "--";
pub const PLACEHOLDER_LOADING: &str = "...";
pub const PLACEHOLDER_ERROR: &str = "!";

pub const MENU_SETTINGS: &str = "Settings";
pub const MENU_REFRESH: &str = "Refresh";
pub const MENU_UPDATE_FREQUENCY: &str = "Update Frequency";
pub const MENU_RESET_CREDIT_FREQUENCY: &str = "Reset Credit Frequency";
pub const MENU_MODELS: &str = "Models";
pub const MENU_SHOW_PERCENT: &str = "Show Percentage";
pub const MENU_EXIT: &str = "Exit";

pub const MENU_5_MINUTES: &str = "5 Minutes";
pub const MENU_15_MINUTES: &str = "15 Minutes";
pub const MENU_1_HOUR: &str = "1 Hour";
pub const MENU_6_HOURS: &str = "6 Hours";
pub const MENU_12_HOURS: &str = "12 Hours";
pub const MENU_1_DAY: &str = "1 Day";

pub const WARN_CLAUDE_NO_CREDENTIALS: &str = "Claude Code credentials were not found.\n\nSign in with the Claude Code CLI (run \"claude\" in a terminal); the monitor will pick it up automatically.";
pub const WARN_CLAUDE_AUTH: &str = "Claude Code authorization expired or was rejected.\n\nSign in with the Claude Code CLI (run \"claude\" in a terminal); the monitor will pick it up automatically.";
pub const WARN_CODEX_NO_CREDENTIALS: &str = "Codex credentials were not found.\n\nSign in with the Codex CLI (run \"codex\" in a terminal); the monitor will pick it up automatically.";
pub const WARN_CODEX_AUTH: &str = "Codex authorization expired or was rejected.\n\nSign in with the Codex CLI (run \"codex\" in a terminal); the monitor will pick it up automatically.";
pub const WARN_ANTIGRAVITY_NO_CREDENTIALS: &str = "Antigravity credentials were not found.\n\nSign in from the Antigravity IDE; the monitor will pick it up automatically.";
pub const WARN_ANTIGRAVITY_AUTH: &str = "Antigravity authorization expired or was rejected.\n\nSign in from the Antigravity IDE; the monitor will pick it up automatically.";
