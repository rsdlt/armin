mod bench;
mod daemon;
mod history;
mod import;
mod rank;

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use history::{Event, now};

const HELP: &str = "\
armin — autocomplete for your shell. It reads the battlefield and knows your next move.

Usage: eval \"$(armin init zsh)\"     in ~/.zshrc
       armin import [file]          bring in your zsh history (default ~/.zsh_history)
       armin bench [--last <n>]     replay your last n commands (default 2000) and
                                    count the keystrokes armin saves
       armin daemon stop            stop the daemon; the next keystroke starts it

Options:
  -h, --help     this help; `armin <command> --help` for one command's
  -v, --version  armin's version

As you type, a menu under the prompt lists the commands you're most likely
after: the ones you run often and lately, ahead of the rest, and the ones you
run in this directory ahead of those.

Keys while the menu is open:
  Tab/S-Tab  C-n/C-p  move through the menu
                      (the selected command goes on the line as you move)
  Enter               run the line
  Esc                 put back what you typed and close the menu
On an empty line, Tab opens the menu with what you usually run next.
With the menu closed, keys do what they always did: Tab still completes.

History lives in ~/.local/share/armin/history ($ARMIN_DATA_DIR or
$XDG_DATA_HOME/armin if set), a text file with one command per line, readable
by you alone.

The shell starts a small daemon that keeps your history in memory and answers
each keystroke over a socket next to the log. It restarts itself when armin is
upgraded; `armin daemon stop` stops it, and the next keystroke starts it again.

Called by the shell, not by you:
  armin daemon
  armin suggest --cwd <dir> [--limit <n>] [--after <command>] -- <typed>
  armin record --time <t> --duration <ms> --exit <code> --session <id> --cwd <dir> -- <command>
";

const INIT_HELP: &str = "\
Usage: armin init zsh

Prints the zsh side of armin: the menu, its keys, and the hooks that record
each command you run. Put this in ~/.zshrc, after anything that binds Tab:

  eval \"$(armin init zsh)\"

zsh is the only shell armin speaks for now.
";

const IMPORT_HELP: &str = "\
Usage: armin import [file]

Adds a zsh history file to armin's history, ~/.zsh_history if you name none.
Commands already there are skipped, so importing twice changes nothing.

zsh keeps times only with `setopt extended_history`. Without them, imported
commands count as run one second apart, ending when you import, in no
directory in particular. Runs armin records itself carry both.
";

const BENCH_HELP: &str = "\
Usage: armin bench [--last <n>]

Replays your last n commands (default 2000), typing each one key at a time,
and counts the keystrokes it takes with armin and with zsh-autosuggestions.
For each command, armin knows only what ran before it.

  armin: the characters typed, and a Tab per row down the menu. Before the
         first character, Tab opens what usually comes next, so a command
         found there costs a Tab per row alone.
  zsh-autosuggestions: the characters typed, and → once its guess is right.

Also prints how long each of armin's answers took.
";

const DAEMON_HELP: &str = "\
Usage: armin daemon [stop]

The shell starts the daemon by itself: it keeps your history in memory and
answers each keystroke over a socket next to the history file. It restarts
itself when armin is upgraded.

  armin daemon stop   stop it; the next keystroke starts it again
";

const SUGGEST_HELP: &str = "\
Usage: armin suggest --cwd <dir> [--limit <n>] [--after <command>] -- <typed>

Called by the shell when the daemon isn't running. Prints up to n suggestions
(default 8) for what's typed, each as its matched ranges, a tab, the command
and a NUL. With nothing typed, it predicts what comes after the command that
ran last.
";

const RECORD_HELP: &str = "\
Usage: armin record --time <t> --duration <ms> --exit <code> --session <id> --cwd <dir> -- <command>

Called by the shell after each command: appends the command to armin's
history, with when and where it ran and how it ended.
";

/// The help for `command`, if armin has one by that name.
fn help_for(command: &str) -> Option<&'static str> {
    Some(match command {
        "init" => INIT_HELP,
        "import" => IMPORT_HELP,
        "bench" => BENCH_HELP,
        "daemon" => DAEMON_HELP,
        "suggest" => SUGGEST_HELP,
        "record" => RECORD_HELP,
        _ => return None,
    })
}

fn is_help(arg: &str) -> bool {
    arg == "-h" || arg == "--help"
}

/// What the command line asks for.
enum Cli {
    Help(&'static str),
    Version,
    Init,
    Import(Option<PathBuf>),
    Bench(usize),
    Daemon,
    DaemonStop,
    Suggest {
        cwd: String,
        limit: usize,
        after: Option<String>,
        typed: String,
    },
    Record(Event),
}

/// The command line, or what's wrong with it and where to read more.
fn parse_args(args: &[String]) -> Result<Cli, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Cli::Help(HELP));
    };
    match cmd.as_str() {
        "-h" | "--help" => Ok(Cli::Help(HELP)),
        "help" => match rest {
            [] => Ok(Cli::Help(HELP)),
            [command] => help_for(command)
                .map(Cli::Help)
                .ok_or(format!("no help for '{command}' (see armin --help)")),
            _ => Err("help takes at most one command (see armin --help)".into()),
        },
        "-V" | "-v" | "--version" => Ok(Cli::Version),
        command => {
            let Some(help) = help_for(command) else {
                return Err(format!("unknown command '{command}' (see armin --help)"));
            };
            // -h before a --: what follows it is the shell's text, not a flag.
            if rest.iter().take_while(|a| *a != "--").any(|a| is_help(a)) {
                return Ok(Cli::Help(help));
            }
            parse_command(command, rest).map_err(|e| format!("{e} (see armin {command} --help)"))
        }
    }
}

fn parse_command(cmd: &str, rest: &[String]) -> Result<Cli, String> {
    match cmd {
        "init" => match rest {
            [shell] if shell == "zsh" => Ok(Cli::Init),
            [shell] => Err(format!("armin speaks zsh only for now, not '{shell}'")),
            _ => Err("init needs a shell: armin init zsh".into()),
        },
        "import" => match rest {
            [] => Ok(Cli::Import(None)),
            [flag] if flag.starts_with('-') => Err(format!("unknown option '{flag}'")),
            [file] => Ok(Cli::Import(Some(file.into()))),
            _ => Err("import takes at most one file".into()),
        },
        "bench" => match rest {
            [] => Ok(Cli::Bench(2000)),
            [flag, n] if flag == "--last" => n
                .parse()
                .map(Cli::Bench)
                .map_err(|_| format!("--last wants a number, not '{n}'")),
            _ => Err("bench takes only --last <n>".into()),
        },
        "daemon" => match rest {
            [] => Ok(Cli::Daemon),
            [arg] if arg == "stop" => Ok(Cli::DaemonStop),
            _ => Err("daemon takes nothing, or stop".into()),
        },
        "suggest" => {
            let (flags, typed) = split_command(rest)?;
            let limit = if flags.get("limit").is_some() {
                flags.number("limit")?
            } else {
                8
            };
            Ok(Cli::Suggest {
                cwd: flags.take("cwd")?,
                limit,
                after: flags.get("after").filter(|a| !a.is_empty()).cloned(),
                typed,
            })
        }
        "record" => {
            let (flags, command) = split_command(rest)?;
            Ok(Cli::Record(Event {
                time: flags.number("time")?,
                duration_ms: Some(flags.number("duration")?),
                exit: Some(flags.number("exit")?),
                session: flags.take("session")?,
                cwd: Some(flags.take("cwd")?),
                command,
            }))
        }
        other => unreachable!("no parser for '{other}'"),
    }
}

/// `--name value` flags before a `--`, and the single argument after it.
struct Flags(Vec<(String, String)>);

impl Flags {
    fn get(&self, name: &str) -> Option<&String> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn take(&self, name: &str) -> Result<String, String> {
        self.get(name).cloned().ok_or(format!("missing --{name}"))
    }

    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<T, String> {
        let value = self.take(name)?;
        value
            .parse()
            .map_err(|_| format!("--{name} wants a number, not '{value}'"))
    }
}

fn split_command(args: &[String]) -> Result<(Flags, String), String> {
    let mut flags = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            let rest: Vec<&String> = args.collect();
            return match rest.as_slice() {
                [one] => Ok((Flags(flags), one.to_string())),
                _ => Err("expected exactly one argument after --".into()),
            };
        }
        let name = arg
            .strip_prefix("--")
            .ok_or(format!("unexpected argument '{arg}'"))?;
        let value = args.next().ok_or(format!("'{arg}' needs a value"))?;
        flags.push((name.to_owned(), value.clone()));
    }
    Err("expected -- and then the text".into())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse_args(&args) {
        Ok(Cli::Help(help)) => {
            print!("{help}");
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("armin {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Init) => print_init(),
        Ok(Cli::Import(file)) => import(file),
        Ok(Cli::Bench(last)) => print_bench(last),
        Ok(Cli::Daemon) => history::data_dir().and_then(|dir| daemon::run(&dir)),
        Ok(Cli::DaemonStop) => history::data_dir()
            .and_then(|dir| daemon::stop(&dir))
            .map(|ran| {
                if !ran {
                    println!("armin: no daemon was running");
                }
            }),
        Ok(Cli::Suggest {
            cwd,
            limit,
            after,
            typed,
        }) => print_suggestions(&cwd, limit, after.as_deref(), &typed),
        Ok(Cli::Record(event)) => {
            history::log_path().and_then(|log| history::append(&log, &[event]))
        }
        Err(e) => {
            eprintln!("armin: {e}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("armin: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The zsh script, pointing at this very binary so a build that isn't on
/// PATH works too, and at the daemon's socket.
fn print_init() -> Result<()> {
    let exe = std::env::current_exe().context("finding the armin binary")?;
    let data = history::data_dir()?;
    history::ensure_dir(&data)?;
    let sock = daemon::socket_path(&data)?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let script = include_str!("init.zsh")
        .replace("__ARMIN_BIN__", &quote(&exe.to_string_lossy()))
        .replace("__ARMIN_SOCK__", &quote(&sock.to_string_lossy()))
        .replace("__ARMIN_VERSION__", env!("CARGO_PKG_VERSION"));
    print!("{script}");
    Ok(())
}

/// What the daemon would answer, for when it isn't running.
fn print_suggestions(cwd: &str, limit: usize, after: Option<&str>, typed: &str) -> Result<()> {
    let index = rank::Index::from_events(&history::read(&history::log_path()?)?);
    let found = if typed.is_empty() {
        index.predict(after, cwd, now(), limit)
    } else {
        index.suggest(typed, cwd, now(), limit)
    };
    let mut out = String::new();
    rank::encode(&found, &mut out);
    io::stdout().lock().write_all(out.as_bytes())?;
    Ok(())
}

fn print_bench(last: usize) -> Result<()> {
    let log = history::log_path()?;
    let events = history::read(&log)?;
    if events.len() < 2 {
        bail!(
            "{} has too little history to replay; try armin import",
            log.display()
        );
    }
    print!("{}", bench::run(&events, last, 8).render());
    Ok(())
}

/// Adds a zsh history file to the log, skipping commands already there, so
/// importing twice changes nothing.
fn import(file: Option<PathBuf>) -> Result<()> {
    let file = match file {
        Some(file) => file,
        None => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".zsh_history")
        }
    };
    let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
    let log = history::log_path()?;
    let existing = history::read(&log)?;
    let known: HashSet<(i64, &str)> = existing
        .iter()
        .map(|e| (e.time, e.command.as_str()))
        .collect();
    let fresh: Vec<Event> = import::parse_zsh(&bytes, now())
        .into_iter()
        .filter(|e| !known.contains(&(e.time, e.command.as_str())))
        .collect();
    if fresh.is_empty() && !bytes.is_empty() && existing.is_empty() {
        bail!("found no commands in {}", file.display());
    }
    history::append(&log, &fresh)?;
    println!(
        "armin: imported {} commands from {} into {}",
        fresh.len(),
        file.display(),
        log.display()
    );
    Ok(())
}
