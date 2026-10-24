mod history;
mod import;
mod rank;

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use history::Event;

const HELP: &str = "\
armin — autocomplete for your shell. It reads the battlefield and knows your next move.

Usage: eval \"$(armin init zsh)\"     in ~/.zshrc
       armin import [file]          bring in your zsh history (default ~/.zsh_history)

As you type, a menu under the prompt lists the commands you're most likely
after: the ones you run often and lately, ahead of the rest, and the ones you
run in this directory ahead of those.

Keys while the menu is open:
  Tab/S-Tab  C-n/C-p  move through the menu
  Enter               put the selected command on the line (Enter again runs it)
  Esc                 close the menu
With the menu closed, keys do what they always did: Tab still completes.

History lives in ~/.local/share/armin/history ($ARMIN_DATA_DIR or
$XDG_DATA_HOME/armin if set), a text file with one command per line.

Called by the shell, not by you:
  armin suggest --cwd <dir> [--limit <n>] -- <typed>
  armin record --time <t> --duration <ms> --exit <code> --session <id> --cwd <dir> -- <command>
";

/// What the command line asks for.
enum Cli {
    Help,
    Version,
    Init,
    Import(Option<PathBuf>),
    Suggest {
        cwd: String,
        limit: usize,
        typed: String,
    },
    Record(Event),
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Cli::Help);
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => Ok(Cli::Help),
        "-V" | "-v" | "--version" => Ok(Cli::Version),
        "init" => match rest {
            [shell] if shell == "zsh" => Ok(Cli::Init),
            [shell] => Err(format!("armin speaks zsh only for now, not '{shell}'")),
            _ => Err("init needs a shell: armin init zsh".into()),
        },
        "import" => match rest {
            [] => Ok(Cli::Import(None)),
            [file] => Ok(Cli::Import(Some(file.into()))),
            _ => Err("import takes at most one file".into()),
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
        other => Err(format!("unknown command '{other}'")),
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
        Ok(Cli::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("armin {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Init) => print_init(),
        Ok(Cli::Import(file)) => import(file),
        Ok(Cli::Suggest { cwd, limit, typed }) => print_suggestions(&cwd, limit, &typed),
        Ok(Cli::Record(event)) => {
            history::log_path().and_then(|log| history::append(&log, &[event]))
        }
        Err(e) => {
            eprintln!("armin: {e} (see --help)");
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
/// PATH works too.
fn print_init() -> Result<()> {
    let exe = std::env::current_exe().context("finding the armin binary")?;
    let quoted = format!("'{}'", exe.to_string_lossy().replace('\'', r"'\''"));
    print!(
        "{}",
        include_str!("init.zsh").replace("__ARMIN_BIN__", &quoted)
    );
    Ok(())
}

/// Ranges and command per suggestion, each ending in a NUL, for the shell.
fn print_suggestions(cwd: &str, limit: usize, typed: &str) -> Result<()> {
    let events = history::read(&history::log_path()?)?;
    let mut out = String::new();
    for s in rank::suggest(&events, typed, cwd, now(), limit) {
        let ranges: Vec<String> = s.matched.iter().map(|(a, b)| format!("{a}-{b}")).collect();
        out.push_str(&ranges.join(","));
        out.push('\t');
        out.push_str(&s.command);
        out.push('\0');
    }
    io::stdout().lock().write_all(out.as_bytes())?;
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
