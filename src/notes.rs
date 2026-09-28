//! Agent notes (`--notes <file>`): the agent's own guide to its diff.
//!
//! Markdown. Everything before the first `## ` heading is the intro (intent,
//! decisions). Each `## path[:a[-b]] [check|mechanical]` section is a note on a
//! file, or on new-file lines a..b of it. `check` asks the reviewer to look
//! closely; `mechanical` says the file is safe to skim.

use serde::Serialize;

/// The `note` a reply to the intro carries (headings are paths, so no clash).
pub const INTRO: &str = "(intro)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tag {
    #[default]
    None,
    Check,
    Mechanical,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    /// The heading as written, for notes that don't match a file.
    pub heading: String,
    pub path: String,
    /// New-file line range; 0 for a note on the whole file.
    pub a: u32,
    pub b: u32,
    pub tag: Tag,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct Notes {
    pub intro: String,
    pub notes: Vec<Note>,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.intro.is_empty() && self.notes.is_empty()
    }

    /// The strongest tag on any note for `path`: check beats mechanical.
    pub fn tag_for(&self, path: &str) -> Tag {
        let mut tag = Tag::None;
        for n in self.notes.iter().filter(|n| n.path == path) {
            match n.tag {
                Tag::Check => return Tag::Check,
                Tag::Mechanical => tag = Tag::Mechanical,
                Tag::None => {}
            }
        }
        tag
    }
}

pub fn parse(src: &str) -> Notes {
    let mut notes = Notes::default();
    let mut intro = String::new();
    let mut cur: Option<Note> = None;
    let mut fence = false;
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && let Some(h) = line.strip_prefix("## ") {
            notes.notes.extend(cur.take());
            cur = Some(heading(h));
            continue;
        }
        let body = match &mut cur {
            Some(n) => &mut n.body,
            None => &mut intro,
        };
        body.push_str(line);
        body.push('\n');
    }
    notes.notes.extend(cur);
    for n in &mut notes.notes {
        n.body = n.body.trim().to_string();
    }
    notes.intro = intro.trim().to_string();
    notes
}

/// `path:12-20 [check]` → a note with no body yet.
fn heading(h: &str) -> Note {
    let heading = h.trim().to_string();
    let mut rest = heading.as_str();
    let mut tag = Tag::None;
    if let Some((before, t)) = rest.rsplit_once('[')
        && let Some(t) = t.trim().strip_suffix(']')
    {
        let known = match t.trim().to_ascii_lowercase().as_str() {
            "check" => Some(Tag::Check),
            "mechanical" => Some(Tag::Mechanical),
            _ => None,
        };
        if let Some(k) = known {
            tag = k;
            rest = before.trim_end();
        }
    }
    let rest = rest.trim_matches('`');
    let (path, a, b) = match rest
        .rsplit_once(':')
        .and_then(|(p, r)| Some((p, range(r)?)))
    {
        Some((p, (a, b))) => (p.trim_matches('`'), a, b),
        None => (rest, 0, 0),
    };
    let path = path.trim().to_string();
    Note {
        heading: heading.clone(),
        path,
        a,
        b,
        tag,
        body: String::new(),
    }
}

fn range(r: &str) -> Option<(u32, u32)> {
    let (a, b): (u32, u32) = match r.split_once('-') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => {
            let n = r.trim().parse().ok()?;
            (n, n)
        }
    };
    (a > 0 && b > 0).then_some((a.min(b), a.max(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_intro_and_notes() {
        let n = parse(
            "Intent: add X.\n\n## src/a.rs:10-20 [check]\nNot sure about this.\n\n## `b/c.rs` [mechanical]\nFormatter.\n## src/d.rs:7\nOne line.\n## Decisions\n```\n## not a heading\n```\n",
        );
        assert_eq!(n.intro, "Intent: add X.");
        assert_eq!(n.notes.len(), 4);
        let a = &n.notes[0];
        assert_eq!(
            (a.path.as_str(), a.a, a.b, a.tag),
            ("src/a.rs", 10, 20, Tag::Check)
        );
        assert_eq!(a.body, "Not sure about this.");
        let b = &n.notes[1];
        assert_eq!(
            (b.path.as_str(), b.a, b.tag),
            ("b/c.rs", 0, Tag::Mechanical)
        );
        assert_eq!((n.notes[2].a, n.notes[2].b), (7, 7));
        assert_eq!(n.notes[3].path, "Decisions");
        assert!(n.notes[3].body.contains("## not a heading"));
        assert_eq!(n.tag_for("src/a.rs"), Tag::Check);
        assert_eq!(n.tag_for("b/c.rs"), Tag::Mechanical);
        assert_eq!(n.tag_for("src/d.rs"), Tag::None);
    }

    #[test]
    fn unknown_tag_stays_in_heading() {
        let n = parse("## src/a.rs [later]\nx\n");
        assert_eq!(n.notes[0].path, "src/a.rs [later]");
        assert_eq!(n.notes[0].tag, Tag::None);
    }
}
