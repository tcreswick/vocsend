//! What the user sees: status lines, a progress meter, fatal errors.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static QUIET: AtomicBool = AtomicBool::new(false);
/// The progress line currently on screen.
static SHOWN: Mutex<String> = Mutex::new(String::new());

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

pub fn log(text: &str) {
    if !quiet() {
        clear_meter();
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
    clear_meter();
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
    // Redraw only when the text changes, and overwrite in place rather than
    // erasing the line first: both avoid flicker.
    let line = format!("  {label} {:3.0}%", 100.0 * fraction.clamp(0.0, 1.0));
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if *shown == line {
        return;
    }
    eprint!("\r{line}\x1b[K");
    let _ = std::io::stderr().flush();
    *shown = line;
}

/// A one-line loudness display, on a terminal only: a bar from -70 to 0 dB.
pub fn level(label: &str, db: f32) {
    if quiet() || !std::io::stderr().is_terminal() {
        return;
    }
    const CELLS: usize = 24;
    let filled = (((db + 70.0) / 70.0).clamp(0.0, 1.0) * CELLS as f32).round() as usize;
    let note = if db <= -99.0 { "no signal".to_string() } else { format!("{db:4.0} dB") };
    let line = format!("  {label} [{}{}] {note}", "#".repeat(filled), "-".repeat(CELLS - filled));
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if *shown == line {
        return;
    }
    eprint!("\r{line}\x1b[K");
    let _ = std::io::stderr().flush();
    *shown = line;
}

pub fn meter_done() {
    clear_meter();
}

/// Wipe the progress line, if one is on screen, so a message can take its place.
fn clear_meter() {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if !shown.is_empty() {
        shown.clear();
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }
}
