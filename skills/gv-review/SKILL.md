---
name: gv-review
description: Open a gv review session so the user can review your changes in the browser like a GitHub pull request review, then act on the review they submit. Use when the user asks to open, start or request a review ("open a review", "let me review this in gv", "review session"), and after addressing a review with requested changes, to re-request review.
argument-hint: "[s | b [branch] | c <sha> | l [n] | <a>..<b>] [--base <ref>] [-w] [--full] [-C <path>]"
---

# gv review sessions

`gv` is a local diff viewer. `gv review` opens the current branch's changes in the
user's browser; the user leaves line comments, writes a summary and submits a
verdict. On submit gv prints the review to stdout and exits. Because you start gv
as a background command, you are notified when it exits and receive the review.

Work with the user the way a coworker handles a pull request review.

## Arguments

Invoked as `/gv-review <args>`, the arguments are exactly what follows
`gv review` on the command line:

| Invocation | Runs |
|---|---|
| `/gv-review` | `gv review` (working tree + unpushed commits vs the default branch) |
| `/gv-review s` | `gv review s` (only changes since the last review) |
| `/gv-review b feat/x --base develop` | `gv review b feat/x --base develop` |
| `/gv-review l 3` | `gv review l 3` |
| `/gv-review main..HEAD -w` | `gv review main..HEAD -w` |

Arguments for this invocation: `$ARGUMENTS`

If that is empty (or still reads as a placeholder because you invoked this
skill yourself), choose them: nothing for a first review, `s` when
re-requesting review after addressing one. Pass the arguments through as
separate words; they must be gv targets and flags (`gv --help` lists them).
Never pass anything else to the shell.

## 1. Write notes for the reviewer

The user reads your notes first and checks your claims against the diff, so
write them before every review, including re-reviews. Write a markdown file
somewhere outside the repo (your scratchpad or `$TMPDIR`):

```markdown
What this change does and why, in a few sentences.

Decisions I made without asking:
- Chose X over Y because ...

## src/billing/charge.rs:40-72 [check]
I wasn't sure whether retries can double-charge; I assumed the gateway
dedupes by idempotency key.

## src/billing/charge.rs:15
Renamed from `do_charge` to match the other handlers.

## src/generated/schema.rs [mechanical]
Regenerated with `make schema`; no hand edits.
```

- Text before the first `## ` heading is the intro: intent, plus every decision
  you made that the user didn't ask for. Be honest; this is where surprises go.
- `## path` or `## path:a-b` notes a file or new-file lines a..b. Paths are
  relative to the repo root.
- Tag `[check]` where the user should look closely: things you were unsure
  about, guessed at, couldn't test, or that could break something else. Flagged
  files are listed first. Don't flag everything; a few real ones beat many.
- Tag `[mechanical]` on files that need no reading (formatting, codegen, pure
  renames). They start collapsed. gv ignores the tag on files it considers
  sensitive (migrations, CI, dependencies, auth, infra).
- On a re-review, say per review comment what you changed, with a note on the
  lines you changed for it.
- Keep it short. Don't narrate the diff line by line.

## 2. Open the review

Run gv in the background (Bash with `run_in_background: true`). It may not be on
the shell's PATH, so prefix it:

```sh
PATH="$HOME/.cargo/bin:$PATH" gv review <args> --notes <notes-file>
```

- Run it from the repository, or pass `-C <repo>` in the arguments.
- gv opens the browser itself. The first stdout line is the URL: wait for it to
  appear in the background task's output file (use Monitor with an until-loop,
  not repeated sleeps), then give the user the URL and say the review is open.
- gv warns on stderr about notes it can't place (a path not in the diff, or
  lines the diff doesn't show). Fix the notes file if the warning shows a
  mistake; pressing `r` in the browser re-reads it.
- Then stop and wait. Do not poll; you will be notified when gv exits.
- If `gv review s` fails with "nothing reviewed yet", use `gv review`.

## 3. When gv exits

**Exit code 0**: stdout holds the review:

```
<gv-review number="2" verdict="request_changes" branch="feat/x" reviewed="0698538a" comments="3">
# Review 2 on feat/x: changes requested
<summary>
## Comments
### 1. path/to/file.rb:42
~~~diff
<a few diff lines ending with the commented line>
~~~
<comment>
### 2. path/to/file.rb:50-58
~~~diff
<exactly the commented block>
~~~
<comment>
...
</gv-review>
```

- `path:line` is a single line and `path:a-b` a block, in the version that was
  reviewed. "removed line(s)" means line numbers in the old version, and
  "replacing removed old lines" marks a block covering both removed and added
  lines.
- `### n. Reply to your note on <heading>` is the user answering one of your
  notes: your note is quoted (`> `), then their reply. `Reply to your intro`
  answers the intro. Treat these like any other comment.
- A single-line excerpt ends with the commented line; a block's excerpt is the
  whole block. Line numbers may have moved if files changed since, so locate
  code by the excerpt, not only the number.

Act on the verdict:

- **request_changes**: address every comment.
  1. Make the changes. If you disagree with a comment or can't do it, don't skip
     it silently: say so in your reply.
  2. Run the relevant tests or checks.
  3. Commit all fixes for this round as one commit: `Address review <number>`,
     with a body listing each comment and what changed.
  4. Re-request review with `gv review s --notes <file>` (background, as
     above, with fresh notes for this round), and tell the
     user, per comment, what you changed. Send the new URL.
- **comment**: answer questions in chat and apply suggestions that are clearly
  asked for. Don't commit or re-request unless the user asks.
- **approve**: acknowledge it and mention any follow-ups. Don't reopen gv.

**Exit code 2**: the user closed gv without submitting. The draft is saved and
`gv review` resumes it. Tell the user; don't retry on your own.

**Exit code 1**: gv failed. Show the user the error from stderr.

## Notes

- Never mark work as reviewed yourself. On submit, gv pins the reviewed snapshot
  under `refs/gv/reviewed/<branch>`; that's what `gv s` and `gv review s` diff
  against.
- Every submitted review is also saved to `.git/gv/reviews/<branch>-<n>.md` in
  the repo's git directory. If the review didn't reach you (for example the
  session restarted), read the newest file there.
- Don't run `gv review` twice at once for the same branch; they share one draft.
