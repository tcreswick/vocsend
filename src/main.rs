//! vocsend: send small amounts of data between computers by sound.
//! Copyright (c) 2026 Tim Creswick. MIT licence. https://github.com/tcreswick/vocsend

mod audio;
mod link;
mod run;
mod ui;

use run::{Options, Stream};
use std::io::IsTerminal;
use vocsend::crypto::Cipher;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "https://github.com/tcreswick/vocsend";

fn banner() -> String {
    format!("vocsend {VERSION} - data by sound, through noise suppression\nby Tim Creswick - {REPO} - MIT licence")
}

fn help() -> String {
    format!(
        "{}

Sends small amounts of data (keys, certificates, signatures, short files)
from one computer's speaker to another's microphone. The sound is made of
voice-like syllables, so it survives the noise suppression that most modern
microphones apply, which destroys ordinary data-over-sound tools.

USAGE
  vocsend tx [negotiate] [FILE]     send
  vocsend rx [negotiate] [FILE]     receive
  vocsend tx test | rx test         find out which speeds work between two machines
  vocsend completions SHELL         print a completion script (bash, zsh, fish)
  vocsend help | --version

QUICK START
  One-way. Start the receiver, then the sender. Nothing is sent back, so the
  receiving machine never makes a sound:
      machine B:   vocsend rx
      machine A:   vocsend tx key.pub

  Negotiated. The two machines agree on the fastest speed that works and
  every block is acknowledged and repeated if needed. Start in either order:
      machine B:   vocsend rx negotiate
      machine A:   vocsend tx negotiate backup.tar.gz

WHAT IS SENT
  vocsend tx FILE        the file, and its name
  vocsend tx             standard input   (same as: vocsend tx -)

WHERE IT GOES
  vocsend rx             a file with the sender's name, if a name was sent
                         (never overwriting: key.pub, then key-1.pub ...);
                         otherwise standard output
  vocsend rx FILE        that file, whatever the sender called it
  vocsend rx -           standard output, always

ONE-WAY OR NEGOTIATED
  Without `negotiate`, sound goes one way only. The receiver has no way to
  ask for anything again, so the sender uses a cautious speed (level 4) and
  cannot know whether the message arrived. A one-way receiver never opens
  the speaker. If the sender tries to negotiate with it, it stops with an
  error and stays silent.

  With `negotiate` on both sides:
    1. The sender calls every few seconds until the receiver answers.
    2. The sender plays a ramp: a short burst at each speed level. The
       receiver reports the fastest level that arrived clean.
    3. The sender plays a longer test at that level. The receiver confirms
       it, asks for more error correction, or steps down a level.
    4. The data goes in blocks of about 30 seconds, each acknowledged. A
       block that fails is sent again one level slower, up to 3 times.
  Both sides show the block count and the time left.

OPTIONS
  --level N          tx, one-way: speed from 1 (slowest, most robust) to
                     10 (fastest, clean links only). Default 4.
                     `--level last` uses the level the last negotiation
                     ended on. Ignored with `negotiate`.
  --encrypt [KEY]    encrypt with a shared key (ChaCha20-Poly1305). Both
                     sides must use it, with the same key. With no KEY, or
                     with --encrypt as the last word, you are asked for it;
                     it can also come from the VOCSEND_KEY environment
                     variable. A key on the command line is visible to
                     other users of the machine and lands in shell history.
  --stream           send standard input a line at a time as it is typed,
                     and print each line on the other side as it arrives.
                     Works one-way or negotiated. End with Ctrl-D.
  --stream-chars     as --stream, but without waiting for Enter: whatever
                     has been typed is sent after a short pause.
  --levels LO-HI     test: which levels to try (default 3-10).
  -q, --quiet        print nothing unless something goes wrong.
  -h, --help         this text.
  -V, --version      the version.

SPEED LEVELS
  level    1    2    3    4    5    6    7    8    9   10
  bits/s   8   18   25   36   51   64   67   83   91  104
  Run `vocsend rx test` on one machine and `vocsend tx test` on the other
  to see a live table of which levels arrive intact between them.

EXIT STATUS
  0 success   1 failed   2 wrong mode (sender negotiating, receiver one-way)
  3 encryption settings or keys differ

SOUND
  Uses the system's own tools: PipeWire (pw-cat), PulseAudio (paplay and
  parec), ALSA (aplay and arecord) or SoX (sox). Set VOCSEND_PLAY and
  VOCSEND_RECORD to use other commands.",
        banner()
    )
}

fn usage(problem: &str) -> ! {
    eprintln!("vocsend: {problem}\nTry `vocsend help`.");
    std::process::exit(2)
}

/// Ask for the key without showing it.
fn ask_key() -> String {
    if let Ok(key) = std::env::var("VOCSEND_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    #[cfg(unix)]
    {
        use std::io::{BufRead, Write};
        if let Ok(tty) = std::fs::File::open("/dev/tty") {
            let hide = |on: bool| {
                let _ = std::process::Command::new("stty")
                    .arg(if on { "-echo" } else { "echo" })
                    .stdin(std::fs::File::open("/dev/tty").map(std::process::Stdio::from).unwrap_or(std::process::Stdio::null()))
                    .status();
            };
            eprint!("Key: ");
            let _ = std::io::stderr().flush();
            hide(true);
            let mut line = String::new();
            let _ = std::io::BufReader::new(tty).read_line(&mut line);
            hide(false);
            eprintln!();
            return line.trim_end_matches(['\r', '\n']).to_string();
        }
    }
    if std::io::stdin().is_terminal() {
        eprint!("Key (it will be visible): ");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        return line.trim_end_matches(['\r', '\n']).to_string();
    }
    usage("--encrypt needs a key: give it as --encrypt KEY or in VOCSEND_KEY")
}

const BASH: &str = r#"# bash completion for vocsend.  Install with:
#   vocsend completions bash > ~/.local/share/bash-completion/completions/vocsend
_vocsend() {
    local cur prev words
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    case "$prev" in
        --level) COMPREPLY=($(compgen -W "1 2 3 4 5 6 7 8 9 10 last" -- "$cur")); return ;;
        --levels) COMPREPLY=($(compgen -W "3-10 1-10 5-9" -- "$cur")); return ;;
        --encrypt) return ;;
        completions) COMPREPLY=($(compgen -W "bash zsh fish" -- "$cur")); return ;;
    esac
    if [ "$COMP_CWORD" -eq 1 ]; then
        COMPREPLY=($(compgen -W "tx rx help completions --help --version" -- "$cur"))
        return
    fi
    if [[ "$cur" == -* ]]; then
        COMPREPLY=($(compgen -W "--level --encrypt --stream --stream-chars --levels --quiet --help" -- "$cur"))
        return
    fi
    words="negotiate test"
    COMPREPLY=($(compgen -W "$words" -- "$cur") $(compgen -f -- "$cur"))
}
complete -o filenames -F _vocsend vocsend
"#;

const ZSH: &str = r#"#compdef vocsend
# zsh completion for vocsend.  Install with:
#   vocsend completions zsh > "${fpath[1]}/_vocsend"
_vocsend() {
    local -a modes
    modes=('tx:send' 'rx:receive' 'help:show help' 'completions:print a completion script')
    if (( CURRENT == 2 )); then
        _describe 'mode' modes
        return
    fi
    if [[ ${words[2]} == completions ]]; then
        _values 'shell' bash zsh fish
        return
    fi
    _arguments -s \
        '--level[speed, 1 slowest to 10 fastest, or last]:level:(1 2 3 4 5 6 7 8 9 10 last)' \
        '--encrypt[encrypt with a shared key]::key:' \
        '--stream[send and print a line at a time]' \
        '--stream-chars[send whatever is typed after a short pause]' \
        '--levels[levels to try in test mode]:range:(3-10 1-10 5-9)' \
        '(-q --quiet)'{-q,--quiet}'[print nothing unless something goes wrong]' \
        '(-h --help)'{-h,--help}'[show help]' \
        '*:word or file:->rest'
    if [[ $state == rest ]]; then
        _alternative 'words:word:(negotiate test)' 'files:file:_files'
    fi
}
_vocsend "$@"
"#;

const FISH: &str = r#"# fish completion for vocsend.  Install with:
#   vocsend completions fish > ~/.config/fish/completions/vocsend.fish
complete -c vocsend -n __fish_use_subcommand -a tx -d 'send'
complete -c vocsend -n __fish_use_subcommand -a rx -d 'receive'
complete -c vocsend -n __fish_use_subcommand -a help -d 'show help'
complete -c vocsend -n __fish_use_subcommand -a completions -d 'print a completion script'
complete -c vocsend -n '__fish_seen_subcommand_from tx rx' -a negotiate -d 'agree a speed and acknowledge each block'
complete -c vocsend -n '__fish_seen_subcommand_from tx rx' -a test -d 'find out which speeds work'
complete -c vocsend -n '__fish_seen_subcommand_from completions' -a 'bash zsh fish'
complete -c vocsend -l level -x -a '1 2 3 4 5 6 7 8 9 10 last' -d 'speed, 1 slowest to 10 fastest'
complete -c vocsend -l encrypt -d 'encrypt with a shared key'
complete -c vocsend -l stream -d 'send and print a line at a time'
complete -c vocsend -l stream-chars -d 'send whatever is typed after a short pause'
complete -c vocsend -l levels -x -d 'levels to try in test mode'
complete -c vocsend -s q -l quiet -d 'print nothing unless something goes wrong'
complete -c vocsend -s h -l help -d 'show help'
"#;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = args.first().map(String::as_str) else {
        println!("{}\n\nSend:      vocsend tx FILE\nReceive:   vocsend rx\nMore:      vocsend help", banner());
        return;
    };
    match mode {
        "help" | "--help" | "-h" => return println!("{}", help()),
        "--version" | "-V" | "version" => return println!("{}", banner()),
        "completions" => {
            return match args.get(1).map(String::as_str) {
                Some("bash") => print!("{BASH}"),
                Some("zsh") => print!("{ZSH}"),
                Some("fish") => print!("{FISH}"),
                _ => usage("say which shell: vocsend completions bash, zsh or fish"),
            }
        }
        "tx" | "rx" => {}
        other => usage(&format!("unknown mode `{other}`: use tx or rx")),
    }

    let mut negotiate = false;
    let mut test = false;
    let mut path: Option<String> = None;
    let mut level = None;
    let mut key: Option<String> = None;
    let mut encrypt = false;
    let mut stream = Stream::Off;
    let mut levels = (3, 10);
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        match arg {
            "negotiate" if !negotiate => negotiate = true,
            "test" if !test && path.is_none() => test = true,
            "-h" | "--help" => return println!("{}", help()),
            "-q" | "--quiet" => ui::set_quiet(true),
            "--stream" => stream = Stream::Lines,
            "--stream-chars" => stream = Stream::Chars,
            "--level" => {
                let value = args.get(i).unwrap_or_else(|| usage("--level needs a number from 1 to 10, or `last`"));
                i += 1;
                level = Some(match value.as_str() {
                    "last" => 0,
                    v => v.parse::<usize>().ok().filter(|l| (1..=10).contains(l)).unwrap_or_else(|| usage("--level needs a number from 1 to 10, or `last`")),
                });
            }
            "--levels" => {
                let value = args.get(i).unwrap_or_else(|| usage("--levels needs a range such as 3-10"));
                i += 1;
                let (lo, hi) = value.split_once('-').unwrap_or((value, value));
                levels = match (lo.parse::<usize>(), hi.parse::<usize>()) {
                    (Ok(lo), Ok(hi)) if 1 <= lo && lo <= hi && hi <= 10 => (lo, hi),
                    _ => usage("--levels needs a range within 1-10, such as 3-10"),
                };
            }
            "--encrypt" => {
                encrypt = true;
                // The next word is the key unless it is another option.
                if let Some(next) = args.get(i).filter(|n| !n.starts_with('-')) {
                    key = Some(next.clone());
                    i += 1;
                }
            }
            a if a.starts_with("--encrypt=") => {
                encrypt = true;
                key = Some(a["--encrypt=".len()..].to_string());
            }
            "-" => path = Some("-".to_string()),
            a if a.starts_with('-') => usage(&format!("unknown option `{a}`")),
            a if path.is_none() => path = Some(a.to_string()),
            a => usage(&format!("unexpected `{a}`: give at most one file")),
        }
    }
    if stream != Stream::Off && path.as_deref().is_some_and(|p| p != "-") {
        usage("--stream works on standard input and output only: leave the file out");
    }
    if test && (negotiate || stream != Stream::Off || encrypt) {
        usage("`test` takes no other mode: use `vocsend tx test` and `vocsend rx test`");
    }
    let cipher = encrypt.then(|| {
        let key = key.unwrap_or_else(ask_key);
        if key.is_empty() {
            usage("the key is empty");
        }
        Cipher::new(&key).unwrap_or_else(|e| ui::die(&format!("no secure random numbers: {e}")))
    });
    let opts = Options { negotiate, path, level, cipher, stream, levels };
    let code = match (mode, test) {
        ("tx", true) => run::tx_test(opts),
        ("rx", true) => run::rx_test(opts),
        ("tx", false) => run::tx(opts),
        _ => run::rx(opts),
    };
    if code != 0 && ui::quiet() {
        ui::error("transfer failed");
    }
    std::process::exit(code);
}
