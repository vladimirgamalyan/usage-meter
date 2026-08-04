//! Usage data structures shared between the poller and the window.

use std::time::SystemTime;

/// One rate-limit window: how much is used (percent, 0..100) and when it resets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageSection {
    /// Consumed share in percent (0..100), not the remainder.
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
}

impl UsageSection {
    /// A window the provider does not report: rendered as `--`, not `0%`.
    pub fn is_unsupported(&self) -> bool {
        self.resets_at.is_none() && self.percentage == 0.0
    }
}

/// Usage of one provider: the 5-hour window and the weekly window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageData {
    pub session: UsageSection,
    pub weekly: UsageSection,
}

/// A weekly limit scoped to a single model (e.g. Fable), reported by the
/// Claude usage endpoint alongside the shared windows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopedLimit {
    /// Model display name from the API, shown as the row label.
    pub label: String,
    pub section: UsageSection,
}

/// Result of one poll round across all enabled providers.
#[derive(Clone, Debug, Default)]
pub struct AppUsageData {
    pub claude_code: Option<UsageData>,
    /// Model-scoped weekly limits reported by the Claude usage endpoint;
    /// empty when the endpoint (or the fallback path) does not report any.
    pub claude_scoped: Vec<ScopedLimit>,
    pub codex: Option<UsageData>,
    pub antigravity: Option<UsageData>,
    /// `available_count` piggybacked on the Codex usage response; used as a
    /// free change signal for re-fetching the full reset-credit list.
    pub codex_reset_credits_available: Option<usize>,
}

/// The Codex rate-limit-reset credit list. `None` expiry means the credit has
/// no known expiration date.
#[derive(Clone, Debug, Default)]
pub struct CodexResetCredits {
    pub expiries: Vec<Option<SystemTime>>,
}
