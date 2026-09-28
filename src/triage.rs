//! Risk ordering: sort files so the ones that deserve attention come first,
//! and collapse the mechanical ones (lockfiles, generated, whitespace-only).

use crate::config::Config;
use crate::diff::{FileDiff, Kind};
use crate::notes::{Notes, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Check,
    Sensitive,
    Code,
    Tests,
    Docs,
    Mechanical,
}

impl Tier {
    pub fn key(self) -> &'static str {
        match self {
            Tier::Check => "check",
            Tier::Sensitive => "sensitive",
            Tier::Code => "code",
            Tier::Tests => "tests",
            Tier::Docs => "docs",
            Tier::Mechanical => "mech",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tier::Check => "Flagged by the agent",
            Tier::Sensitive => "Sensitive",
            Tier::Code => "Code",
            Tier::Tests => "Tests",
            Tier::Docs => "Docs",
            Tier::Mechanical => "Mechanical · collapsed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Triage {
    pub tier: Tier,
    /// Why the file is in a check, sensitive or mechanical tier ("" otherwise).
    pub why: String,
}

/// Classify every file; in risk order, sort by tier (path order within one).
pub fn arrange(files: &mut Vec<FileDiff>, notes: &Notes, cfg: &Config) -> Vec<Triage> {
    let mut both: Vec<(FileDiff, Triage)> = std::mem::take(files)
        .into_iter()
        .map(|f| {
            let t = classify(&f, notes, cfg);
            (f, t)
        })
        .collect();
    if cfg.order_by_risk {
        both.sort_by(|(fa, ta), (fb, tb)| {
            ta.tier.cmp(&tb.tier).then_with(|| fa.path.cmp(&fb.path))
        });
    }
    let (f, t) = both.into_iter().unzip();
    *files = f;
    t
}

pub fn classify(f: &FileDiff, notes: &Notes, cfg: &Config) -> Triage {
    let t = |tier, why: &str| Triage {
        tier,
        why: why.to_string(),
    };
    let tag = notes.tag_for(&f.path);
    if tag == Tag::Check {
        return t(Tier::Check, "flagged by the agent");
    }
    if let Some(why) = mechanical(f, cfg) {
        return t(Tier::Mechanical, &why);
    }
    // The agent can't talk a sensitive file down to mechanical.
    if let Some(why) = sensitive(&f.path, cfg) {
        return t(Tier::Sensitive, why);
    }
    if tag == Tag::Mechanical {
        return t(Tier::Mechanical, "mechanical, says the agent");
    }
    if is_test(&f.path) {
        t(Tier::Tests, "")
    } else if is_doc(&f.path) {
        t(Tier::Docs, "")
    } else {
        t(Tier::Code, "")
    }
}

const GENERATED: &[(&str, &str)] = &[
    ("*.lock", "lockfile"),
    ("package-lock.json", "lockfile"),
    ("npm-shrinkwrap.json", "lockfile"),
    ("pnpm-lock.yaml", "lockfile"),
    ("go.sum", "lockfile"),
    ("*.min.js", "minified"),
    ("*.min.css", "minified"),
    ("*.map", "source map"),
    ("*.snap", "test snapshot"),
    ("**/__snapshots__/**", "test snapshot"),
    ("db/schema.rb", "generated schema"),
    ("db/structure.sql", "generated schema"),
    ("vendor/**", "vendored"),
];

fn mechanical(f: &FileDiff, cfg: &Config) -> Option<String> {
    if let Some((_, why)) = GENERATED.iter().find(|(p, _)| glob(p, &f.path)) {
        return Some(why.to_string());
    }
    if cfg.collapse.iter().any(|p| glob(p, &f.path)) {
        return Some("collapse pattern".into());
    }
    if f.hunks.is_empty() && !f.binary && !f.is_submodule() {
        if f.old_path.is_some() {
            return Some("renamed, no changes".into());
        }
        if f.old_mode != f.new_mode && f.status == 'M' {
            return Some("mode change".into());
        }
    }
    whitespace_only(f).then(|| "whitespace only".into())
}

/// Removed and added lines are the same once whitespace is dropped.
fn whitespace_only(f: &FileDiff) -> bool {
    let (mut del, mut add) = (Vec::new(), Vec::new());
    for l in f.hunks.iter().flat_map(|h| &h.lines) {
        let squashed = || l.text.split_whitespace().collect::<String>();
        match l.kind {
            Kind::Del => del.push(squashed()),
            Kind::Add => add.push(squashed()),
            Kind::Ctx => {}
        }
    }
    if del.is_empty() && add.is_empty() {
        return false;
    }
    // Blank lines added or removed are whitespace too.
    del.retain(|s| !s.is_empty());
    add.retain(|s| !s.is_empty());
    del.sort();
    add.sort();
    del == add
}

const SECURITY_WORDS: &[&str] = &[
    "auth",
    "oauth",
    "authn",
    "authz",
    "authentication",
    "authorization",
    "security",
    "crypto",
    "encryption",
    "permission",
    "permissions",
    "secret",
    "secrets",
    "password",
    "passwords",
    "credential",
    "credentials",
    "acl",
    "csrf",
    "jwt",
];

const MANIFESTS: &[&str] = &[
    "cargo.toml",
    "package.json",
    "gemfile",
    "go.mod",
    "requirements.txt",
    "pyproject.toml",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "composer.json",
];

fn sensitive(path: &str, cfg: &Config) -> Option<&'static str> {
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    let dirs: Vec<&str> = p.split('/').rev().skip(1).collect();
    if dirs.iter().any(|d| *d == "migrate" || *d == "migrations") {
        return Some("migration");
    }
    if p.starts_with(".github/workflows/")
        || p.starts_with(".circleci/")
        || name == ".gitlab-ci.yml"
        || name == "jenkinsfile"
    {
        return Some("CI");
    }
    if name.starts_with("dockerfile")
        || name.starts_with("docker-compose")
        || name.ends_with(".tf")
        || name.ends_with(".tfvars")
    {
        return Some("infrastructure");
    }
    if MANIFESTS.contains(&name) {
        return Some("dependencies");
    }
    if name.starts_with(".env") {
        return Some("environment");
    }
    if p.split(['/', '.', '_', '-'])
        .any(|w| SECURITY_WORDS.contains(&w))
    {
        return Some("security");
    }
    if name.ends_with(".sql") {
        return Some("SQL");
    }
    if cfg.sensitive.iter().any(|g| glob(g, path)) {
        return Some("sensitive pattern");
    }
    None
}

fn is_test(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    p.split('/').rev().skip(1).any(|d| {
        matches!(
            d,
            "test" | "tests" | "spec" | "specs" | "__tests__" | "testdata" | "fixtures"
        )
    }) || name.starts_with("test_")
        || ["_test.", ".test.", "_spec.", ".spec.", "_tests."]
            .iter()
            .any(|s| name.contains(s))
}

fn is_doc(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.starts_with("docs/")
        || p.starts_with("doc/")
        || [".md", ".mdx", ".rst", ".adoc", ".txt"]
            .iter()
            .any(|e| p.ends_with(e))
}

/// Glob match: `*` and `?` stay within a path segment, `**` crosses them.
/// A pattern without `/` matches the file name in any directory.
pub fn glob(pat: &str, path: &str) -> bool {
    if !pat.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return matches(pat.as_bytes(), name.as_bytes());
    }
    matches(pat.as_bytes(), path.as_bytes())
}

fn matches(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => match p[2..].strip_prefix(b"/") {
            // `**/` matches zero or more whole directories.
            Some(rest) => {
                (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && matches(rest, &s[i..]))
            }
            None => (0..=s.len()).any(|i| matches(&p[2..], &s[i..])),
        },
        Some(b'*') => (0..=s.len())
            .take_while(|&i| i == 0 || s[i - 1] != b'/')
            .any(|i| matches(&p[1..], &s[i..])),
        Some(b'?') => s.first().is_some_and(|&c| c != b'/') && matches(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && matches(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Hunk, Line};

    fn file(path: &str, lines: &[(Kind, &str)]) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: 'M',
            old_mode: "100644".into(),
            new_mode: "100644".into(),
            old_blob: "a".into(),
            new_blob: "b".into(),
            binary: false,
            hunks: vec![Hunk {
                header: String::new(),
                old_start: 1,
                new_start: 1,
                lines: lines
                    .iter()
                    .map(|(kind, text)| Line {
                        kind: *kind,
                        old_no: 1,
                        new_no: 1,
                        text: text.to_string(),
                    })
                    .collect(),
            }],
            add: 0,
            del: 0,
        }
    }

    #[test]
    fn globs() {
        assert!(glob("*.lock", "a/b/Cargo.lock"));
        assert!(glob("**/__snapshots__/**", "src/__snapshots__/x.snap"));
        assert!(glob("**/__snapshots__/**", "__snapshots__/x"));
        assert!(!glob("**/__snapshots__/**", "src/x__snapshots__/y"));
        assert!(glob("vendor/**", "vendor/a/b.go"));
        assert!(!glob("vendor/**", "src/vendor/a.go"));
        assert!(glob("db/*.rb", "db/schema.rb"));
        assert!(!glob("db/*.rb", "db/x/schema.rb"));
        assert!(glob("?.rs", "a.rs"));
    }

    #[test]
    fn whitespace() {
        use Kind::*;
        assert!(whitespace_only(&file(
            "a.rs",
            &[(Del, "  x = 1"), (Add, "    x  = 1"), (Add, "")]
        )));
        assert!(!whitespace_only(&file(
            "a.rs",
            &[(Del, "x = 1"), (Add, "x = 2")]
        )));
        assert!(!whitespace_only(&file("a.rs", &[(Ctx, "x")])));
    }

    #[test]
    fn tiers() {
        let cfg = Config::default();
        let notes = crate::notes::parse(
            "## src/a.rs [check]\n## Cargo.lock [check]\n## src/b.rs [mechanical]\n## db/migrate/1.rb [mechanical]\n",
        );
        let tier = |p: &str| classify(&file(p, &[(Kind::Add, "x")]), &notes, &cfg).tier;
        assert_eq!(tier("src/a.rs"), Tier::Check);
        assert_eq!(tier("Cargo.lock"), Tier::Check);
        assert_eq!(tier("src/b.rs"), Tier::Mechanical);
        assert_eq!(tier("db/migrate/1.rb"), Tier::Sensitive);
        assert_eq!(tier("app/lib/auth_helper.rb"), Tier::Sensitive);
        assert_eq!(tier("app/models/author.rb"), Tier::Code);
        assert_eq!(tier("Cargo.toml"), Tier::Sensitive);
        assert_eq!(tier("yarn.lock"), Tier::Mechanical);
        assert_eq!(tier("spec/models/user_spec.rb"), Tier::Tests);
        assert_eq!(tier("src/foo.test.ts"), Tier::Tests);
        assert_eq!(tier("README.md"), Tier::Docs);
        assert_eq!(tier("src/main.rs"), Tier::Code);
    }

    #[test]
    fn arranges_by_tier_then_path() {
        let cfg = Config::default();
        let mut files = vec![
            file("Cargo.lock", &[(Kind::Add, "x")]),
            file("src/b.rs", &[(Kind::Add, "x")]),
            file("README.md", &[(Kind::Add, "x")]),
            file("src/a.rs", &[(Kind::Add, "x")]),
            file("Cargo.toml", &[(Kind::Add, "x")]),
        ];
        let t = arrange(&mut files, &Notes::default(), &cfg);
        let order: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            order,
            [
                "Cargo.toml",
                "src/a.rs",
                "src/b.rs",
                "README.md",
                "Cargo.lock"
            ]
        );
        assert_eq!(t[4].why, "lockfile");
    }
}
