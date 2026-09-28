//! CLI target parsing and resolution to (base, head).

use crate::git::Git;
use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone)]
pub enum Target {
    Worktree,
    Branch(Option<String>),
    Commit(String),
    Last(usize),
    Range {
        from: String,
        to: String,
        symmetric: bool,
    },
}

pub fn parse(args: &[String], git: &Git) -> Result<Target> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let t = match a.as_slice() {
        [] => Target::Worktree,
        ["b" | "branch"] => Target::Branch(None),
        ["b" | "branch", name] => Target::Branch(Some(name.to_string())),
        ["c" | "commit", sha] => Target::Commit(sha.to_string()),
        ["l" | "last"] => Target::Last(1),
        ["l" | "last", n] => Target::Last(
            n.parse()
                .map_err(|_| anyhow!("gv l: expected a number, got '{n}'"))?,
        ),
        ["p" | "pr", ..] => bail!("gv p (GitHub PRs) is not implemented yet (M3)"),
        ["s" | "since"] => bail!("gv s is not implemented yet (M3)"),
        [x] => bare(x, git)?,
        _ => bail!("can't parse target: {}", args.join(" ")),
    };
    Ok(t)
}

/// Bare nouns: range, then local branch, then PR number, then commit.
fn bare(x: &str, git: &Git) -> Result<Target> {
    if let Some((from, to)) = x.split_once("...") {
        return Ok(Target::Range {
            from: or_head(from),
            to: or_head(to),
            symmetric: true,
        });
    }
    if let Some((from, to)) = x.split_once("..") {
        return Ok(Target::Range {
            from: or_head(from),
            to: or_head(to),
            symmetric: false,
        });
    }
    let is_branch = git.rev(&format!("refs/heads/{x}")).is_some()
        || git.rev(&format!("refs/remotes/{x}")).is_some();
    if is_branch {
        return Ok(Target::Branch(Some(x.to_string())));
    }
    if x.len() < 7 && x.bytes().all(|b| b.is_ascii_digit()) {
        bail!("'{x}' looks like a PR number; gv p is not implemented yet (M3)");
    }
    if git.commit(x).is_some() {
        return Ok(Target::Commit(x.to_string()));
    }
    bail!("can't resolve target '{x}' (not a branch, PR number, or commit)")
}

fn or_head(s: &str) -> String {
    if s.is_empty() {
        "HEAD".into()
    } else {
        s.into()
    }
}

const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const EMPTY_TREE_SHA256: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";

pub struct Commit {
    pub short: String,
    pub subject: String,
    pub author: String,
}

pub struct Resolved {
    pub label: String,
    /// Commit or tree to diff from.
    pub base: String,
    /// Commit or tree to diff to (a snapshot tree for the working tree).
    pub head: String,
    pub commits: Vec<Commit>,
}

pub fn resolve(t: &Target, git: &Git, base_override: Option<&str>) -> Result<Resolved> {
    let short = |s: &str| s.chars().take(8).collect::<String>();
    match t {
        Target::Worktree => {
            let def = git.default_branch(base_override)?;
            let head_commit = git.commit("HEAD");
            let base = match &head_commit {
                Some(h) => git.merge_base(h, &def)?,
                None => git.empty_tree()?,
            };
            let head = git.snapshot_worktree()?;
            let commits = match &head_commit {
                Some(h) => log(git, &base, h)?,
                None => Vec::new(),
            };
            Ok(Resolved {
                label: format!("working tree vs {def} ({})", short(&base)),
                base,
                head,
                commits,
            })
        }
        Target::Branch(name) => {
            let def = git.default_branch(base_override)?;
            let name = match name {
                Some(n) => n.clone(),
                None => git
                    .run(&["rev-parse", "--abbrev-ref", "HEAD"])?
                    .trim()
                    .to_string(),
            };
            let head = git
                .commit(&name)
                .ok_or_else(|| anyhow!("no such branch: {name}"))?;
            let base = git.merge_base(&head, &def)?;
            let commits = log(git, &base, &head)?;
            Ok(Resolved {
                label: format!("{name} vs {def}"),
                base,
                head,
                commits,
            })
        }
        Target::Commit(rev) => {
            let head = git
                .commit(rev)
                .ok_or_else(|| anyhow!("no such commit: {rev}"))?;
            let base = match git.commit(&format!("{head}^")) {
                Some(p) => p,
                None => git.empty_tree()?,
            };
            let commits = log(git, &base, &head)?;
            let label = match commits.first() {
                Some(c) => format!("{} {}", c.short, c.subject),
                None => short(&head),
            };
            Ok(Resolved {
                label,
                base,
                head,
                commits,
            })
        }
        Target::Last(n) => {
            let n = (*n).max(1);
            let head = git
                .commit("HEAD")
                .ok_or_else(|| anyhow!("no commits yet"))?;
            let base = match git.commit(&format!("HEAD~{n}")) {
                Some(b) => b,
                None => git.empty_tree()?,
            };
            let commits = log(git, &base, &head)?;
            let label = if n == 1 {
                "last commit".into()
            } else {
                format!("last {n} commits")
            };
            Ok(Resolved {
                label,
                base,
                head,
                commits,
            })
        }
        Target::Range {
            from,
            to,
            symmetric,
        } => {
            let to_sha = git.commit(to).ok_or_else(|| anyhow!("no such rev: {to}"))?;
            let from_sha = git
                .commit(from)
                .ok_or_else(|| anyhow!("no such rev: {from}"))?;
            let base = if *symmetric {
                git.merge_base(&from_sha, &to_sha)?
            } else {
                from_sha
            };
            let commits = log(git, &base, &to_sha)?;
            let sep = if *symmetric { "..." } else { ".." };
            Ok(Resolved {
                label: format!("{from}{sep}{to}"),
                base,
                head: to_sha,
                commits,
            })
        }
    }
}

/// Commits in base..head (oldest first). `base` may be the empty tree (root case).
fn log(git: &Git, base: &str, head: &str) -> Result<Vec<Commit>> {
    let range = if base == EMPTY_TREE_SHA1 || base == EMPTY_TREE_SHA256 {
        head.to_string()
    } else {
        format!("{base}..{head}")
    };
    let out = git.run(&[
        "log",
        "--first-parent",
        "--reverse",
        "--max-count=500",
        "--format=%h%x1f%s%x1f%an%x1e",
        &range,
    ])?;
    Ok(out
        .split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_matches('\n').split('\x1f');
            Some(Commit {
                short: f.next()?.to_string(),
                subject: f.next()?.to_string(),
                author: f.next()?.to_string(),
            })
        })
        .collect())
}
