mod config;
mod diff;
mod git;
mod highlight;
mod notes;
mod render;
mod review;
mod server;
mod store;
mod target;
mod triage;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const TARGETS: &str = "\
Targets:
  (none)          Working tree + unpushed commits vs the default branch
  b [name]        Branch (default: current) vs the default branch
  c <sha>         One commit
  l [n]           Last n commits (default: 1)
  <a>..<b>        Range; <a>...<b> diffs from their merge-base
  <branch|sha>    Bare branch names and commit shas work too
  s               Changes since the last submitted review
  review [target] Review session: comment, then submit; the review is
                  printed to stdout and gv exits (for agents)
  p <n>           GitHub PR (coming in M3)

Examples:
  gv                      review what the agent just did
  gv b feat/x --base develop
  gv l 3 --full
  gv -C ~/code/app main..HEAD
  gv review s             re-review only what changed since last time
  gv review --notes n.md  open a review with the agent's notes on its work

In the browser, press ? for keyboard shortcuts.";

/// Local diff viewer for large, AI-generated changes.
#[derive(Parser)]
#[command(name = "gv", version, after_help = TARGETS)]
struct Cli {
    /// What to review (see Targets below).
    #[arg(value_name = "TARGET")]
    target: Vec<String>,
    /// Port to bind on 127.0.0.1 (default: random).
    #[arg(long)]
    port: Option<u16>,
    /// Don't open a browser.
    #[arg(long)]
    no_open: bool,
    /// Start with every file fully expanded (collapse level 3).
    #[arg(long)]
    full: bool,
    /// Ignore whitespace changes.
    #[arg(short = 'w', long = "ignore-whitespace")]
    ignore_whitespace: bool,
    /// Override the default branch used as the comparison base.
    #[arg(long)]
    base: Option<String>,
    /// Agent notes on the change: markdown, `## path[:a-b] [check|mechanical]`
    /// sections after an intro ("-" reads stdin). Shown inline, and used to
    /// order files.
    #[arg(long, value_name = "FILE")]
    notes: Option<PathBuf>,
    /// Run as if gv was started in <path>.
    #[arg(short = 'C', value_name = "PATH")]
    repo: Option<PathBuf>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("gv: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let t0 = Instant::now();
    highlight::warm();

    let start = match &cli.repo {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let git = git::Git::discover(&start)?;
    let mut cfg = config::load(&git.dir)?;
    if cli.ignore_whitespace {
        cfg.ignore_whitespace = true;
    }
    // `gv review [target]`: a review session whose result goes to stdout.
    let reviewing = cli.target.first().is_some_and(|a| a == "review");
    let target_args = if reviewing {
        &cli.target[1..]
    } else {
        &cli.target[..]
    };
    let target = target::parse(target_args, &git)?;
    let notes_text = cli.notes.as_deref().map(server::read_notes).transpose()?;
    let notes = Arc::new(notes::parse(notes_text.as_deref().unwrap_or("")));
    let mut snap = server::build_snapshot(&git, &cfg, &target, cli.base.as_deref(), notes)?;
    let store = store::Store::open(&store::repo_id(&git)?)?;
    let token = store::random_hex(16)?;
    let review = if reviewing {
        let id = store.draft_review(&snap.branch)?;
        // New notes replace the draft's; without any, a resumed draft keeps its own.
        match &notes_text {
            Some(t) => store.set_notes(id, t)?,
            None => {
                let saved = notes::parse(&store.notes(id)?);
                if !saved.is_empty() {
                    snap.set_notes(Arc::new(saved), &cfg);
                }
            }
        }
        Some(id)
    } else {
        None
    };
    for w in snap.note_warnings() {
        eprintln!("gv: {w}");
    }

    let (add, del) = snap
        .files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.add, d + f.del));
    eprintln!(
        "gv: {} · {} files, +{add} −{del} · ready in {} ms",
        snap.label,
        snap.files.len(),
        t0.elapsed().as_millis()
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", cli.port.unwrap_or(0)))
            .await
            .context("binding 127.0.0.1")?;
        let port = listener.local_addr()?.port();
        let app = Arc::new(server::App::new(
            git,
            cfg,
            store,
            target,
            cli.base,
            cli.full,
            cli.notes,
            token.clone(),
            port,
            snap,
            review,
        ));
        let url = format!("http://127.0.0.1:{port}/?t={token}");
        println!("{url}");
        if !cli.no_open
            && let Err(e) = open::that_detached(&url)
        {
            eprintln!("gv: couldn't open a browser ({e}); open the URL above");
        }
        let waiter = app.clone();
        axum::serve(listener, server::router(app.clone()))
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = waiter.done.notified() => {}
                }
            })
            .await?;
        if let Some(review) = app.output.lock().unwrap().take() {
            print!("{review}");
        } else if reviewing {
            eprintln!("gv: review not submitted; the draft is saved and `gv review` resumes it");
            std::process::exit(2);
        }
        anyhow::Ok(())
    })
}
