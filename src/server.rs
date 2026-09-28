//! HTTP server: shell page, per-file fragments, review state, re-snapshot.

use crate::config::Config;
use crate::diff::{self, DiffOpts, FileDiff};
use crate::git::Git;
use crate::render::{self, escape};
use crate::store::Store;
use crate::target::{self, Commit, Target};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::Embed;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

#[derive(Embed)]
#[folder = "web/"]
struct Assets;

pub struct Snapshot {
    pub generation: u64,
    pub label: String,
    pub files: Vec<FileDiff>,
    pub commits: Vec<Commit>,
}

pub struct App {
    pub git: Git,
    pub cfg: Config,
    pub store: Store,
    pub target: Target,
    pub base_override: Option<String>,
    pub full: bool,
    pub token: String,
    pub port: u16,
    pub snap: RwLock<Arc<Snapshot>>,
    cache: Mutex<HashMap<String, Arc<String>>>,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Resolve the target, snapshot it, and parse the diff.
pub fn build_snapshot(
    git: &Git,
    cfg: &Config,
    t: &Target,
    base_override: Option<&str>,
) -> Result<Snapshot> {
    let r = target::resolve(t, git, base_override)?;
    let files = diff::diff(
        git,
        &r.base,
        &r.head,
        &DiffOpts {
            context: cfg.context,
            ignore_whitespace: cfg.ignore_whitespace,
        },
    )?;
    Ok(Snapshot {
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        label: r.label,
        files,
        commits: r.commits,
    })
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
        token: String,
        port: u16,
        snap: Snapshot,
    ) -> App {
        App {
            git,
            cfg,
            store,
            target,
            base_override,
            full,
            token,
            port,
            snap: RwLock::new(Arc::new(snap)),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn snap(&self) -> Arc<Snapshot> {
        self.snap.read().unwrap().clone()
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

async fn shell(State(app): State<Arc<App>>) -> Response {
    match tokio::task::spawn_blocking(move || render_shell(&app)).await {
        Ok(Ok(html)) => Html(html).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn render_shell(app: &App) -> Result<String> {
    let snap = app.snap();
    let tw = app.cfg.tab_width;
    let mut mfiles = Vec::with_capacity(snap.files.len());
    for f in &snap.files {
        let v = app.store.state(&f.path, &f.new_blob)?;
        mfiles.push(render::model_file(f, v.code(), tw));
    }
    let total: u32 = snap.files.iter().map(|f| f.add + f.del).sum();
    let default_level = if app.full {
        3
    } else if total as usize > app.cfg.collapse_over_lines {
        1
    } else {
        3
    };
    let model = serde_json::json!({
        "generation": snap.generation,
        "defaultLevel": default_level,
        "files": mfiles,
    });
    // Safe to embed in <script>: no "</" sequences survive.
    let model_json = serde_json::to_string(&model)?.replace("</", "<\\/");

    let tmpl = Assets::get("shell.html").ok_or_else(|| anyhow::anyhow!("missing shell.html"))?;
    let mut env = minijinja::Environment::new();
    env.add_template("shell.html", std::str::from_utf8(&tmpl.data)?)?;
    let files: Vec<_> = snap
        .files
        .iter()
        .zip(&mfiles)
        .enumerate()
        .map(|(i, (f, m))| {
            let (dir, name) = match f.path.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), f.path.clone()),
            };
            minijinja::context! {
                i, path => f.path, dir, name, old_path => f.old_path, status => f.status.to_string(),
                add => f.add, del => f.del, viewed => m.viewed, gd => m.gd,
            }
        })
        .collect();
    let commits: Vec<_> = snap
        .commits
        .iter()
        .map(|c| minijinja::context! { short => c.short, subject => c.subject, author => c.author })
        .collect();
    let repo = app
        .git
        .dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(env.get_template("shell.html")?.render(minijinja::context! {
        repo, label => snap.label, files, commits, total_add => snap.files.iter().map(|f| f.add).sum::<u32>(),
        total_del => snap.files.iter().map(|f| f.del).sum::<u32>(), model_json,
    })?)
}

async fn file_fragment(
    State(app): State<Arc<App>>,
    Path((generation, i)): Path<(u64, usize)>,
) -> Response {
    let snap = app.snap();
    if snap.generation != generation {
        return (StatusCode::CONFLICT, "stale snapshot").into_response();
    }
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
    let snap = app.snap();
    if snap.generation != generation {
        return (StatusCode::CONFLICT, "stale snapshot").into_response();
    }
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
        let snap = build_snapshot(
            &app.git,
            &app.cfg,
            &app.target,
            app.base_override.as_deref(),
        )?;
        eprintln!(
            "gv: re-snapshot: {} files in {} ms",
            snap.files.len(),
            t0.elapsed().as_millis()
        );
        let generation = snap.generation;
        *app.snap.write().unwrap() = Arc::new(snap);
        anyhow::Ok(generation)
    })
    .await;
    match res {
        Ok(Ok(generation)) => Json(serde_json::json!({ "generation": generation })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
