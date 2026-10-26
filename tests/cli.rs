//! Runs the real binary the way the shell does.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn armin(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_armin"))
        .args(args)
        .env("ARMIN_DATA_DIR", data)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn record(data: &Path, cwd: &str, exit: &str, command: &str) {
    let out = armin(
        data,
        &[
            "record",
            "--time",
            "1700000000",
            "--duration",
            "5",
            "--exit",
            exit,
            "--session",
            "test",
            "--cwd",
            cwd,
            "--",
            command,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
}

/// (ranges, command) per suggestion.
fn suggest(data: &Path, cwd: &str, typed: &str) -> Vec<(String, String)> {
    let out = armin(data, &["suggest", "--cwd", cwd, "--", typed]);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out)
        .split_terminator('\0')
        .map(|item| {
            let (ranges, command) = item.split_once('\t').unwrap();
            (ranges.to_owned(), command.to_owned())
        })
        .collect()
}

#[test]
fn records_then_suggests() {
    let data = TempDir::new().unwrap();
    record(data.path(), "/a", "0", "git status");
    record(data.path(), "/a", "0", "git status");
    record(data.path(), "/a", "0", "git stash pop");
    record(data.path(), "/a", "127", "git stauts");
    record(data.path(), "/a", "0", "for f in *; do\n  echo $f\ndone");
    assert_eq!(
        suggest(data.path(), "/a", "git st"),
        [
            ("0-6".into(), "git status".into()),
            ("0-6".into(), "git stash pop".into()),
        ]
    );
    assert_eq!(
        suggest(data.path(), "/a", "for")[0].1,
        "for f in *; do\n  echo $f\ndone"
    );
}

#[test]
fn suggests_nothing_without_history() {
    let data = TempDir::new().unwrap();
    assert_eq!(suggest(data.path(), "/a", "git"), []);
}

#[test]
fn imports_once() {
    let data = TempDir::new().unwrap();
    let file = data.path().join("zsh_history");
    std::fs::write(
        &file,
        ": 1700000000:0;make test\n: 1700000001:0;make lint\n",
    )
    .unwrap();
    let file = file.to_str().unwrap();

    let out = armin(data.path(), &["import", file]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("armin: imported 2 commands"),
        "{}",
        stdout(&out)
    );

    let out = armin(data.path(), &["import", file]);
    assert!(
        stdout(&out).starts_with("armin: imported 0 commands"),
        "{}",
        stdout(&out)
    );

    let found: Vec<String> = suggest(data.path(), "/a", "make")
        .into_iter()
        .map(|s| s.1)
        .collect();
    assert_eq!(found, ["make lint", "make test"]);
}

#[test]
fn init_points_at_this_binary() {
    let data = TempDir::new().unwrap();
    let out = armin(data.path(), &["init", "zsh"]);
    assert!(out.status.success());
    let script = stdout(&out);
    assert!(script.contains(&format!("_armin_bin='{}'", env!("CARGO_BIN_EXE_armin"))));
    assert!(!script.contains("__ARMIN_BIN__"));
}

#[test]
fn bad_arguments() {
    let data = TempDir::new().unwrap();
    let out = armin(data.path(), &["init", "fish"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "armin: armin speaks zsh only for now, not 'fish' (see armin init --help)\n"
    );

    let out = armin(data.path(), &["record", "--time", "soon", "--", "ls"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "armin: --time wants a number, not 'soon' (see armin record --help)\n"
    );
}

#[test]
fn help_and_version() {
    let data = TempDir::new().unwrap();
    for args in [&["-h"][..], &["--help"], &["help"], &[]] {
        let out = armin(data.path(), args);
        assert!(out.status.success());
        assert!(stdout(&out).contains("Usage: eval"), "{args:?}");
    }
    for args in [
        &["bench", "--help"][..],
        &["bench", "--last", "5", "-h"],
        &["help", "bench"],
    ] {
        let out = armin(data.path(), args);
        assert!(out.status.success());
        assert!(stdout(&out).starts_with("Usage: armin bench"), "{args:?}");
    }
    // A file named -h is no file to import.
    let out = armin(data.path(), &["import", "-h"]);
    assert!(stdout(&out).starts_with("Usage: armin import"));
    // After --, --help is what was typed.
    assert!(suggest(data.path(), "/a", "--help").is_empty());

    for flag in ["-v", "-V", "--version"] {
        let out = armin(data.path(), &[flag]);
        assert_eq!(
            stdout(&out),
            format!("armin {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    let out = armin(data.path(), &["frob"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "armin: unknown command 'frob' (see armin --help)\n"
    );
    let out = armin(data.path(), &["import", "--frob"]);
    assert_eq!(
        stderr(&out),
        "armin: unknown option '--frob' (see armin import --help)\n"
    );
}

#[test]
fn predicts_on_an_empty_line() {
    let data = TempDir::new().unwrap();
    for command in ["git add .", "git commit", "ls", "ls", "git add ."] {
        record(data.path(), "/a", "0", command);
    }
    let out = armin(
        data.path(),
        &["suggest", "--cwd", "/a", "--after", "git add .", "--", ""],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let first = stdout(&out).split('\0').next().unwrap().to_owned();
    assert_eq!(first, "\tgit commit");
}
