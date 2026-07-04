//! Terminal presentation helpers: colored step headers, aligned key/value
//! detail lines, an in-place progress line, and styled errors. Styling uses
//! `anstyle` for colors and `anstream` for output, which strips escapes
//! automatically when the stream is not a TTY or when `NO_COLOR` is set.

use std::fmt::Display;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anstyle::{AnsiColor, Style};

const KEY_WIDTH: usize = 10;

static QUIET: AtomicBool = AtomicBool::new(false);
static JSON: AtomicBool = AtomicBool::new(false);

/// Suppress informational output (step/success/kv/note/progress). Errors are
/// always shown regardless. Set once from the global `--quiet` flag.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Switch to machine-readable JSON mode: all human step/success/kv/note/progress
/// lines are suppressed and each command instead prints a single JSON object via
/// [`emit`]. Set once from the global `--json` flag.
pub fn set_json(json: bool) {
    JSON.store(json, Ordering::Relaxed);
}

/// Whether `--json` output mode is active.
pub fn json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// Human output is silenced by both `--quiet` and `--json`.
fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed) || JSON.load(Ordering::Relaxed)
}

/// Print a single compact JSON object to stdout when `--json` is active; a no-op
/// otherwise. Commands call this with their result so `--json` yields exactly one
/// machine-readable line and nothing else.
pub fn emit(value: &serde_json::Value) {
    if json() {
        println!("{value}");
    }
}

const STEP: Style = AnsiColor::Cyan.on_default().bold();
const OK: Style = AnsiColor::Green.on_default().bold();
const ERR: Style = AnsiColor::Red.on_default().bold();
const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();

/// A cyan `→` step header, e.g. `→ upload to Bulletin`.
pub fn step(title: impl Display) {
    if quiet() {
        return;
    }
    anstream::println!("{STEP}→{STEP:#} {BOLD}{title}{BOLD:#}");
}

/// A green `✓` success header.
pub fn success(title: impl Display) {
    if quiet() {
        return;
    }
    anstream::println!("{OK}✓{OK:#} {BOLD}{title}{BOLD:#}");
}

/// An indented, aligned `key value` detail line under a step.
pub fn kv(key: &str, value: impl Display) {
    if quiet() {
        return;
    }
    let key = format!("{key:<width$}", width = KEY_WIDTH);
    anstream::println!("  {DIM}{key}{DIM:#} {value}");
}

/// A dim informational line at detail indent.
pub fn note(msg: impl Display) {
    if quiet() {
        return;
    }
    anstream::println!("  {DIM}{msg}{DIM:#}");
}

/// Rewrite an in-place progress line on stderr (no-op when stderr is not a TTY,
/// so piped/CI output is not flooded with carriage returns).
pub fn progress(msg: impl Display) {
    if quiet() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("\r\x1b[2K  {msg}");
    let _ = std::io::stderr().flush();
}

/// Clear the in-place progress line.
pub fn progress_clear() {
    if quiet() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("\r\x1b[2K");
    let _ = std::io::stderr().flush();
}

/// A red `✗` error header on stderr, followed by the dim cause chain. In JSON
/// mode a single `{"error": "..."}` object is printed to stderr instead.
pub fn error(err: &anyhow::Error) {
    if json() {
        let msg = err
            .chain()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(": ");
        eprintln!("{}", serde_json::json!({ "error": msg }));
        return;
    }
    anstream::eprintln!("{ERR}✗{ERR:#} {err}");
    for cause in err.chain().skip(1) {
        anstream::eprintln!("  {DIM}↳{DIM:#} {cause}");
    }
}

/// Shorten a long id (CID / hash) to `head…tail` for step headers; keep the
/// full value in `kv` lines so it stays copyable.
pub fn ellipsize(id: &str) -> String {
    if id.len() <= 16 {
        id.to_string()
    } else {
        format!("{}…{}", &id[..8], &id[id.len() - 6..])
    }
}
