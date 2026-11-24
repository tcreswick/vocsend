# vocsend

Send small amounts of data from one computer to another by sound, through
the noise suppression that stops other tools from working.

```console
receiver$ vocsend rx
sender$ vocsend tx id_ed25519.pub
```

By Tim Creswick. MIT licence.

## The problem

1. **Small things often need to cross a gap.** A public key, a certificate,
   a signature or a one-time code has to get from one machine to another
   that is air-gapped, or simply not easy to connect: different networks, no
   shared drive, no cable to hand.

2. **Sound is the obvious bridge, but modern audio hardware breaks it.**
   Microphones, drivers and sound servers now apply noise suppression, echo
   cancellation and automatic gain by default. They are built to keep
   speech and remove everything else, and a data signal is "everything
   else". Traditional tools such as minimodem are inoperable on a lot of
   current hardware as a result.

## The approach

vocsend uses **vocal-style encoding**: the signal is built from short
synthetic syllables with the properties suppressors look for in a voice. It
is designed to pass through most noise cancellation and suppression systems,
which makes it possible to send data short distances using nothing but a
speaker and a microphone.

- **The data rides on pitch.** Each syllable is one steady pitch out of a
  small set. A room changes loudness and tone, but it cannot move a
  frequency, so pitch arrives intact where other properties do not.
- **Everything else varies, and means nothing.** The vowel, the way each
  syllable starts, its brightness and its loudness change on every syllable
  in a fixed pattern both ends know. An adaptive suppressor removes anything
  that holds still; this never does.
- **Errors are repaired.** Every block carries a checksum and Reed-Solomon
  error correction, and nothing is written out unless it verifies.

It is slow by network standards, between 8 and 104 bits per second, which is
plenty for the things it is meant for: a 32-byte key takes about fifteen
seconds at the default one-way speed.

## Examples

In these, `sender$` and `receiver$` are the prompts on the two machines.
Start the receiver first unless it says otherwise.

**Quick-send a public key from one box to another.** One-way: the receiving
machine never makes a sound.

```console
receiver$ vocsend rx
sender$ vocsend tx ~/.ssh/id_ed25519.pub
```

The receiver saves `id_ed25519.pub` in its current directory. Both sides
print the same short fingerprint, so you can see it arrived intact.

**Send a file, negotiated.** The two machines agree on the fastest speed
the path can take, and every block is acknowledged and repeated if needed.
Start them in either order.

```console
receiver$ vocsend rx negotiate
sender$ vocsend tx negotiate release.sig
```

**Send to several machines at once.** A one-way transmission is a
broadcast: every receiver in earshot gets its own copy.

```console
receiver-1$ vocsend rx
receiver-2$ vocsend rx
receiver-3$ vocsend rx
sender$ vocsend tx ca-certificate.pem
```

**Send with encryption.** Both sides are asked for the shared key, which is
never shown or sent.

```console
receiver$ vocsend rx --encrypt
Key:
sender$ vocsend tx secret.txt --encrypt
Key:
```

Or give the key directly, which is handier in scripts but visible in the
process list and shell history:

```console
receiver$ vocsend rx --encrypt 'correct horse battery staple'
sender$ vocsend tx secret.txt --encrypt 'correct horse battery staple'
```

**Send to one machine among several.** With encryption, `--address` names
the receiver a message is for. The others ignore it.

```console
laptop$ vocsend rx --encrypt --address laptop
desktop$ vocsend rx --encrypt --address desktop
sender$ vocsend tx notes.txt --encrypt --address laptop
```

**Stream between hosts.** Each line is sent when you press Enter and
appears on the other side. End with Ctrl-D.

```console
receiver$ vocsend rx --stream
sender$ vocsend tx --stream
```

**Pipe anything.**

```console
receiver$ vocsend rx | sha256sum
sender$ gpg --export --armor me@example.com | vocsend tx
```

## Install

Download a binary for your system from the
[releases page](https://github.com/tcreswick/vocsend/releases), or build it:

```bash
git clone https://github.com/tcreswick/vocsend
cd vocsend
cargo build --release
```

That produces `target/release/vocsend`. It needs a Rust toolchain and, on
Linux, the ALSA development headers (`libasound2-dev` on Debian and Ubuntu,
`alsa-lib-devel` on Fedora, `alsa-lib` on Arch).

The dependencies are few, widely used and pure Rust: `cpal` for sound, the
RustCrypto libraries for encryption and hashing, `flate2` for compression,
`getrandom` and `rpassword`. The signal processing and error correction are
in this repository.

### Sound

vocsend talks to the system's sound devices directly: ALSA on Linux (which
PipeWire and PulseAudio also answer to), CoreAudio on macOS, WASAPI on
Windows. It uses the default output and input, at whatever rate they run.

If that fails, or if you ask for it with `VOCSEND_AUDIO=commands`, it falls
back to command-line tools: PipeWire (`pw-cat`), PulseAudio (`paplay`,
`parec`), ALSA (`aplay`, `arecord`) or SoX (`sox`). Setting `VOCSEND_PLAY`
and `VOCSEND_RECORD` names commands of your own, which must read or write
raw 16-bit mono samples at 16000 Hz.

To build with no system libraries at all, using only those command-line
tools: `cargo build --release --no-default-features`.

### Shell completion

```bash
vocsend completions bash > ~/.local/share/bash-completion/completions/vocsend
vocsend completions zsh  > "${fpath[1]}/_vocsend"
vocsend completions fish > ~/.config/fish/completions/vocsend.fish
```

## Usage

```
vocsend tx [negotiate] [FILE]     send
vocsend rx [negotiate] [FILE]     receive
vocsend tx test | rx test         find out which speeds work between two machines
vocsend help                      everything, in detail
```

| What is sent | |
|---|---|
| `vocsend tx FILE` | the file, and its name |
| `vocsend tx` or `vocsend tx -` | standard input |

| Where it goes | |
|---|---|
| `vocsend rx` | a file with the sender's name if one was sent (never overwriting: `key.pub`, then `key-1.pub`), otherwise standard output |
| `vocsend rx FILE` | that file, whatever the sender called it |
| `vocsend rx -` | standard output, always |

| Option | |
|---|---|
| `--level N` | one-way speed, 1 (slowest, most robust) to 10 (fastest). Default 4. `--level last` reuses the level the last negotiation ended on. |
| `--encrypt [KEY]` | encrypt with a shared key. With no key you are asked for it. |
| `--address NAME` | with `--encrypt`: who the message is for (see below) |
| `--timeout SECONDS` | receiver: give up if nothing is heard for this long. Default 60; 0 waits for ever. |
| `--stream` | send standard input a line at a time; print each line as it arrives |
| `--stream-chars` | as `--stream`, without waiting for Enter |
| `-q`, `--quiet` | print nothing unless something goes wrong |

### One-way or negotiated

**One-way** (the default) sends sound in one direction only. A one-way
receiver never opens the speaker, so it cannot transmit; if the sender tries
to negotiate with it, it stops with an error and stays silent. The price is
that nothing can be asked for again, so the sender uses a cautious speed and
cannot know whether the message arrived.

**Negotiated** (`negotiate` on both sides):

1. The sender calls every few seconds until the receiver answers.
2. The sender plays a ramp: a short burst at each speed level. The receiver
   reports the fastest level that arrived clean.
3. The sender plays a longer test at that level. The receiver confirms it,
   asks for more error correction, or steps down a level, whichever is
   fastest overall.
4. The data goes in blocks of about 30 seconds, each acknowledged. A block
   that fails is sent again one level slower, up to three times.

Only the sender-to-receiver direction is trained. Everything the receiver
says uses the slowest, most robust setting.

### Speed levels

| Level | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|---|---|---|---|---|---|---|---|---|---|---|
| Pitches | 4 | 8 | 8 | 16 | 16 | 16 | 32 | 32 | 64 | 64 |
| Syllable (ms) | 120 | 100 | 80 | 80 | 60 | 50 | 60 | 50 | 55 | 50 |
| Bits per second | 8 | 18 | 25 | 36 | 51 | 64 | 67 | 83 | 91 | 104 |

Higher levels use more pitches, packed closer together, in shorter
syllables, with less error correction.

### Finding out what works

```console
receiver$ vocsend rx test
sender$ vocsend tx test
```

The sender loops through the levels with a known burst at each. The receiver
shows a live table of how each one is arriving, so you can move the machines
or change the volume and watch what happens:

```
level  pitches  syllable  bits/s   last run           recent average   verdict
                          (net)    wrong   loudness   (up to 3 runs)
    6       16     50 ms      64     0.0%    -21 dB      0.0%           solid
    7       32     60 ms      67     1.7%    -21 dB      2.5%           marginal
    8       32     50 ms      83     6.7%    -21 dB      6.7%           fails

bursts heard: 6.  Fastest solid level so far: 6  (use: vocsend tx --level 6)
```

### Encryption

`--encrypt` on both sides encrypts and authenticates every block with
ChaCha20-Poly1305, using a key derived from the shared passphrase by
Argon2id (both from the RustCrypto libraries). The file name is encrypted
along with the data. Every packet says whether encryption is in use, so a
mismatch is reported at once instead of producing garbage, and a recorded
transmission cannot be replayed into the same session.

Give the key as `--encrypt KEY`, in the `VOCSEND_KEY` environment variable,
or leave it off to be asked. A key on the command line is visible to other
users of the machine and ends up in shell history.

Anyone who can hear the transmission can record it, and a shared passphrase
is only as strong as it is long. For anything that matters, encrypt and
sign the data with an established tool before sending it as well.

### Addressing one receiver among several

With several receivers in earshot, `--address` says which one a message is
for:

```console
laptop$ vocsend rx --encrypt --address laptop
desktop$ vocsend rx --encrypt --address desktop
sender$ vocsend tx notes.txt --encrypt --address laptop
```

Only the laptop takes it; the desktop ignores it without trying its key.
A message sent with `--encrypt` and no address is for anyone with the key.
Addressing only works with encryption: without a key, any receiver could
read the message regardless of what address it carries. The address costs
about half a second of air time, and is a convenience, not a secret.

### Checking it arrived

Both sides print a short fingerprint of the data, such as `3f9a-c21d-77b0`.
If they match, both have the same bytes. Every block already carries a
checksum and error correction, so a mismatch should never happen; the
fingerprint is for peace of mind, and for one-way transfers, where the
sender otherwise hears nothing back.

### Versions

Every transmission states the protocol version the sender speaks and the
oldest receiver version that can follow it. If the two sides are not
compatible, both say so plainly, and which side needs upgrading.

### Exit status

| | |
|---|---|
| 0 | success |
| 1 | the transfer failed, or nothing was heard before the timeout |
| 2 | wrong mode: the sender is negotiating and the receiver is one-way |
| 3 | encryption settings or keys differ |
| 4 | the two sides' versions are not compatible |

## Status

This is the first release of the Rust implementation. The signalling design
came out of measurements on real hardware with aggressive noise suppression,
made with an earlier prototype. The Rust code has so far been exercised by
its unit tests and by a two-process simulator (`tests/sim`) that plays one
side's sound into the other's microphone with echo and noise; treat real
hardware results as still to be confirmed, and use `vocsend rx test` to see
what your own machines can do.

## How it works, briefly

```
bytes -> compress -> [encrypt] -> blocks -> checksum + Reed-Solomon -> pitches -> syllables -> sound
```

Every transmission starts with a fixed preamble of varied syllables, which
the receiver finds by matching its spectrogram, followed by a small control
packet at the slowest setting saying what follows: its kind, the protocol
versions, the address and whether it is encrypted. The receiver then reads
the pitch of each syllable with a comb of harmonics, and keeps time by how
cleanly the pitch stands out.

## Licence

MIT. Copyright (c) 2026 Tim Creswick. See [LICENSE](LICENSE).
