//! The history log: one line per command run, appended by every shell.
//!
//! A plain text file, so you can read it, grep it and back it up. Each line is
//! tab separated: `time duration_ms exit session cwd command`. Fields armin
//! doesn't know, such as the exit code of an imported command, are `-`. Tabs,
//! newlines and backslashes inside a field are escaped as `\t`, `\n` and `\\`.
//!
//! Every shell appends with a single write to a file opened for appending, so
//! lines from shells finishing at the same moment never interleave.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const HEADER: &str = "# armin history v1\n";

/// One command that ran.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Unix seconds when it started.
    pub time: i64,
    pub duration_ms: Option<u64>,
    pub exit: Option<i32>,
    /// Which shell ran it, so commands can be read back in the order each
    /// shell ran them.
    pub session: String,
    pub cwd: Option<String>,
    pub command: String,
}

/// Where armin keeps its data: `$ARMIN_DATA_DIR`, `$XDG_DATA_HOME/armin`, or
/// `~/.local/share/armin`.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ARMIN_DATA_DIR") {
        return Ok(dir.into());
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("armin"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/armin"))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("history"))
}

/// Every event in the log, oldest first. A missing log is an empty history; a
/// line armin can't parse is skipped rather than losing the rest.
pub fn read(path: &Path) -> Result<Vec<Event>> {
    let text = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let text = String::from_utf8_lossy(&text);
    Ok(text.lines().filter_map(parse_line).collect())
}

/// Appends events, creating the log (and its directory) on first use.
pub fn append(path: &Path, events: &[Event]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut buf = String::new();
    let mut file = match OpenOptions::new().append(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            }
            buf.push_str(HEADER);
            create(path)?
        }
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    for event in events {
        push_line(&mut buf, event);
    }
    file.write_all(buf.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

fn create(path: &Path) -> Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))
}

fn push_line(buf: &mut String, e: &Event) {
    let opt = |v: Option<String>| v.unwrap_or_else(|| "-".into());
    buf.push_str(&e.time.to_string());
    buf.push('\t');
    buf.push_str(&opt(e.duration_ms.map(|d| d.to_string())));
    buf.push('\t');
    buf.push_str(&opt(e.exit.map(|x| x.to_string())));
    buf.push('\t');
    escape_into(buf, &e.session);
    buf.push('\t');
    match &e.cwd {
        Some(cwd) => escape_into(buf, cwd),
        None => buf.push('-'),
    }
    buf.push('\t');
    escape_into(buf, &e.command);
    buf.push('\n');
}

fn parse_line(line: &str) -> Option<Event> {
    if line.starts_with('#') {
        return None;
    }
    let mut fields = line.splitn(6, '\t');
    let time = fields.next()?.parse().ok()?;
    let duration_ms = optional(fields.next()?)?;
    let exit = optional(fields.next()?)?;
    let session = unescape(fields.next()?);
    let cwd = match fields.next()? {
        "-" => None,
        cwd => Some(unescape(cwd)),
    };
    let command = unescape(fields.next()?);
    Some(Event {
        time,
        duration_ms,
        exit,
        session,
        cwd,
        command,
    })
}

/// `-` is None; anything else must parse, or the whole line is bad.
fn optional<T: std::str::FromStr>(field: &str) -> Option<Option<T>> {
    match field {
        "-" => Some(None),
        v => v.parse().ok().map(Some),
    }
}

fn escape_into(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => buf.push_str("\\\\"),
            '\t' => buf.push_str("\\t"),
            '\n' => buf.push_str("\\n"),
            c => buf.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(command: &str) -> Event {
        Event {
            time: 1_700_000_000,
            duration_ms: Some(12),
            exit: Some(0),
            session: "s1".into(),
            cwd: Some("/home/me/code".into()),
            command: command.into(),
        }
    }

    #[test]
    fn round_trips_awkward_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/history");
        let events = vec![
            event("git status"),
            event("printf 'a\\tb'\t# real tab"),
            event("for f in *; do\n  echo $f\ndone"),
            event("echo trailing\\"),
            Event {
                duration_ms: None,
                exit: None,
                cwd: None,
                ..event("imported")
            },
        ];
        append(&path, &events[..2]).unwrap();
        append(&path, &events[2..]).unwrap();
        assert_eq!(read(&path).unwrap(), events);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HEADER));
        assert_eq!(text.lines().count(), 6, "one header and one line per event");
    }

    #[test]
    fn missing_log_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("history")).unwrap(), vec![]);
    }

    #[test]
    fn skips_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        fs::write(
            &path,
            "garbage\n1\tx\t0\ts\t-\tbad duration\n5\t-\t-\ts\t-\tgood\n",
        )
        .unwrap();
        let events = read(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].command, "good");
    }
}
