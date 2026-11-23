//! A half-duplex link: speak, or listen, never both at once.

use crate::audio::{Mic, Player, CHUNK};
use crate::ui;
use std::time::{Duration, Instant};
use vocsend::dsp::{Rng, SR};
use vocsend::frame;
use vocsend::modem::{self, Packet, Receiver, CONTROL_SYLLABLES};
use vocsend::synth::Alphabet;

pub struct Link {
    player: Option<Player>,
    mic: Option<Mic>,
    rx: Receiver,
}

impl Link {
    /// A side that never speaks gets no player at all, and one that never
    /// listens no recorder: a one-way receiver cannot make a sound.
    pub fn open(speak: bool, listen: bool) -> Link {
        let player = speak.then(|| Player::open().unwrap_or_else(|e| ui::die(&e)));
        let mic = listen.then(|| Mic::open().unwrap_or_else(|e| ui::die(&e)));
        Link { player, mic, rx: Receiver::new() }
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

    fn pull(&mut self) {
        let chunk = self.chunk();
        self.rx.feed(&chunk);
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
        if self.rx.locked() {
            self.rx.restart();
        }
        let deadline = timeout.map(|t| Instant::now() + Duration::from_secs_f64(t));
        loop {
            if !self.rx.search() {
                if deadline.is_some_and(|d| Instant::now() > d) {
                    return None;
                }
                self.pull();
                continue;
            }
            if let Some(heard) = self.read(modem::control(), CONTROL_SYLLABLES, None) {
                let digits: Vec<usize> = heard.iter().map(|&(s, _)| s).collect();
                if let Some(packet) = Packet::parse(&digits).filter(|p| wanted(p)) {
                    return Some(packet);
                }
            }
            self.rx.restart();
        }
    }

    /// Read `count` syllables following the packet just heard.
    pub fn read(&mut self, alphabet: &Alphabet, count: usize, label: Option<&str>) -> Option<Vec<(usize, f32)>> {
        self.rx.begin_run();
        let allowed = count as f64 * alphabet.ms as f64 / 1000.0 + 4.0;
        let deadline = Instant::now() + Duration::from_secs_f64(allowed);
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            match self.rx.next_symbol(alphabet, out.len(), count) {
                Some(symbol) => out.push(symbol),
                None => {
                    if Instant::now() > deadline {
                        if label.is_some() {
                            ui::meter_done();
                        }
                        return None;
                    }
                    self.pull();
                    if let Some(label) = label {
                        ui::meter(label, out.len() as f64 / count as f64);
                    }
                }
            }
        }
        if label.is_some() {
            ui::meter_done();
        }
        Some(out)
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
        let more = modem::syllables_for(total, bits) - heard.len();
        let deadline = Instant::now() + Duration::from_secs_f64(more as f64 * alphabet.ms as f64 / 1000.0 + 4.0);
        let start = heard.len();
        while heard.len() < start + more {
            match self.rx.next_symbol(alphabet, heard.len(), start + more) {
                Some(symbol) => heard.push(symbol),
                None => {
                    if Instant::now() > deadline {
                        if label.is_some() {
                            ui::meter_done();
                        }
                        return None;
                    }
                    self.pull();
                    if let Some(label) = label {
                        ui::meter(label, heard.len() as f64 / (start + more) as f64);
                    }
                }
            }
        }
        if label.is_some() {
            ui::meter_done();
        }
        let (bytes, conf) = modem::unpack(&heard, bits, total);
        frame::decode_body(&bytes[frame::HEADER_LEN..], &conf[frame::HEADER_LEN..], length, nsym)
    }

    /// Loudness in dB of the last `seconds` of what was just read.
    pub fn loudness(&self, seconds: f64) -> f32 {
        self.rx.level_db((seconds * SR as f64) as usize)
    }
}
