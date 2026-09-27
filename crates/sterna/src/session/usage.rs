//! What each subscription has used of its limits, as the gateway reads it
//! from the provider (`inference-gateway subscriptions usage --json`): the
//! `/usage` panel, and one line at session start for a window that is
//! nearly spent. Figures are the provider's; a window it does not report is
//! not shown, and an account it would not answer for says why.

use serde::Deserialize;

use super::Session;
use crate::tui::{self, Panel};

/// A window at or above this share used is said at session start.
pub(super) const WARN_AT_PERCENT: f64 = 80.0;
const BAR_WIDTH: usize = 20;

#[derive(Debug, Deserialize)]
pub(super) struct Usage {
    #[serde(default)]
    pub accounts: Vec<AccountUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AccountUsage {
    pub account: String,
    pub plan: Option<String>,
    pub email: Option<String>,
    #[serde(default)]
    pub windows: Vec<Window>,
    #[serde(default)]
    pub limited: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Window {
    pub name: String,
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

/// The gateway's reading, or `None` when it could not be run.
pub(super) fn read(session: &Session<'_>) -> Option<Usage> {
    read_from(session.gateway)
}

fn read_from(gateway: &crate::gateway::Gateway) -> Option<Usage> {
    let bytes = gateway.run(&["subscriptions", "usage", "--json"], None)?;
    let usage: Usage = serde_json::from_slice(&bytes).ok()?;
    if let Ok(mut latest) = LATEST.lock() {
        *latest = Some(
            usage
                .accounts
                .iter()
                .map(|a| (a.account.clone(), summary(a)))
                .collect(),
        );
    }
    Some(usage)
}

/// The last reading's one-line summary per account, for the model picker:
/// the picker never waits on the providers, it shows what was last read.
static LATEST: std::sync::Mutex<Option<Vec<(String, String)>>> = std::sync::Mutex::new(None);

/// `Max 20x · 5h 4% · week 16%`, from the last reading, when there was one.
pub(super) fn latest_summary(account: &str) -> Option<String> {
    let latest = LATEST.lock().ok()?;
    let lines: Vec<&str> = latest
        .as_ref()?
        .iter()
        .filter(|(name, _)| name == account)
        .map(|(_, line)| line.as_str())
        .collect();
    (!lines.is_empty()).then(|| lines.join(" | "))
}

fn summary(account: &AccountUsage) -> String {
    let mut parts: Vec<String> = account.plan.iter().cloned().collect();
    if let Some(error) = &account.error {
        parts.push(error.clone());
    }
    parts.extend(
        account
            .windows
            .iter()
            .map(|w| format!("{} {:.0}%", w.name, w.used_percent)),
    );
    parts.join(" · ")
}

/// Warnings the start-of-session check left, shown at the next task's end.
static WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Reads usage once, on its own thread (two provider round trips must not
/// hold the first prompt), and leaves a line for each nearly-spent window.
pub(super) fn check_in_background(gateway: &crate::gateway::Gateway) {
    let gateway = gateway.clone();
    std::thread::spawn(move || {
        let Some(usage) = read_from(&gateway) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        if let Ok(mut held) = WARNINGS.lock() {
            held.extend(warnings(&usage, now));
        }
    });
}

/// The warnings left so far, taken once.
pub(super) fn take_warnings() -> Vec<String> {
    WARNINGS
        .lock()
        .map(|mut held| std::mem::take(&mut *held))
        .unwrap_or_default()
}

/// `/usage`: every subscription with a bar per window.
pub(super) fn panel(usage: Option<&Usage>, now_unix: i64) -> Panel {
    let Some(usage) = usage else {
        return Panel::text(
            "Usage",
            "The gateway could not be asked for subscription usage.",
        );
    };
    if usage.accounts.is_empty() {
        return Panel::text(
            "Usage",
            "No subscription account is configured; /login connects one.",
        );
    }
    let mut rows = Vec::new();
    for account in &usage.accounts {
        let who = [account.plan.as_deref(), account.email.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        rows.push(tui::PanelRow {
            text: if who.is_empty() {
                account.account.clone()
            } else {
                format!("{} — {who}", account.account)
            },
            command: None,
        });
        if let Some(error) = &account.error {
            rows.push(tui::PanelRow {
                text: format!("  {error}"),
                command: None,
            });
        }
        for window in &account.windows {
            rows.push(tui::PanelRow {
                text: window_line(window, now_unix),
                command: None,
            });
        }
        if account.limited {
            rows.push(tui::PanelRow {
                text: "  a limit is reached now".to_string(),
                command: None,
            });
        }
    }
    Panel::rows("Usage", rows)
}

/// `  week   ████████████████░░░░  84%  resets in 2d 22h`
pub(super) fn window_line(window: &Window, now_unix: i64) -> String {
    let used = window.used_percent.clamp(0.0, 100.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=100 above"
    )]
    let filled = ((used / 100.0) * BAR_WIDTH as f64).round() as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled));
    let resets = window
        .resets_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|at| format!("  resets in {}", span(at - now_unix)))
        .unwrap_or_default();
    format!("  {:<14} {bar} {:>3.0}%{resets}", window.name, used)
}

/// The start-of-session lines: every window at or past [`WARN_AT_PERCENT`].
pub(super) fn warnings(usage: &Usage, now_unix: i64) -> Vec<String> {
    let mut lines = Vec::new();
    for account in &usage.accounts {
        for window in &account.windows {
            if window.used_percent >= WARN_AT_PERCENT {
                let resets = window
                    .resets_at
                    .as_deref()
                    .and_then(parse_rfc3339)
                    .map(|at| format!(", resets in {}", span(at - now_unix)))
                    .unwrap_or_default();
                lines.push(format!(
                    "usage: {} {} is {:.0}% used{resets} — /usage shows every limit",
                    account.account, window.name, window.used_percent
                ));
            }
        }
    }
    lines
}

/// `2d 22h`, `3h 14m`, `12m`, `now`.
fn span(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_string();
    }
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{}m", minutes.max(1))
    }
}

/// `2026-09-23T10:29:59.89+00:00` or `…Z` as Unix seconds.
fn parse_rfc3339(text: &str) -> Option<i64> {
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, offset) = match time.find(['+', '-', 'Z']) {
        Some(at) => time.split_at(at),
        None => (time, "Z"),
    };
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: f64 = clock.next().unwrap_or("0").parse().ok()?;
    let offset_seconds = if offset == "Z" {
        0
    } else {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (h, m) = offset[1..].split_once(':')?;
        sign * (h.parse::<i64>().ok()? * 3_600 + m.parse::<i64>().ok()? * 60)
    };
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    #[expect(clippy::cast_possible_truncation, reason = "seconds of a minute")]
    let whole_seconds = second as i64;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + whole_seconds - offset_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reads_as_a_bar_a_share_and_the_time_to_its_reset() {
        let now = parse_rfc3339("2026-09-23T07:16:00Z").unwrap();
        assert_eq!(parse_rfc3339("2026-09-26T09:22:02Z"), Some(1_790_414_522));
        assert_eq!(
            parse_rfc3339("2026-09-23T10:29:59.890924+00:00"),
            parse_rfc3339("2026-09-23T12:29:59+02:00")
        );
        let week = Window {
            name: "week".into(),
            used_percent: 84.0,
            resets_at: Some("2026-09-26T09:22:02Z".into()),
        };
        assert_eq!(
            window_line(&week, now),
            "  week           █████████████████░░░  84%  resets in 3d 2h"
        );
        let usage = Usage {
            accounts: vec![AccountUsage {
                account: "chatgpt".into(),
                plan: Some("Pro Lite".into()),
                email: None,
                windows: vec![
                    week,
                    Window {
                        name: "5h".into(),
                        used_percent: 4.0,
                        resets_at: None,
                    },
                ],
                limited: false,
                error: None,
            }],
        };
        assert_eq!(
            warnings(&usage, now),
            vec!["usage: chatgpt week is 84% used, resets in 3d 2h — /usage shows every limit"]
        );
    }
}
