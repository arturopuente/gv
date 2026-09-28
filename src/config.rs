//! Global (`~/.config/gv/config.toml`) and per-repo (`.gv.toml`) configuration.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub collapse_over_lines: usize,
    pub context: u32,
    pub ignore_whitespace: bool,
    pub tab_width: usize,
    /// Only ever read from the global config (see PRD §7.5).
    #[allow(dead_code)]
    pub editor_command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            collapse_over_lines: 2000,
            context: 3,
            ignore_whitespace: false,
            tab_width: 4,
            editor_command: None,
        }
    }
}

#[derive(Deserialize, Default)]
struct File {
    collapse_over_lines: Option<usize>,
    context: Option<u32>,
    ignore_whitespace: Option<bool>,
    tab_width: Option<usize>,
    editor: Option<Editor>,
}

#[derive(Deserialize)]
struct Editor {
    command: Option<String>,
}

pub fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

fn read(path: &Path) -> Result<Option<File>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn load(repo_root: &Path) -> Result<Config> {
    let mut c = Config::default();
    let global = xdg("XDG_CONFIG_HOME", ".config").join("gv/config.toml");
    if let Some(f) = read(&global)? {
        c.editor_command = f.editor.as_ref().and_then(|e| e.command.clone());
        apply(&mut c, f);
    }
    if let Some(f) = read(&repo_root.join(".gv.toml"))? {
        if f.editor.as_ref().is_some_and(|e| e.command.is_some()) {
            eprintln!(
                "gv: ignoring [editor] command in .gv.toml; set it in {} instead",
                global.display()
            );
        }
        apply(&mut c, f);
    }
    c.tab_width = c.tab_width.clamp(1, 16);
    Ok(c)
}

fn apply(c: &mut Config, f: File) {
    if let Some(v) = f.collapse_over_lines {
        c.collapse_over_lines = v;
    }
    if let Some(v) = f.context {
        c.context = v;
    }
    if let Some(v) = f.ignore_whitespace {
        c.ignore_whitespace = v;
    }
    if let Some(v) = f.tab_width {
        c.tab_width = v;
    }
}
