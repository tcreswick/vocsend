//! What the user sees: status lines, a progress meter, fatal errors.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

pub fn log(text: &str) {
    if !quiet() {
        eprintln!("vocsend: {text}");
    }
}

/// Make a few characters stand out (bold cyan) when writing to a terminal
/// that has not asked for plain output with NO_COLOR.
pub fn highlight(text: &str) -> String {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[1;96m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// An error is shown even in quiet mode.
pub fn error(text: &str) {
    eprintln!("vocsend: {text}");
}

pub fn die(text: &str) -> ! {
    error(text);
    std::process::exit(1)
}

/// A one-line progress display, on a terminal only.
pub fn meter(label: &str, fraction: f64) {
    if quiet() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("\r\x1b[2K  {label} {:3.0}%", 100.0 * fraction.clamp(0.0, 1.0));
    let _ = std::io::stderr().flush();
}

pub fn meter_done() {
    if quiet() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("\r\x1b[2K");
    let _ = std::io::stderr().flush();
}
