//! Model JSON (for keyboard nav + height estimation) and per-file HTML fragments.

use crate::diff::{FileDiff, Kind, has_blob};
use crate::git::Git;
use crate::highlight::{expand_tabs, highlight};
use serde::Serialize;
use std::fmt::Write;
use unicode_width::UnicodeWidthStr;

#[derive(Serialize)]
pub struct MFile<'a> {
    pub path: &'a str,
    pub old_path: Option<&'a str>,
    pub status: char,
    pub add: u32,
    pub del: u32,
    /// Explanation row shown instead of hunks.
    pub note: Option<String>,
    /// Digits in the widest line number (gutter width).
    pub gd: u32,
    pub viewed: &'static str,
    /// Per hunk: display width of each row, tabs expanded.
    pub hunks: Vec<Vec<u32>>,
}

pub fn model_file<'a>(f: &'a FileDiff, viewed: &'static str, tab_width: usize) -> MFile<'a> {
    let max_no = f
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| l.old_no.max(l.new_no))
        .max()
        .unwrap_or(1);
    MFile {
        path: &f.path,
        old_path: f.old_path.as_deref(),
        status: f.status,
        add: f.add,
        del: f.del,
        note: f.note(),
        gd: digits(max_no),
        viewed,
        hunks: f
            .hunks
            .iter()
            .map(|h| {
                h.lines
                    .iter()
                    .map(|l| {
                        UnicodeWidthStr::width(expand_tabs(&l.text, tab_width).as_ref()) as u32
                    })
                    .collect()
            })
            .collect(),
    }
}

fn digits(n: u32) -> u32 {
    n.max(1).ilog10() + 1
}

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// Highlighted lines for one side of a file, if we can get them.
fn side(git: &Git, path: &str, sha: &str, mode: &str, tab_width: usize) -> Option<Vec<String>> {
    if !has_blob(sha, mode) || mode == "120000" {
        return None;
    }
    let bytes = git.blobs(&[sha]).ok()?.pop()??;
    if bytes.contains(&0) {
        return None;
    }
    highlight(path, &String::from_utf8_lossy(&bytes), tab_width)
}

/// Render the body of one file: hunks with highlighted rows.
pub fn fragment(git: &Git, f: &FileDiff, tab_width: usize) -> String {
    let mut out = String::new();
    if let Some(note) = f.note() {
        let _ = write!(out, "<div class=\"note\">{}</div>", escape(&note));
        return out;
    }
    let old_path = f.old_path.as_deref().unwrap_or(&f.path);
    let (old, new) = std::thread::scope(|s| {
        let o = s.spawn(|| side(git, old_path, &f.old_blob, &f.old_mode, tab_width));
        let n = side(git, &f.path, &f.new_blob, &f.new_mode, tab_width);
        (o.join().ok().flatten(), n)
    });
    let pick = |lines: &Option<Vec<String>>, no: u32, text: &str| -> String {
        lines
            .as_ref()
            .and_then(|v| v.get(no as usize - 1))
            .cloned()
            .unwrap_or_else(|| escape(&expand_tabs(text, tab_width)))
    };
    for (hi, h) in f.hunks.iter().enumerate() {
        let _ = write!(
            out,
            "<div class=\"hk\" data-h=\"{hi}\"><div class=\"hh\"><span class=\"hr\">@@ -{},{} +{},{} @@</span> {}</div>",
            h.old_start,
            h.lines.iter().filter(|l| l.kind != Kind::Add).count(),
            h.new_start,
            h.lines.iter().filter(|l| l.kind != Kind::Del).count(),
            escape(&h.header),
        );
        for l in &h.lines {
            let (cls, code) = match l.kind {
                Kind::Ctx => ("", pick(&new, l.new_no, &l.text)),
                Kind::Add => (" a", pick(&new, l.new_no, &l.text)),
                Kind::Del => (" d", pick(&old, l.old_no, &l.text)),
            };
            let num = |n: u32| if n == 0 { String::new() } else { n.to_string() };
            let _ = write!(
                out,
                "<div class=\"l{cls}\"><span class=\"o\">{}</span><span class=\"n\">{}</span><span class=\"c\">{code}</span></div>",
                num(l.old_no),
                num(l.new_no),
            );
        }
        out.push_str("</div>");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn digits() {
        assert_eq!(super::digits(0), 1);
        assert_eq!(super::digits(9), 1);
        assert_eq!(super::digits(10), 2);
        assert_eq!(super::digits(12345), 5);
    }
}
