//! Review sessions (`gv review`): line comments, a summary and a verdict.
//! Submitting pins the reviewed snapshot, prints the review to stdout for
//! the agent that started gv, and shuts the server down.

use crate::diff::Kind;
use crate::server::App;
use crate::store::Comment;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use std::fmt::Write;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/review/comment", post(add_comment))
        .route("/review/comment/{id}", post(edit_comment))
        .route("/review/summary", post(save_summary))
        .route("/review/submit", post(submit))
}

fn err(e: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
}

fn not_reviewing() -> Response {
    (
        StatusCode::NOT_FOUND,
        "not a review session; start gv with `gv review`",
    )
        .into_response()
}

#[derive(Deserialize)]
struct NewComment {
    generation: u64,
    /// File, hunk and row index in that snapshot. `r0` (default `r`) is the
    /// first row of a multi-line block; blocks stay within one hunk.
    i: usize,
    h: usize,
    r: usize,
    r0: Option<usize>,
    body: String,
}

async fn add_comment(State(app): State<Arc<App>>, Json(c): Json<NewComment>) -> Response {
    let Some(review) = app.review else {
        return not_reviewing();
    };
    if c.body.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty comment").into_response();
    }
    let Some(snap) = app.by_generation(c.generation) else {
        return (StatusCode::CONFLICT, "stale snapshot").into_response();
    };
    let Some((f, hunk, line)) = snap
        .files
        .get(c.i)
        .and_then(|f| f.hunks.get(c.h).map(|h| (f, h)))
        .and_then(|(f, h)| h.lines.get(c.r).map(|l| (f, h, l)))
    else {
        return (StatusCode::NOT_FOUND, "no such line").into_response();
    };
    let r0 = c.r0.unwrap_or(c.r);
    if r0 > c.r {
        return (StatusCode::BAD_REQUEST, "block must start before it ends").into_response();
    }
    let (side, no, blob) = match line.kind {
        Kind::Del => ("o", line.old_no, &f.old_blob),
        _ => ("n", line.new_no, &f.new_blob),
    };
    let block = &hunk.lines[r0..=c.r];
    // A single line gets a little context above it; a block is shown as-is.
    let from = if r0 == c.r { c.r.saturating_sub(3) } else { r0 };
    let excerpt = hunk.lines[from..=c.r]
        .iter()
        .map(|l| {
            let sign = match l.kind {
                Kind::Add => '+',
                Kind::Del => '-',
                Kind::Ctx => ' ',
            };
            format!("{sign}{}", l.text)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut comment = Comment {
        id: 0,
        path: f.path.clone(),
        side: side.into(),
        line: no,
        blob: blob.clone(),
        span: block.len() as u32,
        loc: location(block),
        excerpt,
        context: if snap.view.is_empty() {
            String::new()
        } else {
            snap.label.clone()
        },
        body: c.body,
    };
    match app.store.add_comment(review, &comment) {
        Ok(id) => {
            comment.id = id;
            Json(comment).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct Body {
    body: String,
}

async fn edit_comment(
    State(app): State<Arc<App>>,
    Path(id): Path<i64>,
    Json(b): Json<Body>,
) -> Response {
    let Some(review) = app.review else {
        return not_reviewing();
    };
    match app.store.edit_comment(review, id, &b.body) {
        Ok(true) => {
            Json(serde_json::json!({ "deleted": b.body.trim().is_empty() })).into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct Summary {
    summary: String,
}

async fn save_summary(State(app): State<Arc<App>>, Json(s): Json<Summary>) -> Response {
    let Some(review) = app.review else {
        return not_reviewing();
    };
    match app.store.set_summary(review, &s.summary) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct Submit {
    verdict: String,
    summary: String,
}

async fn submit(State(app): State<Arc<App>>, Json(s): Json<Submit>) -> Response {
    let Some(review) = app.review else {
        return not_reviewing();
    };
    if !matches!(
        s.verdict.as_str(),
        "approve" | "request_changes" | "comment"
    ) {
        return (
            StatusCode::BAD_REQUEST,
            "verdict must be approve, request_changes or comment",
        )
            .into_response();
    }
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        // The review covers the whole target snapshot, whichever view it was written in.
        let main = app.snap();
        let tree = app
            .git
            .rev(&format!("{}^{{tree}}", main.head))
            .ok_or_else(|| anyhow::anyhow!("can't resolve reviewed tree"))?;
        let pin = app
            .git
            .pin_review(&main.branch, &tree, main.head_commit.as_deref())?;
        let sub = app.store.submit(review, &s.verdict, &s.summary, &pin)?;
        let comments = app.store.comments(review)?;
        let md = format_review(
            sub.number,
            &s.verdict,
            &main.branch,
            &pin,
            &s.summary,
            &comments,
        );

        let dir = app.git.common_dir().join("gv/reviews");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!(
            "{}-{}.md",
            main.branch.replace('/', "-"),
            sub.number
        ));
        std::fs::write(&file, &md)?;

        *app.output.lock().unwrap() = Some(format!("{md}\nSaved to {}\n", file.display()));
        app.done.notify_one();
        Ok(serde_json::json!({ "number": sub.number, "file": file.display().to_string() }))
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(e),
        Err(e) => err(e.into()),
    }
}

/// Where a block of rows is, for humans and agents: new-file line numbers
/// when the block has any, else old ones (removed lines).
pub fn location(rows: &[crate::diff::Line]) -> String {
    let range = |nos: Vec<u32>| -> Option<String> {
        let (a, b) = (*nos.iter().min()?, *nos.iter().max()?);
        Some(if a == b {
            a.to_string()
        } else {
            format!("{a}-{b}")
        })
    };
    let new = range(
        rows.iter()
            .filter(|l| l.kind != Kind::Del)
            .map(|l| l.new_no)
            .collect(),
    );
    let old = range(
        rows.iter()
            .filter(|l| l.kind == Kind::Del)
            .map(|l| l.old_no)
            .collect(),
    );
    match (new, old) {
        (Some(n), None) => n,
        (Some(n), Some(o)) => format!("{n} (replacing removed old lines {o})"),
        (None, Some(o)) if rows.len() == 1 => {
            format!("{o} (removed line; line number in the old version)")
        }
        (None, Some(o)) => format!("{o} (removed lines; line numbers in the old version)"),
        (None, None) => String::new(),
    }
}

/// The review as markdown for an agent: verdict, summary, then each comment
/// with its location and the diff lines it refers to.
pub fn format_review(
    number: i64,
    verdict: &str,
    branch: &str,
    pin: &str,
    summary: &str,
    comments: &[Comment],
) -> String {
    let title = match verdict {
        "approve" => "approved",
        "request_changes" => "changes requested",
        _ => "comments",
    };
    let short: String = pin.chars().take(8).collect();
    let mut o = String::new();
    let _ = writeln!(
        o,
        "<gv-review number=\"{number}\" verdict=\"{verdict}\" branch=\"{branch}\" reviewed=\"{short}\" comments=\"{}\">",
        comments.len()
    );
    let _ = writeln!(o, "# Review {number} on {branch}: {title}\n");
    let summary = summary.trim();
    let _ = writeln!(
        o,
        "{}\n",
        if summary.is_empty() {
            "(no summary)"
        } else {
            summary
        }
    );
    if !comments.is_empty() {
        let _ = writeln!(o, "## Comments\n");
    }
    for (n, c) in comments.iter().enumerate() {
        let loc = if !c.loc.is_empty() {
            c.loc.clone()
        } else if c.side == "o" {
            format!("{} (removed line; line number in the old version)", c.line)
        } else {
            c.line.to_string()
        };
        let ctx = if c.context.is_empty() {
            String::new()
        } else {
            format!(" — written while viewing {}", c.context)
        };
        let _ = writeln!(o, "### {}. {}:{loc}{ctx}\n", n + 1, c.path);
        let fence = if c.excerpt.contains("~~~") {
            "````"
        } else {
            "~~~"
        };
        let _ = writeln!(o, "{fence}diff\n{}\n{fence}\n", c.excerpt);
        let _ = writeln!(o, "{}\n", c.body.trim());
    }
    o.push_str("</gv-review>\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_review_for_agents() {
        let c = Comment {
            id: 1,
            path: "app/foo.rb".into(),
            side: "n".into(),
            line: 42,
            blob: "abc".into(),
            span: 1,
            loc: "42".into(),
            excerpt: " def x\n+  y".into(),
            context: String::new(),
            body: "Rename this.".into(),
        };
        let md = format_review(
            2,
            "request_changes",
            "feat/x",
            "0123456789",
            "Mostly good.",
            &[c],
        );
        assert!(md.starts_with("<gv-review number=\"2\" verdict=\"request_changes\" branch=\"feat/x\" reviewed=\"01234567\" comments=\"1\">"));
        assert!(md.contains("# Review 2 on feat/x: changes requested"));
        assert!(md.contains("### 1. app/foo.rb:42\n\n~~~diff\n def x\n+  y\n~~~\n\nRename this."));
        assert!(md.trim_end().ends_with("</gv-review>"));
    }

    fn row(kind: Kind, old_no: u32, new_no: u32) -> crate::diff::Line {
        crate::diff::Line {
            kind,
            old_no,
            new_no,
            text: String::new(),
        }
    }

    #[test]
    fn block_locations() {
        use Kind::*;
        assert_eq!(location(&[row(Add, 0, 7)]), "7");
        assert_eq!(
            location(&[row(Del, 5, 0)]),
            "5 (removed line; line number in the old version)"
        );
        assert_eq!(
            location(&[row(Ctx, 9, 10), row(Add, 0, 11), row(Add, 0, 12)]),
            "10-12"
        );
        assert_eq!(
            location(&[row(Del, 20, 0), row(Del, 21, 0), row(Add, 0, 20)]),
            "20 (replacing removed old lines 20-21)"
        );
        assert_eq!(
            location(&[row(Del, 3, 0), row(Del, 4, 0)]),
            "3-4 (removed lines; line numbers in the old version)"
        );
    }
}
