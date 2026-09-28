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
    /// File, hunk and row index in that snapshot.
    i: usize,
    h: usize,
    r: usize,
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
    let (side, no, blob) = match line.kind {
        Kind::Del => ("o", line.old_no, &f.old_blob),
        _ => ("n", line.new_no, &f.new_blob),
    };
    let excerpt = hunk.lines[c.r.saturating_sub(3)..=c.r]
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
        let side = if c.side == "o" {
            " (removed line; line number in the old version)"
        } else {
            ""
        };
        let ctx = if c.context.is_empty() {
            String::new()
        } else {
            format!(" — written while viewing {}", c.context)
        };
        let _ = writeln!(o, "### {}. {}:{}{side}{ctx}\n", n + 1, c.path, c.line);
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
}
