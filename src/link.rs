//! A half-duplex link: speak, or listen, never both at once.

use crate::audio::{Mic, Player, CHUNK};
use crate::ui;
use std::time::{Duration, Instant};
use vocsend::dsp::{Rng, SR};
use vocsend::frame;
use vocsend::modem::{self, Packet, Receiver, REPLY_SYLLABLES};
use vocsend::synth::Alphabet;

pub struct Link {
    player: Option<Player>,
    mic: Option<Mic>,
    rx: Receiver,
    /// Samples of unbroken, complete silence from the microphone.
    silent: usize,
    warned: bool,
}

/// Below this a sample counts as no signal at all (-100 dB). Even a quiet
/// room through a working microphone is far louder; a muted or blocked
/// microphone gives exact zeros.
const DEAD: f32 = 1e-5;
/// How long complete silence lasts before we say so.
const DEAD_SECONDS: f64 = 2.5;

#[cfg(target_os = "linux")]
const MIC_HELP: &[&str] = &[
    "  check the input in your sound settings (pavucontrol, alsamixer), or unmute it with one of:",
    "    wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 0",
    "    pactl set-source-mute @DEFAULT_SOURCE@ 0",
];
#[cfg(target_os = "macos")]
const MIC_HELP: &[&str] = &[
    "  check System Settings > Privacy & Security > Microphone allows your terminal,",
    "  and System Settings > Sound > Input has the right microphone with its level up.",
];
#[cfg(target_os = "windows")]
const MIC_HELP: &[&str] = &[
    "  check Settings > Privacy & security > Microphone allows desktop apps,",
    "  and Settings > System > Sound > Input has the right microphone, unmuted.",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const MIC_HELP: &[&str] = &["  check the input device and its level in your sound settings."];

impl Link {
    /// A side that never speaks gets no player at all, and one that never
    /// listens no recorder: a one-way receiver cannot make a sound.
    pub fn open(speak: bool, listen: bool) -> Link {
        let player = speak.then(|| Player::open().unwrap_or_else(|e| ui::die(&e)));
        let mic = listen.then(|| Mic::open().unwrap_or_else(|e| ui::die(&e)));
        Link { player, mic, rx: Receiver::new(), silent: 0, warned: false }
    }

    fn chunk(&mut self) -> Vec<f32> {
        match &mut self.mic {
            Some(mic) => mic.read().unwrap_or_else(|e| ui::die(&e)),
            None => {
                std::thread::sleep(Duration::from_millis(100));
                vec![0.0; CHUNK]
            }
        }
    }

    fn pull(&mut self) -> Vec<f32> {
        let chunk = self.chunk();
        self.rx.feed(&chunk);
        self.watch_for_silence(&chunk);
        chunk
    }

    /// Say so, once, if the microphone is delivering nothing at all: it is
    /// muted, blocked or the wrong input, and no amount of waiting will help.
    fn watch_for_silence(&mut self, chunk: &[f32]) {
        if self.mic.is_none() {
            return;
        }
        if chunk.iter().all(|v| v.abs() < DEAD) {
            self.silent += chunk.len();
            if !self.warned && self.silent as f64 > DEAD_SECONDS * SR as f64 {
                self.warned = true;
                ui::log("the microphone is giving complete silence. It is probably muted, blocked, or not the input you expect.");
                for line in MIC_HELP {
                    ui::log(line);
                }
            }
        } else {
            if self.warned {
                self.warned = false;
                ui::log("the microphone is live now.");
            }
            self.silent = 0;
        }
    }

    /// Wait for a control packet like `listen`, showing how loud the
    /// microphone is meanwhile, so a dead or quiet input is obvious.
    pub fn wait(&mut self, timeout: Option<f64>, wanted: &dyn Fn(&Packet) -> bool) -> Option<Packet> {
        self.listen_showing(timeout, wanted, true)
    }

    /// Let time pass, keeping the microphone drained.
    pub fn pause(&mut self, seconds: f64) {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            self.chunk();
        }
    }

    pub fn send(&mut self, mut audio: Vec<f32>, label: Option<&str>) {
        let Some(player) = &mut self.player else { return };
        if player.remaining() < -1.0 {
            // A second of faint hiss first: sound cards, amplifiers and
            // wireless links need waking, and some sleep again on pure silence.
            let mut rng = Rng::new(0);
            let mut lead: Vec<f32> = (0..SR * 8 / 10).map(|_| (0.001 * rng.gaussian()) as f32).collect();
            lead.append(&mut audio);
            audio = lead;
        }
        let seconds = audio.len() as f64 / SR as f64;
        player.play(&audio);
        let started = Instant::now();
        // Stay deaf until our own sound has finished: we would only hear ourselves.
        while self.player.as_ref().is_some_and(|p| p.remaining() > -0.3) {
            self.chunk();
            if let Some(label) = label {
                ui::meter(label, started.elapsed().as_secs_f64() / seconds);
            }
        }
        if label.is_some() {
            ui::meter_done();
        }
        self.rx = Receiver::new();
    }

    /// Wait for a control packet that `wanted` accepts. None on timeout.
    pub fn listen(&mut self, timeout: Option<f64>, wanted: &dyn Fn(&Packet) -> bool) -> Option<Packet> {
        self.listen_showing(timeout, wanted, false)
    }

    fn listen_showing(&mut self, timeout: Option<f64>, wanted: &dyn Fn(&Packet) -> bool, show: bool) -> Option<Packet> {
        let mut level = -100.0f32;
        if self.rx.locked() {
            self.rx.restart();
        }
        let deadline = timeout.map(|t| Instant::now() + Duration::from_secs_f64(t));
        loop {
            if !self.rx.search() {
                if deadline.is_some_and(|d| Instant::now() > d) {
                    ui::meter_done();
                    return None;
                }
                let chunk = self.pull();
                if show {
                    // Jump up at once, fall back slowly, like a level meter.
                    let power = chunk.iter().map(|v| v * v).sum::<f32>() / chunk.len().max(1) as f32;
                    let now = (10.0 * (power + 1e-12).log10()).max(-100.0);
                    level = now.max(level - 3.0);
                    ui::level("listening", level);
                }
                continue;
            }
            ui::meter_done();
            // Every packet starts with its kind, which says how long it is.
            if let Some(mut heard) = self.read(modem::control(), REPLY_SYLLABLES, None) {
                let digits = |h: &[(usize, f32)]| h.iter().map(|&(s, _)| s).collect::<Vec<usize>>();
                let complete = match Packet::length(&digits(&heard)) {
                    Some(total) => self.extend(modem::control(), &mut heard, total, None),
                    None => false,
                };
                if complete {
                    if let Some(packet) = Packet::parse(&digits(&heard)).filter(|p| wanted(p)) {
                        return Some(packet);
                    }
                }
            }
            self.rx.restart();
        }
    }

    /// Read `count` syllables following the packet just heard.
    pub fn read(&mut self, alphabet: &Alphabet, count: usize, label: Option<&str>) -> Option<Vec<(usize, f32)>> {
        self.rx.begin_run();
        let mut out = Vec::with_capacity(count);
        self.extend(alphabet, &mut out, count, label).then_some(out)
    }

    /// Carry on reading the current run until `heard` holds `total` syllables.
    /// False if they did not arrive in time.
    fn extend(&mut self, alphabet: &Alphabet, heard: &mut Vec<(usize, f32)>, total: usize, label: Option<&str>) -> bool {
        let more = total.saturating_sub(heard.len());
        let deadline = Instant::now() + Duration::from_secs_f64(more as f64 * alphabet.ms as f64 / 1000.0 + 4.0);
        let mut complete = true;
        while heard.len() < total {
            match self.rx.next_symbol(alphabet, heard.len(), total) {
                Some(symbol) => heard.push(symbol),
                None => {
                    if Instant::now() > deadline {
                        complete = false;
                        break;
                    }
                    self.pull();
                    if let Some(label) = label {
                        ui::meter(label, heard.len() as f64 / total as f64);
                    }
                }
            }
        }
        if label.is_some() {
            ui::meter_done();
        }
        complete
    }

    /// Read one framed block at a level: its bytes, or None if it did not
    /// arrive or could not be repaired.
    pub fn read_frame(&mut self, lv: usize, label: Option<&str>) -> Option<Vec<u8>> {
        let alphabet = modem::alphabet(lv);
        let bits = alphabet.bits();
        let mut heard = self.read(alphabet, modem::syllables_for(frame::HEADER_LEN, bits), None)?;
        let (bytes, conf) = modem::unpack(&heard, bits, frame::HEADER_LEN);
        let (length, nsym) = frame::decode_header(&bytes, &conf)?;
        let total = frame::HEADER_LEN + frame::body_len(length, nsym);
        // Carry straight on: the rest of the frame follows without a gap.
        if !self.extend(alphabet, &mut heard, modem::syllables_for(total, bits), label) {
            return None;
        }
        let (bytes, conf) = modem::unpack(&heard, bits, total);
        frame::decode_body(&bytes[frame::HEADER_LEN..], &conf[frame::HEADER_LEN..], length, nsym)
    }

    /// Loudness in dB of the last `seconds` of what was just read.
    pub fn loudness(&self, seconds: f64) -> f32 {
        self.rx.level_db((seconds * SR as f64) as usize)
    }
}
