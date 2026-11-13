//! Terminal output: progress bars and status lines on stderr.
//!
//! All bars live in one [`MultiProgress`], so several can run at once (the
//! torrent download, one per client transfer). Status lines go through
//! `say!` so they print above the bars instead of tearing
//! them. Without bars (stderr isn't a terminal, `TERM=dumb`, or
//! `--no-progress` / `progress = false`), callers print plain lines instead
//! (see [`bars`]).

use std::{
    io::IsTerminal,
    sync::{LazyLock, OnceLock},
    time::{Duration, Instant},
};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::torrent::human;

static MULTI: LazyLock<MultiProgress> = LazyLock::new(MultiProgress::new);
static BARS: OnceLock<bool> = OnceLock::new();

/// Decide once whether to draw progress bars. `enabled` is false with
/// `--no-progress` or `progress = false`; bars also need stderr to be a
/// capable terminal. Call before anything is printed.
pub fn init(enabled: bool) {
    BARS.set(enabled && capable_terminal()).ok();
}

/// Whether progress bars are shown.
pub fn bars() -> bool {
    *BARS.get_or_init(capable_terminal)
}

/// stderr is a terminal that can redraw lines (not `TERM=dumb`).
fn capable_terminal() -> bool {
    std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Whether to color log lines on stderr. Follows the same rules as the
/// progress bars' colors: `NO_COLOR` and `CLICOLOR=0` turn colors off,
/// `CLICOLOR_FORCE` forces them on, and otherwise only a capable terminal
/// gets them.
pub fn stderr_colors() -> bool {
    let var = |k: &str| std::env::var(k).ok();
    if var("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
        return true;
    }
    if var("NO_COLOR").is_some_and(|v| !v.is_empty()) || var("CLICOLOR").as_deref() == Some("0") {
        return false;
    }
    capable_terminal()
}

/// Print a status line on stderr, above any progress bars.
macro_rules! say {
    () => { $crate::ui::line(String::new()) };
    ($($arg:tt)*) => { $crate::ui::line(format!($($arg)*)) };
}

#[doc(hidden)]
pub fn line(msg: String) {
    suspend(|| eprintln!("{msg}"));
}

/// Run `f` with the bars cleared, e.g. to write to stdout.
pub fn suspend<R>(f: impl FnOnce() -> R) -> R {
    if bars() { MULTI.suspend(f) } else { f() }
}

/// Add a bar to the shared set.
pub fn add(bar: ProgressBar) -> ProgressBar {
    MULTI.add(bar)
}

/// A byte-transfer bar: `{label} [=====>   ] 1.2 GiB/3.4 GiB 50 MiB/s eta 40s`.
/// Without a length yet it shows bytes and speed only. `None` when stderr
/// isn't a terminal.
pub fn transfer_bar(label: &str, len: Option<u64>) -> Option<ProgressBar> {
    if !bars() {
        return None;
    }
    let bar = add(match len {
        Some(len) => ProgressBar::new(len).with_style(sized_style()),
        None => ProgressBar::no_length().with_style(unsized_style()),
    });
    bar.set_prefix(label.to_string());
    bar.enable_steady_tick(Duration::from_millis(100));
    Some(bar)
}

pub fn sized_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} {prefix} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} \
         {binary_bytes_per_sec} eta {eta} {msg}",
    )
    .expect("valid template")
    .progress_chars("=> ")
}

fn unsized_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} {prefix} {bytes} {binary_bytes_per_sec} {elapsed} {msg}",
    )
    .expect("valid template")
}

/// Replace a finished bar with a one-line summary that stays on screen.
pub fn finish_bar(bar: &ProgressBar, summary: String) {
    end_bar(bar, "{msg:.green}", format!("✔ {summary}"));
}

/// Replace a failed bar with a one-line note that stays on screen.
pub fn fail_bar(bar: &ProgressBar, summary: String) {
    end_bar(bar, "{msg:.red}", format!("✘ {summary}"));
}

fn end_bar(bar: &ProgressBar, template: &str, line: String) {
    // Message first, then style, so no in-between state is ever drawn
    // with an empty message.
    bar.set_message(line);
    bar.set_style(ProgressStyle::with_template(template).expect("valid template"));
    bar.finish();
}

/// "300.00 MiB in 3.2s (93.75 MiB/s)"
pub fn rate_summary(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64().max(0.001);
    format!(
        "{} in {:.1?} ({}/s)",
        human(bytes),
        elapsed,
        human((bytes as f64 / secs) as u64)
    )
}

/// Throttle for plain-text progress lines when there are no bars.
pub struct Every(Duration, Option<Instant>);

impl Every {
    pub fn new(period: Duration) -> Self {
        Self(period, Some(Instant::now()))
    }

    /// True at most once per period, starting one period after `new`.
    pub fn due(&mut self) -> bool {
        let now = Instant::now();
        if self.1.is_some_and(|t| now - t < self.0) {
            return false;
        }
        self.1 = Some(now);
        true
    }
}
