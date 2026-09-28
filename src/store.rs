//! Review memory in `~/.local/share/gv/<repo_id>.sqlite`.

use crate::config::xdg;
use crate::git::Git;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::io::Read;
use std::sync::Mutex;

pub struct Store {
    db: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewed {
    No,
    Yes,
    /// Viewed at some other blob; the file changed since.
    Changed,
}

impl Viewed {
    pub fn code(self) -> &'static str {
        match self {
            Viewed::No => "u",
            Viewed::Yes => "v",
            Viewed::Changed => "c",
        }
    }
}

/// Stable per-repo id stored in `<git-common-dir>/gv-id` (shared by worktrees,
/// survives moving the repo).
pub fn repo_id(git: &Git) -> Result<String> {
    let path = git.common_dir().join("gv-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(s.to_string());
        }
    }
    let id = random_hex(16)?;
    std::fs::write(&path, format!("{id}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(id)
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// A comment on a line, or a block of lines, in a review. It anchors to its
/// last row: `side` is "n" (new file line) or "o" (old line, for removed
/// lines), and `span` counts the rows of the block ending there. `blob` is
/// the file version it was written on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Comment {
    pub id: i64,
    pub path: String,
    pub side: String,
    pub line: u32,
    pub blob: String,
    /// Rows in the commented block (1 for a single line).
    pub span: u32,
    /// Human-readable location, e.g. "40-45" or "12 (removed line; ...)".
    pub loc: String,
    /// Diff lines ending with the commented line (the whole block for ranges).
    pub excerpt: String,
    /// Which view it was written in ("" = whole review, else a label).
    pub context: String,
    /// Heading of the agent note this replies to ("" for a line comment);
    /// `excerpt` then holds the note's text.
    pub note: String,
    pub body: String,
}

pub struct Submitted {
    pub number: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    pub fn open(repo_id: &str) -> Result<Store> {
        let dir = xdg("XDG_DATA_HOME", ".local/share").join("gv");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let db = Connection::open(dir.join(format!("{repo_id}.sqlite")))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=2000;
             CREATE TABLE IF NOT EXISTS viewed(
               path TEXT NOT NULL, blob TEXT NOT NULL, ts INTEGER NOT NULL,
               PRIMARY KEY(path, blob));
             CREATE TABLE IF NOT EXISTS review(
               id INTEGER PRIMARY KEY, branch TEXT NOT NULL, created INTEGER NOT NULL,
               summary TEXT NOT NULL DEFAULT '', verdict TEXT, submitted INTEGER,
               number INTEGER, reviewed TEXT, notes TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS comment(
               id INTEGER PRIMARY KEY, review_id INTEGER NOT NULL REFERENCES review(id),
               path TEXT NOT NULL, side TEXT NOT NULL, line INTEGER NOT NULL, blob TEXT NOT NULL,
               excerpt TEXT NOT NULL, context TEXT NOT NULL, body TEXT NOT NULL, created INTEGER NOT NULL,
               span INTEGER NOT NULL DEFAULT 1, loc TEXT NOT NULL DEFAULT '',
               note TEXT NOT NULL DEFAULT '');",
        )?;
        // Databases from before multi-line comments lack span/loc.
        let has_span: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('comment') WHERE name='span'")?
            .exists([])?;
        if !has_span {
            db.execute_batch(
                "ALTER TABLE comment ADD COLUMN span INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE comment ADD COLUMN loc TEXT NOT NULL DEFAULT '';",
            )?;
        }
        // ...and before agent notes, reviews lack notes and comments can't reply to one.
        let has_reply: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('comment') WHERE name='note'")?
            .exists([])?;
        if !has_reply {
            db.execute_batch("ALTER TABLE comment ADD COLUMN note TEXT NOT NULL DEFAULT '';")?;
        }
        let has_notes: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('review') WHERE name='notes'")?
            .exists([])?;
        if !has_notes {
            db.execute_batch("ALTER TABLE review ADD COLUMN notes TEXT NOT NULL DEFAULT '';")?;
        }
        Ok(Store { db: Mutex::new(db) })
    }

    pub fn state(&self, path: &str, blob: &str) -> Result<Viewed> {
        let db = self.db.lock().unwrap();
        let same: bool = db
            .query_row(
                "SELECT 1 FROM viewed WHERE path=?1 AND blob=?2",
                params![path, blob],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if same {
            return Ok(Viewed::Yes);
        }
        let any: bool = db
            .query_row(
                "SELECT 1 FROM viewed WHERE path=?1 LIMIT 1",
                params![path],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        Ok(if any { Viewed::Changed } else { Viewed::No })
    }

    /// The branch's unsubmitted review (drafts survive restarts), or a new one.
    pub fn draft_review(&self, branch: &str) -> Result<i64> {
        let db = self.db.lock().unwrap();
        let existing: Option<i64> = db
            .query_row(
                "SELECT id FROM review WHERE branch=?1 AND submitted IS NULL ORDER BY id DESC LIMIT 1",
                params![branch],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        db.execute(
            "INSERT INTO review(branch, created) VALUES(?1, ?2)",
            params![branch, now()],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn summary(&self, review: i64) -> Result<String> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row(
            "SELECT summary FROM review WHERE id=?1",
            params![review],
            |r| r.get(0),
        )?)
    }

    pub fn set_summary(&self, review: i64, summary: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE review SET summary=?2 WHERE id=?1 AND submitted IS NULL",
            params![review, summary],
        )?;
        Ok(())
    }

    /// The agent notes a review was opened with (kept so resuming shows them).
    pub fn notes(&self, review: i64) -> Result<String> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row(
            "SELECT notes FROM review WHERE id=?1",
            params![review],
            |r| r.get(0),
        )?)
    }

    pub fn set_notes(&self, review: i64, notes: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE review SET notes=?2 WHERE id=?1 AND submitted IS NULL",
            params![review, notes],
        )?;
        Ok(())
    }

    pub fn comments(&self, review: i64) -> Result<Vec<Comment>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare(
            "SELECT id, path, side, line, blob, excerpt, context, body, span, loc, note FROM comment
             WHERE review_id=?1 ORDER BY path, line, id",
        )?;
        let rows = q.query_map(params![review], |r| {
            Ok(Comment {
                id: r.get(0)?,
                path: r.get(1)?,
                side: r.get(2)?,
                line: r.get(3)?,
                blob: r.get(4)?,
                excerpt: r.get(5)?,
                context: r.get(6)?,
                body: r.get(7)?,
                span: r.get(8)?,
                loc: r.get(9)?,
                note: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn add_comment(&self, review: i64, c: &Comment) -> Result<i64> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO comment(review_id, path, side, line, blob, excerpt, context, body, created, span, loc, note)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![review, c.path, c.side, c.line, c.blob, c.excerpt, c.context, c.body, now(), c.span, c.loc, c.note],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Edit a comment's body; an empty body deletes it. Returns false if absent.
    pub fn edit_comment(&self, review: i64, id: i64, body: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let n = if body.trim().is_empty() {
            db.execute(
                "DELETE FROM comment WHERE id=?1 AND review_id=?2",
                params![id, review],
            )?
        } else {
            db.execute(
                "UPDATE comment SET body=?3 WHERE id=?1 AND review_id=?2",
                params![id, review, body],
            )?
        };
        Ok(n > 0)
    }

    pub fn submit(
        &self,
        review: i64,
        verdict: &str,
        summary: &str,
        reviewed: &str,
    ) -> Result<Submitted> {
        let db = self.db.lock().unwrap();
        let branch: String = db.query_row(
            "SELECT branch FROM review WHERE id=?1",
            params![review],
            |r| r.get(0),
        )?;
        let number: i64 = db.query_row(
            "SELECT COUNT(*) + 1 FROM review WHERE branch=?1 AND submitted IS NOT NULL",
            params![branch],
            |r| r.get(0),
        )?;
        db.execute(
            "UPDATE review SET verdict=?2, summary=?3, submitted=?4, number=?5, reviewed=?6 WHERE id=?1",
            params![review, verdict, summary, now(), number, reviewed],
        )?;
        Ok(Submitted { number })
    }

    pub fn set(&self, path: &str, blob: &str, viewed: bool) -> Result<()> {
        let db = self.db.lock().unwrap();
        if viewed {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            db.execute(
                "INSERT OR REPLACE INTO viewed(path, blob, ts) VALUES(?1, ?2, ?3)",
                params![path, blob, ts],
            )?;
        } else {
            db.execute(
                "DELETE FROM viewed WHERE path=?1 AND blob=?2",
                params![path, blob],
            )?;
        }
        Ok(())
    }
}
