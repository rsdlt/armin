//! The resident process: keeps the history indexed in memory and answers each
//! keystroke over a Unix socket, so typing never starts a process or reads the
//! whole log.
//!
//! The log stays the only record. Shells still append to it with `armin
//! record`; before answering, the daemon reads whatever lines were added since
//! it last looked. One daemon serves each data directory: a lock file keeps a
//! second one from starting, and the socket sits next to the log.
//!
//! A request is fields ending in NUL: the client's version, a command, and the
//! command's arguments. The reply starts with `ok`, NUL, and for `suggest` the
//! suggestions follow in the same form `armin suggest` prints. A client newer
//! than the daemon means armin was upgraded: the daemon answers `restart` and
//! exits, and the shell starts the new one.

use std::fs::{self, File, TryLockError};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::history;
use crate::rank::{self, Index};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// A request bigger than this is not from armin's shell side.
const MAX_REQUEST: usize = 64 * 1024;

/// Where the daemon for `data_dir` listens: next to the log, unless that path
/// is too long for a socket address (104 bytes on macOS). Then it's a private
/// directory in the temp dir, which must belong to whoever owns the data, so
/// no one else can plant a socket there to read what you type.
pub fn socket_path(data_dir: &Path) -> Result<PathBuf> {
    let direct = data_dir.join("daemon.sock");
    if direct.as_os_str().len() < 100 {
        return Ok(direct);
    }
    let owner = fs::metadata(data_dir)
        .with_context(|| format!("reading {}", data_dir.display()))?
        .uid();
    let dir = std::env::temp_dir().join(format!("armin-{owner}"));
    history::ensure_dir(&dir)?;
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        bail!("{} isn't a private directory of yours", dir.display());
    }
    let mut hasher = DefaultHasher::new();
    data_dir.hash(&mut hasher);
    Ok(dir.join(format!("{:016x}.sock", hasher.finish())))
}

/// Serves until a newer armin asks it to make way, or `armin daemon stop`.
/// Returns at once when another daemon already serves this directory.
pub fn run(data_dir: &Path) -> Result<()> {
    history::ensure_dir(data_dir)?;
    history::make_private(data_dir)?;
    let lock_path = data_dir.join("daemon.lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    // A daemon making way for this one needs a moment to let go.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", lock_path.display()));
            }
        }
    }

    let sock = socket_path(data_dir)?;
    // Holding the lock, any socket here is left from a daemon that died.
    match fs::remove_file(&sock) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing {}", sock.display()));
        }
        _ => {}
    }
    let state = Arc::new(Mutex::new(State::load(data_dir.join("history"))?));
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("listening on {}", sock.display()))?;
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))?;
    // Don't keep whatever directory it started in busy.
    let _ = std::env::set_current_dir("/");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let sock = sock.clone();
        thread::spawn(move || {
            if let Ok(Flow::Exit) = serve(stream, &state) {
                let _ = fs::remove_file(&sock);
                std::process::exit(0);
            }
        });
    }
    Ok(())
}

/// Asks the daemon serving `data_dir` to exit. Returns whether one was running.
pub fn stop(data_dir: &Path) -> Result<bool> {
    let Ok(mut stream) = UnixStream::connect(socket_path(data_dir)?) else {
        return Ok(false);
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(format!("{VERSION}\0stop\0").as_bytes())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    if reply != b"ok\0" {
        bail!("the daemon didn't stop");
    }
    Ok(true)
}

/// The index, and how far into the log it has read.
struct State {
    log: PathBuf,
    index: Index,
    offset: u64,
    inode: u64,
}

impl State {
    fn load(log: PathBuf) -> Result<State> {
        let mut state = State {
            log,
            index: Index::default(),
            offset: 0,
            inode: 0,
        };
        state.refresh()?;
        Ok(state)
    }

    /// Takes in the lines added since the last look. A log that shrank or was
    /// replaced, say by an editor, is read again from the start.
    fn refresh(&mut self) -> Result<()> {
        let (len, inode) = match fs::metadata(&self.log) {
            Ok(meta) => (meta.len(), meta.ino()),
            Err(_) => (0, 0),
        };
        if inode != self.inode || len < self.offset {
            self.index = Index::default();
            self.offset = 0;
            self.inode = inode;
        }
        if len > self.offset {
            let (events, offset) = history::read_from(&self.log, self.offset)?;
            for e in &events {
                self.index.add(e);
            }
            self.offset = offset;
        }
        Ok(())
    }
}

enum Flow {
    Continue,
    Exit,
}

fn serve(mut stream: UnixStream, state: &Mutex<State>) -> Result<Flow> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let fields = read_request(&mut stream)?;
    let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
    let Some((client, request)) = fields.split_first() else {
        return Ok(Flow::Continue);
    };
    if newer(client, VERSION) {
        stream.write_all(b"restart\0")?;
        return Ok(Flow::Exit);
    }
    match request {
        ["ping"] => stream.write_all(b"ok\0")?,
        ["stop"] => {
            stream.write_all(b"ok\0")?;
            return Ok(Flow::Exit);
        }
        ["suggest", limit, cwd, typed] => {
            let limit = limit.parse().unwrap_or(8);
            let mut out = String::from("ok\0");
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.refresh()?;
                let found = state.index.suggest(typed, cwd, history::now(), limit);
                rank::encode(&found, &mut out);
            }
            stream.write_all(out.as_bytes())?;
        }
        _ => stream.write_all(b"error\0")?,
    }
    Ok(Flow::Continue)
}

/// How many arguments follow each command.
fn arity(command: &str) -> usize {
    match command {
        "suggest" => 3,
        _ => 0,
    }
}

/// Reads NUL-ended fields until the request is whole.
fn read_request(stream: &mut UnixStream) -> Result<Vec<String>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("client left mid-request");
        }
        buf.extend_from_slice(&chunk[..n]);
        let fields: Vec<&[u8]> = buf.split(|&b| b == 0).collect();
        // The piece after the last NUL is unfinished.
        let done = &fields[..fields.len() - 1];
        if done.len() >= 2 && done.len() >= 2 + arity(&String::from_utf8_lossy(done[1])) {
            return Ok(done
                .iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect());
        }
        if buf.len() > MAX_REQUEST {
            bail!("request too big");
        }
    }
}

/// Whether version `a` is later than `b`, comparing numbers dot by dot.
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a) > parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn socket_next_to_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("armin");
        history::ensure_dir(&data).unwrap();
        assert_eq!(socket_path(&data).unwrap(), data.join("daemon.sock"));
    }

    #[test]
    fn socket_in_temp_dir_when_the_path_is_long() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("a".repeat(100));
        history::ensure_dir(&data).unwrap();
        let sock = socket_path(&data).unwrap();
        assert!(sock.as_os_str().len() < 104, "{}", sock.display());
        assert!(sock.starts_with(std::env::temp_dir()));
        assert_eq!(socket_path(&data).unwrap(), sock, "the same every time");
        let other = dir.path().join("b".repeat(100));
        history::ensure_dir(&other).unwrap();
        assert_ne!(socket_path(&other).unwrap(), sock);
    }

    #[test]
    fn arguments_per_command() {
        assert_eq!(arity("suggest"), 3);
        assert_eq!(arity("ping"), 0);
    }
}
