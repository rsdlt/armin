//! The history log: one line per command run, appended by every shell.
//!
//! A plain text file, so you can read it, grep it and back it up. Each line is
//! tab separated: `time duration_ms exit session cwd command`. Fields armin
//! doesn't know, such as the exit code of an imported command, are `-`. Tabs,
//! newlines and backslashes inside a field are escaped as `\t`, `\n` and `\\`.
//!
//! Every shell appends with a single write to a file opened for appending, so
//! lines from shells finishing at the same moment never interleave.
//!
//! History is private: armin creates its directory readable by you alone, and
//! the log too.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("history"))
}

/// Every event in the log, oldest first. A missing log is an empty history; a
/// line armin can't parse is skipped rather than losing the rest.
pub fn read(path: &Path) -> Result<Vec<Event>> {
    Ok(read_from(path, 0)?.0)
}

/// The events in the lines that start at byte `offset` or later, and the offset
/// to read from next time. Only whole lines count: a line still being written
/// waits for the next read.
pub fn read_from(path: &Path, offset: u64) -> Result<(Vec<Event>, u64)> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_to_end(&mut bytes))
        .with_context(|| format!("reading {}", path.display()))?;
    let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&bytes[..whole]);
    let events = text.lines().filter_map(parse_line).collect();
    Ok((events, offset + whole as u64))
}

/// Creates armin's data directory if needed, readable by you alone.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("creating {}", dir.display())),
    }
}

/// Closes up a data directory and log from armin 0.1.0, which other users
/// could read.
pub fn make_private(dir: &Path) -> Result<()> {
    for (path, mode) in [(dir.to_path_buf(), 0o700), (dir.join("history"), 0o600)] {
        match fs::metadata(&path) {
            Ok(meta) if meta.permissions().mode() & 0o077 != 0 => {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("restricting {}", path.display()))?;
            }
            _ => {}
        }
    }
    Ok(())
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
                ensure_dir(dir)?;
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
        .mode(0o600)
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
    fn private_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("armin");
        append(&data.join("history"), &[event("ls")]).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("history")), 0o600);
    }

    #[test]
    fn closes_up_an_old_open_log() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("armin");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("history"), HEADER).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(data.join("history"), fs::Permissions::from_mode(0o644)).unwrap();
        make_private(&data).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("history")), 0o600);
    }

    #[test]
    fn reads_only_what_was_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        append(&path, &[event("one"), event("two")]).unwrap();
        let (first, offset) = read_from(&path, 0).unwrap();
        assert_eq!(first.len(), 2);
        append(&path, &[event("three")]).unwrap();
        // Half a line, as if a shell were writing it right now.
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"17000")
            .unwrap();
        let (more, offset2) = read_from(&path, offset).unwrap();
        assert_eq!(more, vec![event("three")]);
        assert_eq!(read_from(&path, offset2).unwrap(), (vec![], offset2));
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
