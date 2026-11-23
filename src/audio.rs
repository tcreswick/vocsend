//! Sound in and out, through whichever command-line player and recorder the
//! system already has. vocsend speaks and hears 16 kHz mono, 16-bit samples.
//!
//! Linux:   PipeWire (pw-cat), PulseAudio (paplay/parec) or ALSA (aplay/arecord)
//! Anywhere: SoX (sox), which also covers macOS and Windows
//! Or set VOCSEND_PLAY and VOCSEND_RECORD to commands that read or write raw
//! 16-bit little-endian mono samples at 16000 Hz on standard input or output.

use std::io::{Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;
use vocsend::dsp::SR;

pub const CHUNK: usize = SR / 10;

const PLAYERS: &[&[&str]] = &[
    &["pw-cat", "--playback", "--raw", "--format", "s16", "--rate", "16000", "--channels", "1", "-"],
    &["paplay", "--raw", "--format=s16le", "--rate=16000", "--channels=1"],
    &["aplay", "-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"],
    &["sox", "-q", "-t", "raw", "-e", "signed", "-b", "16", "-r", "16000", "-c", "1", "-", "-d"],
];
const RECORDERS: &[&[&str]] = &[
    &["pw-cat", "--record", "--raw", "--format", "s16", "--rate", "16000", "--channels", "1", "-"],
    &["parec", "--raw", "--format=s16le", "--rate=16000", "--channels=1"],
    &["arecord", "-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"],
    &["sox", "-q", "-d", "-t", "raw", "-e", "signed", "-b", "16", "-r", "16000", "-c", "1", "-"],
];

fn on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        dir.join(name).is_file() || (cfg!(windows) && dir.join(format!("{name}.exe")).is_file())
    })
}

fn command(variable: &str, candidates: &[&[&str]], what: &str) -> Result<Command, String> {
    let words: Vec<String> = match std::env::var(variable) {
        Ok(custom) if !custom.trim().is_empty() => custom.split_whitespace().map(String::from).collect(),
        _ => candidates
            .iter()
            .find(|c| on_path(c[0]))
            .map(|c| c.iter().map(|s| s.to_string()).collect())
            .ok_or_else(|| {
                let names: Vec<&str> = candidates.iter().map(|c| c[0]).collect();
                format!("no way to {what} sound. Install one of: {} (or set {variable}).", names.join(", "))
            })?,
    };
    let mut cmd = Command::new(&words[0]);
    cmd.args(&words[1..]);
    Ok(cmd)
}

/// The speaker. Sound is queued and played in the background.
pub struct Player {
    queue: Option<mpsc::Sender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<()>>,
    child: Child,
    ends: Option<Instant>,
}

impl Player {
    pub fn open() -> Result<Player, String> {
        let mut child = command("VOCSEND_PLAY", PLAYERS, "play")?
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the player: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let (queue, pending) = mpsc::channel::<Vec<u8>>();
        let writer = std::thread::spawn(move || {
            for bytes in pending {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        Ok(Player { queue: Some(queue), writer: Some(writer), child, ends: None })
    }

    pub fn play(&mut self, samples: &[f32]) {
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|&v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        let now = Instant::now();
        let from = self.ends.filter(|&e| e > now).unwrap_or(now);
        self.ends = Some(from + std::time::Duration::from_secs_f64(samples.len() as f64 / SR as f64));
        if let Some(q) = &self.queue {
            let _ = q.send(bytes);
        }
    }

    /// Seconds until everything queued has been played; negative once it
    /// has, by how long. Very negative if nothing was ever played.
    pub fn remaining(&self) -> f64 {
        match self.ends {
            None => -1e9,
            Some(e) => {
                let now = Instant::now();
                if e > now { (e - now).as_secs_f64() } else { -(now - e).as_secs_f64() }
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.queue.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let _ = self.child.wait();
    }
}

/// The microphone, in chunks of a tenth of a second.
pub struct Mic {
    child: Child,
    stream: ChildStdout,
}

impl Mic {
    pub fn open() -> Result<Mic, String> {
        let mut child = command("VOCSEND_RECORD", RECORDERS, "record")?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the recorder: {e}"))?;
        let stream = child.stdout.take().unwrap();
        Ok(Mic { child, stream })
    }

    pub fn read(&mut self) -> Result<Vec<f32>, String> {
        let mut bytes = vec![0u8; CHUNK * 2];
        self.stream.read_exact(&mut bytes).map_err(|_| "the recorder stopped".to_string())?;
        Ok(bytes.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
