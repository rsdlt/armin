//! Turns the history log and what you've typed into ranked suggestions.
//!
//! Every past run of a command adds to its score, weighted by how long ago it
//! was: a run a week ago counts half, a month ago a fifth. Runs in the current
//! directory count again on their own, so a project's commands lead inside
//! it. Commands that start with what you typed always come first; fuzzy
//! matches fill in below them.
//!
//! zsh's own completions for a word come in zsh's order; `order` puts first
//! the ones you've used with the same command before.
//!
//! On an empty line there's nothing typed to match, so armin predicts: what
//! followed the previous command in the past counts on top of the score, so
//! after `git add` comes `git commit`.
//!
//! The index keeps each distinct command once, with its runs, so a keystroke
//! only scores the commands that match it.

use std::collections::HashMap;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::history::Event;

/// How much a run in the current directory counts on top of the run itself.
const HERE_WEIGHT: f64 = 1.5;
/// How much a run right after the previous command counts, on an empty line.
const AFTER_WEIGHT: f64 = 5.0;
/// Fuzzy matches start at this many typed characters; below it they match
/// nearly everything.
const FUZZY_MIN_CHARS: usize = 2;
/// The directory of a run armin doesn't know the directory of.
const NO_DIR: u32 = u32::MAX;

/// A match while ranking: prefix match, score, last run, index, matched ranges.
type Ranked = (bool, f64, i64, usize, Vec<(usize, usize)>);

#[derive(Debug, PartialEq)]
pub struct Suggestion {
    pub command: String,
    /// Character ranges of the command that match what was typed, end exclusive.
    pub matched: Vec<(usize, usize)>,
}

/// Every command in the history, once each.
#[derive(Default)]
pub struct Index {
    commands: Vec<Command>,
    by_text: HashMap<String, usize>,
    dirs: HashMap<String, u32>,
    /// Per session, the command that ran last.
    last_in_session: HashMap<String, usize>,
}

struct Command {
    text: String,
    /// The runs that count toward the score.
    runs: Vec<Run>,
    total: u32,
    /// Runs the shell couldn't start: not found (127) or not executable (126).
    broken: u32,
    last: i64,
    /// Runs that came right after another command in the same session.
    after: Vec<After>,
}

struct After {
    previous: usize,
    time: i64,
}

struct Run {
    time: i64,
    weight: f64,
    dir: u32,
}

impl Index {
    pub fn from_events(events: &[Event]) -> Index {
        let mut index = Index::default();
        for e in events {
            index.add(e);
        }
        index
    }

    pub fn add(&mut self, e: &Event) {
        let next = self.commands.len();
        let i = *self.by_text.entry(e.command.clone()).or_insert(next);
        if i == next {
            self.commands.push(Command {
                text: e.command.clone(),
                runs: Vec::new(),
                total: 0,
                broken: 0,
                last: i64::MIN,
                after: Vec::new(),
            });
        }
        let next_dir = self.dirs.len() as u32;
        let dir = match &e.cwd {
            Some(cwd) => *self.dirs.entry(cwd.clone()).or_insert(next_dir),
            None => NO_DIR,
        };
        let c = &mut self.commands[i];
        c.total += 1;
        c.last = c.last.max(e.time);
        let weight = match e.exit {
            None | Some(0) => 1.0,
            Some(126 | 127) => {
                c.broken += 1;
                return;
            }
            // Stopped with Ctrl-C (130) or a closed pipe (141): it ran fine.
            Some(130 | 141) => 1.0,
            // grep finding nothing exits 1 too, so a failure only halves a run.
            Some(_) => 0.5,
        };
        c.runs.push(Run {
            time: e.time,
            weight,
            dir,
        });
        // A typo that never ran isn't what came before the command it was
        // retyped as, so only commands that ran make it here.
        if let Some(&previous) = self.last_in_session.get(&e.session) {
            c.after.push(After {
                previous,
                time: e.time,
            });
        }
        self.last_in_session.insert(e.session.clone(), i);
    }

    /// The command that ran last in `session`.
    pub fn last_in(&self, session: &str) -> Option<&str> {
        self.last_in_session
            .get(session)
            .map(|&i| self.commands[i].text.as_str())
    }

    /// Up to `limit` commands you're likely to run next, best first, for an
    /// empty line: after `previous`, in `cwd`.
    pub fn predict(
        &self,
        previous: Option<&str>,
        cwd: &str,
        now: i64,
        limit: usize,
    ) -> Vec<Suggestion> {
        let here = self.dirs.get(cwd).copied().unwrap_or(NO_DIR);
        let previous = previous.and_then(|p| self.by_text.get(p)).copied();
        let mut ranked: Vec<(f64, i64, usize)> = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.runs.is_empty())
            .map(|(i, c)| {
                let after: f64 = c
                    .after
                    .iter()
                    .filter(|a| Some(a.previous) == previous)
                    .map(|a| decay(now, a.time))
                    .sum();
                let score = c.score(here, now) + AFTER_WEIGHT * (1.0 + after).ln();
                (score, c.last, i)
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then_with(|| self.commands[a.2].text.cmp(&self.commands[b.2].text))
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, i)| Suggestion {
                command: self.commands[i].text.clone(),
                matched: Vec::new(),
            })
            .collect()
    }

    /// Up to `limit` suggestions for `typed`, best first. `now` is Unix seconds.
    pub fn suggest(&self, typed: &str, cwd: &str, now: i64, limit: usize) -> Vec<Suggestion> {
        if typed.is_empty() || limit == 0 {
            return Vec::new();
        }
        let here = self.dirs.get(cwd).copied().unwrap_or(NO_DIR);
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

        let mut ranked: Vec<Ranked> = Vec::new();
        for (i, c) in self.commands.iter().enumerate() {
            if c.text == typed || c.broken == c.total {
                continue;
            }
            if c.text.starts_with(typed) {
                ranked.push((true, c.score(here, now), c.last, i, vec![(0, typed_chars)]));
                continue;
            }
            let Some(atom) = &fuzzy else { continue };
            indices.clear();
            let haystack = Utf32Str::new(&c.text, &mut buf);
            let Some(quality) = atom.indices(haystack, &mut matcher, &mut indices) else {
                continue;
            };
            // nucleo scores a tight match of a few characters around 50 to 150.
            let score = c.score(here, now) + f64::from(quality) / 100.0;
            ranked.push((false, score, c.last, i, ranges(&mut indices)));
        }

        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.total_cmp(&a.1))
                .then(b.2.cmp(&a.2))
                .then_with(|| self.commands[a.3].text.cmp(&self.commands[b.3].text))
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, _, i, matched)| Suggestion {
                command: self.commands[i].text.clone(),
                matched,
            })
            .collect()
    }

    /// The order to show zsh's `candidates` for the word after `line`: the
    /// ones past runs of the same command used first, most used first, then
    /// the rest as zsh gave them. A candidate ending in `/` or `=` counts a
    /// word that starts with it, as `src/` counts `src/main.rs`.
    pub fn order(&self, line: &str, candidates: &[&str], now: i64) -> Vec<usize> {
        // The words before the one being completed.
        let mut context: Vec<&str> = line.split_whitespace().collect();
        if !line.ends_with(char::is_whitespace) {
            context.pop();
        }
        let mut scores = vec![0.0; candidates.len()];
        if let Some((&program, _)) = context.split_first() {
            let mut by_word: HashMap<&str, Vec<usize>> = HashMap::new();
            for (i, c) in candidates.iter().enumerate() {
                by_word.entry(c.trim_end()).or_default().push(i);
            }
            for c in &self.commands {
                let mut words = c.text.split_whitespace();
                if words.next() != Some(program) {
                    continue;
                }
                let mut weight: f64 = c.runs.iter().map(|r| r.weight * decay(now, r.time)).sum();
                // Runs of the very same subcommand say more than the program's.
                if c.text
                    .split_whitespace()
                    .take(context.len())
                    .eq(context.iter().copied())
                {
                    weight *= 2.0;
                }
                let mut used = vec![false; candidates.len()];
                for word in words {
                    let prefixes = word
                        .match_indices(['/', '='])
                        .map(|(at, _)| &word[..=at])
                        .chain([word]);
                    for key in prefixes {
                        for &i in by_word.get(key).into_iter().flatten() {
                            if !used[i] {
                                used[i] = true;
                                scores[i] += weight;
                            }
                        }
                    }
                }
            }
        }
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        // Stable, so unused candidates keep zsh's order.
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        order
    }

    /// The latest command that starts with `typed`, typos and all: what
    /// zsh-autosuggestions would offer.
    pub fn most_recent(&self, typed: &str) -> Option<&str> {
        self.commands
            .iter()
            .filter(|c| c.text.starts_with(typed))
            .max_by_key(|c| c.last)
            .map(|c| c.text.as_str())
    }
}

impl Command {
    fn score(&self, here: u32, now: i64) -> f64 {
        let (mut frecency, mut local) = (0.0, 0.0);
        for run in &self.runs {
            let w = run.weight * decay(now, run.time);
            frecency += w;
            if run.dir == here && here != NO_DIR {
                local += w;
            }
        }
        (1.0f64 + frecency).ln() + HERE_WEIGHT * (1.0f64 + local).ln()
    }
}

/// How much a run at `time` still counts: a week ago half, a month ago a fifth.
fn decay(now: i64, time: i64) -> f64 {
    let age_days = (now - time).max(0) as f64 / 86_400.0;
    1.0 / (1.0 + age_days / 7.0)
}

/// The form the shell reads: per suggestion, its matched ranges as
/// `start-end,...`, a tab, the command, and a NUL.
pub fn encode(suggestions: &[Suggestion], out: &mut String) {
    for s in suggestions {
        for (n, (a, b)) in s.matched.iter().enumerate() {
            if n > 0 {
                out.push(',');
            }
            out.push_str(&format!("{a}-{b}"));
        }
        out.push('\t');
        out.push_str(&s.command);
        out.push('\0');
    }
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

    fn suggest(
        events: &[Event],
        typed: &str,
        cwd: &str,
        now: i64,
        limit: usize,
    ) -> Vec<Suggestion> {
        Index::from_events(events).suggest(typed, cwd, now, limit)
    }

    fn predict(events: &[Event], previous: Option<&str>) -> Vec<String> {
        Index::from_events(events)
            .predict(previous, "/elsewhere", NOW, 10)
            .into_iter()
            .map(|s| s.command)
            .collect()
    }

    #[test]
    fn predicts_what_came_next() {
        let events = [
            ok("git add .", 3),
            ok("git commit", 3),
            ok("git add .", 2),
            ok("git commit", 2),
            ok("ls", 1),
            ok("ls", 1),
            ok("ls", 1),
        ];
        assert_eq!(predict(&events, Some("git add ."))[0], "git commit");
        // With nothing before it, the most run command leads.
        assert_eq!(predict(&events, None)[0], "ls");
        assert_eq!(predict(&events, Some("never ran"))[0], "ls");
    }

    #[test]
    fn a_typo_comes_before_nothing() {
        let events = [
            ok("make", 3),
            run("mkae test", 3, "/elsewhere", Some(127)),
            ok("make test", 3),
        ];
        let index = Index::from_events(&events);
        assert_eq!(index.last_in("s"), Some("make test"));
        assert!(!predict(&events, None).contains(&"mkae test".to_string()));
        assert_eq!(predict(&events, Some("make"))[0], "make test");
        assert!(predict(&events, Some("mkae test")).len() == 2);
    }

    #[test]
    fn sessions_keep_their_own_order() {
        let mut other = ok("cargo test", 1);
        other.session = "t".into();
        let events = [ok("vim notes", 1), other, ok("git diff", 1)];
        assert_eq!(predict(&events, Some("vim notes"))[0], "git diff");
        assert_eq!(Index::from_events(&events).last_in("t"), Some("cargo test"));
    }

    #[test]
    fn orders_completions_by_use() {
        let events = [
            ok("git switch main", 1),
            ok("git switch fix-two", 1),
            ok("git switch fix-two", 2),
            ok("git log fix-one", 1),
            ok("cargo test src/main.rs", 1),
        ];
        let index = Index::from_events(&events);
        let branches = ["feature", "fix-one ", "fix-two ", "main "];
        // The same subcommand counts double: fix-two, main, then fix-one
        // from git log, then the unused one.
        assert_eq!(index.order("git switch f", &branches, NOW), [2, 3, 1, 0]);
        assert_eq!(index.order("git switch ", &branches, NOW), [2, 3, 1, 0]);
        // A folder counts the files used in it.
        assert_eq!(index.order("cargo test s", &["docs/", "src/"], NOW), [1, 0]);
        // Another program's runs don't count, and nothing typed keeps
        // zsh's order.
        assert_eq!(index.order("tig ", &branches, NOW), [0, 1, 2, 3]);
        assert_eq!(index.order("git", &branches, NOW), [0, 1, 2, 3]);
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
    fn grows_one_command_at_a_time() {
        let mut index = Index::from_events(&[ok("make test", 2)]);
        index.add(&ok("make lint", 0));
        index.add(&ok("make lint", 0));
        assert_eq!(index.suggest("make", "/x", NOW, 10)[0].command, "make lint");
    }

    #[test]
    fn most_recent_is_the_autosuggestions_guess() {
        let events = [
            ok("git status", 1),
            ok("git stash", 3),
            run("git stauts", 0, "/x", Some(127)),
        ];
        let index = Index::from_events(&events);
        assert_eq!(index.most_recent("git sta"), Some("git stauts"));
        assert_eq!(index.most_recent("git stas"), Some("git stash"));
        assert_eq!(index.most_recent("make"), None);
    }

    #[test]
    fn ranges_merge_runs() {
        assert_eq!(ranges(&mut [4, 0, 1, 2, 7]), [(0, 3), (4, 5), (7, 8)]);
    }
}
