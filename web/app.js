// gv client: lazy file loading, height estimation, keyboard navigation.
(() => {
  'use strict';

  const M = JSON.parse(document.getElementById('model').textContent);
  const files = M.files;
  const rootStyle = getComputedStyle(document.documentElement);
  const px = (name) => parseFloat(rootStyle.getPropertyValue(name));
  const LH = px('--lh'), HH = px('--hh'), FH = px('--fh'), NH = px('--nh'), GP = px('--gp'), CPR = px('--cpr');

  const main = document.getElementById('main');
  const secs = [...document.querySelectorAll('section.file')];
  const bodies = secs.map((s) => s.querySelector('.body'));
  const items = [...document.querySelectorAll('#tree .fi')];
  const dirs = [...document.querySelectorAll('#tree .dir, #tree .tier')];
  const filter = document.getElementById('filter');
  const help = document.getElementById('help');

  // Viewed and mechanical files (lockfiles, generated, ...) start collapsed.
  const levelFor = (f) => (f.viewed === 'v' || f.tier === 'mech' ? 1 : M.defaultLevel);
  const st = files.map((f) => ({ level: levelFor(f), loaded: false, loading: false, extra: 0 }));
  const R = M.review; // review session (comments, summary) or null
  const NOTES = M.notes || []; // agent notes on files in this view

  // ---- geometry -----------------------------------------------------------

  let cw = 8;          // monospace char width in px
  let mainW = 800;     // width of a diff row
  const colsCache = new Map();

  function measure() {
    cw = document.getElementById('probe').getBoundingClientRect().width / 100;
    mainW = main.clientWidth;
    colsCache.clear();
  }

  // Columns available to code in a file whose gutter has `gd` digits.
  function cols(gd) {
    let c = colsCache.get(gd);
    if (c === undefined) {
      const code = mainW - 2 * (gd * cw + GP) - 2 * cw - CPR;
      c = Math.max(1, Math.floor(code / cw - 0.5));
      colsCache.set(gd, c);
    }
    return c;
  }

  function rowsHeight(f, widths) {
    const c = cols(f.gd);
    let h = 0;
    for (const w of widths) h += LH * Math.max(1, Math.ceil(w / c));
    return h;
  }

  function estimate(i) {
    const f = files[i], lvl = st[i].level;
    if (lvl === 1) return 0;
    if (f.note) return NH;
    let h = 0;
    for (const hk of f.hunks) h += HH + (lvl === 3 ? rowsHeight(f, hk) : 0);
    // Review comments inside the file: measured, since their text wraps freely.
    return h + (lvl === 3 && st[i].loaded ? st[i].extra : 0);
  }

  function sizeBody(i) {
    const b = bodies[i];
    secs[i].style.setProperty('--cols', cols(files[i].gd));
    // No `auto`: a remembered size goes stale on resize, and the estimate is exact.
    if (st[i].loaded) b.style.containIntrinsicSize = `${estimate(i)}px`;
    else b.style.height = `${estimate(i)}px`;
  }

  const docTop = (el) => el.getBoundingClientRect().top + window.scrollY;

  // Index of the section containing document y (binary search).
  function sectionAt(y) {
    let lo = 0, hi = secs.length - 1, ans = 0;
    while (lo <= hi) {
      const m = (lo + hi) >> 1;
      if (docTop(secs[m]) <= y) { ans = m; lo = m + 1; } else hi = m - 1;
    }
    return ans;
  }

  const current = () => (secs.length ? sectionAt(window.scrollY + FH) : -1);

  // Run a DOM change without moving what's on screen: keep the section at the
  // top of the viewport where it is. If that section is the one changing, its
  // top doesn't move, so nothing is corrected.
  function anchored(fn) {
    if (!secs.length) return fn();
    const a = secs[sectionAt(window.scrollY + 1)];
    const before = a.getBoundingClientRect().top;
    fn();
    const delta = a.getBoundingClientRect().top - before;
    if (Math.abs(delta) >= 0.5) window.scrollBy(0, delta);
  }

  // ---- levels & loading -----------------------------------------------------

  function setLevel(i, lvl) {
    if (st[i].level !== lvl) { queueAllLabel(); mmSchedule(); }
    st[i].level = lvl;
    secs[i].classList.remove('lv1', 'lv2', 'lv3');
    secs[i].classList.add(`lv${lvl}`);
    if (st[i].loaded) measureExtra(i);
    sizeBody(i);
    if (lvl > 1) maybeLoad(i);
  }

  const expandLvl = () => (M.defaultLevel === 1 ? 3 : M.defaultLevel);
  const anyOpen = () => st.some((s) => s.level > 1);

  // Set every file's level, keeping the current file's header in view.
  function setAll(lvl) {
    const cur = current();
    files.forEach((_, i) => setLevel(i, lvl));
    if (cur >= 0) window.scrollTo(0, docTop(secs[cur]));
  }

  const toggleAll = () => setAll(anyOpen() ? 1 : expandLvl());

  const allBtn = document.getElementById('allbtn');
  let labelQueued = false;
  function queueAllLabel() {
    if (labelQueued || !allBtn) return;
    labelQueued = true;
    queueMicrotask(() => { labelQueued = false; allBtn.textContent = anyOpen() ? 'Collapse all' : 'Expand all'; });
  }

  function near(i) {
    const r = secs[i].getBoundingClientRect();
    return r.bottom > -2000 && r.top < window.innerHeight + 2000;
  }

  function maybeLoad(i) {
    if (st[i].level > 1 && !st[i].loaded && near(i)) load(i);
  }

  let pending = null; // {i, h}: jump target to re-align once file i loads

  // Load file i's fragment; concurrent calls share one fetch.
  function load(i) {
    const s = st[i];
    return s.loaded ? Promise.resolve() : (s.promise ||= fetchFile(i));
  }

  async function fetchFile(i) {
    const s = st[i];
    s.loading = true;
    let html;
    try {
      const r = await fetch(`/file/${M.generation}/${i}`);
      if (r.status === 409) return reloadAt(files[current()]?.path);
      if (!r.ok) throw new Error(await r.text());
      html = await r.text();
    } catch (e) {
      html = `<div class="note">Failed to load: ${String(e.message).replace(/[<&]/g, '')}</div>`;
    }
    anchored(() => {
      const b = bodies[i];
      b.innerHTML = html;
      b.style.height = '';
      b.classList.add('ld');
      s.loaded = true;
      s.loading = false;
      const top = placeNotes(i);
      if (R) placeComments(i);
      if (top) b.prepend(top); // file-level notes above outdated comments
      measureExtra(i);
      sizeBody(i);
    });
    if (pending && pending.i === i) {
      const p = pending;
      pending = null;
      scrollToHunk(p.i, p.h);
    }
  }

  const io = new IntersectionObserver((entries) => {
    for (const e of entries) if (e.isIntersecting) maybeLoad(+e.target.dataset.i);
  }, { rootMargin: '2000px 0px' });

  // ---- navigation -------------------------------------------------------------

  function hunkTop(i, h) {
    if (st[i].loaded) {
      const el = bodies[i].querySelector(`.hk[data-h="${h}"]`);
      if (el) return docTop(el);
    }
    const f = files[i];
    let y = docTop(bodies[i]);
    for (let k = 0; k < h; k++) y += HH + (st[i].level === 3 ? rowsHeight(f, f.hunks[k]) : 0);
    return y;
  }

  function scrollToFile(i) {
    window.scrollTo(0, docTop(secs[i]));
    maybeLoad(i);
  }

  function scrollToHunk(i, h) {
    window.scrollTo(0, hunkTop(i, h) - FH);
    if (!st[i].loaded) {
      pending = { i, h };
      maybeLoad(i);
    }
  }

  const hasHunks = (i) => st[i].level > 1 && files[i].hunks.length > 0;

  function hunk(dir) {
    if (!secs.length) return;
    const y = window.scrollY + FH;
    const cur = current();
    if (dir > 0) {
      for (let i = cur; i < files.length; i++) {
        if (!hasHunks(i)) continue;
        for (let h = 0; h < files[i].hunks.length; h++) {
          if (hunkTop(i, h) > y + 2) return scrollToHunk(i, h);
        }
      }
    } else {
      for (let i = cur; i >= 0; i--) {
        if (!hasHunks(i)) continue;
        for (let h = files[i].hunks.length - 1; h >= 0; h--) {
          if (hunkTop(i, h) < y - 2) return scrollToHunk(i, h);
        }
      }
    }
  }

  function file(dir) {
    if (!secs.length) return;
    const cur = current();
    if (dir > 0) {
      if (cur + 1 < secs.length) scrollToFile(cur + 1);
    } else if (docTop(secs[cur]) < window.scrollY - 1) {
      scrollToFile(cur);
    } else if (cur > 0) {
      scrollToFile(cur - 1);
    }
  }

  function unviewed(dir, from = current()) {
    for (let k = 1; k <= files.length; k++) {
      const i = (from + dir * k + files.length * 2) % files.length;
      if (files[i].viewed !== 'v') return scrollToFile(i);
    }
  }

  // ---- viewed state ---------------------------------------------------------

  async function setViewed(i, viewed) {
    const r = await fetch(`/viewed/${M.generation}/${i}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ viewed }),
    });
    if (r.status === 409) return reloadAt(files[i].path);
    if (!r.ok) return;
    const v = (await r.json()).viewed;
    files[i].viewed = v;
    secs[i].querySelector('.vw input').checked = v === 'v';
    const dot = items[i].querySelector('.vs');
    dot.className = `vs vs-${v}`;
    const wasAbove = docTop(secs[i]) < window.scrollY;
    setLevel(i, v === 'v' ? 1 : (M.defaultLevel === 1 ? 3 : M.defaultLevel));
    if (v === 'v' && wasAbove) window.scrollTo(0, docTop(secs[i]));
  }

  // ---- re-snapshot ------------------------------------------------------------

  function reloadAt(path) {
    try { if (path) sessionStorage.setItem('gv:at', path); } catch (_) { /* storage may be blocked */ }
    location.reload();
  }

  async function resnapshot() {
    const path = files[current()]?.path;
    document.body.style.cursor = 'progress';
    const r = await fetch('/resnapshot', { method: 'POST' });
    document.body.style.cursor = '';
    if (!r.ok) return alert(`Re-snapshot failed: ${await r.text()}`);
    reloadAt(path);
  }

  // ---- sidebar ----------------------------------------------------------------

  function initBars() {
    const max = Math.max(1, ...files.map((f) => f.add + f.del));
    items.forEach((el, i) => {
      el.querySelector('.ba').style.width = `${(100 * files[i].add) / max}%`;
      el.querySelector('.bd').style.width = `${(100 * files[i].del) / max}%`;
    });
  }

  let lastCur = -1;
  function spy() {
    const cur = current();
    if (cur === lastCur) return;
    if (lastCur >= 0) { items[lastCur]?.classList.remove('cur'); secs[lastCur]?.classList.remove('cur'); }
    lastCur = cur;
    if (cur < 0) return;
    items[cur].classList.add('cur');
    secs[cur].classList.add('cur');
    items[cur].scrollIntoView({ block: 'nearest' });
  }

  function applyFilter() {
    const q = filter.value.trim().toLowerCase();
    items.forEach((el, i) => el.classList.toggle('hidden', q !== '' && !files[i].path.toLowerCase().includes(q)));
    dirs.forEach((d) => d.classList.toggle('hidden', q !== ''));
  }

  // ---- minimap --------------------------------------------------------------------
  // Whole-diff overview at VS Code's scale (a 20px line → 2px). Rows are drawn
  // from the model (width, indent, kind), so unloaded files show too. When the
  // diff is taller than the window, the minimap scrolls in proportion.

  const mm = document.getElementById('minimap');
  const mmCtx = mm && mm.getContext('2d');
  const MS = 2 / LH;
  const MM_COLS = 120; // columns that fit across the minimap
  let mmColors = {};
  let mmQueued = false;

  function mmReadColors() {
    const cs = getComputedStyle(document.documentElement);
    const v = (n) => cs.getPropertyValue(n).trim();
    mmColors = { add: v('--add-fg'), del: v('--del-fg'), ctx: v('--muted'), file: v('--border'), hunk: v('--cur') };
  }

  function mmGeom() {
    const H = window.innerHeight, D = document.documentElement.scrollHeight;
    const travel = Math.max(0, D * MS - H);
    const frac = D > H ? window.scrollY / (D - H) : 0;
    // How far the viewport box moves per pixel of page scroll.
    const ratio = MS - (D > H ? travel / (D - H) : 0);
    return { H, D, off: frac * travel, ratio };
  }

  function mmSchedule() {
    if (mmQueued || !mmCtx) return;
    mmQueued = true;
    requestAnimationFrame(() => { mmQueued = false; mmDraw(); });
  }

  function mmDraw() {
    if (!mmCtx || !secs.length || mm.clientWidth === 0) return; // hidden on narrow screens
    const dpr = window.devicePixelRatio || 1;
    const W = mm.clientWidth, H = mm.clientHeight;
    if (mm.width !== Math.round(W * dpr) || mm.height !== Math.round(H * dpr)) {
      mm.width = Math.round(W * dpr);
      mm.height = Math.round(H * dpr);
    }
    const c = mmCtx;
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, W, H);
    const { off } = mmGeom();
    const y0 = off / MS, y1 = (off + H) / MS; // document range the minimap shows
    const cpx = (W - 8) / MM_COLS;
    const line = LH * MS;

    for (let i = sectionAt(y0); i < secs.length; i++) {
      const top = docTop(secs[i]);
      if (top > y1) break;
      const f = files[i];
      c.globalAlpha = 1;
      c.fillStyle = mmColors.file;
      c.fillRect(0, top * MS - off, W, Math.max(1, FH * MS));
      if (st[i].level === 1 || f.note) continue;
      const cl = cols(f.gd);
      let y = docTop(bodies[i]);
      for (let h = 0; h < f.hunks.length && y <= y1; h++) {
        c.globalAlpha = 0.18;
        c.fillStyle = mmColors.hunk;
        c.fillRect(0, y * MS - off, W, HH * MS);
        y += HH;
        if (st[i].level < 3) continue;
        const ws = f.hunks[h], ks = f.kinds[h], ind = f.indent[h];
        for (let r = 0; r < ws.length && y <= y1; r++) {
          const n = Math.max(1, Math.ceil(ws[r] / cl)), rh = n * LH;
          if (y + rh >= y0) {
            const k = ks[r], my = y * MS - off;
            const color = k === 'a' ? mmColors.add : k === 'd' ? mmColors.del : mmColors.ctx;
            if (k !== 'c') {
              c.globalAlpha = 0.16;
              c.fillStyle = color;
              c.fillRect(0, my, W, rh * MS);
            }
            c.globalAlpha = k === 'c' ? 0.4 : 0.9;
            c.fillStyle = color;
            for (let v = 0; v < n; v++) { // one bar per wrapped visual line
              const start = v === 0 ? ind[r] : 0;
              const end = Math.min(ws[r] - v * cl, cl, MM_COLS);
              if (end > start) c.fillRect(4 + start * cpx, my + v * line + 0.4, (end - start) * cpx, line - 0.8);
            }
          }
          y += rh;
        }
      }
    }
    // Viewport box.
    const by = window.scrollY * MS - off, bh = H * MS;
    c.globalAlpha = 0.14;
    c.fillStyle = mmColors.ctx;
    c.fillRect(0, by, W, bh);
    c.globalAlpha = 0.6;
    c.strokeStyle = mmColors.ctx;
    c.lineWidth = 1;
    c.strokeRect(0.5, Math.round(by) + 0.5, W - 1, Math.max(1, Math.round(bh) - 1));
    c.globalAlpha = 1;
  }

  function initMinimap() {
    if (!mmCtx) return;
    mmReadColors();
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { mmReadColors(); mmSchedule(); });
    let drag = null;
    mm.addEventListener('pointerdown', (e) => {
      const { off, H, ratio } = mmGeom();
      const boxTop = window.scrollY * MS - off;
      // Outside the viewport box: jump so the clicked point is centered.
      if (e.offsetY < boxTop || e.offsetY > boxTop + H * MS) window.scrollTo(0, (e.offsetY + off) / MS - H / 2);
      drag = { y: e.clientY, scroll: window.scrollY, ratio };
      mm.setPointerCapture(e.pointerId);
      e.preventDefault();
    });
    mm.addEventListener('pointermove', (e) => {
      if (drag && drag.ratio > 1e-6) window.scrollTo(0, drag.scroll + (e.clientY - drag.y) / drag.ratio);
    });
    const end = () => { drag = null; };
    mm.addEventListener('pointerup', end);
    mm.addEventListener('pointercancel', end);
    mmSchedule();
  }

  // ---- agent notes ------------------------------------------------------------------
  // Notes the agent wrote about its own change (--notes). A note on lines a..b
  // hangs under the last of those new-file lines shown in the diff; a note on
  // the whole file, or on lines the diff doesn't show, goes at the top.

  const noteRows = new WeakMap();
  const TAGS = { check: 'look closely', mechanical: 'mechanical' };

  function noteEl(n, where) {
    const el = document.createElement('div');
    el.className = `anote t-${n.tag}`;
    el.dataset.note = n.heading;
    el.innerHTML = `<div class="cmt-h"><b>Agent</b><span class="tag"></span><span class="where"></span><span class="sp"></span>${R ? '<button type="button" data-act="reply">Reply</button>' : ''}<button type="button" data-act="nprev" title="Previous agent note (A)">‹</button><button type="button" data-act="nnext" title="Next agent note (a)">›</button></div><div class="cmt-b"></div>`;
    el.querySelector('.tag').textContent = TAGS[n.tag] || '';
    el.querySelector('.where').textContent = where;
    el.querySelector('.cmt-b').textContent = n.body;
    return el;
  }

  // Inline notes are placed now; returns the box for the top of the file, if any.
  function placeNotes(i) {
    const mine = NOTES.filter((n) => n.path === files[i].path);
    if (!mine.length) return null;
    const rows = [...bodies[i].querySelectorAll('.l')].filter((r) => !r.classList.contains('d'));
    const box = document.createElement('div');
    box.className = 'anotes';
    for (const n of mine) {
      const span = n.a === n.b ? `line ${n.a}` : `lines ${n.a}–${n.b}`;
      const hit = n.a ? rows.filter((r) => { const no = +r.children[1].textContent; return no >= n.a && no <= n.b; }) : [];
      const el = noteEl(n, !n.a ? '' : hit.length ? span : `${span}, not shown in this diff`);
      if (hit.length) {
        noteRows.set(el, hit);
        insertAfterRow(hit[hit.length - 1], el);
      } else box.append(el);
      if (R) el.after(...R.comments.filter((c) => c.note === n.heading).map((c) => replyEl(c)));
    }
    return box.childElementCount ? box : null;
  }

  const isReplyTo = (c, i) => c.note && NOTES.some((n) => n.heading === c.note && n.path === files[i].path);

  function replyEl(c) {
    const el = commentEl(c, false);
    el.classList.add('reply');
    el.querySelector('.where').textContent = 'reply to the agent';
    return el;
  }

  // Run a DOM change in file i keeping the view steady, or directly for the
  // notes card above the files (i < 0), which should grow in place.
  function inFile(i, fn) {
    if (i < 0) return fn();
    anchored(() => { fn(); refit(i); });
  }
  const fileOf = (el) => { const sec = el.closest('section.file'); return sec ? +sec.dataset.i : -1; };

  // A note in a file (.anote) or in the card above the files (.inote).
  function openReply(noteEl) {
    const i = fileOf(noteEl);
    let at = noteEl; // below the note and any replies it already has
    while (at.nextElementSibling && at.nextElementSibling.matches('.cmt.reply')) at = at.nextElementSibling;
    let form;
    const close = () => inFile(i, () => form.remove());
    form = commentForm('', async (body) => {
      const res = await post('/review/reply', { generation: M.generation, note: noteEl.dataset.note, body });
      if (res.status === 409) return reloadAt(files[i]?.path);
      if (!res.ok) throw new Error(await res.text());
      const c = await res.json();
      R.comments.push(c);
      inFile(i, () => form.replaceWith(replyEl(c)));
      updateCounts();
    }, close);
    form.classList.add('reply');
    inFile(i, () => at.after(form));
  }

  // a / A: next / previous agent note. Steps from the note it last landed on
  // while the page hasn't scrolled since, else from the top of the viewport.
  // Collapsed and unloaded files are expanded and loaded on the way.
  const hasNotes = files.map((f) => NOTES.some((n) => n.path === f.path));
  let noteAt = null; // { el, y: scrollY after landing }

  async function note(dir) {
    const from = noteAt && noteAt.el.isConnected && Math.abs(window.scrollY - noteAt.y) < 2 ? noteAt.el : null;
    const ref = from ? docTop(from) : window.scrollY + FH;
    const start = sectionAt(ref);
    for (let i = start; i >= 0 && i < files.length; i += dir) {
      if (!hasNotes[i]) continue;
      const open = st[i].loaded && st[i].level === 3;
      // A collapsed file at the top is behind us going up: its notes are below its header.
      if (!open && i === start && dir < 0) continue;
      if (!open) {
        setLevel(i, 3);
        await load(i);
      }
      const els = [...bodies[i].querySelectorAll('.anote')];
      const el = open
        ? (dir > 0 ? els.find((e) => docTop(e) > ref + 1) : els.reverse().find((e) => docTop(e) < ref - 1))
        : (dir > 0 ? els[0] : els[els.length - 1]);
      if (!el) continue;
      // Leave room above for the lines the note is about.
      window.scrollTo(0, docTop(el) - Math.max(FH, window.innerHeight * 0.4));
      noteAt?.el.classList.remove('flash');
      noteAt = { el, y: window.scrollY };
      el.classList.remove('flash');
      void el.offsetWidth; // restart the animation
      el.classList.add('flash');
      return;
    }
  }

  function initNotes() {
    // Replies already written to notes in the card above the files.
    if (R) document.querySelectorAll('#intro .inote').forEach((el) => {
      el.after(...R.comments.filter((c) => c.note === el.dataset.note).map((c) => replyEl(c)));
    });
    main.addEventListener('click', (e) => {
      const act = e.target.dataset && e.target.dataset.act;
      const el = act && e.target.closest('.anote, .inote');
      if (!el) return;
      if (act === 'reply' && R) openReply(el);
      if (act === 'nprev' || act === 'nnext') {
        noteAt = { el, y: window.scrollY }; // step from this note, wherever it is on screen
        note(act === 'nnext' ? 1 : -1);
      }
    });
    document.getElementById('noteprev')?.addEventListener('click', (e) => { note(-1); e.currentTarget.blur(); });
    document.getElementById('notenext')?.addEventListener('click', (e) => { note(1); e.currentTarget.blur(); });
    // Hovering a note shows the lines it's about.
    main.addEventListener('mouseover', (e) => {
      const el = e.target.closest('.anote');
      if (el && !el.classList.contains('hl')) { el.classList.add('hl'); markRows(noteRows.get(el) || [], 'rhl', true); }
    });
    main.addEventListener('mouseout', (e) => {
      const el = e.target.closest('.anote');
      if (el && !el.contains(e.relatedTarget)) { el.classList.remove('hl'); markRows(noteRows.get(el) || [], 'rhl', false); }
    });
  }

  // ---- review comments ------------------------------------------------------------
  // Comments anchor to (path, side, line, blob): side "n" is a new-file line,
  // "o" a removed line. A comment whose line isn't in this view's diff (other
  // view, or the file changed) is listed at the top of its file as outdated.

  let reviewDone = false;
  const post = (url, body) => fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });

  function measureExtra(i) {
    const b = bodies[i];
    st[i].extra = 0;
    if (st[i].level !== 3 || !b.querySelector('.cmt, .cmt-form, .cmt-out, .anote')) return;
    const prev = b.style.contentVisibility;
    b.style.contentVisibility = 'visible'; // offscreen sections must really lay out
    const h = b.getBoundingClientRect().height;
    b.style.contentVisibility = prev;
    st[i].extra = Math.max(0, h - estimate(i));
  }

  function rowFor(i, c) {
    const f = files[i];
    if (c.blob !== (c.side === 'n' ? f.new_blob : f.old_blob)) return null;
    for (const row of bodies[i].querySelectorAll('.l')) {
      const del = row.classList.contains('d');
      if (c.side === 'n' && !del && row.children[1].textContent === String(c.line)) return row;
      if (c.side === 'o' && del && row.children[0].textContent === String(c.line)) return row;
    }
    return null;
  }

  // Insert after the row and any comments/forms already hanging off it.
  function insertAfterRow(row, el) {
    let at = row;
    while (at.nextElementSibling && at.nextElementSibling.matches('.cmt, .cmt-form, .anote')) at = at.nextElementSibling;
    at.after(el);
  }

  function commentEl(c, outdated) {
    const el = document.createElement('div');
    el.className = 'cmt';
    el.dataset.id = c.id;
    const loc = c.loc || `${c.side === 'o' ? 'old ' : ''}${c.line}`;
    const where = c.note ? `reply to note: ${c.note}` : outdated || c.span > 1 ? `line${c.span > 1 ? 's' : ''} ${loc}` : '';
    el.innerHTML = `<div class="cmt-h"><b>You</b><span class="where"></span><span class="sp"></span>
      <button type="button" data-act="edit">Edit</button><button type="button" data-act="del">Delete</button></div><div class="cmt-b"></div>`;
    el.querySelector('.where').textContent = where;
    el.querySelector('.cmt-b').textContent = c.body;
    return el;
  }

  function placeComments(i) {
    const mine = R.comments.filter((c) => c.path === files[i].path);
    if (!mine.length) return;
    const out = [];
    for (const c of mine) {
      if (isReplyTo(c, i)) continue; // placed with its note
      const row = c.note ? null : rowFor(i, c);
      if (row) insertAfterRow(row, commentEl(c, false));
      else out.push(c);
    }
    if (out.length) {
      const box = document.createElement('div');
      box.className = 'cmt-out';
      out.forEach((c) => box.append(commentEl(c, true)));
      bodies[i].prepend(box);
    }
  }

  function refit(i) {
    measureExtra(i);
    sizeBody(i);
    mmSchedule();
  }

  function updateCounts() {
    if (!R) return;
    const n = R.comments.length;
    document.getElementById('rvcount').textContent = n ? `(${n})` : '';
    items.forEach((el, i) => {
      const k = R.comments.filter((c) => c.path === files[i].path).length;
      let cc = el.querySelector('.cc');
      if (!k) { cc?.remove(); return; }
      if (!cc) { cc = document.createElement('span'); cc.className = 'cc'; el.querySelector('.nm').after(cc); }
      cc.textContent = `💬${k}`;
    });
  }

  // A comment form; `onSave(text)` resolves to the element that replaces it.
  function commentForm(initial, onSave, onCancel) {
    const form = document.createElement('div');
    form.className = 'cmt-form';
    form.innerHTML = `<textarea placeholder="Leave a comment for the agent"></textarea>
      <div class="btns"><span class="hint">⌘/Ctrl+Enter to save · Esc to cancel</span>
      <button type="button" data-act="cancel">Cancel</button><button type="button" class="primary" data-act="save">Comment</button></div>`;
    const ta = form.querySelector('textarea');
    ta.value = initial;
    const save = async () => {
      if (!ta.value.trim()) return;
      form.querySelector('[data-act=save]').disabled = true;
      try { await onSave(ta.value); } catch (e) { alert(`Saving failed: ${e.message}`); form.querySelector('[data-act=save]').disabled = false; }
    };
    form.addEventListener('click', (e) => {
      const act = e.target.dataset && e.target.dataset.act;
      if (act === 'save') save();
      if (act === 'cancel') onCancel();
    });
    ta.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); save(); }
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onCancel(); }
    });
    queueMicrotask(() => ta.focus());
    return form;
  }

  const hunkRows = (hk) => [...hk.children].filter((el) => el.classList.contains('l'));
  const markRows = (rows, cls, on) => rows.forEach((r) => r.classList.toggle(cls, on));

  // Rows from a to b (either order) within their hunk.
  function blockRows(a, b) {
    const rows = hunkRows(a.closest('.hk'));
    let x = rows.indexOf(a), y = rows.indexOf(b);
    if (x > y) [x, y] = [y, x];
    return rows.slice(x, y + 1);
  }

  // The rows a posted comment covers: its anchor row and span-1 rows above.
  function commentRows(el) {
    let row = el.previousElementSibling;
    while (row && !row.classList.contains('l')) row = row.previousElementSibling;
    const c = R.comments.find((x) => x.id === +el.dataset.id);
    if (!row || !c || c.note) return [];
    const rows = hunkRows(row.closest('.hk'));
    const end = rows.indexOf(row);
    return rows.slice(Math.max(0, end - (c.span || 1) + 1), end + 1);
  }

  function openNewComment(startRow, endRow) {
    const rows = blockRows(startRow, endRow);
    const first = rows[0], last = rows[rows.length - 1];
    const sec = last.closest('section.file'), i = +sec.dataset.i;
    const hk = last.closest('.hk'), h = +hk.dataset.h;
    const all = hunkRows(hk);
    const r0 = all.indexOf(first), r = all.indexOf(last);
    markRows(rows, 'rsel', true); // keep the block highlighted while writing
    let form;
    const close = () => anchored(() => { markRows(rows, 'rsel', false); form.remove(); refit(i); });
    form = commentForm('', async (body) => {
      const res = await post('/review/comment', { generation: M.generation, i, h, r, r0, body });
      if (res.status === 409) return reloadAt(files[i].path);
      if (!res.ok) throw new Error(await res.text());
      const c = await res.json();
      R.comments.push(c);
      anchored(() => { markRows(rows, 'rsel', false); form.replaceWith(commentEl(c, false)); refit(i); });
      updateCounts();
    }, close);
    anchored(() => { insertAfterRow(last, form); refit(i); });
  }

  function onCommentAction(el, act) {
    const id = +el.dataset.id, i = fileOf(el);
    const c = R.comments.find((x) => x.id === id);
    if (!c) return;
    if (act === 'del') {
      if (!confirm('Delete this comment?')) return;
      post(`/review/comment/${id}`, { body: '' }).then((res) => {
        if (!res.ok) return;
        R.comments.splice(R.comments.indexOf(c), 1);
        inFile(i, () => el.remove());
        updateCounts();
      });
    } else if (act === 'edit') {
      let form;
      const back = () => inFile(i, () => form.replaceWith(el));
      form = commentForm(c.body, async (body) => {
        const res = await post(`/review/comment/${id}`, { body });
        if (!res.ok) throw new Error(await res.text());
        c.body = body;
        el.querySelector('.cmt-b').textContent = body;
        back();
      }, back);
      inFile(i, () => el.replaceWith(form));
    }
  }

  function initReview() {
    if (!R) return;
    document.body.classList.add('reviewing');
    main.addEventListener('click', (e) => {
      const act = e.target.dataset && e.target.dataset.act;
      const cmt = e.target.closest('.cmt');
      if (cmt && (act === 'edit' || act === 'del')) return onCommentAction(cmt, act);
    });

    // Line numbers: click for one line; drag, or click then Shift-click, for
    // a block (within one hunk, like GitHub).
    let drag = null, anchor = null;
    main.addEventListener('mousedown', (e) => {
      const num = e.button === 0 && e.target.closest('.l > .o, .l > .n');
      if (!num) return;
      e.preventDefault(); // no text selection while dragging
      const row = num.parentElement;
      const sameHunk = anchor && anchor.isConnected && anchor.closest('.hk') === row.closest('.hk');
      const start = e.shiftKey && sameHunk ? anchor : row;
      drag = { start, rows: blockRows(start, row), end: row };
      markRows(drag.rows, 'rsel', true);
    });
    main.addEventListener('mouseover', (e) => {
      if (!drag) return;
      const row = e.target.closest('.l');
      if (!row || row === drag.end || row.closest('.hk') !== drag.start.closest('.hk')) return;
      markRows(drag.rows, 'rsel', false);
      drag.end = row;
      drag.rows = blockRows(drag.start, row);
      markRows(drag.rows, 'rsel', true);
    });
    window.addEventListener('mouseup', () => {
      if (!drag) return;
      const d = drag;
      drag = null;
      markRows(d.rows, 'rsel', false);
      anchor = d.start;
      openNewComment(d.start, d.end);
    });
    // Hovering a comment shows which lines it covers.
    main.addEventListener('mouseover', (e) => {
      const el = e.target.closest('.cmt');
      if (el && !el.classList.contains('hl')) { el.classList.add('hl'); markRows(commentRows(el), 'rhl', true); }
    });
    main.addEventListener('mouseout', (e) => {
      const el = e.target.closest('.cmt');
      if (el && !el.contains(e.relatedTarget)) { el.classList.remove('hl'); markRows(commentRows(el), 'rhl', false); }
    });

    const dlg = document.getElementById('rvdlg');
    const sum = document.getElementById('rvsum');
    sum.value = R.summary || '';
    let saveT = 0;
    sum.addEventListener('input', () => { clearTimeout(saveT); saveT = setTimeout(() => post('/review/summary', { summary: sum.value }), 400); });

    function openDialog() {
      const list = document.getElementById('rvlist');
      list.replaceChildren(...R.comments.map((c) => {
        const a = document.createElement('a');
        a.href = '#';
        a.innerHTML = '<code></code> ';
        a.querySelector('code').textContent = c.note === '(intro)' ? '↩ intro' : c.note ? `↩ ${c.note}` : `${c.path}:${c.line}`;
        a.append(c.body.split('\n')[0]);
        a.addEventListener('click', (e) => {
          e.preventDefault();
          dlg.hidden = true;
          const i = files.findIndex((f) => f.path === c.path);
          if (i >= 0) { if (st[i].level === 1) setLevel(i, 3); scrollToFile(i); } else if (c.note) window.scrollTo(0, 0);
        });
        return a;
      }));
      if (!R.comments.length) list.textContent = 'No line comments.';
      document.getElementById('rverr').textContent = '';
      dlg.hidden = false;
      sum.focus();
    }
    document.getElementById('finish').addEventListener('click', openDialog);
    document.getElementById('rvcancel').addEventListener('click', () => { dlg.hidden = true; });
    // Submitting waits 3 s so a slip can be undone; Escape cancels it too.
    let countdown = null;
    const btns = dlg.querySelector('.rvbtns'), wait = document.getElementById('rvwait');
    const lock = (on) => {
      btns.hidden = on;
      wait.hidden = !on;
      sum.disabled = on;
      dlg.querySelectorAll('input[name=verdict]').forEach((r) => { r.disabled = on; });
    };
    function cancelSubmit() {
      clearInterval(countdown);
      countdown = null;
      lock(false);
      document.getElementById('rvsubmit').focus();
    }
    dlg.addEventListener('click', (e) => { if (e.target === dlg && !countdown) dlg.hidden = true; });
    // Escape wherever focus is (clicking the card's text leaves it on <body>);
    // capture phase, so it wins over the textarea's blur-on-Escape.
    document.addEventListener('keydown', (e) => {
      if (e.key !== 'Escape' || dlg.hidden) return;
      e.preventDefault();
      e.stopPropagation();
      if (countdown) cancelSubmit(); else dlg.hidden = true;
    }, true);
    document.getElementById('rvundo').addEventListener('click', cancelSubmit);

    document.getElementById('rvsubmit').addEventListener('click', () => {
      if (countdown) return;
      document.getElementById('rverr').textContent = '';
      let left = 3;
      const secs = document.getElementById('rvsecs');
      secs.textContent = left;
      lock(true);
      document.getElementById('rvundo').focus();
      countdown = setInterval(() => {
        left -= 1;
        secs.textContent = left;
        if (left > 0) return;
        clearInterval(countdown);
        submitNow();
      }, 1000);
    });

    async function submitNow() {
      const verdict = dlg.querySelector('input[name=verdict]:checked').value;
      document.getElementById('rvundo').disabled = true; // too late to cancel
      let res;
      try {
        res = await post('/review/submit', { verdict, summary: sum.value });
        if (!res.ok) throw new Error(await res.text());
      } catch (e) {
        document.getElementById('rvundo').disabled = false;
        cancelSubmit();
        document.getElementById('rverr').textContent = `Submit failed: ${e.message}`;
        return;
      }
      const out = await res.json();
      reviewDone = true;
      dlg.hidden = true;
      document.getElementById('rvfile').textContent = `Review ${out.number} · saved to ${out.file}`;
      document.getElementById('rvdone').hidden = false;
      // Browsers only let a page close a tab with no back history; if this
      // one refuses, the "Review submitted" card stays up instead.
      window.close();
    }
    updateCounts();
  }

  // ---- keyboard -----------------------------------------------------------------

  let lastG = 0;
  function onKey(e) {
    if (reviewDone) return;
    if (e.target.closest && e.target.closest('input, textarea, [contenteditable]')) {
      if (e.key === 'Escape') { e.target.blur(); e.preventDefault(); }
      if (e.key === 'Enter' && e.target === filter) {
        const first = items.findIndex((el) => !el.classList.contains('hidden'));
        if (first >= 0) { filter.blur(); scrollToFile(first); }
      }
      return;
    }
    if (e.metaKey || e.ctrlKey) return; // leave browser shortcuts alone
    if (!help.hidden && e.key !== '?') { if (e.key === 'Escape') help.hidden = true; return; }

    // Digits by physical key so Alt+1 works on macOS (where it types "¡").
    const digit = /^Digit([123])$/.exec(e.code);
    if (digit) {
      const lvl = +digit[1];
      const cur = current();
      if (cur < 0) return;
      if (e.shiftKey || e.altKey) setAll(lvl);
      else {
        setLevel(cur, lvl);
        if (docTop(secs[cur]) < window.scrollY) window.scrollTo(0, docTop(secs[cur]));
      }
      e.preventDefault();
      return;
    }
    if (e.altKey) return;
    if (e.key === 'Tab' && e.shiftKey) { toggleAll(); e.preventDefault(); return; }

    const cur = current();
    switch (e.key) {
      case 'j': hunk(1); break;
      case 'k': hunk(-1); break;
      case 'J': file(1); break;
      case 'K': file(-1); break;
      case 'n': unviewed(1); break;
      case 'N': unviewed(-1); break;
      case 'a': note(1); break;
      case 'A': note(-1); break;
      case 'v': if (cur >= 0) setViewed(cur, files[cur].viewed !== 'v'); break;
      case 'V':
        if (cur >= 0) {
          const next = () => unviewed(1, cur);
          if (files[cur].viewed !== 'v') setViewed(cur, true).then(next); else next();
        }
        break;
      case 'g':
        if (Date.now() - lastG < 600) { window.scrollTo(0, 0); lastG = 0; } else lastG = Date.now();
        break;
      case 'G': window.scrollTo(0, document.documentElement.scrollHeight); break;
      case '/': filter.focus(); filter.select(); break;
      case 'r': resnapshot(); break;
      case '[': case ']': {
        // Step through views: All changes → each commit → uncommitted.
        const links = [...document.querySelectorAll('#views .cm')];
        const at = links.findIndex((a) => a.classList.contains('sel'));
        const next = links[at + (e.key === ']' ? 1 : -1)];
        if (next) location.href = next.href;
        break;
      }
      case '?': help.hidden = !help.hidden; break;
      default: return;
    }
    e.preventDefault();
  }

  // ---- wiring -------------------------------------------------------------------

  function onResize() {
    anchored(() => {
      measure();
      files.forEach((_, i) => { if (st[i].loaded) measureExtra(i); sizeBody(i); });
    });
    mmSchedule();
  }

  function init() {
    measure();
    files.forEach((_, i) => setLevel(i, st[i].level));
    queueAllLabel();
    initMinimap();
    initNotes();
    initReview();
    bodies.forEach((b, i) => { b.dataset.i = i; io.observe(b); });
    initBars();

    secs.forEach((s, i) => {
      s.querySelector('.vw input').addEventListener('click', (e) => { e.preventDefault(); setViewed(i, files[i].viewed !== 'v'); });
      const toggle = () => {
        if (String(window.getSelection())) return; // selecting the path to copy it, not toggling
        setLevel(i, st[i].level === 1 ? 3 : 1);
        if (docTop(secs[i]) < window.scrollY) window.scrollTo(0, docTop(secs[i]));
      };
      s.querySelector('.tw').addEventListener('click', toggle);
      s.querySelector('.path').addEventListener('click', toggle);
    });
    // Sidebar click: jump to the file and expand it; clicking the file that's
    // already open at the top collapses it instead.
    items.forEach((el, i) => el.addEventListener('click', (e) => {
      e.preventDefault();
      const atTop = Math.abs(docTop(secs[i]) - window.scrollY) < 2;
      if (atTop && st[i].level > 1) setLevel(i, 1);
      else if (st[i].level === 1) setLevel(i, M.defaultLevel === 1 ? 3 : M.defaultLevel);
      scrollToFile(i);
    }));
    filter.addEventListener('input', applyFilter);
    allBtn?.addEventListener('click', (e) => { e.preventDefault(); toggleAll(); allBtn.blur(); });
    document.getElementById('helpbtn').addEventListener('click', (e) => { e.preventDefault(); help.hidden = false; });
    help.addEventListener('click', (e) => { if (e.target === help) help.hidden = true; });
    document.addEventListener('keydown', onKey);

    let raf = 0;
    window.addEventListener('scroll', () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; spy(); mmDraw(); }); }, { passive: true });
    let rt = 0;
    window.addEventListener('resize', () => { clearTimeout(rt); rt = setTimeout(onResize, 100); });

    let at = null;
    try { at = sessionStorage.getItem('gv:at'); sessionStorage.removeItem('gv:at'); } catch (_) { /* ignore */ }
    const i = at ? files.findIndex((f) => f.path === at) : -1;
    if (i >= 0) scrollToFile(i);
    spy();
  }

  // Measure only after fonts settle, or char width (and every estimate) is off.
  (document.fonts ? document.fonts.ready : Promise.resolve()).then(init);
})();
