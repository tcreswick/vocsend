//! The two sides of a transfer: what the sender does and what the receiver does.

use crate::link::Link;
use crate::ui::{self, log};
use std::io::{BufRead, IsTerminal, Read, Write};
use vocsend::crypto::Cipher;
use vocsend::crypto;
use vocsend::modem::flag::*;
use vocsend::modem::kind::*;
use vocsend::modem::{self, level, Packet};
use vocsend::payload;
use vocsend::plan::{self, clock, ATTEMPTS, RAMP_LEVELS, RAMP_SYLLABLES, TEST_SYLLABLES, TURNAROUND};

pub const EXIT_FAILED: i32 = 1;
pub const EXIT_MODE: i32 = 2;
pub const EXIT_KEY: i32 = 3;
pub const EXIT_VERSION: i32 = 4;

#[derive(Clone, Copy, PartialEq)]
pub enum Stream {
    Off,
    Lines,
    Chars,
}

pub struct Options {
    pub negotiate: bool,
    pub path: Option<String>,
    /// None: the default; Some(0): whatever the last negotiation ended on.
    pub level: Option<usize>,
    pub cipher: Option<Cipher>,
    pub stream: Stream,
    pub levels: (usize, usize),
    /// The receiver this is for (sender) or the name this receiver answers to.
    pub address: Option<String>,
    /// Seconds a receiver waits to hear something; None waits for ever.
    pub timeout: Option<f64>,
}

// ---------------------------------------------------------------- settings

fn state_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("vocsend").join("level"))
}

fn saved_level() -> usize {
    state_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|l| (1..=10).contains(l))
        .unwrap_or(plan::ONE_WAY_LEVEL)
}

fn save_level(lv: usize) {
    if let Some(path) = state_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{lv}\n"));
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ------------------------------------------------------------------ sender

struct Sender {
    link: Link,
    cipher: Option<Cipher>,
    flags: u8,
    address: u8,
    acknowledged: bool,
    lv: usize,
    redundancy: f64,
    seq: usize,
}

impl Sender {
    fn encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    /// Call the receiver, find the fastest level the path can take, and agree it.
    fn negotiate(&mut self, stream_len: usize) -> Result<(), i32> {
        log("calling the receiver...");
        let mut answered = false;
        for _ in 0..40 {
            self.link.send(Packet::sender(CALL, 0, 0, self.flags, self.address).audio(&[]), None);
            match self.link.listen(Some(5.5), &|p| p.kind == ANSWER || p.kind == REFUSE) {
                Some(p) if p.kind == REFUSE => return Err(self.refused(p.a)),
                Some(_) => {
                    answered = true;
                    break;
                }
                None => {}
            }
        }
        if !answered {
            ui::error("nobody answered. Is the receiver running `vocsend rx negotiate`, and can it hear this machine?");
            return Err(EXIT_FAILED);
        }
        log("receiver answered; playing the ramp");

        let (lo, hi) = RAMP_LEVELS;
        let bursts: Vec<Vec<usize>> = (lo..=hi).map(|lv| plan::known_symbols(lv, RAMP_SYLLABLES, 1)).collect();
        let sections: Vec<(&vocsend::synth::Alphabet, &[usize])> =
            (lo..=hi).zip(&bursts).map(|(lv, b)| (modem::alphabet(lv), b.as_slice())).collect();
        let mut clean = None;
        for _ in 0..ATTEMPTS {
            self.link.pause(TURNAROUND);
            self.link.send(Packet::sender(RAMP, lo, hi, self.flags, self.address).audio(&sections), None);
            if let Some(p) = self.link.listen(Some(9.0), &|p| p.kind == REPORT) {
                clean = Some(p.a as usize);
                break;
            }
        }
        let Some(clean) = clean else {
            ui::error("the receiver answered the call but sent no report after the ramp");
            return Err(EXIT_FAILED);
        };
        let try_level = if (1..=10).contains(&clean) { clean } else { 2 };
        if clean == 0 {
            log("no ramp level arrived clean; testing level 2");
        } else {
            log(&format!("receiver heard level {try_level} clean; testing it"));
        }

        let size = plan::size_digits(stream_len);
        let known = plan::known_symbols(try_level, TEST_SYLLABLES, 2);
        self.link.pause(TURNAROUND);
        self.link.send(
            Packet::sender(TEST, try_level, 0, self.flags, self.address)
                .audio(&[(modem::control(), &size), (modem::alphabet(try_level), &known)]),
            None,
        );
        match self.link.listen(Some(9.0), &|p| p.kind == ACCEPT && (1..=10).contains(&p.a)) {
            Some(p) => {
                self.lv = p.a as usize;
                self.redundancy = plan::REDUNDANCY[(p.b as usize).min(plan::REDUNDANCY.len() - 1)];
            }
            None => {
                self.lv = try_level.saturating_sub(1).max(1);
                self.redundancy = level(self.lv).redundancy;
            }
        }
        log(&format!(
            "agreed on level {} with {:.0}% error correction (about {:.0} bits/s before compression)",
            self.lv,
            100.0 * self.redundancy,
            modem::speed(self.lv, self.redundancy)
        ));
        self.link.pause(TURNAROUND);
        Ok(())
    }

    fn refused(&self, reason: u8) -> i32 {
        match reason {
            1 if self.encrypted() => ui::error("the receiver refused: this side is encrypting, but the receiver was started without --encrypt"),
            1 => ui::error("the receiver refused: it was started with --encrypt, but this side is not encrypting"),
            2 => ui::error("the receiver refused: the keys do not match"),
            3 => {
                ui::error("the receiver refused: its vocsend is too old for this one. Upgrade the receiving side.");
                return EXIT_VERSION;
            }
            4 => {
                ui::error("the receiver refused: this vocsend is too old for it. Upgrade this side.");
                return EXIT_VERSION;
            }
            _ => ui::error("the receiver refused the transfer"),
        }
        EXIT_KEY
    }

    /// Send one stream of bytes in blocks. False if a block could not be delivered.
    fn transfer(&mut self, stream: &[u8], show_progress: bool) -> Result<(), i32> {
        let mut sent = 0;
        let mut done = 0;
        loop {
            let mut size = plan::split(stream.len() - sent, self.lv, self.redundancy, self.acknowledged, self.encrypted())[0];
            let mut delivered = false;
            for attempt in 1..=ATTEMPTS {
                let chunk = &stream[sent..sent + size];
                let last = sent + size >= stream.len();
                let field = (self.seq & 7) | (last as usize) << 3;
                if show_progress {
                    let (more, left) =
                        plan::estimate(stream.len() - sent, self.lv, self.redundancy, self.acknowledged, self.encrypted());
                    let again = if attempt > 1 { format!(" - attempt {attempt}, now at level {}", self.lv) } else { String::new() };
                    log(&format!("block {} of {} ({} bytes), about {} left{again}", done + 1, done + more, size, clock(left)));
                }
                let body = match &mut self.cipher {
                    Some(c) => c.seal(chunk, &[field as u8, self.flags, self.address]).unwrap_or_else(|e| ui::die(e)),
                    None => chunk.to_vec(),
                };
                let alphabet = modem::alphabet(self.lv);
                let symbols = modem::pack(&vocsend::frame::encode(&body, self.redundancy), alphabet.bits());
                let audio = Packet::sender(DATA, self.lv, field, self.flags, self.address).audio(&[(alphabet, &symbols)]);
                self.link.send(audio, show_progress.then_some("sending"));
                if !self.acknowledged {
                    delivered = true;
                    break;
                }
                let want = (self.seq & 7) as u8;
                match self.link.listen(Some(10.0), &|p| matches!(p.kind, ACK | NAK | REFUSE)) {
                    Some(p) if p.kind == ACK && p.a == want => {
                        delivered = true;
                        break;
                    }
                    Some(p) if p.kind == REFUSE => return Err(self.refused(p.a)),
                    Some(_) => log("  the receiver could not decode it"),
                    None => log("  not acknowledged"),
                }
                self.lv = self.lv.saturating_sub(1).max(1);
                self.redundancy = self.redundancy.max(level(self.lv).redundancy);
                size = plan::split(stream.len() - sent, self.lv, self.redundancy, true, self.encrypted())[0];
                self.link.pause(TURNAROUND);
            }
            if !delivered {
                ui::error(&format!("gave up after {ATTEMPTS} attempts at a block"));
                return Err(EXIT_FAILED);
            }
            sent += size;
            done += 1;
            self.seq += 1;
            self.link.pause(TURNAROUND);
            if sent >= stream.len() {
                return Ok(());
            }
        }
    }

    /// Tell a streaming receiver there is no more to come.
    fn bye(&mut self) {
        for _ in 0..ATTEMPTS {
            self.link.send(Packet::sender(BYE, 0, 0, self.flags, self.address).audio(&[]), None);
            if !self.acknowledged || self.link.listen(Some(9.0), &|p| p.kind == ACK).is_some() {
                break;
            }
        }
    }
}

/// Bytes from standard input as they arrive, without waiting for a newline.
fn spawn_char_reader() -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let on_terminal = std::io::stdin().is_terminal();
        // A terminal hands over whole lines unless told otherwise. A small
        // shell wrapper switches that off and restores it however we exit.
        let mut child = (on_terminal && cfg!(unix))
            .then(|| {
                std::process::Command::new("sh")
                    .args(["-c", "trap 'stty sane' EXIT INT TERM; stty -icanon min 1 time 0; cat"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .ok()
            })
            .flatten();
        let mut source: Box<dyn Read> = match child.as_mut().and_then(|c| c.stdout.take()) {
            Some(out) => Box::new(out),
            None => Box::new(std::io::stdin()),
        };
        let mut buf = [0u8; 256];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Ctrl-D ends the stream when typing straight into a terminal.
                    let end = on_terminal.then(|| buf[..n].iter().position(|&b| b == 4)).flatten();
                    let take = end.unwrap_or(n);
                    if take > 0 && tx.send(buf[..take].to_vec()).is_err() {
                        break;
                    }
                    if end.is_some() {
                        break;
                    }
                }
            }
        }
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
    });
    rx
}

pub fn tx(opts: Options) -> i32 {
    let streaming = opts.stream != Stream::Off;
    let (data, name) = if streaming {
        (Vec::new(), None)
    } else {
        match opts.path.as_deref() {
            None | Some("-") => {
                let mut data = Vec::new();
                if let Err(e) = std::io::stdin().read_to_end(&mut data) {
                    ui::die(&format!("could not read standard input: {e}"));
                }
                (data, None)
            }
            Some(path) => {
                let data = std::fs::read(path).unwrap_or_else(|e| ui::die(&format!("{path}: {e}")));
                let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned());
                (data, name)
            }
        }
    };
    let stream = payload::wrap(&data, name.as_deref());

    let lv = match opts.level {
        Some(0) => saved_level(),
        Some(l) => l,
        None => plan::ONE_WAY_LEVEL,
    };
    let flags = if opts.cipher.is_some() { ENCRYPTED } else { 0 }
        | if opts.negotiate { NEGOTIATED } else { 0 }
        | if streaming { STREAMING } else { 0 };
    if !streaming {
        log(&format!("fingerprint {} (the receiver should show the same)", ui::highlight(&crypto::fingerprint(&data))));
    }
    let mut sender = Sender {
        link: Link::open(true, opts.negotiate),
        cipher: opts.cipher,
        flags,
        address: opts.address.as_deref().map(crypto::address_tag).unwrap_or(0),
        acknowledged: opts.negotiate,
        lv,
        redundancy: level(lv).redundancy,
        seq: 0,
    };
    if opts.negotiate {
        if let Err(code) = sender.negotiate(if streaming { 0 } else { stream.len() }) {
            return code;
        }
    }

    let result = match opts.stream {
        Stream::Off => {
            let (blocks, seconds) =
                plan::estimate(stream.len(), sender.lv, sender.redundancy, sender.acknowledged, sender.encrypted());
            let squeezed = if payload::is_compressed(&stream) {
                format!(", {} after compression", stream.len() - payload::header_len(&stream))
            } else {
                String::new()
            };
            let what = match &name {
                Some(n) => format!("{n} ({} bytes{squeezed})", data.len()),
                None => format!("{} bytes{}", data.len(), if squeezed.is_empty() { String::new() } else { format!(" ({})", &squeezed[2..]) }),
            };
            log(&format!("sending {what} in {blocks} block{} at level {}: about {}", plural(blocks), sender.lv, clock(seconds)));
            sender.transfer(&stream, true)
        }
        Stream::Lines => {
            log("streaming: each line is sent when you press Enter; end with Ctrl-D");
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut result = Ok(());
            loop {
                let mut line = Vec::new();
                match input.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                result = sender.transfer(&payload::wrap(&line, None), false);
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() {
                sender.bye();
            }
            result
        }
        Stream::Chars => {
            log("streaming: whatever you type is sent after a short pause; end with Ctrl-D");
            let input = spawn_char_reader();
            let mut result = Ok(());
            // Wait for something to arrive, gather whatever follows closely, send, repeat.
            while let Ok(mut pending) = input.recv() {
                while let Ok(more) = input.recv_timeout(std::time::Duration::from_millis(250)) {
                    pending.extend(more);
                }
                result = sender.transfer(&payload::wrap(&pending, None), false);
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() {
                sender.bye();
            }
            result
        }
    };
    match result {
        Ok(()) => {
            if opts.negotiate {
                save_level(sender.lv);
                if !streaming {
                    log(&format!("delivered {} bytes; level {} saved for next time", data.len(), sender.lv));
                }
            } else if !streaming {
                log("sent (one-way: nothing confirms that it arrived)");
            }
            0
        }
        Err(code) => code,
    }
}

// ---------------------------------------------------------------- receiver

/// `name`, or name-1, name-2 ... if that is taken: a name chosen by the
/// sender never overwrites anything.
fn free_name(name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let mut candidate = name.to_string();
    let mut n = 0;
    while std::path::Path::new(&candidate).exists() {
        n += 1;
        candidate = format!("{stem}-{n}{ext}");
    }
    candidate
}

/// Put a finished transfer where it belongs and say what happened.
fn deliver(name: Option<String>, data: &[u8], target: Option<&str>) -> Result<String, String> {
    let sent_as = name.as_deref().map(payload::safe_name);
    let path = match (target, &sent_as) {
        (Some("-"), _) | (None, None) => None,
        (Some(chosen), _) => Some(chosen.to_string()),
        (None, Some(n)) => Some(free_name(n)),
    };
    match path {
        None => {
            let mut out = std::io::stdout().lock();
            out.write_all(data).and_then(|_| out.flush()).map_err(|e| format!("could not write to standard output: {e}"))?;
            Ok(match sent_as {
                Some(n) => format!("received {} bytes (sent as file {n}; written to standard output)", data.len()),
                None => format!("received {} bytes", data.len()),
            })
        }
        Some(path) => {
            let part = format!("{path}.part");
            std::fs::write(&part, data).and_then(|_| std::fs::rename(&part, &path)).map_err(|e| format!("could not save {path}: {e}"))?;
            Ok(match sent_as {
                Some(n) => format!("received file {n} ({} bytes) and saved as {path}", data.len()),
                None => format!("received {} bytes and saved as {path}", data.len()),
            })
        }
    }
}

pub fn rx(mut opts: Options) -> i32 {
    // A receiver that is not negotiating gets no way of making sound at all.
    let mut link = Link::open(opts.negotiate, true);
    log(if opts.negotiate { "waiting for the sender..." } else { "listening (one-way: this side will not make a sound)..." });
    let mut collected: Vec<u8> = Vec::new();
    let mut last_seq: Option<u8> = None;
    let mut blocks = 0usize;
    let mut expected: Option<usize> = None;
    let mut asked: Option<f64> = None;
    let mut streaming = opts.stream != Stream::Off;
    let mut told_about_probe = false;
    let mut told_about_others = false;
    let mut contacted = false;
    let has_key = opts.cipher.is_some();
    let mine = opts.address.as_deref().map(crypto::address_tag).unwrap_or(0);

    loop {
        // Once a stream is under way, silence just means nobody is typing.
        let wait = if streaming && contacted { None } else { opts.timeout };
        let Some(p) = link.listen(wait, &|p| modem::kind::from_sender(p.kind)) else {
            ui::error(&format!(
                "nothing heard for {:.0} seconds; giving up. (--timeout 0 waits for ever.)",
                opts.timeout.unwrap_or(0.0)
            ));
            return EXIT_FAILED;
        };
        // Addressed to another receiver: not ours to open, or even to answer.
        if p.address != 0 && p.address != mine {
            if !told_about_others {
                log(if mine == 0 {
                    "heard a transmission addressed to a named receiver; ignoring it (this side has no --address)"
                } else {
                    "heard a transmission addressed to a different receiver; ignoring it"
                });
                told_about_others = true;
            }
            continue;
        }
        let wants_reply = p.flags & NEGOTIATED != 0 || matches!(p.kind, CALL | RAMP | TEST);
        if p.needs > modem::PROTOCOL || p.version < modem::MIN_SENDER {
            let too_old_here = p.needs > modem::PROTOCOL;
            ui::error(&if too_old_here {
                format!(
                    "the sender uses protocol version {} and needs a receiver at version {} or newer; this vocsend speaks version {}. Upgrade this side.",
                    p.version, p.needs, modem::PROTOCOL
                )
            } else {
                format!(
                    "the sender uses protocol version {}; this vocsend needs version {} or newer. Upgrade the sending side.",
                    p.version, modem::MIN_SENDER
                )
            });
            if wants_reply && opts.negotiate {
                link.pause(TURNAROUND);
                link.send(Packet::reply(REFUSE, if too_old_here { 3 } else { 4 }, 0).audio(&[]), None);
            }
            return EXIT_VERSION;
        }
        contacted = true;

        if p.kind == PROBE {
            if !told_about_probe {
                log("the sender is running `vocsend tx test`; run `vocsend rx test` on this side to see the results");
                told_about_probe = true;
            }
            continue;
        }
        if wants_reply && !opts.negotiate {
            ui::error("the sender is trying to negotiate, but this receiver is in one-way mode and will not transmit.");
            ui::error("Either run `vocsend rx negotiate` here, or send without `negotiate` on the other side.");
            return EXIT_MODE;
        }
        if (p.flags & ENCRYPTED != 0) != has_key {
            ui::error(if has_key {
                "this side was started with --encrypt, but the sender is not encrypting. Nothing was accepted."
            } else {
                "the sender is encrypting, but this side was started without --encrypt. Run it again with --encrypt."
            });
            if wants_reply {
                link.pause(TURNAROUND);
                link.send(Packet::reply(REFUSE, 1, 0).audio(&[]), None);
            }
            return EXIT_KEY;
        }

        match p.kind {
            CALL => {
                log("sender is calling; answering");
                link.pause(TURNAROUND);
                link.send(Packet::reply(ANSWER, 0, 0).audio(&[]), None);
            }
            RAMP => {
                let (lo, hi) = (p.a as usize, p.b as usize);
                if lo < 1 || hi > 10 || lo > hi {
                    continue;
                }
                let mut clean = 0;
                let mut errors = Vec::new();
                for lv in lo..=hi {
                    let want = plan::known_symbols(lv, RAMP_SYLLABLES, 1);
                    let wrong = match link.read(modem::alphabet(lv), RAMP_SYLLABLES, None) {
                        Some(got) => got.iter().zip(&want).filter(|(g, w)| g.0 != **w).count(),
                        None => RAMP_SYLLABLES,
                    };
                    if wrong == 0 {
                        clean = lv;
                    }
                    errors.push(format!("{lv}:{wrong}"));
                }
                let pick = if clean > 0 { format!("level {clean}") } else { "none clean".to_string() };
                log(&format!("ramp, wrong syllables per level {} -> {pick}", errors.join(" ")));
                link.pause(TURNAROUND);
                link.send(Packet::reply(REPORT, clean, 0).audio(&[]), None);
            }
            TEST => {
                let lv = p.a as usize;
                if !(1..=10).contains(&lv) {
                    continue;
                }
                expected = link
                    .read(modem::control(), 14, None)
                    .and_then(|d| plan::read_size(&d.iter().map(|s| s.0).collect::<Vec<_>>()))
                    .filter(|&n| n > 0);
                let want = plan::known_symbols(lv, TEST_SYLLABLES, 2);
                let wrong = match link.read(modem::alphabet(lv), TEST_SYLLABLES, None) {
                    Some(got) => got.iter().zip(&want).filter(|(g, w)| g.0 != **w).count(),
                    None => TEST_SYLLABLES,
                };
                let (use_lv, redundancy) = plan::choose(lv, wrong);
                asked = Some(redundancy);
                log(&format!(
                    "test at level {lv}: {wrong} of {TEST_SYLLABLES} wrong -> level {use_lv} with {:.0}% error correction (about {:.0} bits/s)",
                    100.0 * redundancy,
                    modem::speed(use_lv, redundancy)
                ));
                if let Some(n) = expected {
                    let (count, seconds) = plan::estimate(n, use_lv, redundancy, true, has_key);
                    log(&format!("expecting {n} bytes in {count} block{}: about {}", plural(count), clock(seconds)));
                }
                link.pause(TURNAROUND);
                link.send(Packet::reply(ACCEPT, use_lv, plan::redundancy_code(redundancy)).audio(&[]), None);
            }
            BYE => {
                if wants_reply {
                    link.pause(TURNAROUND);
                    link.send(Packet::reply(ACK, 0, 0).audio(&[]), None);
                }
                log("the sender has finished");
                return 0;
            }
            _ => {
                // DATA
                let lv = p.a as usize;
                if !(1..=10).contains(&lv) {
                    continue;
                }
                let (seq, last) = (p.b & 7, p.b & 8 != 0);
                if p.flags & STREAMING != 0 && !streaming {
                    streaming = true;
                    log("the sender is streaming: writing to standard output until it finishes");
                }
                let show = !streaming;
                let Some(body) = link.read_frame(lv, show.then_some("receiving")) else {
                    log(&format!("block {} at level {lv} did not decode", blocks + 1));
                    if wants_reply {
                        link.pause(TURNAROUND);
                        link.send(Packet::reply(NAK, seq as usize, 0).audio(&[]), None);
                        continue;
                    }
                    if streaming {
                        continue;
                    }
                    ui::error("the message did not arrive intact. One-way transfers cannot be repeated automatically: send it again, perhaps at a lower --level.");
                    return EXIT_FAILED;
                };
                let block = match &mut opts.cipher {
                    Some(cipher) => match cipher.open(&body, &[p.b, p.flags, p.address]) {
                        Ok(plain) => plain,
                        Err(why) => {
                            ui::error(&format!("could not decrypt: {why}"));
                            if wants_reply {
                                link.pause(TURNAROUND);
                                link.send(Packet::reply(REFUSE, 2, 0).audio(&[]), None);
                            }
                            return EXIT_KEY;
                        }
                    },
                    None => body,
                };
                let repeat = last_seq == Some(seq) && wants_reply;
                if !repeat {
                    let size = block.len();
                    collected.extend_from_slice(&block);
                    blocks += 1;
                    last_seq = Some(seq);
                    if show {
                        match expected.filter(|_| !last) {
                            Some(n) => {
                                let use_r = asked.unwrap_or(0.0).max(level(lv).redundancy);
                                let (more, left) = plan::estimate(n.saturating_sub(collected.len()), lv, use_r, true, has_key);
                                log(&format!("block {blocks} of {} received ({size} bytes), about {} left", blocks + more, clock(left)));
                            }
                            None if expected.is_some() => log(&format!("block {blocks} of {blocks} received ({size} bytes)")),
                            None => log(&format!("block {blocks} received ({size} bytes)")),
                        }
                    }
                }
                if wants_reply {
                    link.pause(TURNAROUND);
                    link.send(Packet::reply(ACK, seq as usize, 0).audio(&[]), None);
                }
                if !last || repeat {
                    continue;
                }
                let stream = std::mem::take(&mut collected);
                blocks = 0;
                let (name, data) = match payload::unwrap(&stream) {
                    Ok(v) => v,
                    Err(why) => {
                        ui::error(why);
                        return EXIT_FAILED;
                    }
                };
                if streaming {
                    let mut out = std::io::stdout().lock();
                    if out.write_all(&data).and_then(|_| out.flush()).is_err() {
                        return EXIT_FAILED;
                    }
                    continue;
                }
                if wants_reply {
                    // Say it twice: a lost final acknowledgement cannot be asked for again.
                    link.pause(0.3);
                    link.send(Packet::reply(ACK, seq as usize, 0).audio(&[]), None);
                }
                return match deliver(name, &data, opts.path.as_deref()) {
                    Ok(summary) => {
                        log(&summary);
                        log(&format!("fingerprint {} (the sender should show the same)", ui::highlight(&crypto::fingerprint(&data))));
                        0
                    }
                    Err(why) => {
                        ui::error(&why);
                        EXIT_FAILED
                    }
                };
            }
        }
    }
}

// --------------------------------------------------------------- test mode

const PROBE_SYLLABLES: usize = 60;

pub fn tx_test(opts: Options) -> i32 {
    let (lo, hi) = opts.levels;
    let mut link = Link::open(true, false);
    log(&format!("sending test bursts at levels {lo} to {hi}, over and over; Ctrl-C to stop"));
    loop {
        for lv in lo..=hi {
            let known = plan::known_symbols(lv, PROBE_SYLLABLES, 3);
            link.send(Packet::sender(PROBE, lv, 0, 0, 0).audio(&[(modem::alphabet(lv), &known)]), None);
            link.pause(0.3);
        }
    }
}

pub fn rx_test(opts: Options) -> i32 {
    let (lo, hi) = opts.levels;
    let mut link = Link::open(false, true);
    let mut results: Vec<Vec<(f64, f32)>> = vec![Vec::new(); 11];
    let mut heard = 0;
    let fancy = std::io::stdout().is_terminal();
    let mut drawn = 0;
    let mut show = |results: &Vec<Vec<(f64, f32)>>, heard: usize| {
        let mut rows = vec![
            "level  pitches  syllable  bits/s   last run           recent average   verdict".to_string(),
            "                          (net)    wrong   loudness   (up to 3 runs)".to_string(),
        ];
        let mut best = None;
        for lv in lo..=hi {
            let l = level(lv);
            let head = format!("{lv:>5}  {:>7}  {:>5} ms  {:>6.0}   ", l.pitches, l.ms, modem::speed(lv, l.redundancy));
            let runs = &results[lv];
            match runs.last() {
                None => rows.push(format!("{head}waiting...")),
                Some(&(wrong, loud)) => {
                    let recent = &runs[runs.len().saturating_sub(3)..];
                    let average = recent.iter().map(|r| r.0).sum::<f64>() / recent.len() as f64;
                    let grade = plan::verdict(lv, average);
                    if grade == "solid" {
                        best = Some(lv);
                    }
                    rows.push(format!("{head}{:>5.1}%  {loud:>5.0} dB    {:>5.1}%           {grade}", 100.0 * wrong, 100.0 * average));
                }
            }
        }
        rows.push(String::new());
        rows.push(match best {
            Some(lv) => format!("bursts heard: {heard}.  Fastest solid level so far: {lv}  (use: vocsend tx --level {lv})"),
            None => format!("bursts heard: {heard}.  No level is solid yet."),
        });
        let mut out = std::io::stdout().lock();
        if fancy && drawn > 0 {
            let _ = write!(out, "\x1b[{drawn}A");
        }
        for row in &rows {
            let _ = if fancy { writeln!(out, "\x1b[2K{row}") } else { writeln!(out, "{row}") };
        }
        let _ = out.flush();
        drawn = rows.len();
    };
    log("listening for `vocsend tx test` on the other machine; the table updates after each burst. Ctrl-C to stop.");
    show(&results, heard);
    loop {
        let wait = if heard == 0 { opts.timeout } else { None };
        let Some(p) = link.listen(wait, &|p| p.kind == PROBE && (1..=10).contains(&p.a)) else {
            ui::error("no test bursts heard; giving up. Is `vocsend tx test` running on the other machine? (--timeout 0 waits for ever.)");
            return EXIT_FAILED;
        };
        let lv = p.a as usize;
        let known = plan::known_symbols(lv, PROBE_SYLLABLES, 3);
        let wrong = match link.read(modem::alphabet(lv), PROBE_SYLLABLES, None) {
            Some(got) => got.iter().zip(&known).filter(|(g, w)| g.0 != **w).count(),
            None => PROBE_SYLLABLES,
        };
        let seconds = PROBE_SYLLABLES as f64 * level(lv).ms as f64 / 1000.0;
        results[lv].push((wrong as f64 / PROBE_SYLLABLES as f64, link.loudness(seconds)));
        heard += 1;
        if fancy || heard % (hi - lo + 1) == 0 {
            show(&results, heard);
        }
    }
}
