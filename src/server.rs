//! HTTP server: shell page, per-file fragments, review state, re-snapshot.

use crate::config::Config;
use crate::diff::{self, DiffOpts, FileDiff};
use crate::git::Git;
use crate::notes::Notes;
use crate::render::{self, escape};
use crate::store::Store;
use crate::target::{self, Commit, Target};
use crate::triage::{self, Triage};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::Embed;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

#[derive(Embed)]
#[folder = "web/"]
struct Assets;

pub struct Snapshot {
    pub generation: u64,
    pub label: String,
    /// In review order (see triage), with each file's tier alongside.
    pub files: Vec<FileDiff>,
    pub triage: Vec<Triage>,
    pub notes: Arc<Notes>,
    /// Commits of the whole target (views keep the full list for navigation).
    pub commits: Vec<Commit>,
    /// Diff head: a commit, or the working-tree snapshot tree.
    pub head: String,
    /// Working-tree target with changes beyond HEAD.
    pub uncommitted: bool,
    /// "" for the whole target, a commit sha, "wt" (uncommitted) or "since".
    pub view: String,
    /// Branch reviews are recorded against.
    pub branch: String,
    /// Last review pin on that branch when this snapshot was taken.
    pub pin: Option<String>,
    /// Commit at the head of the review (HEAD for working-tree snapshots).
    pub head_commit: Option<String>,
}

pub struct App {
    pub git: Git,
    pub cfg: Config,
    pub store: Store,
    pub target: Target,
    pub base_override: Option<String>,
    pub full: bool,
    /// Where the agent notes came from, re-read on re-snapshot ("-" is stdin).
    pub notes_src: Option<PathBuf>,
    pub token: String,
    pub port: u16,
    pub snap: RwLock<Arc<Snapshot>>,
    /// Per-commit / uncommitted views of the current snapshot, by view key.
    views: Mutex<HashMap<String, Arc<Snapshot>>>,
    cache: Mutex<HashMap<String, Arc<String>>>,
    /// Review id when started as `gv review`.
    pub review: Option<i64>,
    /// Signalled once a review is submitted; the server then shuts down.
    pub done: tokio::sync::Notify,
    /// The submitted review, printed to stdout on exit.
    pub output: Mutex<Option<String>>,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Resolve the target, snapshot it, and parse the diff.
pub fn build_snapshot(
    git: &Git,
    cfg: &Config,
    t: &Target,
    base_override: Option<&str>,
    notes: Arc<Notes>,
) -> Result<Snapshot> {
    let r = target::resolve(t, git, base_override)?;
    let mut files = diff::diff(git, &r.base, &r.head, &diff_opts(cfg))?;
    let triage = triage::arrange(&mut files, &notes, cfg);
    let uncommitted = r.worktree && git.rev("HEAD^{tree}").as_deref() != Some(r.head.as_str());
    let head_commit = if r.worktree {
        git.commit("HEAD")
    } else {
        git.commit(&r.head)
    };
    Ok(Snapshot {
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        label: r.label,
        files,
        triage,
        notes,
        commits: r.commits,
        head: r.head,
        uncommitted,
        view: String::new(),
        pin: git.review_pin(&r.branch),
        branch: r.branch,
        head_commit,
    })
}

impl Snapshot {
    /// Re-classify and re-sort with new agent notes.
    pub fn set_notes(&mut self, notes: Arc<Notes>, cfg: &Config) {
        self.triage = triage::arrange(&mut self.files, &notes, cfg);
        self.notes = notes;
    }

    /// Notes that won't show where the agent meant them to, for its stderr.
    pub fn note_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for n in &self.notes.notes {
            let Some(f) = self.files.iter().find(|f| f.path == n.path) else {
                if n.path.contains(['/', '.']) && !n.path.contains(' ') {
                    out.push(format!(
                        "note \"{}\": no such file in this diff; shown with the intro",
                        n.heading
                    ));
                }
                continue;
            };
            let touched = f
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .any(|l| l.kind != diff::Kind::Del && (n.a..=n.b).contains(&l.new_no));
            if n.a > 0 && !touched {
                out.push(format!(
                    "note \"{}\": lines not in the diff; shown at the top of the file",
                    n.heading
                ));
            }
        }
        out
    }
}

/// Agent notes from a file, or stdin for "-".
pub fn read_notes(src: &std::path::Path) -> Result<String> {
    use anyhow::Context;
    if src.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
            .context("reading notes from stdin")?;
        return Ok(s);
    }
    std::fs::read_to_string(src).with_context(|| format!("reading notes {}", src.display()))
}

fn diff_opts(cfg: &Config) -> DiffOpts {
    DiffOpts {
        context: cfg.context,
        ignore_whitespace: cfg.ignore_whitespace,
    }
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        git: Git,
        cfg: Config,
        store: Store,
        target: Target,
        base_override: Option<String>,
        full: bool,
        notes_src: Option<PathBuf>,
        token: String,
        port: u16,
        snap: Snapshot,
        review: Option<i64>,
    ) -> App {
        App {
            git,
            cfg,
            store,
            target,
            base_override,
            full,
            notes_src,
            token,
            port,
            snap: RwLock::new(Arc::new(snap)),
            views: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            review,
            done: tokio::sync::Notify::new(),
            output: Mutex::new(None),
        }
    }

    pub(crate) fn snap(&self) -> Arc<Snapshot> {
        self.snap.read().unwrap().clone()
    }

    /// The snapshot for a view: "" (whole target), a commit from the target's
    /// commit list, or "wt" (uncommitted changes). Only those are accepted, so
    /// the URL can't make gv run arbitrary revisions.
    fn view(&self, v: &str) -> Result<Arc<Snapshot>> {
        let main = self.snap();
        if v.is_empty() {
            return Ok(main);
        }
        let key = format!("{}:{v}", main.generation);
        if let Some(s) = self.views.lock().unwrap().get(&key) {
            return Ok(s.clone());
        }
        let (base, head, label) = if v == "wt" {
            if !main.uncommitted {
                anyhow::bail!("no uncommitted changes");
            }
            let base = match self.git.commit("HEAD") {
                Some(h) => h,
                None => self.git.empty_tree()?,
            };
            (base, main.head.clone(), "uncommitted changes".to_string())
        } else if v == "since" {
            let pin = main
                .pin
                .clone()
                .ok_or_else(|| anyhow::anyhow!("nothing reviewed yet on {}", main.branch))?;
            let base = self
                .git
                .rev(&format!("{pin}^{{tree}}"))
                .ok_or_else(|| anyhow::anyhow!("review pin {pin} is not a commit"))?;
            (base, main.head.clone(), "since last review".to_string())
        } else {
            let c = main
                .commits
                .iter()
                .find(|c| c.sha == v)
                .ok_or_else(|| anyhow::anyhow!("not a commit in this review: {v}"))?;
            let base = target::parent_or_empty(&self.git, &c.sha)?;
            (base, c.sha.clone(), format!("{} {}", c.short, c.subject))
        };
        let mut files = diff::diff(&self.git, &base, &head, &diff_opts(&self.cfg))?;
        let triage = triage::arrange(&mut files, &main.notes, &self.cfg);
        let snap = Arc::new(Snapshot {
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            label,
            files,
            triage,
            notes: main.notes.clone(),
            commits: main.commits.clone(),
            head,
            uncommitted: main.uncommitted,
            view: v.to_string(),
            branch: main.branch.clone(),
            pin: main.pin.clone(),
            head_commit: main.head_commit.clone(),
        });
        self.views.lock().unwrap().insert(key, snap.clone());
        Ok(snap)
    }

    /// Any live snapshot (whole target or a view) by generation.
    pub(crate) fn by_generation(&self, generation: u64) -> Option<Arc<Snapshot>> {
        let main = self.snap();
        if main.generation == generation {
            return Some(main);
        }
        self.views
            .lock()
            .unwrap()
            .values()
            .find(|s| s.generation == generation)
            .cloned()
    }

    fn cookie_name(&self) -> String {
        format!("gv_{}", self.port)
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(shell))
        .route("/theme.css", get(theme_css))
        .route("/a/{*path}", get(asset))
        .route("/file/{generation}/{i}", get(file_fragment))
        .route("/viewed/{generation}/{i}", post(set_viewed))
        .route("/resnapshot", post(resnapshot))
        .merge(crate::review::routes())
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Host check (DNS rebinding), Origin check on POST (CSRF), session token.
async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let port = app.port;
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if req.method() != axum::http::Method::GET {
        let origin_ok = match req
            .headers()
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
        {
            None => true,
            Some(o) => {
                o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
            }
        };
        if !origin_ok {
            return (StatusCode::FORBIDDEN, "bad origin").into_response();
        }
    }
    // Token in the query string: exchange for a cookie and drop it from the URL.
    if let Some(q) = req.uri().query()
        && let Some(t) = q.split('&').find_map(|kv| kv.strip_prefix("t="))
    {
        if t != app.token {
            return (StatusCode::FORBIDDEN, "bad token").into_response();
        }
        let cookie = format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            app.cookie_name(),
            app.token
        );
        return (
            StatusCode::SEE_OTHER,
            [
                (header::SET_COOKIE, cookie),
                (header::LOCATION, "/".to_string()),
            ],
        )
            .into_response();
    }
    if cookie(req.headers(), &app.cookie_name()).as_deref() != Some(app.token.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            "missing session token; open the URL gv printed",
        )
            .into_response();
    }
    next.run(req).await
}

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

async fn asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(f) => {
            let ct = match path.rsplit('.').next() {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            (
                [
                    (header::CONTENT_TYPE, ct),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                f.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn theme_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        crate::highlight::theme_css(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ShellQuery {
    #[serde(default)]
    v: String,
}

async fn shell(State(app): State<Arc<App>>, Query(q): Query<ShellQuery>) -> Response {
    let is_view = !q.v.is_empty();
    match tokio::task::spawn_blocking(move || render_shell(&app, &q.v)).await {
        Ok(Ok(html)) => Html(html).into_response(),
        Ok(Err(e)) if is_view => (
            StatusCode::NOT_FOUND,
            Html(format!(
                "<p>{}</p><p><a href=\"/\">Back to all changes</a></p>",
                escape(&format!("{e:#}"))
            )),
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn render_shell(app: &App, view: &str) -> Result<String> {
    let snap = app.view(view)?;
    let tw = app.cfg.tab_width;
    let mut mfiles = Vec::with_capacity(snap.files.len());
    for (f, t) in snap.files.iter().zip(&snap.triage) {
        let v = app.store.state(&f.path, &f.new_blob)?;
        let mut m = render::model_file(f, v.code(), tw);
        m.tier = t.tier.key();
        mfiles.push(m);
    }
    // Notes on files in this view go inline; the rest show with the intro.
    let (notes, stray): (Vec<_>, Vec<_>) = snap
        .notes
        .notes
        .iter()
        .partition(|n| snap.files.iter().any(|f| f.path == n.path));
    let total: u32 = snap.files.iter().map(|f| f.add + f.del).sum();
    let default_level = if app.full {
        3
    } else if total as usize > app.cfg.collapse_over_lines {
        1
    } else {
        3
    };
    let review = match app.review {
        Some(id) => Some(serde_json::json!({
            "summary": app.store.summary(id)?,
            "comments": app.store.comments(id)?,
        })),
        None => None,
    };
    let model = serde_json::json!({
        "generation": snap.generation,
        "defaultLevel": default_level,
        "files": mfiles,
        "notes": notes,
        "review": review,
    });
    // Safe to embed in <script>: no "</" sequences survive.
    let model_json = serde_json::to_string(&model)?.replace("</", "<\\/");

    let tmpl = Assets::get("shell.html").ok_or_else(|| anyhow::anyhow!("missing shell.html"))?;
    let mut env = minijinja::Environment::new();
    env.add_template("shell.html", std::str::from_utf8(&tmpl.data)?)?;
    // Sidebar group headings, when files are in risk order across tiers.
    let tiers = snap.triage.iter().map(|t| t.tier);
    let grouped = app.cfg.order_by_risk && snap.triage.windows(2).any(|w| w[0].tier != w[1].tier);
    let files: Vec<_> = snap
        .files
        .iter()
        .zip(&mfiles)
        .zip(&snap.triage)
        .enumerate()
        .map(|(i, ((f, m), t))| {
            let (dir, name) = match f.path.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), f.path.clone()),
            };
            let tier_head = if grouped && (i == 0 || snap.triage[i - 1].tier != t.tier) {
                let n = tiers.clone().filter(|&x| x == t.tier).count();
                format!("{} · {n}", t.tier.label())
            } else {
                String::new()
            };
            let nnotes = notes.iter().filter(|n| n.path == f.path).count();
            minijinja::context! {
                i, path => f.path, dir, name, old_path => f.old_path, status => f.status.to_string(),
                add => f.add, del => f.del, viewed => m.viewed, gd => m.gd,
                tier => t.tier.key(), why => t.why, tier_head, nnotes,
            }
        })
        .collect();
    let stray: Vec<_> = stray
        .iter()
        .map(|n| minijinja::context! { heading => n.heading, body => n.body })
        .collect();
    let commits: Vec<_> = snap
        .commits
        .iter()
        .map(|c| minijinja::context! { sha => c.sha, short => c.short, subject => c.subject, author => c.author })
        .collect();
    let repo = app
        .git
        .dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(env.get_template("shell.html")?.render(minijinja::context! {
        repo, label => snap.label, files, commits, view => snap.view, uncommitted => snap.uncommitted,
        has_since => snap.pin.is_some() && !matches!(app.target, Target::Since),
        reviewing => app.review.is_some(), branch => snap.branch,
        total_add => snap.files.iter().map(|f| f.add).sum::<u32>(),
        total_del => snap.files.iter().map(|f| f.del).sum::<u32>(), model_json,
        intro => snap.notes.intro, stray, nnotes_total => notes.len(),
    })?)
}

async fn file_fragment(
    State(app): State<Arc<App>>,
    Path((generation, i)): Path<(u64, usize)>,
) -> Response {
    let Some(snap) = app.by_generation(generation) else {
        return (StatusCode::CONFLICT, "stale snapshot").into_response();
    };
    if i >= snap.files.len() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let res = tokio::task::spawn_blocking(move || {
        let f = &snap.files[i];
        let cfg = &app.cfg;
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            f.old_blob, f.new_blob, f.path, cfg.context, cfg.ignore_whitespace, cfg.tab_width
        );
        if let Some(hit) = app.cache.lock().unwrap().get(&key) {
            return hit.clone();
        }
        let html = Arc::new(render::fragment(&app.git, f, cfg.tab_width));
        app.cache.lock().unwrap().insert(key, html.clone());
        html
    })
    .await;
    match res {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, escape(&e.to_string())).into_response(),
    }
}

#[derive(Deserialize)]
struct ViewedBody {
    viewed: bool,
}

async fn set_viewed(
    State(app): State<Arc<App>>,
    Path((generation, i)): Path<(u64, usize)>,
    Json(body): Json<ViewedBody>,
) -> Response {
    let Some(snap) = app.by_generation(generation) else {
        return (StatusCode::CONFLICT, "stale snapshot").into_response();
    };
    let Some(f) = snap.files.get(i) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Record the blob from the snapshot on screen, not whatever is on disk now.
    let res = app
        .store
        .set(&f.path, &f.new_blob, body.viewed)
        .and_then(|_| app.store.state(&f.path, &f.new_blob));
    match res {
        Ok(v) => Json(serde_json::json!({ "viewed": v.code() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn resnapshot(State(app): State<Arc<App>>) -> Response {
    let res = tokio::task::spawn_blocking(move || {
        let t0 = Instant::now();
        // The agent may have updated its notes file since; stdin can't be re-read.
        let notes = match &app.notes_src {
            Some(p) if p.as_os_str() != "-" => {
                let text = read_notes(p)?;
                if let Some(review) = app.review {
                    app.store.set_notes(review, &text)?;
                }
                Arc::new(crate::notes::parse(&text))
            }
            _ => app.snap().notes.clone(),
        };
        let snap = build_snapshot(
            &app.git,
            &app.cfg,
            &app.target,
            app.base_override.as_deref(),
            notes,
        )?;
        for w in snap.note_warnings() {
            eprintln!("gv: {w}");
        }
        eprintln!(
            "gv: re-snapshot: {} files in {} ms",
            snap.files.len(),
            t0.elapsed().as_millis()
        );
        let generation = snap.generation;
        *app.snap.write().unwrap() = Arc::new(snap);
        app.views.lock().unwrap().clear();
        anyhow::Ok(generation)
    })
    .await;
    match res {
        Ok(Ok(generation)) => Json(serde_json::json!({ "generation": generation })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
