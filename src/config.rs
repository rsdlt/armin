//! `~/.config/armin/config.toml`: the menu's size and colors.
//!
//! armin reads it once, at `armin init`, and hands the shell what it says as
//! the `ARMIN_*` variables, so a keystroke never reads a file. A variable set
//! before the `eval` line still wins over the file. A theme sets every color
//! at once; `[colors]` changes any of them.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// What `armin config` prints: every setting, at its default.
pub const SAMPLE: &str = r#"# armin's settings. Each one is optional; delete what you don't change.
# A variable set before `eval "$(armin init zsh)"` wins over this file.

# The colors, at once: "default", "survey-corps" or "colossal".
theme = "default"

[menu]
rows = 8              # how many suggestions the menu shows
panel = true          # the panel beside the menu, about the selected row
# prompt_width = 2    # your prompt's last line, when armin can't measure it

# Each color in zsh's highlight syntax, like "fg=blue,bold" or "fg=108".
# These replace the theme's.
[colors]
# match = "bold"            # the letters that match what you typed
# selected = "standout"     # the selected row
# description = "fg=8"      # a completion's description
# panel = "fg=8"            # the line between the menu and the panel
"#;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    theme: Option<String>,
    #[serde(default)]
    menu: Menu,
    #[serde(default)]
    colors: Colors,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Menu {
    rows: Option<u32>,
    panel: Option<bool>,
    prompt_width: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct Colors {
    #[serde(rename = "match")]
    matched: Option<String>,
    selected: Option<String>,
    description: Option<String>,
    panel: Option<String>,
}

/// A theme's colors: match, selected, description, panel.
fn theme(name: &str) -> Option<[&'static str; 4]> {
    Some(match name {
        "default" => ["bold", "standout", "fg=8", "fg=8"],
        // The Survey Corps' cloak and a map's parchment.
        "survey-corps" => ["fg=71,bold", "fg=230,bg=22", "fg=137", "fg=137"],
        // Steam and fire.
        "colossal" => ["fg=208,bold", "fg=232,bg=208", "fg=244", "fg=202"],
        _ => return None,
    })
}

/// Where the file lives: $ARMIN_CONFIG, else $XDG_CONFIG_HOME/armin, else
/// ~/.config/armin.
pub fn path() -> Result<PathBuf> {
    if let Some(file) = std::env::var_os("ARMIN_CONFIG").filter(|f| !f.is_empty()) {
        return Ok(file.into());
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("armin/config.toml"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/armin/config.toml"))
}

/// The zsh lines that set what the file says, each only where the variable
/// isn't set already. No file means no lines.
pub fn zsh_defaults(text: Option<&str>) -> Result<String> {
    let Some(text) = text else {
        return Ok(String::new());
    };
    // toml's own message says where: line, column and the text around it.
    let file: File =
        toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.to_string().trim_end()))?;

    let mut settings: Vec<(&str, String)> = Vec::new();
    if let Some(name) = &file.theme {
        let Some([matched, selected, description, panel]) = theme(name) else {
            bail!("no theme called '{name}': try default, survey-corps or colossal");
        };
        settings.extend([
            ("ARMIN_HL_MATCH", matched.to_owned()),
            ("ARMIN_HL_SELECTED", selected.to_owned()),
            ("ARMIN_HL_DESCRIPTION", description.to_owned()),
            ("ARMIN_HL_PANEL", panel.to_owned()),
        ]);
    }
    let colors = file.colors;
    for (name, value) in [
        ("ARMIN_HL_MATCH", colors.matched),
        ("ARMIN_HL_SELECTED", colors.selected),
        ("ARMIN_HL_DESCRIPTION", colors.description),
        ("ARMIN_HL_PANEL", colors.panel),
    ] {
        if let Some(value) = value {
            settings.retain(|(n, _)| *n != name);
            settings.push((name, value));
        }
    }
    if let Some(rows) = file.menu.rows {
        if !(1..=50).contains(&rows) {
            bail!("menu.rows is {rows}; it goes from 1 to 50");
        }
        settings.push(("ARMIN_MAX_ROWS", rows.to_string()));
    }
    if let Some(panel) = file.menu.panel {
        settings.push(("ARMIN_PANEL", u8::from(panel).to_string()));
    }
    if let Some(width) = file.menu.prompt_width {
        settings.push(("ARMIN_PROMPT_WIDTH", width.to_string()));
    }

    let mut out = String::new();
    for (name, value) in settings {
        out.push_str(&format!(
            "typeset -g {name}=\"${{{name}:-{}}}\"\n",
            quote(&value)
        ));
    }
    Ok(out)
}

/// `value` inside zsh's double quotes, inside a ${name:-...}.
fn quote(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '$' | '`' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_sets_nothing() {
        assert_eq!(zsh_defaults(None).unwrap(), "");
        assert_eq!(zsh_defaults(Some("")).unwrap(), "");
    }

    #[test]
    fn the_sample_is_the_defaults() {
        let out = zsh_defaults(Some(SAMPLE)).unwrap();
        assert!(out.contains("typeset -g ARMIN_HL_MATCH=\"${ARMIN_HL_MATCH:-bold}\"\n"));
        assert!(out.contains("typeset -g ARMIN_MAX_ROWS=\"${ARMIN_MAX_ROWS:-8}\"\n"));
        assert!(out.contains("typeset -g ARMIN_PANEL=\"${ARMIN_PANEL:-1}\"\n"));
        assert!(!out.contains("PROMPT_WIDTH"));
    }

    #[test]
    fn colors_replace_the_themes() {
        let out = zsh_defaults(Some(
            "theme = \"survey-corps\"\n[colors]\nselected = \"fg=black,bg=green\"\n[menu]\nrows = 5\npanel = false\nprompt_width = 3\n",
        ))
        .unwrap();
        assert_eq!(
            out,
            "typeset -g ARMIN_HL_MATCH=\"${ARMIN_HL_MATCH:-fg=71,bold}\"\n\
             typeset -g ARMIN_HL_DESCRIPTION=\"${ARMIN_HL_DESCRIPTION:-fg=137}\"\n\
             typeset -g ARMIN_HL_PANEL=\"${ARMIN_HL_PANEL:-fg=137}\"\n\
             typeset -g ARMIN_HL_SELECTED=\"${ARMIN_HL_SELECTED:-fg=black,bg=green}\"\n\
             typeset -g ARMIN_MAX_ROWS=\"${ARMIN_MAX_ROWS:-5}\"\n\
             typeset -g ARMIN_PANEL=\"${ARMIN_PANEL:-0}\"\n\
             typeset -g ARMIN_PROMPT_WIDTH=\"${ARMIN_PROMPT_WIDTH:-3}\"\n"
        );
    }

    #[test]
    fn says_what_is_wrong() {
        let err = |text: &str| zsh_defaults(Some(text)).err().unwrap().to_string();
        assert!(err("theme = \"wall-maria\"").contains("no theme called 'wall-maria'"));
        assert!(err("[menu]\nrows = 0").contains("from 1 to 50"));
        assert!(err("[menu]\nrowz = 3").contains("rowz"));
        let wrong_type = err("[menu]\nrows = \"many\"");
        assert!(wrong_type.contains("line 2"), "{wrong_type}");
        assert!(wrong_type.contains("expected u32"), "{wrong_type}");
    }

    #[test]
    fn values_stay_values() {
        let out = zsh_defaults(Some("[colors]\nmatch = \"$(rm -rf ~)}\\\"`x`\"")).unwrap();
        assert_eq!(
            out,
            "typeset -g ARMIN_HL_MATCH=\"${ARMIN_HL_MATCH:-\\$(rm -rf ~)\\}\\\"\\`x\\`}\"\n"
        );
    }
}
