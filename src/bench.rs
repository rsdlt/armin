//! `armin bench`: replays your own history and counts keystrokes.
//!
//! For each of your latest commands, armin knows only what ran before it, and
//! the command is typed one character at a time. After each character, armin
//! is asked for its menu, as is zsh-autosuggestions' one guess: the latest
//! command starting with what's typed. A command costs the fewest keys either
//! way could finish it in:
//!
//! - armin: the characters typed, and a Tab per row down the menu, which puts
//!   that row on the line. Before the first character, Tab opens the menu of
//!   what usually comes next, so a command there costs a Tab per row alone.
//! - zsh-autosuggestions: the characters typed, and → once its guess is right.
//!
//! Neither beats typing the whole command, and running it takes an Enter
//! either way, so that isn't counted.

use std::time::{Duration, Instant};

use crate::history::Event;
use crate::rank::Index;

pub struct Report {
    /// Commands in the history.
    pub total: usize,
    pub replayed: usize,
    /// Keystrokes to type every replayed command in full.
    pub typed: usize,
    pub autosuggest: usize,
    pub armin: usize,
    /// Commands armin offered on the empty line, at their cheapest.
    pub predicted: usize,
    /// How long each of armin's answers took.
    pub latencies: Vec<Duration>,
}

/// Replays the last `last` commands, with a menu of `rows`.
pub fn run(events: &[Event], last: usize, rows: usize) -> Report {
    let split = events.len().saturating_sub(last);
    let mut index = Index::from_events(&events[..split]);
    let mut report = Report {
        total: events.len(),
        replayed: events.len() - split,
        typed: 0,
        autosuggest: 0,
        armin: 0,
        predicted: 0,
        latencies: Vec::new(),
    };
    for e in &events[split..] {
        let command = e.command.as_str();
        let cwd = e.cwd.as_deref().unwrap_or("");
        // Byte offsets where each prefix ends: after 1 character, 2, ...
        let ends: Vec<usize> = command
            .char_indices()
            .skip(1)
            .map(|(i, _)| i)
            .chain([command.len()])
            .collect();
        let len = ends.len();
        report.typed += len;

        let mut auto = len;
        for (k, &end) in ends.iter().enumerate().map(|(k, end)| (k + 1, end)) {
            if k + 1 >= auto {
                break;
            }
            if index.most_recent(&command[..end]) == Some(command) {
                auto = k + 1;
            }
        }
        report.autosuggest += auto;

        let mut best = len;
        let start = Instant::now();
        let menu = index.predict(index.last_in(&e.session), cwd, e.time, rows);
        report.latencies.push(start.elapsed());
        if let Some(row) = menu.iter().position(|s| s.command == command) {
            best = row + 1;
        }
        let empty_line = best;
        for (k, &end) in ends.iter().enumerate().map(|(k, end)| (k + 1, end)) {
            // Picking costs at least a Tab on top.
            if k + 1 >= best {
                break;
            }
            let start = Instant::now();
            let menu = index.suggest(&command[..end], cwd, e.time, rows);
            report.latencies.push(start.elapsed());
            if let Some(row) = menu.iter().position(|s| s.command == command) {
                best = best.min(k + row + 1);
            }
        }
        report.armin += best;
        if best == empty_line && best < len {
            report.predicted += 1;
        }

        index.add(e);
    }
    report
}

impl Report {
    pub fn render(&self) -> String {
        let saved = |keys: usize| {
            if self.typed == 0 {
                String::new()
            } else {
                format!("{:>5.0}%", 100.0 * (1.0 - keys as f64 / self.typed as f64))
            }
        };
        let mut out = format!(
            "armin bench: replayed your last {} commands (of {}), typing each one key at a time.\n\n",
            thousands(self.replayed),
            thousands(self.total),
        );
        out.push_str("                       keystrokes   saved\n");
        out.push_str(&format!(
            "  type it all          {:>11}\n",
            thousands(self.typed)
        ));
        out.push_str(&format!(
            "  zsh-autosuggestions  {:>11}  {}\n",
            thousands(self.autosuggest),
            saved(self.autosuggest)
        ));
        out.push_str(&format!(
            "  armin                {:>11}  {}\n",
            thousands(self.armin),
            saved(self.armin)
        ));
        if self.replayed > 0 {
            out.push_str(&format!(
                "\nPer command: {:.1} keys typed in full, {:.1} with zsh-autosuggestions, {:.1} with armin.\n",
                self.typed as f64 / self.replayed as f64,
                self.autosuggest as f64 / self.replayed as f64,
                self.armin as f64 / self.replayed as f64,
            ));
        }
        if self.replayed > 0 {
            out.push_str(&format!(
                "armin had {} of them ({:.0}%) on an empty line, before the first key.\n",
                thousands(self.predicted),
                100.0 * self.predicted as f64 / self.replayed as f64,
            ));
        }
        out.push_str(
            "armin counts the Tabs to pick from its menu; zsh-autosuggestions, the → to accept.\n",
        );
        if !self.latencies.is_empty() {
            let mut sorted = self.latencies.clone();
            sorted.sort_unstable();
            let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
            out.push_str(&format!(
                "Ranking took {} per keystroke (median), {} at the 99th percentile.\n",
                millis(at(0.5)),
                millis(at(0.99)),
            ));
        }
        out
    }
}

fn millis(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(command: &str, time: i64) -> Event {
        Event {
            time,
            duration_ms: None,
            exit: Some(0),
            session: "s".into(),
            cwd: Some("/x".into()),
            command: command.into(),
        }
    }

    #[test]
    fn counts_keys_both_ways() {
        // Before the last run: "git status" twice, then "git stash" once, most
        // recently. Replaying "git status":
        // - zsh-autosuggestions guesses "git stash" until "git stat", 8 keys
        //   in, then → makes 9.
        // - armin has "git status" on top of the empty line's menu: 1 Tab.
        let events = [
            ok("git status", 1),
            ok("git status", 2),
            ok("git stash", 3),
            ok("git status", 4),
        ];
        let report = run(&events, 1, 8);
        assert_eq!((report.total, report.replayed), (4, 1));
        assert_eq!(report.typed, 10);
        assert_eq!(report.autosuggest, 9);
        assert_eq!(report.armin, 1);
        assert_eq!(report.predicted, 1);
    }

    #[test]
    fn a_new_command_is_typed_in_full() {
        let report = run(&[ok("ls", 1), ok("make", 2)], 1, 8);
        assert_eq!((report.typed, report.autosuggest, report.armin), (4, 4, 4));
    }

    #[test]
    fn lower_rows_cost_tabs() {
        // "make lint" is second on the empty line: 2 Tabs.
        let events = [
            ok("make test", 1),
            ok("make test", 2),
            ok("make lint", 3),
            ok("make lint", 4),
        ];
        let report = run(&events, 1, 8);
        assert_eq!(report.armin, 2);
    }

    #[test]
    fn typing_can_beat_predicting() {
        // "git push" is last of five on the empty line, 5 Tabs, but on top
        // after "g": 1 + Tab = 2.
        let mut events = Vec::new();
        for (t, command) in ["ls", "make", "cd ..", "vim"].iter().enumerate() {
            for _ in 0..3 {
                events.push(ok(command, t as i64));
            }
        }
        events.push(ok("git push", 5));
        events.push(ok("git push", 6));
        let report = run(&events, 1, 8);
        assert_eq!((report.armin, report.predicted), (2, 0));
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
