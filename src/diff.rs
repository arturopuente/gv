//! Diff model and parsers for `git diff --raw -z` and `git diff -p`.

use crate::git::{Git, is_null_sha};
use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ctx,
    Add,
    Del,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub kind: Kind,
    pub old_no: u32,
    pub new_no: u32,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: char,
    pub old_mode: String,
    pub new_mode: String,
    pub old_blob: String,
    pub new_blob: String,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    pub add: u32,
    pub del: u32,
}

impl FileDiff {
    pub fn is_submodule(&self) -> bool {
        self.old_mode == "160000" || self.new_mode == "160000"
    }

    /// A one-line explanation shown instead of hunks, when there are none.
    pub fn note(&self) -> Option<String> {
        if !self.hunks.is_empty() {
            return None;
        }
        Some(if self.binary {
            "Binary file not shown".into()
        } else if self.is_submodule() {
            "Submodule changed".into()
        } else if self.old_mode != self.new_mode && self.status == 'M' {
            format!("Mode changed {} → {}", self.old_mode, self.new_mode)
        } else if self.old_path.is_some() {
            "Renamed without changes".into()
        } else if self.status == 'A' || self.status == 'D' {
            "Empty file".into()
        } else {
            "No content changes shown".into()
        })
    }
}

pub struct DiffOpts {
    pub context: u32,
    pub ignore_whitespace: bool,
}

pub fn diff(git: &Git, base: &str, head: &str, opts: &DiffOpts) -> Result<Vec<FileDiff>> {
    let mut common = vec![
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--full-index",
    ];
    if opts.ignore_whitespace {
        common.push("-w");
    }

    let mut raw_args = vec!["diff", "--raw", "-z", "--no-abbrev"];
    raw_args.extend(&common);
    raw_args.extend(["--end-of-options", base, head]);

    let ctx = format!("-U{}", opts.context);
    let mut patch_args = vec![
        "diff",
        "-p",
        "--no-color",
        "--histogram",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        &ctx,
    ];
    patch_args.extend(&common);
    patch_args.extend(["--end-of-options", base, head]);

    let (raw, patch) = std::thread::scope(|s| {
        let p = s.spawn(|| git.run_bytes(&patch_args).map(|b| parse_patch(&b)));
        (
            git.run_bytes(&raw_args),
            p.join().expect("patch thread panicked"),
        )
    });
    let mut files = parse_raw(&raw?)?;
    let sections = patch?;

    // Sections come out in the same order as raw entries, but a raw entry may
    // lack a section (e.g. whitespace-only with -w). Match by blob ids when known.
    let mut it = sections.into_iter().peekable();
    for f in &mut files {
        let Some(s) = it.peek() else { break };
        let matches = match &s.index {
            Some((o, n)) => o == &f.old_blob && n == &f.new_blob,
            None => true,
        };
        if matches {
            let s = it.next().unwrap();
            f.binary = s.binary;
            f.hunks = s.hunks;
            for h in &f.hunks {
                for l in &h.lines {
                    match l.kind {
                        Kind::Add => f.add += 1,
                        Kind::Del => f.del += 1,
                        Kind::Ctx => {}
                    }
                }
            }
        }
    }
    Ok(files)
}

fn parse_raw(out: &[u8]) -> Result<Vec<FileDiff>> {
    let mut fields = out
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut files = Vec::new();
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let meta = meta
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("bad raw diff line: {meta}"))?;
        let parts: Vec<&str> = meta.split(' ').collect();
        if parts.len() < 5 {
            return Err(anyhow!("bad raw diff line: {meta}"));
        }
        let status = parts[4].chars().next().unwrap_or('M');
        let p1 = fields
            .next()
            .ok_or_else(|| anyhow!("raw diff: missing path"))?;
        let (path, old_path) = if status == 'R' || status == 'C' {
            let p2 = fields
                .next()
                .ok_or_else(|| anyhow!("raw diff: missing rename target"))?;
            (p2, Some(p1))
        } else {
            (p1, None)
        };
        files.push(FileDiff {
            path,
            old_path,
            status,
            old_mode: parts[0].into(),
            new_mode: parts[1].into(),
            old_blob: parts[2].into(),
            new_blob: parts[3].into(),
            binary: false,
            hunks: Vec::new(),
            add: 0,
            del: 0,
        });
    }
    Ok(files)
}

struct Section {
    index: Option<(String, String)>,
    binary: bool,
    hunks: Vec<Hunk>,
}

fn parse_patch(out: &[u8]) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    // Remaining old/new line counts of the hunk being read; while either is
    // non-zero, every line is hunk content regardless of how it looks.
    let (mut rem_old, mut rem_new) = (0u32, 0u32);
    let (mut old_no, mut new_no) = (0u32, 0u32);

    for raw in out.split(|&b| b == b'\n') {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if rem_old > 0 || rem_new > 0 {
            let Some(sec) = sections.last_mut() else {
                continue;
            };
            let Some(h) = sec.hunks.last_mut() else {
                continue;
            };
            let (tag, rest) = match raw.split_first() {
                Some((t, r)) => (*t, r),
                None => (b' ', &b""[..]), // some tools strip the space on empty context lines
            };
            let text = String::from_utf8_lossy(rest).into_owned();
            let line = match tag {
                b'+' => {
                    rem_new = rem_new.saturating_sub(1);
                    new_no += 1;
                    Line {
                        kind: Kind::Add,
                        old_no: 0,
                        new_no,
                        text,
                    }
                }
                b'-' => {
                    rem_old = rem_old.saturating_sub(1);
                    old_no += 1;
                    Line {
                        kind: Kind::Del,
                        old_no,
                        new_no: 0,
                        text,
                    }
                }
                b'\\' => continue,
                _ => {
                    rem_old = rem_old.saturating_sub(1);
                    rem_new = rem_new.saturating_sub(1);
                    old_no += 1;
                    new_no += 1;
                    Line {
                        kind: Kind::Ctx,
                        old_no,
                        new_no,
                        text,
                    }
                }
            };
            h.lines.push(line);
            continue;
        }
        if raw.starts_with(b"diff --git ") {
            sections.push(Section {
                index: None,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(sec) = sections.last_mut() else {
            continue;
        };
        if raw.starts_with(b"@@ ") {
            let s = String::from_utf8_lossy(raw);
            if let Some((os, ol, ns, nl, ctx)) = parse_hunk_header(&s) {
                rem_old = ol;
                rem_new = nl;
                old_no = os.saturating_sub(1);
                new_no = ns.saturating_sub(1);
                // For a zero-length side, git reports start as the line *before*.
                if ol == 0 {
                    old_no = os;
                }
                if nl == 0 {
                    new_no = ns;
                }
                sec.hunks.push(Hunk {
                    header: ctx,
                    old_start: os,
                    new_start: ns,
                    lines: Vec::new(),
                });
            }
        } else if let Some(rest) = raw.strip_prefix(b"index ") {
            let s = String::from_utf8_lossy(rest);
            let range = s.split(' ').next().unwrap_or("");
            if let Some((o, n)) = range.split_once("..") {
                sec.index = Some((o.to_string(), n.to_string()));
            }
        } else if raw.starts_with(b"Binary files ") || raw.starts_with(b"GIT binary patch") {
            sec.binary = true;
        }
    }
    sections
}

/// `@@ -a[,b] +c[,d] @@ ctx` → (a, b, c, d, ctx)
fn parse_hunk_header(s: &str) -> Option<(u32, u32, u32, u32, String)> {
    let rest = s.strip_prefix("@@ -")?;
    let (ranges, ctx) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(u32, u32)> {
        match r.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (os, ol) = range(old)?;
    let (ns, nl) = range(new)?;
    Some((os, ol, ns, nl, ctx.trim_start().to_string()))
}

pub fn has_blob(sha: &str, mode: &str) -> bool {
    !is_null_sha(sha) && mode != "160000"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_header() {
        assert_eq!(
            parse_hunk_header("@@ -1,3 +1,4 @@ def x"),
            Some((1, 3, 1, 4, "def x".into()))
        );
        assert_eq!(
            parse_hunk_header("@@ -5 +0,0 @@"),
            Some((5, 1, 0, 0, "".into()))
        );
    }

    #[test]
    fn patch_lines_and_numbers() {
        let p = b"diff --git a/x b/x\nindex 111..222 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@ ctx\n a\n-b\n+B\n c\n\\ No newline at end of file\ndiff --git a/y b/y\nindex 333..444\nBinary files a/y and b/y differ\n";
        let s = parse_patch(p);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].index, Some(("111".into(), "222".into())));
        let l = &s[0].hunks[0].lines;
        assert_eq!(l.len(), 4);
        assert_eq!((l[1].kind, l[1].old_no), (Kind::Del, 2));
        assert_eq!((l[2].kind, l[2].new_no), (Kind::Add, 2));
        assert_eq!((l[3].old_no, l[3].new_no), (3, 3));
        assert!(s[1].binary);
    }

    #[test]
    fn content_that_looks_like_a_header() {
        let p = b"diff --git a/x b/x\nindex 1..2\n@@ -1,2 +1,2 @@\n-diff --git a/q b/q\n+@@ -1 +1 @@\n x\n";
        let s = parse_patch(p);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].hunks.len(), 1);
        assert_eq!(s[0].hunks[0].lines.len(), 3);
    }

    #[test]
    fn raw_with_rename() {
        let r = b":100644 100644 aaa bbb R090\0old.rs\0new.rs\0:100644 000000 ccc 000 D\0gone.rs\0";
        let f = parse_raw(r).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "new.rs");
        assert_eq!(f[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(f[1].status, 'D');
    }
}
