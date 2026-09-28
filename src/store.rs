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
               PRIMARY KEY(path, blob));",
        )?;
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
