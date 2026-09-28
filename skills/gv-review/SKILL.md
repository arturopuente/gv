---
name: gv-review
description: Open a gv review session so the user can review your changes in the browser like a GitHub pull request review, then act on the review they submit. Use when the user asks to open, start or request a review ("open a review", "let me review this in gv", "review session"), and after addressing a review with requested changes, to re-request review.
---

# gv review sessions

`gv` is a local diff viewer. `gv review` opens the current branch's changes in the
user's browser; the user leaves line comments, writes a summary and submits a
verdict. On submit gv prints the review to stdout and exits. Because you start gv
as a background command, you are notified when it exits and receive the review.

Work with the user the way a coworker handles a pull request review.

## 1. Open the review

Run gv in the background (Bash with `run_in_background: true`). It may not be on
the shell's PATH, so prefix it:

```sh
PATH="$HOME/.cargo/bin:$PATH" gv review        # first review: everything on the branch
PATH="$HOME/.cargo/bin:$PATH" gv review s      # re-review: only changes since the last review
```

- Run it from the repository (or pass `-C <repo>`). Other targets work too, e.g.
  `gv review b feat/x`.
- gv opens the browser itself. The first stdout line is the URL: wait for it to
  appear in the background task's output file (use Monitor with an until-loop,
  not repeated sleeps), then give the user the URL and say the review is open.
- Then stop and wait. Do not poll; you will be notified when gv exits.
- If `gv review s` fails with "nothing reviewed yet", use `gv review`.

## 2. When gv exits

**Exit code 0**: stdout holds the review:

```
<gv-review number="2" verdict="request_changes" branch="feat/x" reviewed="0698538a" comments="3">
# Review 2 on feat/x: changes requested
<summary>
## Comments
### 1. path/to/file.rb:42
~~~diff
<the diff lines leading up to the commented line; the last one is it>
~~~
<comment>
...
</gv-review>
```

- `path:line` is a line in the version that was reviewed. A comment marked
  "removed line" refers to a line number in the old version.
- The last line of each excerpt is the exact line commented on. Line numbers may
  have moved if files changed since, so locate code by the excerpt, not only
  the number.

Act on the verdict:

- **request_changes**: address every comment.
  1. Make the changes. If you disagree with a comment or can't do it, don't skip
     it silently: say so in your reply.
  2. Run the relevant tests or checks.
  3. Commit all fixes for this round as one commit: `Address review <number>`,
     with a body listing each comment and what changed.
  4. Re-request review with `gv review s` (background, as above), and tell the
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
