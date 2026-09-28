mod config;
mod diff;
mod git;
mod highlight;
mod render;
mod server;
mod store;
mod target;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Local diff viewer for large changes.
///
/// Targets: (none) working tree + unpushed commits · b [name] branch ·
/// c <sha> commit · l [n] last n commits · <a>..<b> range.
/// Bare branch names and commit shas are accepted too.
#[derive(Parser)]
#[command(name = "gv", version)]
struct Cli {
    /// What to review (see above).
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
    let target = target::parse(&cli.target, &git)?;
    let snap = server::build_snapshot(&git, &cfg, &target, cli.base.as_deref())?;
    let store = store::Store::open(&store::repo_id(&git)?)?;
    let token = store::random_hex(16)?;

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
            token.clone(),
            port,
            snap,
        ));
        let url = format!("http://127.0.0.1:{port}/?t={token}");
        println!("{url}");
        if !cli.no_open
            && let Err(e) = open::that_detached(&url)
        {
            eprintln!("gv: couldn't open a browser ({e}); open the URL above");
        }
        axum::serve(listener, server::router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        anyhow::Ok(())
    })
}
