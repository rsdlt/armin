# Changelog

All notable changes to armin are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and armin uses
[Semantic Versioning](https://semver.org/). Its public surface is the keys, the commands, the
`ARMIN_*` variables and the history file format.

## [Unreleased]

### Added

- A daemon keeps your history in memory and answers each keystroke over a socket, so typing
  no longer starts a process. The shell starts it, and it restarts itself after an upgrade;
  `armin daemon stop` stops it.
- `armin bench` replays your history one key at a time and counts the keystrokes armin saves,
  next to zsh-autosuggestions.

### Changed

- `Tab` puts the selected command on the line as you move through the menu, and `Enter` runs
  it: one key fewer per pick. `Esc` puts back what you typed.

### Removed

- `ARMIN_HL_GHOST`: with the selection on the line, there is no grey text after the cursor.

## [0.1.0] - 2026-10-24

### Added

- A menu under the prompt as you type, listing the commands you're most likely after. `Tab`
  and `S-Tab` (or `C-n` and `C-p`) move through it, the selection shows as grey text after the
  cursor, `Enter` puts it on the line without running it, and `Esc` closes the menu. With the
  menu closed, every key does what it did before: `Tab` still completes, and `Esc` still enters
  vi command mode.
- Ranking from your history: commands you run often and lately come first, and runs in the
  current directory count again on top. Failed runs count half, and commands that only ever
  failed with "command not found" never come back. Commands that start with what you typed
  lead; fuzzy matches fill in below, with the matched letters in bold.
- `eval "$(armin init zsh)"` sets armin up, and `armin import` brings in `~/.zsh_history`,
  multi-line commands included. Importing twice changes nothing.
- Every command you run is recorded with its directory, exit code and duration, in the
  background, to a plain text file in `~/.local/share/armin/history`.
- `ARMIN_MAX_ROWS`, `ARMIN_HL_MATCH`, `ARMIN_HL_SELECTED` and `ARMIN_HL_GHOST` set the menu's
  height and colors.

[Unreleased]: https://github.com/rsdlt/armin/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rsdlt/armin/releases/tag/v0.1.0
