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
  const dirs = [...document.querySelectorAll('#tree .dir')];
  const filter = document.getElementById('filter');
  const help = document.getElementById('help');

  const levelFor = (f) => (f.viewed === 'v' ? 1 : M.defaultLevel);
  const st = files.map((f) => ({ level: levelFor(f), loaded: false, loading: false }));

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
    return h;
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
    st[i].level = lvl;
    secs[i].classList.remove('lv1', 'lv2', 'lv3');
    secs[i].classList.add(`lv${lvl}`);
    sizeBody(i);
    if (lvl > 1) maybeLoad(i);
  }

  function near(i) {
    const r = secs[i].getBoundingClientRect();
    return r.bottom > -2000 && r.top < window.innerHeight + 2000;
  }

  function maybeLoad(i) {
    if (st[i].level > 1 && !st[i].loaded && near(i)) load(i);
  }

  let pending = null; // {i, h}: jump target to re-align once file i loads

  async function load(i) {
    const s = st[i];
    if (s.loaded || s.loading) return;
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

  // ---- keyboard -----------------------------------------------------------------

  let lastG = 0;
  function onKey(e) {
    if (e.target instanceof HTMLInputElement) {
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
      if (e.altKey) files.forEach((_, i) => setLevel(i, lvl));
      else setLevel(cur, lvl);
      if (docTop(secs[cur]) < window.scrollY) window.scrollTo(0, docTop(secs[cur]));
      e.preventDefault();
      return;
    }
    if (e.altKey) return;

    const cur = current();
    switch (e.key) {
      case 'j': hunk(1); break;
      case 'k': hunk(-1); break;
      case 'J': file(1); break;
      case 'K': file(-1); break;
      case 'n': unviewed(1); break;
      case 'N': unviewed(-1); break;
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
      case '?': help.hidden = !help.hidden; break;
      default: return;
    }
    e.preventDefault();
  }

  // ---- wiring -------------------------------------------------------------------

  function onResize() {
    anchored(() => {
      measure();
      files.forEach((_, i) => sizeBody(i));
    });
  }

  function init() {
    measure();
    files.forEach((_, i) => setLevel(i, st[i].level));
    bodies.forEach((b, i) => { b.dataset.i = i; io.observe(b); });
    initBars();

    secs.forEach((s, i) => {
      s.querySelector('.vw input').addEventListener('click', (e) => { e.preventDefault(); setViewed(i, files[i].viewed !== 'v'); });
      s.querySelector('.tw').addEventListener('click', () => {
        setLevel(i, st[i].level === 1 ? 3 : 1);
        if (docTop(secs[i]) < window.scrollY) window.scrollTo(0, docTop(secs[i]));
      });
    });
    items.forEach((el, i) => el.addEventListener('click', (e) => { e.preventDefault(); scrollToFile(i); }));
    filter.addEventListener('input', applyFilter);
    document.getElementById('helpbtn').addEventListener('click', (e) => { e.preventDefault(); help.hidden = false; });
    help.addEventListener('click', (e) => { if (e.target === help) help.hidden = true; });
    document.addEventListener('keydown', onKey);

    let raf = 0;
    window.addEventListener('scroll', () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; spy(); }); }, { passive: true });
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
