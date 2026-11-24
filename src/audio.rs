//! Sound in and out. vocsend speaks and hears 16 kHz mono.
//!
//! By default it talks to the system's sound devices directly (through the
//! cpal library: ALSA on Linux, CoreAudio on macOS, WASAPI on Windows) at
//! whatever rate they run, converting as it goes. If that is unavailable, or
//! if asked, it falls back to command-line tools:
//!
//!   PipeWire (pw-cat), PulseAudio (paplay/parec), ALSA (aplay/arecord), SoX
//!
//! VOCSEND_AUDIO=commands forces the fallback, VOCSEND_AUDIO=native forbids
//! it. VOCSEND_PLAY and VOCSEND_RECORD name commands of your own that read or
//! write raw 16-bit little-endian mono samples at 16000 Hz.

use std::io::{Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;
use vocsend::dsp::SR;

pub const CHUNK: usize = SR / 10;

fn preference() -> String {
    std::env::var("VOCSEND_AUDIO").unwrap_or_default().to_lowercase()
}

// ------------------------------------------------------------ native devices

#[cfg(feature = "native-audio")]
mod native {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
    use std::collections::VecDeque;
    use std::sync::{mpsc, Arc, Mutex};
    use vocsend::resample::Resampler;

    pub struct Output {
        _stream: cpal::Stream,
        queue: Arc<Mutex<VecDeque<f32>>>,
        resampler: Resampler,
    }

    fn output_stream<T>(device: &cpal::Device, config: StreamConfig, queue: Arc<Mutex<VecDeque<f32>>>) -> Result<cpal::Stream, String>
    where
        T: SizedSample + FromSample<f32>,
    {
        let channels = config.channels as usize;
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    let mut queue = queue.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let value = T::from_sample(queue.pop_front().unwrap_or(0.0));
                        frame.iter_mut().for_each(|s| *s = value);
                    }
                },
                |_| {},
                None,
            )
            .map_err(|e| e.to_string())
    }

    impl Output {
        pub fn open() -> Result<Output, String> {
            let device = cpal::default_host().default_output_device().ok_or("no sound output device")?;
            let supported = device.default_output_config().map_err(|e| e.to_string())?;
            let format = supported.sample_format();
            let config: StreamConfig = supported.into();
            let rate = config.sample_rate;
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            let q = queue.clone();
            let stream = match format {
                SampleFormat::F32 => output_stream::<f32>(&device, config, q),
                SampleFormat::I16 => output_stream::<i16>(&device, config, q),
                SampleFormat::U16 => output_stream::<u16>(&device, config, q),
                SampleFormat::I32 => output_stream::<i32>(&device, config, q),
                SampleFormat::I8 => output_stream::<i8>(&device, config, q),
                SampleFormat::U8 => output_stream::<u8>(&device, config, q),
                SampleFormat::F64 => output_stream::<f64>(&device, config, q),
                other => Err(format!("unsupported output sample format {other}")),
            }?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(Output { _stream: stream, queue, resampler: Resampler::new(super::SR as u32, rate) })
        }

        pub fn play(&mut self, samples: &[f32]) {
            let converted = self.resampler.push(samples);
            self.queue.lock().unwrap().extend(converted);
        }

        /// Samples still waiting to be played, as seconds.
        pub fn queued(&self, rate_hint: f64) -> f64 {
            self.queue.lock().unwrap().len() as f64 / rate_hint
        }
    }

    pub struct Input {
        _stream: cpal::Stream,
        incoming: mpsc::Receiver<Vec<f32>>,
        resampler: Resampler,
        ready: Vec<f32>,
    }

    fn input_stream<T>(device: &cpal::Device, config: StreamConfig, out: mpsc::SyncSender<Vec<f32>>) -> Result<cpal::Stream, String>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let channels = config.channels as usize;
        device
            .build_input_stream(
                config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    // Mix every channel down to one.
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32)
                        .collect();
                    let _ = out.send(mono);
                },
                |_| {},
                None,
            )
            .map_err(|e| e.to_string())
    }

    impl Input {
        pub fn open() -> Result<Input, String> {
            let device = cpal::default_host().default_input_device().ok_or("no sound input device")?;
            let supported = device.default_input_config().map_err(|e| e.to_string())?;
            let format = supported.sample_format();
            let config: StreamConfig = supported.into();
            let rate = config.sample_rate;
            // Bounded: if the reader ever stalls, the device waits instead of
            // memory filling up.
            let (tx, incoming) = mpsc::sync_channel(256);
            let stream = match format {
                SampleFormat::F32 => input_stream::<f32>(&device, config, tx),
                SampleFormat::I16 => input_stream::<i16>(&device, config, tx),
                SampleFormat::U16 => input_stream::<u16>(&device, config, tx),
                SampleFormat::I32 => input_stream::<i32>(&device, config, tx),
                SampleFormat::I8 => input_stream::<i8>(&device, config, tx),
                SampleFormat::U8 => input_stream::<u8>(&device, config, tx),
                SampleFormat::F64 => input_stream::<f64>(&device, config, tx),
                other => Err(format!("unsupported input sample format {other}")),
            }?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(Input { _stream: stream, incoming, resampler: Resampler::new(rate, super::SR as u32), ready: Vec::new() })
        }

        pub fn read(&mut self) -> Result<Vec<f32>, String> {
            while self.ready.len() < super::CHUNK {
                let block = self
                    .incoming
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| "the microphone stopped delivering sound".to_string())?;
                self.ready.extend(self.resampler.push(&block));
            }
            Ok(self.ready.drain(..super::CHUNK).collect())
        }
    }
}

// ------------------------------------------------------- command-line tools

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
                format!("no command-line tool to {what} sound either (looked for {}; or set {variable})", names.join(", "))
            })?,
    };
    let mut cmd = Command::new(&words[0]);
    cmd.args(&words[1..]);
    Ok(cmd)
}

struct CommandPlayer {
    queue: Option<mpsc::Sender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<()>>,
    child: Child,
}

impl CommandPlayer {
    fn open() -> Result<CommandPlayer, String> {
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
        Ok(CommandPlayer { queue: Some(queue), writer: Some(writer), child })
    }

    fn play(&mut self, samples: &[f32]) {
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|&v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        if let Some(q) = &self.queue {
            let _ = q.send(bytes);
        }
    }
}

impl Drop for CommandPlayer {
    fn drop(&mut self) {
        self.queue.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let _ = self.child.wait();
    }
}

struct CommandMic {
    child: Child,
    stream: ChildStdout,
}

impl CommandMic {
    fn open() -> Result<CommandMic, String> {
        let mut child = command("VOCSEND_RECORD", RECORDERS, "record")?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the recorder: {e}"))?;
        let stream = child.stdout.take().unwrap();
        Ok(CommandMic { child, stream })
    }

    fn read(&mut self) -> Result<Vec<f32>, String> {
        let mut bytes = vec![0u8; CHUNK * 2];
        self.stream.read_exact(&mut bytes).map_err(|_| "the recorder stopped".to_string())?;
        Ok(bytes.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
    }
}

impl Drop for CommandMic {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --------------------------------------------------------- the two together

enum Output {
    #[cfg(feature = "native-audio")]
    Native(native::Output),
    Command(CommandPlayer),
}

/// The speaker. Sound is queued and played in the background.
pub struct Player {
    output: Output,
    ends: Option<Instant>,
}

/// Run `open` with the process's error output switched off. ALSA writes its
/// own diagnostics straight to the terminal when a device is missing, which
/// would bury the one clear message we print ourselves.
#[cfg(unix)]
fn quietly<T>(open: impl FnOnce() -> T) -> T {
    use std::os::fd::AsRawFd;
    let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") else { return open() };
    // Safety: plain file-descriptor juggling on descriptors this function owns.
    unsafe {
        let saved = libc::dup(2);
        if saved < 0 {
            return open();
        }
        libc::dup2(null.as_raw_fd(), 2);
        let result = open();
        libc::dup2(saved, 2);
        libc::close(saved);
        result
    }
}

#[cfg(not(unix))]
fn quietly<T>(open: impl FnOnce() -> T) -> T {
    open()
}

/// Try the system's devices first, then command-line tools, unless told otherwise.
fn choose<T>(variable: &str, native: impl FnOnce() -> Result<T, String>, fallback: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let custom = std::env::var(variable).is_ok_and(|v| !v.trim().is_empty());
    let want = preference();
    if custom || want == "commands" {
        return fallback();
    }
    match quietly(native) {
        Ok(found) => Ok(found),
        Err(why) if want == "native" => Err(why),
        Err(why) => fallback().map_err(|also| format!("{why}; {also}")),
    }
}

impl Player {
    pub fn open() -> Result<Player, String> {
        let output = choose(
            "VOCSEND_PLAY",
            || {
                #[cfg(feature = "native-audio")]
                return native::Output::open().map(Output::Native);
                #[cfg(not(feature = "native-audio"))]
                return Err("built without native sound support".to_string());
            },
            || CommandPlayer::open().map(Output::Command),
        )
        .map_err(|e| format!("cannot play sound: {e}"))?;
        Ok(Player { output, ends: None })
    }

    pub fn play(&mut self, samples: &[f32]) {
        let now = Instant::now();
        let from = self.ends.filter(|&e| e > now).unwrap_or(now);
        self.ends = Some(from + std::time::Duration::from_secs_f64(samples.len() as f64 / SR as f64));
        match &mut self.output {
            #[cfg(feature = "native-audio")]
            Output::Native(out) => out.play(samples),
            Output::Command(out) => out.play(samples),
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
        // Let the last of the sound out before the device closes.
        #[cfg(feature = "native-audio")]
        if let Output::Native(out) = &self.output {
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while self.remaining() > -0.2 && out.queued(8000.0) > 0.0 && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

enum Input {
    #[cfg(feature = "native-audio")]
    Native(native::Input),
    Command(CommandMic),
}

/// The microphone, in chunks of a tenth of a second.
pub struct Mic {
    input: Input,
}

impl Mic {
    pub fn open() -> Result<Mic, String> {
        let input = choose(
            "VOCSEND_RECORD",
            || {
                #[cfg(feature = "native-audio")]
                return native::Input::open().map(Input::Native);
                #[cfg(not(feature = "native-audio"))]
                return Err("built without native sound support".to_string());
            },
            || CommandMic::open().map(Input::Command),
        )
        .map_err(|e| format!("cannot record sound: {e}"))?;
        Ok(Mic { input })
    }

    pub fn read(&mut self) -> Result<Vec<f32>, String> {
        match &mut self.input {
            #[cfg(feature = "native-audio")]
            Input::Native(input) => input.read(),
            Input::Command(input) => input.read(),
        }
    }
}
