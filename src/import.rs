//! Reads a zsh history file into events.
//!
//! zsh writes `: <start>:<duration>;<command>` with EXTENDED_HISTORY set, and
//! the bare command without it. A command spanning lines has a backslash
//! before each newline. Bytes zsh treats as special are "metafied": 0x83,
//! then the byte xor 0x20, which is how non-ASCII text reaches the file.

use crate::history::Event;

/// The session imported commands belong to.
pub const SESSION: &str = "zsh-import";

/// Parses the file's bytes. Lines without a timestamp get one second apart,
/// ending at `fallback_end`, so their order survives.
pub fn parse_zsh(bytes: &[u8], fallback_end: i64) -> Vec<Event> {
    let text = String::from_utf8_lossy(&unmetafy(bytes)).into_owned();
    let mut entries: Vec<(Option<(i64, u64)>, String)> = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.split('\n') {
        let line = match pending.take() {
            Some(mut prev) => {
                prev.push('\n');
                prev.push_str(line);
                prev
            }
            None => line.to_owned(),
        };
        if let Some(head) = line.strip_suffix('\\') {
            pending = Some(head.to_owned());
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        entries.push(split_extended(&line));
    }
    if let Some(rest) = pending.filter(|r| !r.trim().is_empty()) {
        entries.push(split_extended(&rest));
    }

    let untimed = entries.iter().filter(|(t, _)| t.is_none()).count() as i64;
    let mut next_untimed = fallback_end - untimed;
    entries
        .into_iter()
        .map(|(timing, command)| {
            let (time, duration) = timing.unwrap_or_else(|| {
                next_untimed += 1;
                (next_untimed, 0)
            });
            Event {
                time,
                // zsh rounds to whole seconds and writes 0 for anything quick,
                // so its duration only means something above zero.
                duration_ms: (duration > 0).then_some(duration * 1000),
                exit: None,
                session: SESSION.into(),
                cwd: None,
                command,
            }
        })
        .collect()
}

/// `: 1700000000:3;git status` → (time, duration) and the command.
fn split_extended(line: &str) -> (Option<(i64, u64)>, String) {
    let parsed = line.strip_prefix(": ").and_then(|rest| {
        let (meta, command) = rest.split_once(';')?;
        let (time, duration) = meta.split_once(':')?;
        Some(((time.parse().ok()?, duration.parse().ok()?), command))
    });
    match parsed {
        Some((timing, command)) => (Some(timing), command.to_owned()),
        None => (None, line.to_owned()),
    }
}

fn unmetafy(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut bytes = bytes.iter();
    while let Some(&b) = bytes.next() {
        if b == 0x83 {
            if let Some(&next) = bytes.next() {
                out.push(next ^ 0x20);
            }
        } else {
            out.push(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.command.as_str()).collect()
    }

    #[test]
    fn extended_history() {
        let events = parse_zsh(
            b": 1700000000:0;git status\n: 1700000005:3;cargo build\n",
            0,
        );
        assert_eq!(commands(&events), ["git status", "cargo build"]);
        assert_eq!(events[0].time, 1_700_000_000);
        assert_eq!(events[0].duration_ms, None);
        assert_eq!(events[1].duration_ms, Some(3000));
        assert_eq!(events[1].session, SESSION);
    }

    #[test]
    fn multi_line_commands() {
        let file = b": 1:0;for f in *; do\\\n  echo $f\\\ndone\n: 2:0;ls\n";
        assert_eq!(
            commands(&parse_zsh(file, 0)),
            ["for f in *; do\n  echo $f\ndone", "ls"]
        );
    }

    #[test]
    fn plain_history_keeps_its_order() {
        let events = parse_zsh(b"ls\n\ncd src\nmake\n", 100);
        assert_eq!(commands(&events), ["ls", "cd src", "make"]);
        let times: Vec<i64> = events.iter().map(|e| e.time).collect();
        assert_eq!(times, [98, 99, 100]);
    }

    #[test]
    fn metafied_bytes() {
        // → is 0xE2 0x86 0x92; zsh metafies both of the last two.
        let file = [
            b": 1:0;echo a".as_slice(),
            &[0xE2, 0x83, 0xA6, 0x83, 0xB2],
            b"b\n",
        ]
        .concat();
        assert_eq!(commands(&parse_zsh(&file, 0)), ["echo a→b"]);
    }

    #[test]
    fn not_quite_extended_is_a_plain_command() {
        let events = parse_zsh(b": not a timestamp;x\n", 7);
        assert_eq!(commands(&events), [": not a timestamp;x"]);
        assert_eq!(events[0].time, 7);
    }
}
