//! Thin wrapper over the `git` CLI. Every call is `git -C <toplevel> ...`;
//! gv never changes its own working directory.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone)]
pub struct Git {
    /// Worktree toplevel.
    pub dir: PathBuf,
    /// This worktree's git dir (holds its index).
    git_dir: PathBuf,
    /// Git dir shared by all worktrees.
    common_dir: PathBuf,
}

impl Git {
    /// Locate the repository containing `start` and anchor all calls at its toplevel.
    pub fn discover(start: &Path) -> Result<Git> {
        let probe = Git {
            dir: start.to_path_buf(),
            git_dir: PathBuf::new(),
            common_dir: PathBuf::new(),
        };
        let out = probe
            .try_run(&[
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
            ])
            .ok_or_else(|| anyhow!("not inside a git working tree: {}", start.display()))?;
        let mut lines = out.lines();
        let (Some(top), Some(git_dir), Some(common)) = (lines.next(), lines.next(), lines.next())
        else {
            bail!("not inside a git working tree: {}", start.display());
        };
        let dir = PathBuf::from(top);
        let git_dir = PathBuf::from(git_dir);
        // --git-common-dir may be relative to the directory git ran in.
        let common = Path::new(common);
        let common_dir = if common.is_absolute() {
            common.to_path_buf()
        } else {
            start.join(common)
        };
        let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
        Ok(Git {
            dir,
            git_dir,
            common_dir,
        })
    }

    pub fn cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.dir);
        // Never let git refresh/rewrite the real index as a side effect of reads.
        c.env("GIT_OPTIONAL_LOCKS", "0");
        c.env("GIT_TERMINAL_PROMPT", "0");
        c.env("LC_ALL", "C");
        for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            c.env_remove(v);
        }
        c.args(["-c", "core.quotePath=false"]);
        c
    }

    pub fn run_bytes_with(&self, mut c: Command) -> Result<Vec<u8>> {
        let out = c
            .stdin(Stdio::null())
            .output()
            .context("failed to run git")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(out.stdout)
    }

    pub fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut c = self.cmd();
        c.args(args);
        self.run_bytes_with(c)
            .with_context(|| format!("git {}", args.join(" ")))
    }

    pub fn run(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run_bytes(args)?).into_owned())
    }

    pub fn try_run(&self, args: &[&str]) -> Option<String> {
        self.run(args).ok()
    }

    /// Resolve a revision to a full sha, or None.
    pub fn rev(&self, rev: &str) -> Option<String> {
        self.try_run(&["rev-parse", "--verify", "--quiet", "--end-of-options", rev])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn commit(&self, rev: &str) -> Option<String> {
        self.rev(&format!("{rev}^{{commit}}"))
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        Ok(self.run(&["merge-base", a, b])?.trim().to_string())
    }

    pub fn empty_tree(&self) -> Result<String> {
        let mut c = self.cmd();
        c.args(["hash-object", "-t", "tree", "--stdin"]);
        let out = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?
            .wait_with_output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Absolute path of the common git dir (shared by all worktrees).
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// Default branch: origin/HEAD, else main, else master (local or origin/).
    pub fn default_branch(&self, override_: Option<&str>) -> Result<String> {
        if let Some(b) = override_ {
            self.commit(b)
                .ok_or_else(|| anyhow!("--base {b}: no such ref"))?;
            return Ok(b.to_string());
        }
        if let Some(r) = self.try_run(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ]) {
            let r = r.trim();
            if !r.is_empty() && self.commit(r).is_some() {
                return Ok(r.to_string());
            }
        }
        for name in ["main", "master"] {
            for r in [format!("origin/{name}"), name.to_string()] {
                if self.commit(&r).is_some() {
                    return Ok(r);
                }
            }
        }
        bail!("can't detect the default branch; pass --base <ref>")
    }

    /// Snapshot the working tree (tracked + untracked, honoring .gitignore) into a
    /// tree object. Uses a throwaway index seeded from the real one (for its stat
    /// cache); the real index is never written. Blobs land in .git/objects.
    pub fn snapshot_worktree(&self) -> Result<String> {
        let tmp = self
            .git_dir
            .join(format!("gv-snapshot-{}.index", std::process::id()));
        let real = self.git_dir.join("index");
        let _guard = RemoveOnDrop(tmp.clone());
        let with_index = |args: &[&str]| -> Result<String> {
            let mut c = self.cmd();
            c.env("GIT_INDEX_FILE", &tmp).args(args);
            Ok(String::from_utf8_lossy(&self.run_bytes_with(c)?).into_owned())
        };
        if real.exists() {
            std::fs::copy(&real, &tmp).context("copying index for snapshot")?;
        } else if self.commit("HEAD").is_some() {
            with_index(&["read-tree", "HEAD"])?;
        }
        with_index(&["add", "-A", "--ignore-errors"])?;
        Ok(with_index(&["write-tree"])?.trim().to_string())
    }

    /// Current branch name, or None when detached.
    pub fn current_branch(&self) -> Option<String> {
        self.try_run(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Pin a reviewed snapshot under `refs/gv/reviewed/<branch>` so gc keeps it
    /// and "since last review" has a base. The pin is a commit of the reviewed
    /// tree whose parents are the reviewed HEAD commit and the previous pin, so
    /// every earlier review point stays reachable too.
    pub fn pin_review(
        &self,
        branch: &str,
        tree: &str,
        head_commit: Option<&str>,
    ) -> Result<String> {
        let refname = format!("refs/gv/reviewed/{branch}");
        let prev = self.rev(&refname);
        let mut c = self.cmd();
        c.args(["commit-tree", tree, "-m", "gv: reviewed snapshot"]);
        for p in head_commit.into_iter().chain(prev.as_deref()) {
            c.args(["-p", p]);
        }
        for (k, v) in [
            ("GIT_AUTHOR_NAME", "gv"),
            ("GIT_AUTHOR_EMAIL", "gv@localhost"),
            ("GIT_COMMITTER_NAME", "gv"),
            ("GIT_COMMITTER_EMAIL", "gv@localhost"),
        ] {
            c.env(k, v);
        }
        let sha = String::from_utf8_lossy(&self.run_bytes_with(c)?)
            .trim()
            .to_string();
        self.run(&["update-ref", "-m", "gv review", &refname, &sha])?;
        Ok(sha)
    }

    /// The last reviewed pin for a branch, if any.
    pub fn review_pin(&self, branch: &str) -> Option<String> {
        self.rev(&format!("refs/gv/reviewed/{branch}"))
    }

    /// Read blobs by sha with one `git cat-file --batch` process.
    pub fn blobs(&self, shas: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut c = self.cmd();
        c.args(["cat-file", "--batch"]);
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            for s in shas {
                writeln!(stdin, "{s}")?;
            }
        }
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out)?;
        child.wait()?;
        let mut res = Vec::with_capacity(shas.len());
        let mut pos = 0;
        for _ in shas {
            let nl = out[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|n| pos + n)
                .ok_or_else(|| anyhow!("cat-file: truncated output"))?;
            let header = String::from_utf8_lossy(&out[pos..nl]).into_owned();
            pos = nl + 1;
            let parts: Vec<&str> = header.split(' ').collect();
            if parts.len() == 3 && parts[1] == "blob" {
                let size: usize = parts[2].parse()?;
                res.push(Some(out[pos..pos + size].to_vec()));
                pos += size + 1;
            } else if parts.len() == 3 {
                // non-blob object: skip its body
                let size: usize = parts[2].parse()?;
                pos += size + 1;
                res.push(None);
            } else {
                res.push(None); // "<sha> missing"
            }
        }
        Ok(res)
    }
}

struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
    }
}

pub fn is_null_sha(s: &str) -> bool {
    s.bytes().all(|b| b == b'0')
}
