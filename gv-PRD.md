# gv — local diff viewer for large, AI-generated changes

**Status:** PRD v1 · **Owner:** Arturo · **Target:** single Rust binary, browser UI, Linux + macOS

---

## 1. Problem

Reviewing AI-generated changes means reading diffs of 1,000–3,000+ lines, often with individual files that changed by hundreds of lines. Terminal tools (git + delta, Magit, lazygit, tig) work up to roughly 500 lines; past that the problem is *orientation*, not rendering: you lose track of which file and which method you are in. GitHub's "Files changed" view has the spatial affordances a browser gives (proportional scrollbar, sticky headers, find-in-page) but requires pushing first, is slow on large diffs, and has no memory of what you already reviewed when the agent makes a second pass.

## 2. Goals

1. **Orientation inside a multi-thousand-line file.** At any scroll position the reader knows the file *and* the enclosing class/method, without scrolling to find out.
2. **Speed.** A 5,000-line diff is interactive in under 500 ms from `gv` to a usable page. Every subsequent interaction is under 100 ms.
3. **Iterative review.** After the agent's second pass, show only what changed since the last review, per file.
4. **Keyboard-first.** Everything reachable without the mouse.

## 3. Non-goals (v1)

- Writing comments back to GitHub/GitLab.
- Editing files, staging, committing.
- Stacked-PR management (rebasing dependent branches).
- A TUI. The browser is the frontend; the terminal is the launcher.
- Multi-user or remote access. Binds to `127.0.0.1` only.
- Watching the repo. gv reviews a snapshot taken when the command ran; to see newer changes, re-snapshot (`r`) or rerun `gv`.
- Line comments or feedback export to an AI agent.
- Windows.

## 4. Prior art and what we take from each

Research was done across hosted review UIs, IDE diff editors, and terminal tools. Summary of what exists and what does not:

| Feature | Who has it | Our take |
|---|---|---|
| Sticky *file* header | GitHub, GitLab, Gerrit, ReviewStack | Baseline |
| Sticky *enclosing symbol* header inside a diff | Graphite only (TS/Py/Go/JS/Java, not Ruby) | **Core feature, gap in the market** |
| Fold bar listing hidden symbols | VS Code (clickable per symbol), IntelliJ (breadcrumb path) | Copy VS Code's per-symbol reveal |
| Expand to enclosing syntactic block | Reviewable (documented); Gerrit (`+Block`, gated/internal) | Yes, one key |
| Fixed-N expand | GitHub ~20, GitLab 20, Phabricator 20, Gerrit 10, VS Code 20 + drag | Provide as secondary |
| Per-file "changed symbols" outline | GitHub had a "Jump to" dropdown (may be gone post-2026 redesign) | **Core feature** |
| Collapsed region categorized (unchanged / whitespace-only) | Reviewable | Yes, cheap |
| Collapse levels (files / hunk headers / everything) | Magit (`1`–`4`, `M-1`–`M-4`, cycle) | Yes |
| Viewed state that resets when content changes | GitHub, GitLab (resets when file changes), Reviewable, Critique | Key on blob SHA |
| Interdiff between revisions | Gerrit patch sets, Critique snapshots | Yes, via review memory |
| Lazy render with server-supplied row counts + `content-visibility: auto` | GitLab Rapid Diffs | Copy exactly |
| Virtualization | GitHub (only >10k lines) | Not needed if lazy + collapse |
| Moved-code detection | git `--color-moved` (paint only), VS Code `showMoves` (arrows, split only) | Own block-hash detection, in unified view |
| Function context from git `xfuncname` | Magit, delta, tig, lazygit, webdiff, GitLab | Insufficient for Ruby (`class|module|def`, one level, no `end` awareness). Use tree-sitter. |

## 5. CLI

Binary name: `gv`. Every target has a one-letter alias. Bare nouns are accepted when unambiguous.

```
gv                 working tree + unpushed commits vs merge-base with default branch
gv b [name]        branch (default: current) vs merge-base with default branch
gv p <n>           GitHub PR #n (via `gh`); fetches head if missing (`pull/<n>/head`, works for forks)
gv l [n]           last n commits (default 1); n>1 shown as a stack
gv s               "since": <last reviewed sha>..HEAD for current branch; interdiff if rewritten
gv c <sha>         one commit
gv <a>..<b>        raw range, escape hatch

gv 115             → gv p 115
gv 3f2a1c          → gv c 3f2a1c
gv feat/x          → gv b feat/x
```

Flags common to all targets: `--port`, `--no-open`, `--full` (start at collapse level 3), `-w` (ignore whitespace), `--stack` (force per-commit view), `--base <ref>` (override default branch).

Behavior:
- Default branch detection: `origin/HEAD`, else `main`, else `master`, else `--base`.
- Bare `gv` = working tree (staged, unstaged, untracked) + unpushed commits, vs merge-base with default branch.
- Binds `127.0.0.1:0` (random port) unless `--port`. Prints URL (with session token, see §7.5), opens browser, stays in foreground; `Ctrl-C` exits. Also `gv --daemon` for a long-lived instance and `gv` reuses it if running (optional, v1.1).
- Remembers last target per repo, so bare `gv` after `gv p 115` reopens the PR *only if* explicitly configured (`remember_target = true`); default is working tree.
- Never `cd` into the repo; all git calls use `git -C <repo>`.
- Exit code 0 on clean exit; non-zero if the target can't be resolved, with a one-line message.

## 6. UI

### 6.1 Layout

```
┌──────────────┬──────────────────────────────────────────────────┐
│ sidebar      │ [sticky] app/services/foo.rb   +120 −14  [viewed]│
│              │ [sticky] FooService › #call                        │
│ ▸ commit msg │ ─────────────────────────────────────────────────│
│ ▾ files      │  @@ hunk                                          │
│   ▸ migr…    │  ... diff lines ...                               │
│   ▾ models   │  ⋯ 140 lines: #validate · #persist · #notify  [+]│
│     foo.rb ● │  @@ hunk                                          │
│     bar.rb ○ │                                                   │
│   ▸ specs    │                                                   │
│              │                                                   │
│ minimap      │                                                   │
└──────────────┴──────────────────────────────────────────────────┘
```

- **Sidebar**: file tree grouped by configured ordering (see §9), each with `+/−` counts, viewed state (●/○/◐ = changed since viewed), bar height proportional to size. Scroll-spy highlights the current file. Commit message(s) shown as the first entry (Gerrit).
- **Two sticky rows per file**: file header (path, counts, viewed toggle, open-in-editor) and **breadcrumb** (enclosing symbol path at the top visible line, e.g. `FooService › #call › each block`). Breadcrumb segments are clickable → scroll to that symbol.
- **Touched-symbols outline**: expandable row under the file header listing every symbol intersecting a hunk with its own `+/−`, e.g. `#call +12 −3`, `#build_query +40`, `#normalize (new)`. Click to jump.
- **Fold bars** between hunks: `⋯ N lines · <symbols hidden inside>`, plus a category breakdown when applicable (`120 unchanged · 15 whitespace-only`). Click a symbol name to reveal exactly that symbol. Controls: expand 20 up / 20 down / to enclosing block / all.
- **Collapse levels** (Magit): `1` files only, `2` files + hunk headers, `3` everything. Applies globally with `M-1..3`, or to the current file with `1..3`. Diffs over a configurable line count (default 2,000) open at level 1.
- **Per-file minimap**: thin gutter strip with colored ticks for hunks against total file length; click to jump.
- **Full-file mode** (`f` on a file): entire new file with added/changed lines tinted, removed lines as inline ghost rows. For files that are more rewrite than edit.
- **Moved blocks**: removed/added block pairs detected as moves are rendered dimmed with a "moved from/to line N" link, in the unified view. Whitespace-insensitive matching.
- **Word-level intraline diff** on changed-line pairs.
- **Long lines wrap** (no horizontal scroll). Continuation rows get a wrap marker in the gutter and no line number. Wrapped lines stay monospace; tabs are expanded server-side to the configured width so column math stays exact.
- **Snapshot, no live updates**: the diff is frozen at the moment `gv` runs (see §8 for working-tree snapshots). gv never watches the repo. `r` re-snapshots in place: re-resolves the target, recomputes the diff, and keeps scroll position on the same file where possible.

### 6.2 Keyboard

| Key | Action |
|---|---|
| `j` / `k` | next / prev hunk |
| `J` / `K` | next / prev file |
| `n` / `N` | next / prev *unviewed* file |
| `v` | toggle viewed on current file (and collapse it) |
| `V` | mark viewed and go to next unviewed |
| `1` `2` `3` | collapse level for current file |
| `M-1` `M-2` `M-3` | global collapse level |
| `Tab` / `S-Tab` | cycle current section / cycle all (Magit) |
| `e` | expand hunk to enclosing block |
| `E` | expand all context in file |
| `f` | toggle full-file mode |
| `o` | open current line in editor |
| `w` | toggle ignore whitespace |
| `m` | toggle move detection |
| `s` | toggle stack view (per commit ↔ whole range) |
| `[` / `]` | prev / next commit in stack |
| `/` | filter files |
| `g g` / `G` | top / bottom |
| `r` | re-snapshot (re-read the repo and recompute the diff) |
| `?` | help |

Navigation operates on an in-memory model of files/hunks (shipped as JSON with the page), not the DOM, so it works on files not yet loaded.

**No cursor.** "Current" is defined by the viewport: the current file and hunk are the ones at the top visible line (the same line the breadcrumb describes). `j`/`k`/`J`/`K` scroll the target to the top of the viewport, below the sticky rows. `e`, `1..3`, `v`, `f` act on the current hunk/file. `o` opens the line under the mouse if hovering over a diff line, otherwise the top visible line.

Key handling uses `event.code` (so `M-1` works on macOS, where Option-1 types `¡`) and calls `preventDefault` for bound keys (`/` would otherwise trigger Firefox quick-find). Keys are ignored while an input (e.g. the `/` filter) has focus.

### 6.3 Review memory

- State keyed on `(repo_id, path, new_blob_sha)`. Marking a file viewed stores the blob SHA and timestamp. The SHA recorded is the one from the snapshot on screen, not whatever is on disk at the time of the click.
- On load, each file is: **unviewed**, **viewed** (blob unchanged), or **changed since viewed** (blob differs). For the third state the default diff shown is `git diff <reviewed_blob> <current_blob>` (the interdiff), with a toggle to the full diff vs base.
- `gv s` uses the newest reviewed commit on the branch as the base. If that commit is no longer an ancestor of HEAD (rebase/amend), compute the interdiff by diffing the two patch texts (`base1..head1` vs `base2..head2`) and show per-file "unchanged / modified / new in this revision".
- Reviewed SHAs are pinned with `refs/gv/reviewed/<branch>` so `git gc` keeps them. Working-tree blobs that were marked viewed are pinned the same way (`refs/gv/blobs/<sha>`, or one pin commit whose tree holds them), since they exist only as loose objects written by the snapshot.
- Renames: match by blob first, path second (`-M`).
- Whitespace-only blob change: show as "changed since viewed" but with an empty `-w` interdiff and a one-key re-mark.

## 7. Architecture

Single Rust binary. Server-rendered HTML fragments, small vanilla-JS layer.

```
gv <target>
 ├─ resolve target → (base_sha, head_sha, worktree?: bool, commits: Vec<sha>)
 ├─ if worktree: snapshot (§8) → every file on the new side is a blob SHA
 ├─ git -C repo diff --numstat -M base..head            → file list + sizes
 ├─ git -C repo diff -p -U3 -M base..head               → parse ONCE into
 │      Vec<FileDiff { path, old_path, old_blob, new_blob, hunks: Vec<Hunk> }>
 │      stored in Arc<AppState>
 ├─ open sqlite  ~/.local/share/gv/<repo_id>.sqlite    (review memory, cache)
 ├─ axum on 127.0.0.1:0; open browser
 │
 GET  /                → shell: sidebar + one <section> per file with reserved
 │                       height (see "Height estimation" below) and content-visibility:auto
 GET  /file/{i}        → highlight old+new blobs, outline new blob, word-diff,
 │                       move-detect → HTML fragment (cached by (old_blob,new_blob,opts))
 GET  /file/{i}/full   → full-file mode fragment
 GET  /file/{i}/expand?hunk=h&dir=up|down|block|all
 POST /viewed/{i}      → sqlite upsert
 POST /resnapshot      → re-resolve target, re-snapshot, rebuild AppState
 GET  /model.json      → files/hunks/symbols for keyboard nav

 All fragments are rendered from blobs in the snapshot, never from the live
 worktree, so lazily loaded files always match model.json.
```

### 7.1 Crates

| Role | Crate |
|---|---|
| CLI | `clap` |
| Server | `axum` + `tokio` |
| Templates | `minijinja` (disk-loaded in debug, embedded in release) |
| Static assets | `rust-embed` |
| Git | `std::process::Command` (`git -C`); `gix` optional for blob reads |
| Diff parse | hand-written unified-diff parser (structured hunks) |
| Word diff / patch interdiff | `similar` |
| Highlight | `tree-sitter-highlight` for Ruby/ERB/JS/TS/CSS/YAML/JSON/SQL; `syntect` fallback |
| Outline | `tree-sitter` + grammar crates; `outline.scm`-style queries (start from Zed's Ruby query) |
| State/cache | `rusqlite` (bundled) |
| Parallelism | `rayon` for prefetching adjacent files |
| Browser | `open` |
| GitHub | shell out to `gh pr view <n> --json baseRefName,headRefName,headRefOid` |

### 7.2 Outline pipeline (one parse, three features)

1. Parse new blob with tree-sitter; run outline query → `Vec<Symbol { kind, name, start_row, end_row, parent }>`.
2. Breadcrumb for a viewport line = chain of symbols whose range contains it, outermost→innermost.
3. Touched symbols = symbols whose range intersects any hunk's new-side range; counts from hunk lines within the range.
4. Fold-bar symbols = symbols fully inside a skipped region.
5. "Expand to block" = extend hunk to the innermost symbol range containing it.

Ruby query must handle: `class`, `module`, `class << self`, `def`, `def self.x`, `private def x`, `define_method(:x)`, and Rails/RSpec block DSL (`scope :x`, `describe`, `context`, `it`) as leaf items. Fall back to git's `xfuncname` text when no grammar is available.

### 7.3 Performance budget

| Step | Budget (5,000-line diff, 60 files) |
|---|---|
| Process start → server listening | < 50 ms |
| Diff parse | < 20 ms |
| Shell page (numstat only) | < 100 ms TTFB |
| Per-file fragment (3,000-line blobs, cold) | < 150 ms |
| Per-file fragment (cached) | < 5 ms |
| Keyboard action | < 16 ms (one frame) |

Techniques: parse once; render per file on demand; reserve estimated heights (below) so the scrollbar is stable before any file loads; `content-visibility: auto` on file sections; prefetch ±2 files with `rayon`; cache fragments by `(old_blob, new_blob, options)` in SQLite; no highlighting for blobs > 20,000 lines or lines > 3,000 chars (fall back to plain); skip intraline diff for lines > 300 chars.

**Height estimation with wrapping.** Wrapping means height depends on viewport width, so the server can't send final heights. Instead:

1. The parsed diff is complete at startup, so `model.json` carries, per file, the display width of every visible row (tabs expanded, East Asian wide chars counted as 2 via `unicode-width`), run-length or bucketed to keep it small, plus the count of fixed rows (hunk headers, fold bars).
2. The client measures one monospace character width and the code column width, then computes `rows = Σ max(1, ceil(width / cols))` per file and sets `contain-intrinsic-size` on each section. This is exact for monospace text, so estimates should match rendered height.
3. After a fragment renders, its measured height replaces the estimate (`contain-intrinsic-size: auto <h>` keeps it when it scrolls offscreen). Any residual correction above the viewport is absorbed with scroll anchoring (`overflow-anchor`, plus a manual `scrollBy` for corrections the browser doesn't anchor), so visible content never jumps.
4. On resize, recompute estimates for unrendered sections; debounce 100 ms.

### 7.4 Filesystem

- `~/.local/share/gv/<repo_id>.sqlite` — review memory, fragment cache, last target. XDG paths on both Linux and macOS (`$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` respected).
- `repo_id`: a random id stored in `<git-common-dir>/gv-id`, created on first run. Lives in the common dir, so all worktrees of a repo share it, and survives moving the repo. A fresh clone gets a new id (acceptable: review state is blob-keyed).
- `~/.config/gv/config.toml` — global defaults.
- `<repo>/.gv.toml` — per-repo config (see §9).
- **Repo write rule:** `git status` stays clean. Writes under `.git/` are allowed and limited to: `gv-id`, snapshot objects (`hash-object -w`), `refs/gv/*`, and objects/refs from `gv p` fetches. gv never writes to the working tree or index.

### 7.5 Security

- Binds `127.0.0.1` only.
- Random per-session token in the URL (`http://127.0.0.1:PORT/?t=…`), exchanged for an `HttpOnly; SameSite=Strict` cookie; every request without it gets 403.
- `Host` header must be `127.0.0.1:PORT` or `localhost:PORT` (DNS-rebinding defense). POSTs also check `Origin`.
- Settings that run commands (`editor.command`) are honored **only in the global config**. If a repo's `.gv.toml` sets one, it is ignored with a one-line warning. Reviewing an untrusted PR must never execute code from it.
- `o` runs the editor command via argv (no shell); `{path}` is validated to be inside the repo.

## 8. Git details

- Diff generation: `git -C <repo> diff -p -U3 -M --no-color --no-ext-diff [--diff-algorithm=histogram] [-w] base..head`.
- Working-tree snapshot: at startup (and on `r`), collect changed files (`git diff --name-only base`) plus untracked files (`git ls-files --others --exclude-standard`), write each with `git hash-object -w --stdin-paths`, and build a snapshot tree with a temporary index (`GIT_INDEX_FILE=<tmp> git read-tree HEAD` + `update-index` + `write-tree`; the real index is never touched). The diff is then `git diff base <snapshot_tree>`, a plain tree-to-tree diff; untracked files appear as additions. The snapshot tree is what everything downstream reads.
- Numstat: `git diff --numstat -M`.
- Blobs: `git cat-file --batch` (one process, many blobs) or `gix`.
- Stack: `git log --first-parent --reverse --format=%H base..head`; per-commit `git diff-tree -p -M <sha>`; cumulative `git diff base..<sha>`.
- Function context: `git diff -W` for the "expand to block" fallback when no tree-sitter grammar.
- Never rely on `--color-moved` for move detection (paint only); implement block matching on trimmed lines.

## 9. Configuration (`.gv.toml`)

```toml
default_target = "worktree"      # or "branch"
remember_target = false
collapse_over_lines = 2000       # open at level 1 above this
context = 3
ignore_whitespace = false
tab_width = 4                    # tabs expanded server-side; needed for wrap/height math

[order]                          # file ordering groups, first match wins
groups = [
  "db/migrate/**",
  "app/models/**",
  "app/services/**",
  "app/controllers/**",
  "app/views/**",
  "app/javascript/**",
  "spec/**", "test/**",
]

[collapse]                       # collapsed by default, one-click to open
patterns = ["db/schema.rb", "*.lock", "Gemfile.lock", "**/__snapshots__/**", "*.min.*"]

[editor]                         # GLOBAL CONFIG ONLY (~/.config/gv/config.toml); ignored in .gv.toml, see §7.5
command = "zed://file/{path}:{line}"   # or "cursor://...", or "nvim --server $NVIM --remote +{line} {path}"
```

## 10. Milestones

**M1 — Readable (weekend 1)**
`gv`, `gv b`, `gv c`, `gv l`; diff parse; sidebar with numstat; working-tree snapshot; lazy per-file fragments with reserved heights; syntect highlighting; sticky file header; `j/k/J/K/v/1/2/3/r`; viewed state by blob SHA in SQLite; session token + Host check.

**M2 — Oriented (weekend 2)**
tree-sitter outline for Ruby + JS/TS; sticky breadcrumb; touched-symbols row; fold bars with symbols; expand-to-block; per-file minimap; collapse patterns and ordering config.

**M3 — Iterative (weekend 3)**
`gv s`; changed-since-viewed with blob-to-blob interdiff; rewritten-history interdiff; `refs/gv/reviewed`; `gv p` via `gh`.

**M4 — Polish**
Stack view; move detection; full-file mode; word-level diff; `-w` toggle; editor links; `cargo-dist` release with Homebrew tap.

## 11. Acceptance tests

1. A 5,000-line, 60-file diff renders a scrollable shell in < 100 ms TTFB; scrolling (or `G` / sidebar click) to the last file lands on it without layout shift, including when files contain long wrapped lines and after resizing the window.
2. Scrolling inside a 3,000-line Ruby file always shows the correct `Class › #method` breadcrumb at the top of the viewport.
3. Marking a file viewed, then changing one line and reloading, shows the file as "changed since viewed" and the default diff contains exactly that line.
4. After `git rebase -i` squashing two commits, `gv s` shows no per-file changes for files whose content didn't change.
5. All keyboard actions in §6.2 work with the mouse unplugged, including on files not yet loaded.
6. `gv` inside a repo with a different `.ruby-version` / `Gemfile` / `node_modules` works identically (no toolchain leakage).
7. Killing `gv` with Ctrl-C leaves `git status` clean and the index unchanged (writes under `.git/` per §7.4 are allowed).
8. Editing a worktree file after `gv` starts does not change what the page shows, including for files not yet loaded; `r` picks up the edit.
9. A `.gv.toml` containing `editor.command` does not cause that command to run on `o`. A request to the server without the session token, or with a foreign `Host`, gets 403.

## 12. Open questions

- `gv s` when nothing has been reviewed yet on the branch: fall back to `gv b` silently, or say so?
- Should viewed state be per-branch or per-repo? (Blob-keyed makes it mostly moot; the `refs/gv/reviewed/<branch>` pin is per-branch.)
- Grammar set to bundle by default vs. lazy download.

## 13. References

- GitHub, diff-line performance: https://github.blog/engineering/architecture-optimization/the-uphill-climb-of-making-diff-lines-performant/
- GitLab Rapid Diffs (server row counts + content-visibility): https://docs.gitlab.com/development/fe_guide/rapid_diffs/
- Reviewable file view (expand to syntactic unit, categorized collapse): https://docs.reviewable.io/files.html
- Graphite pinned function declarations: https://graphite.com/blog/graphite-changelog-5-16-23
- VS Code hidden-regions feature (symbols in fold bar): https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/widget/diffEditor/features/hideUnchangedRegionsFeature.ts
- IntelliJ fold separator breadcrumbs: https://github.com/JetBrains/intellij-community/blob/master/platform/diff-impl/src/com/intellij/diff/tools/util/FoldingModelSupport.java
- Zed Ruby outline query: https://raw.githubusercontent.com/zed-extensions/ruby/main/languages/ruby/outline.scm
- Magit section visibility: https://magit.vc/manual/magit/Section-Visibility.html
- git userdiff drivers: https://github.com/git/git/blob/master/userdiff.c
- delta hunk-header rendering (syntect reference): https://github.com/dandavison/delta/blob/main/src/handlers/hunk_header.rs
- difftastic tree-sitter bundling: https://github.com/Wilfred/difftastic/blob/master/src/parse/tree_sitter_parser.rs
- difit (closest existing local viewer): https://github.com/yoshiko-pg/difit
