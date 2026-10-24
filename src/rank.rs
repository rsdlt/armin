//! Turns the history log and what you've typed into ranked suggestions.
//!
//! Every past run of a command adds to its score, weighted by how long ago it
//! was: a run a week ago counts half, a month ago a fifth. Runs in the current
//! directory count again on their own, so a project's commands lead inside
//! it. Commands that start with what you typed always come first; fuzzy
//! matches fill in below them.

use std::collections::HashMap;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::history::Event;

/// How much a run in the current directory counts on top of the run itself.
const HERE_WEIGHT: f64 = 1.5;
/// Fuzzy matches start at this many typed characters; below it they match
/// nearly everything.
const FUZZY_MIN_CHARS: usize = 2;

#[derive(Debug, PartialEq)]
pub struct Suggestion {
    pub command: String,
    /// Character ranges of the command that match what was typed, end exclusive.
    pub matched: Vec<(usize, usize)>,
}

#[derive(Default)]
struct Stats {
    frecency: f64,
    here: f64,
    runs: u32,
    /// Runs the shell couldn't start: not found (127) or not executable (126).
    broken: u32,
    last: i64,
}

impl Stats {
    fn score(&self) -> f64 {
        (1.0 + self.frecency).ln() + HERE_WEIGHT * (1.0 + self.here).ln()
    }
}

/// Up to `limit` suggestions for `typed`, best first. `now` is Unix seconds.
pub fn suggest(
    events: &[Event],
    typed: &str,
    cwd: &str,
    now: i64,
    limit: usize,
) -> Vec<Suggestion> {
    if typed.is_empty() || limit == 0 {
        return Vec::new();
    }
    let stats = gather(events, cwd, now);

    let typed_chars = typed.chars().count();
    let fuzzy = (typed_chars >= FUZZY_MIN_CHARS).then(|| {
        Atom::new(
            typed,
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        )
    });
    let mut matcher = Matcher::new(Config::DEFAULT);
    let (mut buf, mut indices) = (Vec::new(), Vec::new());

    // (prefix match, score, last run, suggestion)
    let mut ranked: Vec<(bool, f64, i64, Suggestion)> = Vec::new();
    for (command, s) in &stats {
        if *command == typed || s.broken == s.runs {
            continue;
        }
        let base = s.score();
        if command.starts_with(typed) {
            let matched = vec![(0, typed_chars)];
            ranked.push((
                true,
                base,
                s.last,
                Suggestion {
                    command: command.to_string(),
                    matched,
                },
            ));
            continue;
        }
        let Some(atom) = &fuzzy else { continue };
        indices.clear();
        let Some(quality) =
            atom.indices(Utf32Str::new(command, &mut buf), &mut matcher, &mut indices)
        else {
            continue;
        };
        // nucleo scores a tight match of a few characters around 50 to 150.
        let score = base + f64::from(quality) / 100.0;
        let matched = ranges(&mut indices);
        ranked.push((
            false,
            score,
            s.last,
            Suggestion {
                command: command.to_string(),
                matched,
            },
        ));
    }

    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then_with(|| a.3.command.cmp(&b.3.command))
    });
    ranked.into_iter().take(limit).map(|r| r.3).collect()
}

fn gather<'a>(events: &'a [Event], cwd: &str, now: i64) -> HashMap<&'a str, Stats> {
    let mut stats: HashMap<&str, Stats> = HashMap::new();
    for e in events {
        let s = stats.entry(e.command.as_str()).or_default();
        s.runs += 1;
        s.last = s.last.max(e.time);
        let weight = match e.exit {
            None | Some(0) => 1.0,
            Some(126 | 127) => {
                s.broken += 1;
                continue;
            }
            // Stopped with Ctrl-C (130) or a closed pipe (141): it ran fine.
            Some(130 | 141) => 1.0,
            // grep finding nothing exits 1 too, so a failure only halves a run.
            Some(_) => 0.5,
        };
        let age_days = (now - e.time).max(0) as f64 / 86_400.0;
        let w = weight / (1.0 + age_days / 7.0);
        s.frecency += w;
        if e.cwd.as_deref() == Some(cwd) {
            s.here += w;
        }
    }
    stats
}

/// Sorted character indices → ranges of consecutive ones.
fn ranges(indices: &mut [u32]) -> Vec<(usize, usize)> {
    indices.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in indices.iter() {
        let i = i as usize;
        match out.last_mut() {
            Some((_, end)) if *end == i => *end += 1,
            Some((_, end)) if *end > i => {}
            _ => out.push((i, i + 1)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn run(command: &str, days_ago: i64, cwd: &str, exit: Option<i32>) -> Event {
        Event {
            time: NOW - days_ago * DAY,
            duration_ms: None,
            exit,
            session: "s".into(),
            cwd: Some(cwd.into()),
            command: command.into(),
        }
    }

    fn ok(command: &str, days_ago: i64) -> Event {
        run(command, days_ago, "/elsewhere", Some(0))
    }

    fn top(events: &[Event], typed: &str, cwd: &str) -> Vec<String> {
        suggest(events, typed, cwd, NOW, 10)
            .into_iter()
            .map(|s| s.command)
            .collect()
    }

    #[test]
    fn frequent_beats_once() {
        let events = [ok("git status", 3), ok("git status", 2), ok("git stash", 1)];
        assert_eq!(top(&events, "git st", "/x"), ["git status", "git stash"]);
    }

    #[test]
    fn recent_beats_old() {
        let events = [ok("make old", 300), ok("make old", 299), ok("make new", 0)];
        assert_eq!(top(&events, "make", "/x"), ["make new", "make old"]);
    }

    #[test]
    fn current_directory_leads() {
        let events = [
            ok("cargo test --all", 1),
            ok("cargo test --all", 1),
            run("cargo test -p armin", 2, "/code/armin", Some(0)),
        ];
        assert_eq!(
            top(&events, "cargo t", "/code/armin")[0],
            "cargo test -p armin"
        );
        assert_eq!(top(&events, "cargo t", "/other")[0], "cargo test --all");
    }

    #[test]
    fn typos_never_come_back() {
        let events = [run("gti status", 0, "/x", Some(127)), ok("git status", 5)];
        assert_eq!(top(&events, "g", "/x"), ["git status"]);
    }

    #[test]
    fn failures_count_half() {
        let events = [
            run("make deploy", 0, "/x", Some(2)),
            run("make deploy", 0, "/x", Some(2)),
            ok("make debug", 0),
            ok("make debug", 0),
        ];
        assert_eq!(top(&events, "make de", "/y"), ["make debug", "make deploy"]);
    }

    #[test]
    fn prefix_before_fuzzy() {
        let events = [
            ok("git commit", 0),
            ok("git commit", 0),
            ok("gc --prune", 30),
        ];
        assert_eq!(top(&events, "gc", "/x"), ["gc --prune", "git commit"]);
    }

    #[test]
    fn skips_what_is_already_typed() {
        assert_eq!(top(&[ok("ls", 0), ok("ls -la", 0)], "ls", "/x"), ["ls -la"]);
    }

    #[test]
    fn single_character_is_prefix_only() {
        assert_eq!(
            top(&[ok("ls", 0), ok("cargo", 0)], "g", "/x"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn matched_ranges() {
        let events = [ok("git status", 0), ok("cargo test", 0)];
        let got = suggest(&events, "git", "/x", NOW, 10);
        assert_eq!(got[0].matched, [(0, 3)]);
        let got = suggest(&events, "ctst", "/x", NOW, 10);
        assert_eq!(got[0].command, "cargo test");
        assert_eq!(got[0].matched, [(0, 1), (6, 7), (8, 10)]);
    }

    #[test]
    fn ranges_merge_runs() {
        assert_eq!(ranges(&mut [4, 0, 1, 2, 7]), [(0, 3), (4, 5), (7, 8)]);
    }
}
