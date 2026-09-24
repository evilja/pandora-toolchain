/* Pandora Subs — the editor, served at /subs/app.js.
   Runs entirely in the browser: files are opened from the device, edited here, autosaved to
   IndexedDB and downloaded (or shared) back. Nothing is sent to the API, so the page needs no
   token. The model lives in ass.js and the preview in render.js; this file is state, history
   and the UI around them. */
(function () {
  "use strict";
  var A = window.ASS, R = window.SubRender;
  var $ = function (id) { return document.getElementById(id); };
  var esc = function (s) {
    return String(s === undefined || s === null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
      .replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  };

  // ---- icons (24-box, 1.8 stroke, currentColor) ------------------------------------
  var IC = {
    menu: '<path d="M4 6h16M4 12h16M4 18h16"/>',
    undo: '<path d="M9 14L4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
    redo: '<path d="M15 14l5-5-5-5"/><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>',
    save: '<path d="M12 3v12"/><path d="M7.5 10.5L12 15l4.5-4.5"/><path d="M4 20h16"/>',
    play: '<path d="M7 4.5v15l12-7.5z"/>',
    pause: '<path d="M8 5v14M16 5v14"/>',
    left: '<path d="M15 5l-7 7 7 7"/>',
    right: '<path d="M9 5l7 7-7 7"/>',
    up: '<path d="M5 15l7-7 7 7"/>',
    down: '<path d="M5 9l7 7 7-7"/>',
    stepb: '<path d="M18 6l-8 6 8 6z"/><path d="M6 6v12"/>',
    stepf: '<path d="M6 6l8 6-8 6z"/><path d="M18 6v12"/>',
    film: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M3 15h18M8 4v16M16 4v16"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/>',
    sort: '<path d="M7 4v16M3.5 16.5L7 20l3.5-3.5"/><path d="M14 6h7M14 12h5M14 18h3"/>',
    select: '<rect x="3.5" y="3.5" width="17" height="17" rx="3"/><path d="M8 12.5l3 3 5-6"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    list: '<path d="M9 6h12M9 12h12M9 18h12"/><path d="M4 6h1M4 12h1M4 18h1"/>',
    edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    video: '<rect x="2.5" y="5" width="14" height="14" rx="2"/><path d="M16.5 10l5-3v10l-5-3"/>',
    styles: '<path d="M5 7V5h14v2"/><path d="M12 5v14"/><path d="M9 19h6"/>',
    tools: '<path d="M4 20L14 10"/><path d="M15 3.5l1.2 2.3 2.3 1.2-2.3 1.2L15 10.5l-1.2-2.3L11.5 7l2.3-1.2z"/><path d="M19.5 13l.7 1.3 1.3.7-1.3.7-.7 1.3-.7-1.3-1.3-.7 1.3-.7z"/>',
    comment: '<path d="M4 5h16v11H9l-5 4z"/><path d="M8 9h8M8 12h5"/>',
    trash: '<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6 7l1 13h10l1-13"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>',
    split: '<circle cx="6" cy="6" r="2.6"/><circle cx="6" cy="18" r="2.6"/><path d="M8.2 7.6L20 18M8.2 16.4L20 6"/>',
    join: '<path d="M4 5v4a3 3 0 0 0 3 3h10a3 3 0 0 1 3 3v4"/><path d="M20 5v4a3 3 0 0 1-3 3"/><path d="M4 19v-4a3 3 0 0 1 3-3"/>',
    above: '<path d="M12 20V10"/><path d="M8 14l4-4 4 4"/><path d="M5 5h14"/>',
    below: '<path d="M12 4v10"/><path d="M8 10l4 4 4-4"/><path d="M5 19h14"/>',
    x: '<path d="M6 6l12 12M18 6L6 18"/>',
    folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    share: '<path d="M12 3v12"/><path d="M8 7l4-4 4 4"/><path d="M5 12v7a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-7"/>',
    gear: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 7.8v.1"/>',
    target: '<circle cx="12" cy="12" r="6.5"/><circle cx="12" cy="12" r="1.5"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3"/>',
    zoomin: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5M11 8v6M8 11h6"/>',
    zoomout: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5M8 11h6"/>',
    wave: '<path d="M3 12h1.5M7 8v8M11 4v16M15 8v8M19 10v4M21.5 12h-1"/>',
    flag: '<path d="M5 21V4"/><path d="M5 4h11l-2 4 2 4H5"/>',
    clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>',
    home: '<path d="M3 12l9-8 9 8"/><path d="M5 10v10h14V10"/><path d="M10 20v-6h4v6"/>',
    file: '<path d="M6 3h8l5 5v13H6z"/><path d="M14 3v5h5"/>',
    link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
    paste: '<rect x="5" y="4" width="14" height="17" rx="2"/><path d="M9 4V3h6v1"/><path d="M9 10h6M9 14h6"/>',
    enter: '<path d="M20 5v7a3 3 0 0 1-3 3H5"/><path d="M9 11l-4 4 4 4"/>',
    loop: '<path d="M4 12a8 8 0 0 1 14-5.3"/><path d="M18 3v4h-4"/><path d="M20 12a8 8 0 0 1-14 5.3"/><path d="M6 21v-4h4"/>',
    setstart: '<path d="M5 4v16"/><path d="M19 12H9M13 8l-4 4 4 4"/>',
    setend: '<path d="M19 4v16"/><path d="M5 12h10M11 8l4 4-4 4"/>',
    palette: '<path d="M12 3a9 9 0 0 0 0 18c1.1 0 1.6-.8 1.6-1.6 0-1.3-1.2-1.6-1.2-2.8 0-.9.7-1.6 1.6-1.6H16a5 5 0 0 0 5-5c0-3.9-4-7-9-7z"/><circle cx="7.5" cy="11" r="1"/><circle cx="10.5" cy="7" r="1"/><circle cx="15" cy="7.5" r="1"/>',
    wand: '<path d="M4 20L14 10"/><path d="M15 3.5l1.2 2.3 2.3 1.2-2.3 1.2L15 10.5l-1.2-2.3L11.5 7l2.3-1.2z"/>',
    shift: '<path d="M3 12h14"/><path d="M13 8l4 4-4 4"/><path d="M21 5v14"/>',
    type: '<path d="M4 7V5h16v2"/><path d="M12 5v14"/><path d="M9 19h6"/>',
    brk: '<path d="M4 6h16M4 12h10a3 3 0 0 1 0 6h-3"/><path d="M13 16l-2 2 2 2"/><path d="M4 18h3"/>',
    broom: '<path d="M14 4l6 6"/><path d="M17 7l-7 7"/><path d="M10 14l-5 1-2 6 6-2 1-5"/>',
    fade: '<path d="M3 18L9 6h6l6 12"/>',
    resize: '<path d="M4 9V4h5M20 15v5h-5M4 4l6 6M20 20l-6-6"/>',
    reading: '<path d="M2 6s3.5-2 10 0 10 0 10 0v12s-3.5 2-10 0-10 0-10 0z"/><path d="M12 6v12"/>',
    abc: '<path d="M3 17l3-10 3 10M4 14h4"/><path d="M11 7v10h3a2.5 2.5 0 0 0 0-5h-3 2.5a2.5 2.5 0 0 0 0-5z"/><path d="M22 9a3 3 0 0 0-5 2v2a3 3 0 0 0 5 2"/>',
    replace: '<circle cx="10" cy="10" r="5.5"/><path d="M14 14l6 6"/><path d="M7.5 10h5"/>',
    keyboard: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h1M9.5 10h1M13 10h1M16.5 10h1M7 14h10"/>'
  };
  function ic(name, size) {
    return '<svg viewBox="0 0 24 24"' + (size ? ' width="' + size + '" height="' + size + '"' : "") + ' aria-hidden="true">' + (IC[name] || "") + "</svg>";
  }

  // ---- settings ----------------------------------------------------------------------
  var SETTINGS_KEY = "pandora_subs_settings";
  var LAST_KEY = "pandora_subs_last";
  var HYDRA_KEY = "pandora_subs_hydra";
  var DEFAULT_SETTINGS = { fps: 24000 / 1001, cpsLimit: 17, lenLimit: 50, enterNext: true, seekOnSelect: true,
    snap: true, waveSpan: 8000, defaultDur: 2000, leadIn: 150, leadOut: 300 };
  var settings = (function () {
    var s = {};
    for (var k in DEFAULT_SETTINGS) s[k] = DEFAULT_SETTINGS[k];
    try {
      var raw = JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}");
      for (var j in raw) if (j in s) s[j] = raw[j];
    } catch (e) {}
    return s;
  })();
  function saveSettings() { try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)); } catch (e) {} }

  // ---- state ------------------------------------------------------------------------
  var S = {
    doc: A.newDoc(), name: "Untitled.ass", projectId: null, dirty: false,
    sel: new Set(), active: null, anchor: null, selMode: false, filter: "",
    view: "lines", side: "tools", time: 0, stopAt: null, caret: 0, posMode: false,
    media: null, wave: null, waveStart: 0, waveSpan: settings.waveSpan
  };
  var H = { undo: [], redo: [], lastKey: null, lastTime: 0 };
  var app = $("app");
  var isWide = function () { return window.matchMedia("(min-width: 1100px)").matches; };
  function toast(msg, tone) { if (window.PN && PN.toast) PN.toast(msg, tone); }

  // ---- derived caches ----------------------------------------------------------------
  // Events are immutable, so anything computed from one can be cached against the object.
  var derived = new WeakMap();
  function info(e) {
    var d = derived.get(e);
    if (!d) {
      d = { cps: A.cps(e), len: A.maxLineLength(e), preview: null };
      derived.set(e, d);
    }
    return d;
  }
  var indexOf = { arr: null, map: null };
  function idx(id) {
    if (indexOf.arr !== S.doc.events) {
      indexOf.map = new Map();
      S.doc.events.forEach(function (e, i) { indexOf.map.set(e.id, i); });
      indexOf.arr = S.doc.events;
    }
    var i = indexOf.map.get(id);
    return i === undefined ? -1 : i;
  }
  var overlapCache = { arr: null, set: null };
  function overlapSet() {
    if (overlapCache.arr !== S.doc.events) { overlapCache.set = A.overlaps(S.doc.events); overlapCache.arr = S.doc.events; }
    return overlapCache.set;
  }
  function activeEvent() { var i = idx(S.active); return i === -1 ? null : S.doc.events[i]; }
  function selectedEvents() { return S.doc.events.filter(function (e) { return S.sel.has(e.id); }); }
  function styleNames() { return S.doc.styles.map(function (s) { return s.Name; }); }
  function fps() { return Number(settings.fps) || 24000 / 1001; }
  function frameMs() { return 1000 / fps(); }

  // ---- history -----------------------------------------------------------------------
  function snapshot(label) {
    return { label: label, info: S.doc.info, styles: S.doc.styles, events: S.doc.events, extras: S.doc.extras,
      sel: Array.from(S.sel), active: S.active };
  }
  function restore(snap) {
    S.doc = { info: snap.info, styles: snap.styles, events: snap.events, extras: snap.extras };
    S.sel = new Set(snap.sel.filter(function (id) { return idx(id) !== -1; }));
    S.active = idx(snap.active) !== -1 ? snap.active : (S.doc.events[0] ? S.doc.events[0].id : null);
  }
  // Every edit goes through here. `key` coalesces a burst of the same edit (typing into one line,
  // dragging one handle) into a single undo step.
  function change(label, fn, key) {
    var t = Date.now();
    var coalesce = key && H.lastKey === key && t - H.lastTime < 1500;
    if (!coalesce) {
      H.undo.push(snapshot(label));
      if (H.undo.length > 300) H.undo.shift();
    }
    H.redo = [];
    H.lastKey = key || null; H.lastTime = t;
    fn();
    if (!S.doc.events.length) S.doc.events = [A.makeEvent({ start: 0, end: settings.defaultDur, style: firstStyle() })];
    if (idx(S.active) === -1) S.active = S.doc.events[0].id;
    S.dirty = true;
    refresh();
    scheduleAutosave();
  }
  function undo() {
    if (!H.undo.length) return;
    var snap = H.undo.pop();
    H.redo.push(snapshot(snap.label));
    restore(snap); H.lastKey = null; S.dirty = true;
    refresh(); scheduleAutosave();
    toast("Undid: " + snap.label);
  }
  function redo() {
    if (!H.redo.length) return;
    var snap = H.redo.pop();
    H.undo.push(snapshot(snap.label));
    restore(snap); H.lastKey = null; S.dirty = true;
    refresh(); scheduleAutosave();
    toast("Redid: " + snap.label);
  }
  function setEvent(id, patch) {
    var i = idx(id);
    if (i === -1) return;
    var evs = S.doc.events.slice();
    evs[i] = A.withEvent(evs[i], patch);
    S.doc.events = evs;
  }
  function mapEvents(ids, fn) {
    var set = ids instanceof Set ? ids : new Set(ids);
    S.doc.events = S.doc.events.map(function (e, i) { return set.has(e.id) ? fn(e, i) : e; });
  }
  function firstStyle() { return S.doc.styles.length ? S.doc.styles[0].Name : "Default"; }
  // The lines a tool acts on: the selection, or every line when asked (or when nothing is selected).
  function scopeIds(scope) {
    if (scope === "all" || !S.sel.size) return new Set(S.doc.events.map(function (e) { return e.id; }));
    if (scope === "later") {
      var first = Math.min.apply(null, Array.from(S.sel).map(idx));
      return new Set(S.doc.events.slice(first).map(function (e) { return e.id; }));
    }
    return new Set(S.sel);
  }

  // ---- render scheduling --------------------------------------------------------------
  // Batched to the next frame; the timer covers a hidden or throttled page, where frames stop but
  // an edit (an undo from a keyboard shortcut, a restore) must still reach the DOM.
  var pending = false;
  function refresh() {
    if (pending) return;
    pending = true;
    var run = function () {
      if (!pending) return;
      pending = false;
      renderBar(); computeRows(); renderList(); renderSelBar(); renderEditor(); renderStyles(); paintTime(true);
    };
    requestAnimationFrame(run);
    setTimeout(run, 100);
  }

  // ---- app bar ------------------------------------------------------------------------
  function renderBar() {
    var title = A.getInfo(S.doc, "Title");
    $("docTitle").textContent = S.name || title || "Untitled";
    var n = S.doc.events.length;
    $("docSub").innerHTML = n + " line" + (n === 1 ? "" : "s") + " · " + S.doc.styles.length + " style" + (S.doc.styles.length === 1 ? "" : "s") +
      (S.dirty ? ' · <span class="sb-dirty">not downloaded</span>' : " · saved");
    $("undoBtn").disabled = !H.undo.length;
    $("redoBtn").disabled = !H.redo.length;
    $("undoBtn").title = H.undo.length ? "Undo " + H.undo[H.undo.length - 1].label + " (Ctrl+Z)" : "Undo";
    $("redoBtn").title = H.redo.length ? "Redo " + H.redo[H.redo.length - 1].label + " (Ctrl+Y)" : "Redo";
    document.title = (S.dirty ? "• " : "") + (S.name || "Subtitles") + " — Pandora Subs";
  }

  // ---- line list (virtualised) -----------------------------------------------------------
  var rows = [], rowH = 62;
  function computeRows() {
    var f = S.filter.trim().toLowerCase();
    rows = [];
    S.doc.events.forEach(function (e, i) {
      if (f) {
        var hay = (A.strippedText(e.text) + "\u0000" + e.actor + "\u0000" + e.style + "\u0000" + e.effect).toLowerCase();
        if (hay.indexOf(f) === -1 && String(i + 1) !== f) return;
      }
      rows.push(i);
    });
  }
  function measureRow() {
    var v = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--sb-row"));
    rowH = v > 0 ? v : 62;
  }
  function previewHtml(text) {
    var out = "";
    A.splitBlocks(text).forEach(function (seg) {
      if (seg.type === "text") out += esc(seg.text).replace(/\\N|\\n/g, "<i> ↵ </i>").replace(/\\h/g, "\u00A0");
      else if (seg.type === "comment") out += "<i>{" + esc(seg.text.slice(0, 24)) + "}</i>";
      else out += "<i>✦</i>";
    });
    return out || '<i>(empty)</i>';
  }
  function fmtDur(ms) { return (ms / 1000).toFixed(2); }
  function renderList() {
    var scroll = $("listScroll"), list = $("list");
    var headH = isWide() ? 28 : 0;
    list.style.height = rows.length * rowH + "px";
    var top = Math.max(0, scroll.scrollTop - headH), h = scroll.clientHeight || 600;
    var from = Math.max(0, Math.floor(top / rowH) - 6), to = Math.min(rows.length, Math.ceil((top + h) / rowH) + 6);
    var ov = overlapSet(), html = "", playing = S.time;
    for (var r = from; r < to; r++) {
      var i = rows[r], e = S.doc.events[i], d = info(e);
      if (d.preview === null) d.preview = previewHtml(e.text);
      var c = d.cps, warn = c > settings.cpsLimit || d.len > settings.lenLimit;
      html += '<div class="sb-lrow" data-id="' + e.id + '" style="top:' + (r * rowH) + 'px"' +
        (S.sel.has(e.id) ? " data-sel" : "") + (S.active === e.id ? " data-active" : "") +
        (e.comment ? " data-comment" : "") + (ov.has(e.id) ? " data-overlap" : "") +
        (!e.comment && e.start <= playing && e.end > playing ? " data-playing" : "") + ">" +
        '<span class="c-chk"></span><span class="c-n">' + (i + 1) + '</span><span class="c-l">' + e.layer + "</span>" +
        '<span class="c-s">' + A.formatTime(e.start) + '</span><span class="c-e">' + A.formatTime(e.end) + "</span>" +
        '<span class="c-d">' + fmtDur(e.end - e.start) + "</span>" +
        '<span class="c-cps"' + (warn ? " data-warn" : "") + ' title="' + (d.len > settings.lenLimit ? "Line longer than " + settings.lenLimit + " characters" : "Characters per second") + '">' + (e.comment ? "—" : Math.round(c)) + "</span>" +
        '<span class="c-st">' + esc(e.style) + '</span><span class="c-a">' + esc(e.actor) + "</span>" +
        '<span class="c-t">' + d.preview + "</span></div>";
    }
    list.innerHTML = html;
    if (!rows.length) list.innerHTML = '<div class="sb-empty"><b>No lines match</b>Clear the filter to see every line.</div>';
  }
  function ensureRowVisible(id) {
    var r = rows.indexOf(idx(id));
    if (r === -1) return;
    var scroll = $("listScroll"), headH = isWide() ? 28 : 0;
    var y = r * rowH + headH, h = scroll.clientHeight;
    if (y < scroll.scrollTop + headH) scroll.scrollTop = y - headH - rowH;
    else if (y + rowH > scroll.scrollTop + h) scroll.scrollTop = y + rowH * 2 - h;
  }

  function setActive(id, opts) {
    opts = opts || {};
    if (idx(id) === -1) return;
    S.active = id;
    if (!opts.keepSel) { S.sel = new Set([id]); S.anchor = id; }
    var e = activeEvent();
    if (e && settings.seekOnSelect && !isPlaying() && !opts.noSeek) seek(e.start);
    if (e) centerWaveOn(e);
    H.lastKey = null;
    refresh();
    if (!opts.noScroll) requestAnimationFrame(function () { ensureRowVisible(id); });
  }

  // Tap selects; tap on the active line opens it (on a phone); long-press or the select button
  // enters multi-select, where taps toggle. Shift/Ctrl work as in any desktop list.
  function initList() {
    var scroll = $("listScroll"), list = $("list");
    scroll.addEventListener("scroll", function () { renderList(); }, { passive: true });
    var press = null, suppress = false;
    list.addEventListener("pointerdown", function (ev) {
      var row = ev.target.closest(".sb-lrow");
      if (!row || ev.pointerType === "mouse") return;
      var id = Number(row.getAttribute("data-id"));
      press = { id: id, x: ev.clientX, y: ev.clientY, timer: setTimeout(function () {
        press = null; suppress = true;
        if (!S.selMode) { S.selMode = true; S.sel = new Set(); }
        toggleSel(id);
        if (navigator.vibrate) try { navigator.vibrate(12); } catch (e) {}
      }, 450) };
    });
    var cancel = function (ev) {
      if (!press) return;
      if (ev.type === "pointermove" && Math.abs(ev.clientX - press.x) < 8 && Math.abs(ev.clientY - press.y) < 8) return;
      clearTimeout(press.timer); press = null;
    };
    list.addEventListener("pointermove", cancel);
    list.addEventListener("pointerup", cancel);
    list.addEventListener("pointercancel", cancel);
    list.addEventListener("contextmenu", function (ev) { if (ev.target.closest(".sb-lrow") && !isWide()) ev.preventDefault(); });
    list.addEventListener("click", function (ev) {
      if (suppress) { suppress = false; return; }
      var row = ev.target.closest(".sb-lrow");
      if (!row) return;
      var id = Number(row.getAttribute("data-id"));
      if (S.selMode && !isWide()) { toggleSel(id); return; }
      if (ev.shiftKey && S.anchor !== null) {
        var a = rows.indexOf(idx(S.anchor)), b = rows.indexOf(idx(id));
        if (a === -1) a = b;
        var lo = Math.min(a, b), hi = Math.max(a, b);
        S.sel = new Set(rows.slice(lo, hi + 1).map(function (i) { return S.doc.events[i].id; }));
        S.active = id; refresh(); return;
      }
      if (ev.ctrlKey || ev.metaKey) { toggleSel(id); S.active = id; S.anchor = id; refresh(); return; }
      if (S.active === id && S.sel.size === 1 && !isWide()) { setView("edit"); return; }
      setActive(id, { noScroll: true });
    });
    list.addEventListener("dblclick", function (ev) {
      if (ev.target.closest(".sb-lrow")) { setView("edit"); var t = $("edText"); if (t) t.focus(); }
    });
    $("listHead").innerHTML = [["#", "index"], ["L", "layer"], ["Start", "start"], ["End", "end"], ["Dur", "duration"],
      ["CPS", "cps"], ["Style", "style"], ["Actor", "actor"], ["Text", "text"]].map(function (c) {
      return '<span data-sort="' + c[1] + '" title="Sort the file by ' + c[0] + '">' + c[0] + "</span>";
    }).join("");
    var lastSort = { key: null, desc: false };
    $("listHead").addEventListener("click", function (ev) {
      var k = ev.target.getAttribute("data-sort");
      if (!k || k === "index") return;
      var desc = lastSort.key === k ? !lastSort.desc : false;
      lastSort = { key: k, desc: desc };
      sortLines(k, desc, "all");
    });
    $("filter").addEventListener("input", function () { S.filter = this.value; computeRows(); $("listScroll").scrollTop = 0; renderList(); });
    $("addFab").addEventListener("click", function () { insertLine(true); setView("edit"); });
    $("selModeBtn").addEventListener("click", function () {
      S.selMode = !S.selMode;
      if (!S.selMode && S.active !== null) S.sel = new Set([S.active]);
      refresh();
    });
    $("sortBtn").addEventListener("click", openSort);
  }
  function toggleSel(id) {
    var s = new Set(S.sel);
    if (s.has(id)) s.delete(id); else s.add(id);
    S.sel = s;
    if (s.size && !s.has(S.active)) S.active = id;
    refresh();
  }

  function renderSelBar() {
    app.setAttribute("data-selmode", S.selMode ? "1" : "0");
    $("selModeBtn").setAttribute("aria-pressed", S.selMode ? "true" : "false");
    var bar = $("selBar");
    var show = S.selMode || S.sel.size > 1;
    bar.hidden = !show;
    if (!show) return;
    bar.innerHTML = "<b>" + S.sel.size + " selected</b>" +
      '<button class="sb-btn sb-sm" data-act="all">All</button>' +
      '<button class="sb-btn sb-sm" data-act="none">None</button>' +
      '<button class="sb-btn sb-sm" data-act="invert">Invert</button>' +
      '<button class="sb-btn sb-sm" data-act="hydra">' + ic("wand") + "Tags</button>" +
      '<button class="sb-btn sb-sm" data-act="style">' + ic("type") + "Style</button>" +
      '<button class="sb-btn sb-sm" data-act="shift">' + ic("shift") + "Shift</button>" +
      '<button class="sb-btn sb-sm" data-act="comment">' + ic("comment") + "Comment</button>" +
      '<button class="sb-btn sb-sm" data-act="dup">' + ic("copy") + "Duplicate</button>" +
      '<button class="sb-btn sb-sm" data-act="join">' + ic("join") + "Join</button>" +
      '<button class="sb-btn sb-sm sb-danger" data-act="delete">' + ic("trash") + "Delete</button>" +
      (S.selMode ? '<button class="sb-btn sb-sm sb-quiet" data-act="done">Done</button>' : "");
  }
  function initSelBar() {
    $("selBar").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-act]");
      if (!b) return;
      var act = b.getAttribute("data-act");
      if (act === "all") { S.sel = new Set(rows.map(function (i) { return S.doc.events[i].id; })); refresh(); }
      else if (act === "none") { S.sel = new Set(); refresh(); }
      else if (act === "invert") {
        var inv = new Set();
        rows.forEach(function (i) { var id = S.doc.events[i].id; if (!S.sel.has(id)) inv.add(id); });
        S.sel = inv; refresh();
      }
      else if (act === "hydra") openHydra();
      else if (act === "style") openSetStyle();
      else if (act === "shift") openShift();
      else if (act === "comment") toggleComment();
      else if (act === "dup") duplicateLines();
      else if (act === "join") joinLines();
      else if (act === "delete") deleteLines();
      else if (act === "done") { S.selMode = false; S.sel = S.active !== null ? new Set([S.active]) : new Set(); refresh(); }
    });
  }

  // ---- line operations ------------------------------------------------------------------
  function insertLine(after) {
    var cur = activeEvent();
    var start = cur ? (after ? cur.end : Math.max(0, cur.start - settings.defaultDur)) : 0;
    var ev = A.makeEvent({ start: start, end: start + settings.defaultDur, style: cur ? cur.style : firstStyle(), actor: cur ? cur.actor : "", layer: cur ? cur.layer : 0 });
    change("Insert line", function () {
      var evs = S.doc.events.slice(), i = cur ? idx(cur.id) + (after ? 1 : 0) : evs.length;
      evs.splice(i, 0, ev);
      S.doc.events = evs;
      S.active = ev.id; S.sel = new Set([ev.id]);
    });
    requestAnimationFrame(function () { ensureRowVisible(ev.id); });
    return ev;
  }
  function duplicateLines() {
    var ids = S.sel.size ? S.sel : new Set([S.active]);
    var added = [];
    change("Duplicate", function () {
      var evs = [];
      S.doc.events.forEach(function (e) {
        evs.push(e);
        if (ids.has(e.id)) { var c = A.cloneEvent(e); added.push(c.id); evs.push(c); }
      });
      S.doc.events = evs;
      S.sel = new Set(added); S.active = added[0];
    });
    toast("Duplicated " + added.length + " line" + (added.length === 1 ? "" : "s"));
  }
  function deleteLines() {
    var ids = S.sel.size ? new Set(S.sel) : new Set([S.active]);
    var first = Math.min.apply(null, Array.from(ids).map(idx));
    change("Delete " + ids.size + " line" + (ids.size === 1 ? "" : "s"), function () {
      S.doc.events = S.doc.events.filter(function (e) { return !ids.has(e.id); });
      var next = S.doc.events[Math.min(first, S.doc.events.length - 1)];
      S.active = next ? next.id : null; S.sel = next ? new Set([next.id]) : new Set();
      S.selMode = false;
    });
    toast("Deleted " + ids.size + " line" + (ids.size === 1 ? "" : "s") + " — Undo to restore");
  }
  function toggleComment() {
    var ids = S.sel.size ? new Set(S.sel) : new Set([S.active]);
    var sel = S.doc.events.filter(function (e) { return ids.has(e.id); });
    var to = !sel.every(function (e) { return e.comment; });
    change(to ? "Comment out" : "Uncomment", function () { mapEvents(ids, function (e) { return A.withEvent(e, { comment: to }); }); });
  }
  function joinLines(sep) {
    var list = selectedEvents();
    if (list.length < 2) {
      var a = activeEvent(), i = idx(S.active);
      if (!a || i + 1 >= S.doc.events.length) { toast("Select two or more lines to join", "bad"); return; }
      list = [a, S.doc.events[i + 1]];
    }
    var joined = A.joinEvents(list, sep || " ");
    var ids = new Set(list.map(function (e) { return e.id; }));
    change("Join " + list.length + " lines", function () {
      var out = [], placed = false;
      S.doc.events.forEach(function (e) {
        if (!ids.has(e.id)) { out.push(e); return; }
        if (!placed) { out.push(joined); placed = true; }
      });
      S.doc.events = out; S.active = joined.id; S.sel = new Set([joined.id]); S.selMode = false;
    });
  }
  function splitAtCursor() {
    var e = activeEvent();
    if (!e) return;
    var ta = $("edText"), off = ta ? ta.selectionStart : S.caret;
    if (!off || off >= e.text.length) { toast("Put the cursor where the line should split", "bad"); return; }
    var parts = A.splitEvent(e, off, null);
    change("Split line", function () {
      var evs = S.doc.events.slice(), i = idx(e.id);
      evs.splice(i, 1, parts[0], parts[1]);
      S.doc.events = evs; S.active = parts[1].id; S.sel = new Set([parts[1].id]);
    });
  }
  function sortLines(key, desc, scope) {
    change("Sort by " + key, function () {
      if (scope === "sel" && S.sel.size > 1) {
        var slots = [], picked = [];
        S.doc.events.forEach(function (e, i) { if (S.sel.has(e.id)) { slots.push(i); picked.push(e); } });
        var sorted = A.sortEvents(picked, key, desc), evs = S.doc.events.slice();
        slots.forEach(function (slot, k) { evs[slot] = sorted[k]; });
        S.doc.events = evs;
      } else S.doc.events = A.sortEvents(S.doc.events, key, desc);
    });
    toast("Sorted by " + key + (desc ? " (descending)" : ""));
  }
  // Aegisub's "commit and next": move on, creating the next line if this was the last, and
  // start an untimed next line where this one ends.
  function nextLine(create) {
    var i = idx(S.active);
    if (i === -1) return;
    if (i + 1 < S.doc.events.length) { setActive(S.doc.events[i + 1].id); return; }
    if (create) insertLine(true);
  }
  function prevLine() {
    var i = idx(S.active);
    if (i > 0) setActive(S.doc.events[i - 1].id);
  }

  // ---- editor --------------------------------------------------------------------------
  var TAGBAR = [
    { l: "\\N", t: "Line break", fn: function () { insertAtCaret("\\N"); } },
    { l: "B", t: "Bold", fn: function () { toggleTag("b"); } },
    { l: "I", t: "Italic", fn: function () { toggleTag("i"); } },
    { l: "U", t: "Underline", fn: function () { toggleTag("u"); } },
    { l: "S", t: "Strikeout", fn: function () { toggleTag("s"); } },
    { l: "an8", t: "Top of screen", fn: function () { toggleAn8(); } },
    { l: "Colour", t: "Colour at the cursor", fn: function () { pickColor(); } },
    { l: "\\pos", t: "Tap the video to position", fn: function () { togglePosMode(); } },
    { l: "\\fad", t: "Fade in and out", fn: function () { applyToActive("Fade", function (t) { return A.setStartTags(t, [A.makeTag("fad", "150,150")]); }); } },
    { l: "\\blur", t: "Soften edges", fn: function () { applyToActive("Blur", function (t) { return A.setStartTags(t, [A.makeTag("blur", "0.6")]); }); } },
    { l: "{ }", t: "Note (comment block)", fn: function () { wrapSelection("{", "}"); } },
    { l: "HYDRA", t: "Tagging tool", fn: function () { openHydra("cursor"); } },
    { l: "Clean", t: "Merge and remove dead tags", fn: function () { applyToActive("Clean tags", A.cleanTags); } },
    { l: "Strip", t: "Remove every tag", fn: function () { applyToActive("Strip tags", function (t) { return A.stripTags(t); }); } }
  ];
  function buildEditor() {
    var body = $("editBody");
    body.innerHTML =
      '<div class="sb-times">' +
      timeBox("Start", "edStart", "start") + timeBox("End", "edEnd", "end") + timeBox("Duration", "edDur", "dur") +
      "</div>" +
      '<div class="sb-row"><div class="sb-field sb-grow"><label class="sb-label" for="edStyle">Style</label><select class="sb-select" id="edStyle"></select></div>' +
      '<div class="sb-field sb-grow"><label class="sb-label" for="edActor">Actor</label><input class="sb-input" id="edActor" list="actorList" autocomplete="off" autocapitalize="words"><datalist id="actorList"></datalist></div></div>' +
      '<div><div class="sb-tagbar" id="tagBar">' + TAGBAR.map(function (b, i) {
        return '<button type="button" data-tb="' + i + '" title="' + esc(b.t) + '">' + esc(b.l) + "</button>";
      }).join("") + "</div>" +
      '<textarea class="sb-textarea sb-text" id="edText" rows="3" spellcheck="true" autocapitalize="sentences" enterkeyhint="next" aria-label="Line text"></textarea>' +
      '<div class="sb-preview" id="edPreview"></div><div class="sb-stats" id="edStats"></div></div>' +
      '<div class="sb-editacts">' +
      '<button class="sb-btn" data-ea="above">' + ic("above") + "Insert before</button>" +
      '<button class="sb-btn" data-ea="below">' + ic("below") + "Insert after</button>" +
      '<button class="sb-btn" data-ea="split">' + ic("split") + "Split</button>" +
      '<button class="sb-btn" data-ea="join">' + ic("join") + "Join next</button>" +
      '<button class="sb-btn" data-ea="dup">' + ic("copy") + "Duplicate</button>" +
      '<button class="sb-btn" data-ea="play">' + ic("play") + "Play line</button>" +
      '<button class="sb-btn" data-ea="break">' + ic("brk") + "Auto break</button>" +
      '<button class="sb-btn sb-danger" data-ea="delete">' + ic("trash") + "Delete</button>" +
      "</div>" +
      '<details class="sb-more" id="edMore"><summary>Layer, margins and effect</summary><div class="sb-stack">' +
      '<div class="sb-grid4"><div class="sb-field"><label class="sb-label" for="edLayer">Layer</label><input class="sb-input" id="edLayer" type="number" inputmode="numeric" min="0"></div>' +
      '<div class="sb-field"><label class="sb-label" for="edML">Margin L</label><input class="sb-input" id="edML" type="number" inputmode="numeric" min="0"></div>' +
      '<div class="sb-field"><label class="sb-label" for="edMR">Margin R</label><input class="sb-input" id="edMR" type="number" inputmode="numeric" min="0"></div>' +
      '<div class="sb-field"><label class="sb-label" for="edMV">Margin V</label><input class="sb-input" id="edMV" type="number" inputmode="numeric" min="0"></div></div>' +
      '<div class="sb-field"><label class="sb-label" for="edEffect">Effect</label><input class="sb-input" id="edEffect" autocomplete="off"></div>' +
      '<p class="sb-hint">Margins of 0 use the style\'s own. Enter moves to the next line (Shift+Enter inserts \\N) — change it in Settings.</p>' +
      "</div></details>";

    var ta = $("edText");
    var remember = function () { S.caret = ta.selectionStart; };
    ["keyup", "click", "select", "blur"].forEach(function (t) { ta.addEventListener(t, remember); });
    ta.addEventListener("input", function () {
      var v = ta.value;
      if (/[\r\n]/.test(v)) {
        // A paste or a keyboard that ignored Enter handling: newlines are \N in ASS.
        var pos = ta.selectionStart;
        var before = v.slice(0, pos).replace(/\r\n|\r|\n/g, "\\N").length;
        v = v.replace(/\r\n|\r|\n/g, "\\N"); ta.value = v; ta.setSelectionRange(before, before);
      }
      var id = S.active;
      change("Edit text", function () { setEvent(id, { text: v }); }, "text:" + id);
      remember();
    });
    ta.addEventListener("keydown", function (ev) {
      if (ev.key !== "Enter" || ev.isComposing) return;
      ev.preventDefault();
      var toNext = settings.enterNext ? !ev.shiftKey : ev.shiftKey || ev.ctrlKey || ev.metaKey;
      if (toNext) nextLine(true); else insertAtCaret("\\N");
    });
    // Mobile keyboards that report Enter only as an input event.
    ta.addEventListener("beforeinput", function (ev) {
      if (ev.inputType !== "insertLineBreak" && ev.inputType !== "insertParagraph") return;
      ev.preventDefault();
      if (settings.enterNext) nextLine(true); else insertAtCaret("\\N");
    });
    $("tagBar").addEventListener("pointerdown", function (ev) { if (ev.target.closest("button")) ev.preventDefault(); });
    $("tagBar").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-tb]");
      if (b) TAGBAR[Number(b.getAttribute("data-tb"))].fn();
    });
    $("edStyle").addEventListener("change", function () { var v = this.value, id = S.active; change("Set style", function () { setEvent(id, { style: v }); }); });
    $("edActor").addEventListener("input", function () { var v = this.value, id = S.active; change("Set actor", function () { setEvent(id, { actor: v }); }, "actor:" + id); });
    $("edEffect").addEventListener("input", function () { var v = this.value, id = S.active; change("Set effect", function () { setEvent(id, { effect: v }); }, "effect:" + id); });
    [["edLayer", "layer"], ["edML", "marginL"], ["edMR", "marginR"], ["edMV", "marginV"]].forEach(function (p) {
      $(p[0]).addEventListener("change", function () {
        var v = Math.max(0, parseInt(this.value, 10) || 0), id = S.active, patch = {};
        patch[p[1]] = v;
        change("Set " + p[1], function () { setEvent(id, patch); });
      });
    });
    ["edStart", "edEnd", "edDur"].forEach(function (fid) {
      var el = $(fid);
      el.addEventListener("change", function () { commitTimeField(fid, el.value); });
      el.addEventListener("keydown", function (ev) { if (ev.key === "Enter") { ev.preventDefault(); el.blur(); } });
      el.addEventListener("focus", function () { el.select(); });
    });
    body.addEventListener("click", function (ev) {
      var n = ev.target.closest("button[data-nudge]");
      if (n) { nudge(n.getAttribute("data-nudge")); return; }
      var a = ev.target.closest("button[data-ea]");
      if (!a) return;
      var act = a.getAttribute("data-ea");
      if (act === "above") insertLine(false);
      else if (act === "below") insertLine(true);
      else if (act === "split") splitAtCursor();
      else if (act === "join") { var i = idx(S.active); if (i + 1 < S.doc.events.length) { S.sel = new Set([S.active, S.doc.events[i + 1].id]); joinLines(" "); } }
      else if (act === "dup") { S.sel = new Set([S.active]); duplicateLines(); }
      else if (act === "play") playLine();
      else if (act === "break") applyToActive("Break line", function (t) { return A.autoBreak(t, Math.min(settings.lenLimit, 42), true); });
      else if (act === "delete") { S.sel = new Set([S.active]); deleteLines(); }
    });
    // Keep the caret in the text box when a nudge or tag button is pressed.
    body.addEventListener("pointerdown", function (ev) { if (ev.target.closest("button[data-nudge]")) ev.preventDefault(); });
  }
  function timeBox(label, id, kind) {
    var mid = kind === "start" ? '<button type="button" data-nudge="start=" title="Start at the playhead (Ctrl+3)">' + ic("setstart") + "</button>"
      : kind === "end" ? '<button type="button" data-nudge="end=" title="End at the playhead (Ctrl+4)">' + ic("setend") + "</button>"
      : '<button type="button" data-nudge="dur=" title="Duration from reading speed">CPS</button>';
    return '<div class="sb-timebox"><label class="sb-label" for="' + id + '">' + label + '</label>' +
      '<input class="sb-input" id="' + id + '" inputmode="decimal" autocomplete="off" spellcheck="false">' +
      '<div class="sb-nudge"><button type="button" data-nudge="' + kind + '-" title="Earlier by a frame">−</button>' + mid +
      '<button type="button" data-nudge="' + kind + '+" title="Later by a frame">+</button></div></div>';
  }
  function commitTimeField(fid, value) {
    var e = activeEvent();
    if (!e) return;
    var ms = A.parseLooseTime(value);
    if (ms === null) { renderEditor(true); toast("Not a time: " + value, "bad"); return; }
    var patch = fid === "edStart" ? { start: Math.max(0, ms) } : fid === "edEnd" ? { end: Math.max(0, ms) } : { end: e.start + Math.max(0, ms) };
    if (patch.start !== undefined && patch.start > e.end) patch.end = patch.start + (e.end - e.start);
    change("Set time", function () { setEvent(e.id, patch); });
  }
  // One frame per press when a video is open (so the line lands on frame boundaries), 10 ms per
  // press otherwise — the resolution a .ass file actually stores.
  function nudge(what) {
    var e = activeEvent();
    if (!e) return;
    var step = hasVideo() ? frameMs() : 10, t = S.time, patch = {};
    var snapT = hasVideo() ? A.frameStart(A.frameAt(t, fps()), fps()) : Math.round(t / 10) * 10;
    switch (what) {
      case "start-": patch.start = Math.max(0, e.start - step); break;
      case "start+": patch.start = Math.min(e.end, e.start + step); break;
      case "end-": patch.end = Math.max(e.start, e.end - step); break;
      case "end+": patch.end = e.end + step; break;
      case "dur-": patch.end = Math.max(e.start, e.end - step); break;
      case "dur+": patch.end = e.end + step; break;
      case "start=": patch.start = snapT; if (snapT > e.end) patch.end = snapT + (e.end - e.start); break;
      case "end=": patch.end = hasVideo() ? A.frameStart(A.frameAt(t, fps()) + 1, fps()) : snapT; if (patch.end < e.start) patch.start = patch.end; break;
      case "dur=": patch.end = A.durationFromCps(e, settings.cpsLimit, 1000).end; break;
    }
    if (hasVideo() && (what.indexOf("-") !== -1 || what.indexOf("+") !== -1)) {
      if (patch.start !== undefined) patch.start = A.snapToFrame(patch.start, fps());
      if (patch.end !== undefined) patch.end = A.snapToFrame(patch.end, fps());
    }
    change("Adjust time", function () { setEvent(e.id, patch); }, "nudge:" + e.id + ":" + what.charAt(0));
  }
  function renderEditor(force) {
    var e = activeEvent();
    var i = idx(S.active);
    $("editPos").innerHTML = e ? "Line <b>" + (i + 1) + "</b> of " + S.doc.events.length + (S.sel.size > 1 ? " · " + S.sel.size + " selected" : "") : "No line";
    $("prevLine").disabled = i <= 0;
    $("commentBtn").setAttribute("aria-pressed", e && e.comment ? "true" : "false");
    if (!e) return;
    var setVal = function (id, v) {
      var el = $(id);
      if (!el) return;
      if (document.activeElement === el && !force) return;
      if (el.value !== String(v)) el.value = v;
    };
    setVal("edStart", A.formatTime(e.start));
    setVal("edEnd", A.formatTime(e.end));
    setVal("edDur", fmtDur(e.end - e.start));
    var sel = $("edStyle"), names = styleNames();
    var opts = names.concat(names.indexOf(e.style) === -1 ? [e.style] : []);
    var optHtml = opts.map(function (n) { return '<option value="' + esc(n) + '">' + esc(n) + (names.indexOf(n) === -1 ? " (missing)" : "") + "</option>"; }).join("");
    if (sel.getAttribute("data-opts") !== optHtml) { sel.innerHTML = optHtml; sel.setAttribute("data-opts", optHtml); }
    sel.value = e.style;
    setVal("edActor", e.actor);
    setVal("edEffect", e.effect);
    setVal("edLayer", e.layer); setVal("edML", e.marginL); setVal("edMR", e.marginR); setVal("edMV", e.marginV);
    var ta = $("edText");
    if (ta.value !== e.text && (document.activeElement !== ta || force || H.lastKey !== "text:" + e.id)) {
      ta.value = e.text;
      var p = Math.min(S.caret, e.text.length);
      if (document.activeElement === ta) ta.setSelectionRange(p, p);
    }
    var actors = {};
    S.doc.events.forEach(function (x) { if (x.actor) actors[x.actor] = 1; });
    var al = Object.keys(actors).sort().slice(0, 200).map(function (a) { return '<option value="' + esc(a) + '">'; }).join("");
    if ($("actorList").innerHTML !== al) $("actorList").innerHTML = al;
    // The preview shows what a viewer reads, with tags as quiet markers.
    var html = "";
    A.splitBlocks(e.text).forEach(function (seg) {
      if (seg.type === "text") html += esc(seg.text).replace(/\\N/g, '<span class="br">\\N</span>\n').replace(/\\n/g, '<span class="br">\\n</span>').replace(/\\h/g, "\u00A0");
      else if (seg.type === "comment") html += '<span class="cm">{' + esc(seg.text) + "}</span>";
      else html += '<span class="tg">{' + esc(seg.text) + "}</span>";
    });
    $("edPreview").innerHTML = html || '<span class="br">Empty line</span>';
    var d = info(e), dur = e.end - e.start;
    var chips = [];
    chips.push('<span class="sb-chip" data-tone="' + (d.cps > settings.cpsLimit ? "bad" : d.cps > settings.cpsLimit * 0.85 ? "warn" : "ok") + '">' + d.cps.toFixed(1) + " CPS</span>");
    chips.push('<span class="sb-chip" data-tone="' + (d.len > settings.lenLimit ? "bad" : "") + '">' + d.len + " chars/line</span>");
    if (dur <= 0) chips.push('<span class="sb-chip" data-tone="bad">Zero duration</span>');
    if (overlapSet().has(e.id)) chips.push('<span class="sb-chip" data-tone="warn">Overlaps</span>');
    if (styleNames().indexOf(e.style) === -1) chips.push('<span class="sb-chip" data-tone="bad">Missing style</span>');
    if (e.comment) chips.push('<span class="sb-chip">Comment</span>');
    $("edStats").innerHTML = chips.join("");
  }
  function insertAtCaret(str) {
    var ta = $("edText"), e = activeEvent();
    if (!e) return;
    var s = document.activeElement === ta ? ta.selectionStart : S.caret, en = document.activeElement === ta ? ta.selectionEnd : S.caret;
    var v = e.text.slice(0, s) + str + e.text.slice(en);
    S.caret = s + str.length;
    change("Edit text", function () { setEvent(e.id, { text: v }); }, "text:" + e.id);
    ta.value = v; ta.focus(); ta.setSelectionRange(S.caret, S.caret);
  }
  function wrapSelection(open, close) {
    var ta = $("edText"), e = activeEvent();
    if (!e) return;
    var s = ta.selectionStart, en = ta.selectionEnd;
    var v = e.text.slice(0, s) + open + e.text.slice(s, en) + close + e.text.slice(en);
    S.caret = en + open.length;
    change("Edit text", function () { setEvent(e.id, { text: v }); });
    ta.value = v; ta.focus(); ta.setSelectionRange(s + open.length, en + open.length);
  }
  // B/I/U/S: with a selection, wrap it on-off; otherwise switch the state at the caret.
  function toggleTag(name) {
    var ta = $("edText"), e = activeEvent();
    if (!e) return;
    var s = ta.selectionStart, en = ta.selectionEnd;
    if (en > s) { wrapSelection("{\\" + name + "1}", "{\\" + name + "0}"); change("Clean tags", function () { setEvent(e.id, { text: A.cleanTags(activeEvent().text) }); }, "text:" + e.id); return; }
    var state = false;
    A.splitBlocks(e.text.slice(0, s)).forEach(function (seg) {
      if (seg.type === "tags") A.parseTags(seg.text).forEach(function (t) { if (t.name === name) state = t.args !== "0" && t.args !== ""; });
    });
    var style = S.doc.styles.filter(function (x) { return x.Name === e.style; })[0];
    if (!s && style) state = !!style[{ b: "Bold", i: "Italic", u: "Underline", s: "StrikeOut" }[name]];
    var v = A.insertTagsAt(e.text, s, [A.makeTag(name, state ? "0" : "1")]);
    change((state ? "Turn off " : "Turn on ") + name, function () { setEvent(e.id, { text: v }); });
    ta.focus();
  }
  function toggleAn8() {
    applyToActive("Alignment", function (t) {
      var has = /^\{[^}]*\\an8/.test(t);
      return has ? A.stripTags(t, ["an"]) : A.setStartTags(t, [A.makeTag("an", "8")]);
    });
  }
  function applyToActive(label, fn) {
    var ids = S.sel.size > 1 ? S.sel : new Set([S.active]);
    change(label, function () { mapEvents(ids, function (e) { return A.withEvent(e, { text: fn(e.text) }); }); });
  }
  function pickColor() {
    var input = document.createElement("input");
    input.type = "color"; input.value = "#ffffff";
    input.style.position = "fixed"; input.style.opacity = "0"; input.style.pointerEvents = "none";
    document.body.appendChild(input);
    var e = activeEvent(), caret = $("edText").selectionStart;
    input.addEventListener("change", function () {
      var tag = A.makeTag("c", A.tagColor(A.hexToColor(input.value)));
      change("Colour", function () { setEvent(e.id, { text: A.insertTagsAt(activeEvent().text, caret, [tag]) }); });
      input.remove();
    });
    input.addEventListener("blur", function () { setTimeout(function () { input.remove(); }, 1000); });
    input.click();
  }

  // ---- media & clock ----------------------------------------------------------------------
  var video = $("video");
  var clock = { playing: false, base: 0, t0: 0 };
  function hasMedia() { return !!(S.media && video.src); }
  function hasVideo() { return hasMedia() && S.media.kind === "video" && video.videoWidth > 0; }
  function isPlaying() { return hasMedia() ? !video.paused && !video.ended : clock.playing; }
  function mediaDuration() {
    if (hasMedia() && isFinite(video.duration) && video.duration > 0) return video.duration * 1000;
    var max = 0;
    S.doc.events.forEach(function (e) { if (e.end > max) max = e.end; });
    return Math.max(60000, max + 5000);
  }
  function now() {
    if (hasMedia()) return Math.round(video.currentTime * 1000);
    if (clock.playing) return Math.round(clock.base + performance.now() - clock.t0);
    return S.time;
  }
  function seek(ms) {
    ms = Math.max(0, Math.min(mediaDuration(), Math.round(ms)));
    S.time = ms;
    if (hasMedia()) {
      // Land inside the frame, not on its edge, so the decoder shows the frame we mean.
      var t = hasVideo() ? (A.frameStart(A.frameAt(ms, fps()), fps()) + frameMs() * 0.5) / 1000 : ms / 1000;
      try { video.currentTime = t; } catch (e) {}
    } else { clock.base = ms; clock.t0 = performance.now(); }
    paintTime(true);
  }
  function play() {
    if (hasMedia()) { var p = video.play(); if (p && p.catch) p.catch(function () {}); }
    else { clock.playing = true; clock.base = S.time; clock.t0 = performance.now(); }
    $("playBtn").innerHTML = ic("pause");
    requestAnimationFrame(loop);
  }
  function pause() {
    if (hasMedia()) video.pause();
    else { S.time = now(); clock.playing = false; }
    S.stopAt = null;
    $("playBtn").innerHTML = ic("play");
    paintTime(true);
  }
  function togglePlay() { if (isPlaying()) pause(); else { S.stopAt = null; play(); } }
  function playRange(a, b) { seek(a); S.stopAt = b; play(); }
  function playLine() { var e = activeEvent(); if (e) playRange(e.start, e.end); }
  function loop() {
    if (!isPlaying()) { $("playBtn").innerHTML = ic("play"); paintTime(true); return; }
    S.time = now();
    if (S.stopAt !== null && S.time >= S.stopAt) { var at = S.stopAt; pause(); S.time = at; paintTime(true); return; }
    if (S.time >= mediaDuration() && !hasMedia()) { pause(); return; }
    paintTime(false);
    requestAnimationFrame(loop);
  }
  var lastPlayingKey = "";
  function paintTime(full) {
    var t = S.time;
    var f = A.frameAt(t, fps());
    $("clock").innerHTML = A.formatTime(t) + (hasVideo() ? " <small>f" + f + "</small>" : "");
    var seekEl = $("seek");
    if (document.activeElement !== seekEl) seekEl.value = String(Math.round(t / mediaDuration() * 1000));
    renderOverlay();
    drawWave();
    // Only rebuild the list when the set of on-screen lines changes.
    var key = "";
    S.doc.events.forEach(function (e) { if (!e.comment && e.start <= t && e.end > t) key += e.id + ","; });
    if (key !== lastPlayingKey || full) { lastPlayingKey = key; if (!full) renderList(); }
  }

  function openMedia(file) {
    if (S.media && S.media.url && S.media.url.indexOf("blob:") === 0) URL.revokeObjectURL(S.media.url);
    var url = typeof file === "string" ? file : URL.createObjectURL(file);
    var name = typeof file === "string" ? file.split("/").pop().split("?")[0] : file.name;
    var kind = typeof file !== "string" && /^audio\//.test(file.type) || /\.(mka|mp3|aac|m4a|flac|ogg|opus|wav)$/i.test(name) ? "audio" : "video";
    S.media = { url: url, name: name, kind: kind, file: typeof file === "string" ? null : file };
    S.wave = null;
    video.src = url;
    video.load();
    $("stageHint").hidden = true;
    toast("Opened " + name);
    if (!isWide() && S.view === "lines") setView("video");
    paintWaveNote();
  }
  function initMedia() {
    video.addEventListener("loadedmetadata", function () {
      fitStage();
      if (S.media && video.videoWidth === 0) S.media.kind = "audio";
      seek(S.time);
      detectFps();
    });
    video.addEventListener("error", function () {
      toast("This browser cannot play " + (S.media ? S.media.name : "that file") + " — try an MP4 or WebM copy", "bad");
    });
    video.addEventListener("seeked", function () { S.time = now(); paintTime(true); });
    video.addEventListener("play", function () { $("playBtn").innerHTML = ic("pause"); requestAnimationFrame(loop); });
    video.addEventListener("pause", function () { $("playBtn").innerHTML = ic("play"); });
    $("playBtn").addEventListener("click", togglePlay);
    $("frameBack").addEventListener("click", function () { stepFrame(-1); });
    $("frameFwd").addEventListener("click", function () { stepFrame(1); });
    $("seek").addEventListener("input", function () { seek(Number(this.value) / 1000 * mediaDuration()); });
    $("mediaBtn").addEventListener("click", openMediaMenu);
    $("vCollapse").addEventListener("click", function () {
      var c = app.getAttribute("data-vcollapsed") === "1";
      app.setAttribute("data-vcollapsed", c ? "0" : "1");
      this.innerHTML = ic(c ? "up" : "down");
      requestAnimationFrame(fitStage);
    });
    $("mediaFile").addEventListener("change", function () {
      var f = this.files && this.files[0];
      this.value = "";
      if (!f) return;
      if (this.getAttribute("data-purpose") === "wave") loadWave(f); else openMedia(f);
      this.removeAttribute("data-purpose");
    });
    var stage = $("stage");
    stage.addEventListener("pointerdown", onStagePointer);
    stage.addEventListener("pointermove", function (ev) { if (stageDrag) onStagePointer(ev); });
    ["pointerup", "pointercancel"].forEach(function (t) { stage.addEventListener(t, function () { stageDrag = false; }); });
    stage.addEventListener("click", function () { if (!S.posMode && hasMedia()) togglePlay(); });
    if (window.ResizeObserver) new ResizeObserver(function () { fitStage(); }).observe(stage);
    fitStage();
  }
  function stepFrame(n) {
    if (isPlaying()) pause();
    var f = A.frameAt(S.time, fps()) + n;
    seek(A.frameStart(Math.max(0, f), fps()));
  }
  // Frame rate from the decoder when the browser reports presented frames; the setting otherwise.
  function detectFps() {
    if (!video.requestVideoFrameCallback || !hasVideo()) return;
    var times = [];
    var cb = function (nowT, meta) {
      times.push(meta.mediaTime);
      if (times.length < 12 && !video.paused) { video.requestVideoFrameCallback(cb); return; }
      var deltas = [];
      for (var i = 1; i < times.length; i++) { var d = times[i] - times[i - 1]; if (d > 0.001) deltas.push(d); }
      if (deltas.length < 5) return;
      deltas.sort(function (a, b) { return a - b; });
      var med = deltas[Math.floor(deltas.length / 2)], guess = 1 / med;
      var known = [24000 / 1001, 24, 25, 30000 / 1001, 30, 50, 60000 / 1001, 60];
      var best = known.reduce(function (b, k) { return Math.abs(k - guess) < Math.abs(b - guess) ? k : b; }, known[0]);
      if (Math.abs(best - guess) / best < 0.02 && Math.abs(best - settings.fps) > 0.001) {
        settings.fps = best; saveSettings();
        toast("Frame rate detected: " + A.fmtNum(best, 3) + " fps");
      }
    };
    video.addEventListener("play", function once() { video.removeEventListener("play", once); video.requestVideoFrameCallback(cb); });
  }

  // ---- preview overlay ------------------------------------------------------------------------
  var stageRect = { x: 0, y: 0, w: 0, h: 0 };
  function fitStage() {
    var stage = $("stage"), canvas = $("overlay");
    var res = A.playRes(S.doc);
    var aspect = hasVideo() ? video.videoWidth / video.videoHeight : res.x / res.y;
    if (!isWide()) stage.style.aspectRatio = String(aspect);
    else stage.style.aspectRatio = "";
    var W = stage.clientWidth, Hh = stage.clientHeight;
    if (!W || !Hh) return;
    var w = W, h = W / aspect;
    if (h > Hh) { h = Hh; w = Hh * aspect; }
    stageRect = { x: (W - w) / 2, y: (Hh - h) / 2, w: w, h: h };
    var dpr = Math.min(2, window.devicePixelRatio || 1);
    canvas.style.left = stageRect.x + "px"; canvas.style.top = stageRect.y + "px";
    canvas.style.width = w + "px"; canvas.style.height = h + "px";
    canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr);
    renderOverlay();
    fitWave();
  }
  function renderOverlay() {
    var canvas = $("overlay");
    if (!canvas.width) return;
    R.render(canvas, S.doc, S.time, { highlight: S.active, videoHeight: hasVideo() ? video.videoHeight : null });
  }
  var stageDrag = false;
  function onStagePointer(ev) {
    if (!S.posMode) return;
    ev.preventDefault();
    var stage = $("stage").getBoundingClientRect();
    var x = ev.clientX - stage.left - stageRect.x, y = ev.clientY - stage.top - stageRect.y;
    var res = A.playRes(S.doc);
    var px = Math.round(x / stageRect.w * res.x * 10) / 10, py = Math.round(y / stageRect.h * res.y * 10) / 10;
    if (ev.type === "pointerdown") { stageDrag = true; try { $("stage").setPointerCapture(ev.pointerId); } catch (e) {} }
    var e = activeEvent();
    if (!e) return;
    change("Position", function () { setEvent(e.id, { text: A.setStartTags(e.text, [A.makeTag("pos", A.fmtNum(px, 1) + "," + A.fmtNum(py, 1))]) }); }, "pos:" + e.id);
  }
  function togglePosMode() {
    S.posMode = !S.posMode;
    $("stage").setAttribute("data-posmode", S.posMode ? "1" : "0");
    var hint = $("stageHint");
    hint.hidden = !S.posMode;
    hint.textContent = "Tap or drag to place the line · tap \\pos again to finish";
    if (S.posMode && !isWide() && app.getAttribute("data-vcollapsed") === "1") $("vCollapse").click();
    if (S.posMode) { var e = activeEvent(); if (e && (S.time < e.start || S.time >= e.end)) seek(e.start); }
  }

  // ---- waveform & timing -----------------------------------------------------------------------
  // Audio is decoded once at a low sample rate into a peak per 10 ms, then the decoded buffer is
  // dropped: an episode's full PCM would not fit in a phone's memory, its peaks do.
  var PEAK_RATE = 100;
  function paintWaveNote() {
    var note = $("waveNote");
    if (S.wave && S.wave.loading) { note.hidden = false; note.innerHTML = "Decoding audio… " + (S.wave.progress || ""); return; }
    note.hidden = !!S.wave;
    if (S.wave) return;
    note.innerHTML = '<button class="sb-btn sb-sm" id="waveLoad">' + ic("wave") + (hasMedia() ? "Show waveform" : "Load audio for a waveform") + "</button>";
    note.style.pointerEvents = "none";
    $("waveLoad").style.pointerEvents = "auto";
    $("waveLoad").addEventListener("click", function (ev) {
      ev.stopPropagation();
      if (S.media && S.media.file) loadWave(S.media.file);
      else if (S.media && S.media.url) loadWave(S.media.url);
      else { $("mediaFile").setAttribute("data-purpose", "wave"); $("mediaFile").click(); }
    });
  }
  function loadWave(src) {
    var size = src && src.size;
    if (size && size > 900 * 1048576) { toast("That file is too large to decode here — load an audio-only copy instead", "bad"); return; }
    S.wave = { loading: true };
    paintWaveNote();
    var getBuf = typeof src === "string"
      ? fetch(src).then(function (r) { if (!r.ok) throw new Error("HTTP " + r.status); return r.arrayBuffer(); })
      : src.arrayBuffer();
    getBuf.then(function (buf) {
      var Ctx = window.OfflineAudioContext || window.webkitOfflineAudioContext;
      var ctx;
      try { ctx = new Ctx(1, 1, 8000); } catch (e) { ctx = new Ctx(1, 1, 22050); }
      return new Promise(function (res, rej) {
        var p = ctx.decodeAudioData(buf, res, rej);
        if (p && p.then) p.then(res, rej);
      });
    }).then(function (audio) {
      var rate = audio.sampleRate, per = Math.max(1, Math.round(rate / PEAK_RATE));
      var n = Math.ceil(audio.length / per), peaks = new Float32Array(n);
      var chans = [];
      for (var c = 0; c < audio.numberOfChannels; c++) chans.push(audio.getChannelData(c));
      var max = 0;
      for (var i = 0; i < n; i++) {
        var p = 0, from = i * per, to = Math.min(audio.length, from + per);
        for (var ch = 0; ch < chans.length; ch++) {
          var d = chans[ch];
          for (var j = from; j < to; j++) { var v = d[j] < 0 ? -d[j] : d[j]; if (v > p) p = v; }
        }
        peaks[i] = p; if (p > max) max = p;
      }
      if (max > 0) for (var k = 0; k < n; k++) peaks[k] = peaks[k] / max;
      S.wave = { peaks: peaks, rate: PEAK_RATE, duration: audio.duration * 1000 };
      paintWaveNote(); drawWave();
      toast("Waveform ready");
    }).catch(function (err) {
      S.wave = null; paintWaveNote();
      toast("Could not decode that audio (" + (err && err.message || "unsupported format") + "). An MKV often needs an audio-only file.", "bad");
    });
  }
  function fitWave() {
    var wrap = $("waveWrap"), c = $("wave");
    var dpr = Math.min(2, window.devicePixelRatio || 1);
    if (!wrap.clientWidth) return;
    c.width = Math.round(wrap.clientWidth * dpr); c.height = Math.round(wrap.clientHeight * dpr);
    drawWave();
  }
  function centerWaveOn(e) {
    var span = S.waveSpan, len = e.end - e.start;
    if (len > span * 0.8) S.waveSpan = span = Math.min(120000, len * 1.4);
    if (e.start < S.waveStart || e.end > S.waveStart + span) S.waveStart = Math.max(0, e.start - (span - len) / 2);
  }
  function waveX(ms, W) { return (ms - S.waveStart) / S.waveSpan * W; }
  function drawWave() {
    var c = $("wave");
    if (!c.width || !c.offsetParent) return;
    var ctx = c.getContext("2d"), W = c.width, Hh = c.height, dpr = W / (c.clientWidth || 1);
    var css = getComputedStyle(document.documentElement);
    var col = function (n) { return css.getPropertyValue(n).trim() || "#888"; };
    ctx.clearRect(0, 0, W, Hh);
    var t = S.time;
    if (isPlaying() && (t > S.waveStart + S.waveSpan * 0.92 || t < S.waveStart)) S.waveStart = Math.max(0, t - S.waveSpan * 0.1);
    // Other lines as faint bands, the active one as a strong band with handles.
    var active = activeEvent();
    ctx.font = (11 * dpr) + "px " + (css.getPropertyValue("--pn-body") || "sans-serif");
    S.doc.events.forEach(function (e, i) {
      if (e.comment || e.end < S.waveStart || e.start > S.waveStart + S.waveSpan || (active && e.id === active.id)) return;
      var x1 = waveX(e.start, W), x2 = waveX(e.end, W);
      ctx.fillStyle = "rgba(128,140,160,0.12)";
      ctx.fillRect(x1, 0, x2 - x1, Hh);
      ctx.fillStyle = "rgba(128,140,160,0.45)";
      ctx.fillRect(x1, 0, 1 * dpr, Hh); ctx.fillRect(x2 - dpr, 0, dpr, Hh);
      ctx.fillStyle = col("--pn-faint");
      ctx.fillText(String(i + 1), x1 + 4 * dpr, Hh - 6 * dpr);
    });
    if (active) {
      var ax1 = waveX(active.start, W), ax2 = waveX(active.end, W);
      ctx.fillStyle = "rgba(60,129,235,0.18)";
      ctx.fillRect(ax1, 0, ax2 - ax1, Hh);
    }
    // Peaks, one column per device pixel.
    if (S.wave && S.wave.peaks) {
      var peaks = S.wave.peaks, mid = Hh / 2;
      ctx.fillStyle = col("--pn-k1-hi");
      ctx.globalAlpha = 0.85;
      for (var x = 0; x < W; x += 1) {
        var a = Math.floor((S.waveStart + x / W * S.waveSpan) / 1000 * S.wave.rate);
        var b = Math.max(a + 1, Math.floor((S.waveStart + (x + 1) / W * S.waveSpan) / 1000 * S.wave.rate));
        var p = 0;
        for (var j = a; j < b && j < peaks.length; j++) if (j >= 0 && peaks[j] > p) p = peaks[j];
        var hgt = Math.max(dpr * 0.5, p * (Hh * 0.46));
        ctx.fillRect(x, mid - hgt, 1, hgt * 2);
      }
      ctx.globalAlpha = 1;
    }
    // Ruler: a tick every second (every 5 when zoomed out).
    var stepS = S.waveSpan > 30000 ? 5000 : S.waveSpan > 12000 ? 2000 : 1000;
    ctx.fillStyle = col("--pn-dim");
    for (var s = Math.ceil(S.waveStart / stepS) * stepS; s < S.waveStart + S.waveSpan; s += stepS) {
      var tx = waveX(s, W);
      ctx.fillRect(tx, 0, dpr, 6 * dpr);
      ctx.fillText(A.formatTime(s).replace(/^0:/, "").replace(/\.00$/, ""), tx + 3 * dpr, 14 * dpr);
    }
    if (active) {
      var hx1 = waveX(active.start, W), hx2 = waveX(active.end, W);
      [[hx1, "#38c172"], [hx2, "#e0555f"]].forEach(function (h) {
        ctx.fillStyle = h[1];
        ctx.fillRect(h[0] - dpr, 0, 2 * dpr, Hh);
        // A grab tab at the bottom, big enough for a thumb.
        ctx.beginPath();
        var tw = 14 * dpr, th = 22 * dpr;
        if (ctx.roundRect) ctx.roundRect(h[0] - tw / 2, Hh - th, tw, th, 4 * dpr); else ctx.rect(h[0] - tw / 2, Hh - th, tw, th);
        ctx.fill();
      });
    }
    var px = waveX(t, W);
    ctx.fillStyle = col("--pn-ink");
    ctx.fillRect(px - dpr / 2, 0, dpr, Hh);
  }
  function initWave() {
    var wrap = $("waveWrap"), pointers = new Map(), drag = null, pinch = null;
    var toMs = function (clientX) {
      var r = wrap.getBoundingClientRect();
      return S.waveStart + (clientX - r.left) / r.width * S.waveSpan;
    };
    var snapMs = function (ms, which, e) {
      var r = wrap.getBoundingClientRect(), tol = 10 / r.width * S.waveSpan;
      if (!settings.snap) return Math.round(ms / 10) * 10;
      var cands = [S.time];
      S.doc.events.forEach(function (x) { if (x.id !== e.id && !x.comment) { cands.push(x.start, x.end); } });
      var best = null;
      cands.forEach(function (c) { if (Math.abs(c - ms) < tol && (best === null || Math.abs(c - ms) < Math.abs(best - ms))) best = c; });
      if (best !== null) return best;
      return hasVideo() ? A.snapToFrame(ms, fps()) : Math.round(ms / 10) * 10;
    };
    wrap.addEventListener("pointerdown", function (ev) {
      if (ev.target.closest("button")) return;
      wrap.setPointerCapture(ev.pointerId);
      pointers.set(ev.pointerId, ev.clientX);
      if (pointers.size === 2) {
        var xs = Array.from(pointers.values());
        pinch = { d: Math.abs(xs[0] - xs[1]) || 1, span: S.waveSpan, center: toMs((xs[0] + xs[1]) / 2) };
        drag = null; return;
      }
      var e = activeEvent(), r = wrap.getBoundingClientRect(), tol = ev.pointerType === "mouse" ? 8 : 22;
      var x = ev.clientX - r.left;
      var which = null;
      if (e) {
        var sx = (e.start - S.waveStart) / S.waveSpan * r.width, ex = (e.end - S.waveStart) / S.waveSpan * r.width;
        var ds = Math.abs(x - sx), de = Math.abs(x - ex);
        if (ds < tol || de < tol) which = ds <= de ? "start" : "end";
      }
      drag = { which: which, x0: ev.clientX, start0: S.waveStart, moved: false, id: e ? e.id : null };
    });
    wrap.addEventListener("pointermove", function (ev) {
      if (!pointers.has(ev.pointerId)) return;
      pointers.set(ev.pointerId, ev.clientX);
      if (pinch && pointers.size === 2) {
        var xs = Array.from(pointers.values());
        var d = Math.abs(xs[0] - xs[1]) || 1;
        S.waveSpan = Math.max(1000, Math.min(180000, pinch.span * pinch.d / d));
        var r = wrap.getBoundingClientRect(), cx = ((xs[0] + xs[1]) / 2 - r.left) / r.width;
        S.waveStart = Math.max(0, pinch.center - cx * S.waveSpan);
        drawWave(); return;
      }
      if (!drag) return;
      if (Math.abs(ev.clientX - drag.x0) > 5) drag.moved = true;
      if (!drag.moved) return;
      if (drag.which && drag.id !== null) {
        var e = S.doc.events[idx(drag.id)];
        if (!e) return;
        var ms = snapMs(toMs(ev.clientX), drag.which, e), patch = {};
        if (drag.which === "start") patch.start = Math.max(0, Math.min(ms, e.end));
        else patch.end = Math.max(ms, e.start);
        change("Time with waveform", function () { setEvent(e.id, patch); }, "wave:" + e.id + ":" + drag.which);
      } else {
        var r2 = wrap.getBoundingClientRect();
        S.waveStart = Math.max(0, drag.start0 - (ev.clientX - drag.x0) / r2.width * S.waveSpan);
        drawWave();
      }
    });
    var end = function (ev) {
      if (!pointers.has(ev.pointerId)) return;
      pointers.delete(ev.pointerId);
      if (pointers.size < 2) pinch = null;
      if (drag && !drag.moved && ev.type === "pointerup") {
        var ms = toMs(ev.clientX);
        if (drag.which) { /* a tap on a handle does nothing */ }
        else seek(ms);
      }
      drag = null;
    };
    wrap.addEventListener("pointerup", end);
    wrap.addEventListener("pointercancel", end);
    wrap.addEventListener("wheel", function (ev) {
      ev.preventDefault();
      if (ev.ctrlKey || ev.metaKey || Math.abs(ev.deltaY) > Math.abs(ev.deltaX) && !ev.shiftKey) {
        var at = toMs(ev.clientX), k = Math.exp(ev.deltaY * 0.0015);
        var r = wrap.getBoundingClientRect(), cx = (ev.clientX - r.left) / r.width;
        S.waveSpan = Math.max(1000, Math.min(180000, S.waveSpan * k));
        S.waveStart = Math.max(0, at - cx * S.waveSpan);
      } else S.waveStart = Math.max(0, S.waveStart + (ev.deltaX || ev.deltaY) / 800 * S.waveSpan);
      drawWave();
    }, { passive: false });
    if (window.ResizeObserver) new ResizeObserver(fitWave).observe(wrap);
    var acts = [
      ["playline", "play", "Line", function () { playLine(); }],
      ["before", "left", "Before", function () { var e = activeEvent(); if (e) playRange(Math.max(0, e.start - 500), e.start); }],
      ["after", "right", "After", function () { var e = activeEvent(); if (e) playRange(e.end, e.end + 500); }],
      ["next", "enter", "Next", function () { commitNext(); }],
      ["start", "setstart", "Start", function () { nudge("start="); }],
      ["end", "setend", "End", function () { nudge("end="); }],
      ["zout", "zoomout", "Zoom", function () { zoomWave(1.6); }],
      ["zin", "zoomin", "Zoom", function () { zoomWave(1 / 1.6); }]
    ];
    var titles = { playline: "Play the line (Ctrl+P)", before: "Play 500 ms before", after: "Play 500 ms after", next: "Next line, timed on from this one",
      start: "Start at the playhead", end: "End at the playhead", zout: "Zoom out", zin: "Zoom in" };
    $("timingActs").innerHTML = acts.map(function (a) {
      return '<button class="sb-btn" data-ta="' + a[0] + '" title="' + titles[a[0]] + '">' + ic(a[1]) + "<span>" + a[2] + "</span></button>";
    }).join("");
    $("timingActs").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-ta]");
      if (!b) return;
      acts.filter(function (a) { return a[0] === b.getAttribute("data-ta"); })[0][3]();
    });
  }
  function zoomWave(k) {
    var center = S.waveStart + S.waveSpan / 2;
    S.waveSpan = Math.max(1000, Math.min(180000, S.waveSpan * k));
    S.waveStart = Math.max(0, center - S.waveSpan / 2);
    drawWave();
  }
  // Timing a fresh script: the next line starts where this one ended if it has no time yet.
  function commitNext() {
    var cur = activeEvent();
    if (!cur) return;
    var i = idx(cur.id);
    var next = S.doc.events[i + 1];
    if (!next) { insertLine(true); return; }
    if (next.start === 0 && next.end === 0 || next.end <= next.start) {
      change("Time next line", function () { setEvent(next.id, { start: cur.end, end: cur.end + settings.defaultDur }); });
    }
    setActive(next.id);
  }

  // ---- sheets ------------------------------------------------------------------------------
  var sheet = $("sheet"), sheetOnClose = null;
  function openSheet(title, body, buttons, onClose) {
    $("sheetTitle").textContent = title;
    var b = $("sheetBody");
    if (typeof body === "string") b.innerHTML = body; else { b.innerHTML = ""; b.appendChild(body); }
    var foot = $("sheetFoot");
    foot.innerHTML = "";
    (buttons || []).forEach(function (btn) {
      var el = document.createElement("button");
      el.className = "sb-btn" + (btn.primary ? " sb-primary" : "") + (btn.danger ? " sb-danger" : "");
      el.type = "button";
      el.innerHTML = btn.label;
      el.addEventListener("click", function () { var r = btn.onClick && btn.onClick(); if (r !== false) closeSheet(); });
      foot.appendChild(el);
    });
    foot.hidden = !buttons || !buttons.length;
    sheetOnClose = onClose || null;
    if (!sheet.open) { if (sheet.showModal) sheet.showModal(); else sheet.setAttribute("open", ""); }
    b.scrollTop = 0;
  }
  function closeSheet() {
    if (sheet.open) { if (sheet.close) sheet.close(); else sheet.removeAttribute("open"); }
  }
  sheet.addEventListener("close", function () { var f = sheetOnClose; sheetOnClose = null; if (f) f(); });
  sheet.addEventListener("click", function (ev) { if (ev.target === sheet) closeSheet(); });
  function q(sel) { return $("sheetBody").querySelector(sel); }
  function qa(sel) { return Array.prototype.slice.call($("sheetBody").querySelectorAll(sel)); }
  function seg(name, options, value) {
    return '<div class="sb-seg" data-seg="' + name + '">' + options.map(function (o) {
      return '<button type="button" data-v="' + esc(o[0]) + '"' + (String(o[0]) === String(value) ? ' aria-pressed="true"' : "") + ">" + esc(o[1]) + "</button>";
    }).join("") + "</div>";
  }
  function segVal(name) { var b = q('[data-seg="' + name + '"] [aria-pressed="true"]'); return b ? b.getAttribute("data-v") : null; }
  $("sheetBody").addEventListener("click", function (ev) {
    var b = ev.target.closest(".sb-seg button");
    if (!b) return;
    Array.prototype.forEach.call(b.parentNode.children, function (x) { x.removeAttribute("aria-pressed"); });
    b.setAttribute("aria-pressed", "true");
    b.parentNode.dispatchEvent(new CustomEvent("segchange", { bubbles: true, detail: b.getAttribute("data-v") }));
  });
  function scopeField(defaultAll) {
    var n = S.sel.size;
    return '<div class="sb-field"><span class="sb-label">Apply to</span>' +
      seg("scope", [["sel", "Selected (" + n + ")"], ["all", "All lines"]], defaultAll || n < 2 ? "all" : "sel") + "</div>";
  }
  function field(label, inner, hint) {
    return '<div class="sb-field"><label class="sb-label">' + label + "</label>" + inner + (hint ? '<p class="sb-hint">' + hint + "</p>" : "") + "</div>";
  }
  function numInput(id, value, attrs) {
    return '<input class="sb-input" id="' + id + '" type="number" inputmode="decimal" value="' + esc(value) + '" ' + (attrs || "") + ">";
  }

  // ---- menu -------------------------------------------------------------------------------------
  function menuItem(icon, label, act, note) {
    return '<button type="button" data-m="' + act + '">' + ic(icon) + "<span>" + label + "</span>" + (note ? "<small>" + note + "</small>" : "") + "</button>";
  }
  function openMenu() {
    var canShare = !!(navigator.canShare && navigator.share);
    openSheet("Pandora Subs", '<div class="sb-menu">' +
      "<h4>File</h4>" +
      menuItem("folder", "Open subtitle file…", "open", "Ctrl+O") +
      menuItem("paste", "Paste subtitle text…", "paste") +
      menuItem("link", "Open from a link…", "url") +
      menuItem("file", "New script", "new") +
      menuItem("clock", "Recent scripts…", "recent") +
      "<h4>Save</h4>" +
      menuItem("save", "Download .ass", "save", "Ctrl+S") +
      menuItem("save", "Export .srt", "srt") +
      (canShare ? menuItem("share", "Share .ass…", "share") : "") +
      "<h4>Video & audio</h4>" +
      menuItem("video", "Open video or audio…", "media") +
      menuItem("link", "Open video from a link…", "mediaurl") +
      menuItem("wave", "Load waveform from a separate audio file…", "wavefile") +
      (S.media ? menuItem("x", "Close " + esc(S.media.name), "closemedia") : "") +
      "<h4>Script</h4>" +
      menuItem("info", "Script properties…", "props") +
      menuItem("gear", "Settings…", "settings") +
      menuItem("keyboard", "Shortcuts & gestures", "help") +
      menuItem("home", "Pandora console", "home") +
      "</div>");
    q(".sb-menu").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-m]");
      if (!b) return;
      var m = b.getAttribute("data-m");
      closeSheet();
      setTimeout(function () { menuAction(m); }, 30);
    });
  }
  function menuAction(m) {
    if (m === "open") $("subFile").click();
    else if (m === "paste") openPaste();
    else if (m === "url") openUrl(false);
    else if (m === "new") newScript();
    else if (m === "recent") openRecent();
    else if (m === "save") download("ass");
    else if (m === "srt") download("srt");
    else if (m === "share") share();
    else if (m === "media") $("mediaFile").click();
    else if (m === "mediaurl") openUrl(true);
    else if (m === "wavefile") { $("mediaFile").setAttribute("data-purpose", "wave"); $("mediaFile").click(); }
    else if (m === "closemedia") closeMedia();
    else if (m === "props") openProps();
    else if (m === "settings") openSettings();
    else if (m === "help") openHelp();
    else if (m === "home") location.href = "/";
  }
  function openMediaMenu() {
    openSheet("Video & audio", '<div class="sb-menu">' +
      menuItem("video", "Open video or audio file…", "media") +
      menuItem("link", "Open from a link…", "mediaurl") +
      menuItem("wave", "Waveform from a separate audio file…", "wavefile") +
      (S.media ? menuItem("x", "Close " + esc(S.media.name), "closemedia") : "") +
      '</div><p class="sb-hint">Files stay on this device. Phones play MP4 (H.264/HEVC) and WebM; an MKV usually needs a remux. Without a video, the preview draws the subtitles on black at the script\'s resolution.</p>');
    q(".sb-menu").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-m]");
      if (!b) return;
      closeSheet();
      var m = b.getAttribute("data-m");
      setTimeout(function () { menuAction(m); }, 30);
    });
  }
  function closeMedia() {
    pause();
    if (S.media && S.media.url && S.media.url.indexOf("blob:") === 0) URL.revokeObjectURL(S.media.url);
    S.media = null; S.wave = null;
    video.removeAttribute("src"); video.load();
    fitStage(); paintWaveNote();
  }

  // ---- files ---------------------------------------------------------------------------------------
  // Subtitle files in the wild are UTF-8, UTF-16 with a BOM, or (older Turkish releases)
  // Windows-1254. A strict UTF-8 decode tells the last case apart.
  function decodeText(buf) {
    var b = new Uint8Array(buf);
    if (b[0] === 0xFF && b[1] === 0xFE) return new TextDecoder("utf-16le").decode(buf);
    if (b[0] === 0xFE && b[1] === 0xFF) return new TextDecoder("utf-16be").decode(buf);
    try { return new TextDecoder("utf-8", { fatal: true }).decode(buf); }
    catch (e) { return new TextDecoder("windows-1254").decode(buf); }
  }
  function loadText(text, name) {
    var doc;
    try { doc = ensureLine(A.parseAny(text, name)); } catch (e) { toast("Could not read that file: " + e.message, "bad"); return; }
    flushAutosave();
    S.doc = doc;
    S.name = (name || "Untitled").replace(/\.(srt|vtt|ssa|txt)$/i, ".ass");
    if (!/\.ass$/i.test(S.name)) S.name += ".ass";
    S.projectId = null; S.dirty = false;
    H.undo = []; H.redo = []; H.lastKey = null;
    S.active = doc.events[0] ? doc.events[0].id : null;
    S.sel = S.active !== null ? new Set([S.active]) : new Set();
    S.selMode = false; S.filter = ""; $("filter").value = "";
    $("listScroll").scrollTop = 0;
    fitStage();
    refresh();
    scheduleAutosave(true);
    toast("Opened " + S.name + " — " + doc.events.length + " lines");
  }
  function openFile(file) {
    if (/^(video|audio)\//.test(file.type) || /\.(mkv|mp4|webm|mov|mka|mp3|m4a|aac|flac|ogg|opus|wav)$/i.test(file.name)) { openMedia(file); return; }
    file.arrayBuffer().then(function (buf) { loadText(decodeText(buf), file.name); });
  }
  function newScript() {
    flushAutosave();
    var doc = A.newDoc();
    S.doc = doc; S.name = "Untitled.ass"; S.projectId = null; S.dirty = false;
    H.undo = []; H.redo = [];
    S.active = doc.events[0].id; S.sel = new Set([S.active]);
    fitStage(); refresh(); scheduleAutosave(true);
    setView("edit");
  }
  function serialized(kind) { return kind === "srt" ? A.serializeSrt(S.doc) : "\uFEFF" + A.serializeAss(S.doc); }
  function outName(kind) { return S.name.replace(/\.ass$/i, "") + "." + kind; }
  function download(kind) {
    var blob = new Blob([serialized(kind)], { type: "text/plain;charset=utf-8" });
    var a = document.createElement("a");
    a.href = URL.createObjectURL(blob); a.download = outName(kind);
    document.body.appendChild(a); a.click();
    setTimeout(function () { URL.revokeObjectURL(a.href); a.remove(); }, 2000);
    if (kind === "ass") { S.dirty = false; renderBar(); scheduleAutosave(); }
    toast("Downloaded " + outName(kind));
  }
  function share() {
    var file = new File([serialized("ass")], outName("ass"), { type: "text/plain" });
    if (navigator.canShare && !navigator.canShare({ files: [file] })) { download("ass"); return; }
    navigator.share({ files: [file], title: outName("ass") }).then(function () { S.dirty = false; renderBar(); }).catch(function () {});
  }
  function openPaste() {
    openSheet("Paste subtitle text", field("ASS, SSA, SRT or WebVTT", '<textarea class="sb-textarea sb-mono" id="pasteText" rows="10" style="min-height:220px;font-size:13px" placeholder="[Script Info]&#10;…"></textarea>') +
      field("File name", '<input class="sb-input" id="pasteName" value="Pasted.ass">'),
      [{ label: "Cancel" }, { label: "Open", primary: true, onClick: function () {
        var t = q("#pasteText").value;
        if (!t.trim()) { toast("Nothing to open", "bad"); return false; }
        loadText(t, q("#pasteName").value || "Pasted.ass");
      } }]);
    if (navigator.clipboard && navigator.clipboard.readText) {
      navigator.clipboard.readText().then(function (t) { if (t && /-->|\[Script Info\]|Dialogue:/i.test(t)) q("#pasteText").value = t; }).catch(function () {});
    }
  }
  function openUrl(media) {
    openSheet(media ? "Open video from a link" : "Open subtitles from a link",
      field("Link", '<input class="sb-input" id="urlIn" type="url" inputmode="url" placeholder="https://…" autocomplete="off">',
        media ? "Direct links to MP4/WebM files or an HLS playlist the browser can play. The server must allow it (CORS) for the waveform."
          : "A raw file link — for example a Forgejo “Raw” link. The server must allow cross-origin reads."),
      [{ label: "Cancel" }, { label: "Open", primary: true, onClick: function () {
        var u = q("#urlIn").value.trim();
        if (!/^https?:\/\//i.test(u) && u.charAt(0) !== "/") { toast("Enter an http(s) link", "bad"); return false; }
        if (media) { openMedia(u); return; }
        fetch(u).then(function (r) { if (!r.ok) throw new Error("HTTP " + r.status); return r.arrayBuffer(); })
          .then(function (buf) { loadText(decodeText(buf), decodeURIComponent(u.split("/").pop().split("?")[0]) || "Linked.ass"); })
          .catch(function (e) { toast("Could not fetch: " + e.message, "bad"); });
      } }]);
  }

  // ---- autosave (IndexedDB) --------------------------------------------------------------------------
  // Every change lands on the device within a couple of seconds, so a phone that kills the tab
  // loses nothing. Downloading is still how a file leaves the device.
  var DB = (function () {
    var dbp = null;
    function open() {
      if (dbp) return dbp;
      dbp = new Promise(function (res, rej) {
        if (!window.indexedDB) { rej(new Error("no IndexedDB")); return; }
        var r = indexedDB.open("pandora-subs", 1);
        r.onupgradeneeded = function () { r.result.createObjectStore("projects", { keyPath: "id" }); };
        r.onsuccess = function () { res(r.result); };
        r.onerror = function () { rej(r.error); };
      });
      return dbp;
    }
    function tx(mode, fn) {
      return open().then(function (db) {
        return new Promise(function (res, rej) {
          var t = db.transaction("projects", mode), store = t.objectStore("projects"), out = fn(store);
          t.oncomplete = function () { res(out instanceof IDBRequest ? out.result : undefined); };
          t.onerror = function () { rej(t.error); };
        });
      });
    }
    return {
      put: function (p) { return tx("readwrite", function (s) { s.put(p); }); },
      get: function (id) { return tx("readonly", function (s) { return s.get(id); }); },
      del: function (id) { return tx("readwrite", function (s) { s.delete(id); }); },
      list: function () { return tx("readonly", function (s) { return s.getAll(); }); }
    };
  })();
  var saveTimer = null;
  function scheduleAutosave(now) {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(flushAutosave, now ? 0 : 1500);
  }
  function flushAutosave() {
    clearTimeout(saveTimer); saveTimer = null;
    if (!S.projectId) S.projectId = "p" + Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
    var p = { id: S.projectId, name: S.name, text: A.serializeAss(S.doc), updated: Date.now(), lines: S.doc.events.length,
      active: idx(S.active), dirty: S.dirty };
    DB.put(p).then(function () {
      try { localStorage.setItem(LAST_KEY, p.id); } catch (e) {}
      pruneProjects();
    }).catch(function () {});
  }
  function pruneProjects() {
    DB.list().then(function (all) {
      all.sort(function (a, b) { return b.updated - a.updated; });
      all.slice(20).forEach(function (p) { DB.del(p.id); });
    }).catch(function () {});
  }
  // An editor with no line has nothing to put in the edit box, so an empty file gets one.
  function ensureLine(doc) {
    if (!doc.events.length) doc.events = [A.makeEvent({ start: 0, end: settings.defaultDur, style: doc.styles[0] ? doc.styles[0].Name : "Default" })];
    return doc;
  }
  function loadProject(p) {
    var doc = ensureLine(A.parseAss(p.text));
    S.doc = doc; S.name = p.name; S.projectId = p.id; S.dirty = !!p.dirty;
    H.undo = []; H.redo = []; H.lastKey = null;
    var a = doc.events[Math.max(0, Math.min(doc.events.length - 1, p.active || 0))];
    S.active = a ? a.id : null; S.sel = a ? new Set([a.id]) : new Set();
    try { localStorage.setItem(LAST_KEY, p.id); } catch (e) {}
    fitStage(); refresh();
    requestAnimationFrame(function () { if (S.active !== null) ensureRowVisible(S.active); });
  }
  function openRecent() {
    DB.list().then(function (all) {
      all.sort(function (a, b) { return b.updated - a.updated; });
      var html = all.length ? '<div class="sb-card">' + all.map(function (p) {
        return '<div class="sb-stylerow" data-p="' + esc(p.id) + '"><div class="sb-stylemeta"><b>' + esc(p.name) + (p.id === S.projectId ? " · open" : "") +
          "</b><span>" + p.lines + " lines · " + new Date(p.updated).toLocaleString() + (p.dirty ? " · not downloaded" : "") + "</span></div>" +
          '<button class="sb-ibtn" data-del="' + esc(p.id) + '" title="Remove from this device" aria-label="Remove">' + ic("trash") + "</button></div>";
      }).join("") + "</div>" : '<div class="sb-empty"><b>Nothing saved yet</b>Scripts you open or create are kept here automatically.</div>';
      openSheet("Recent scripts", html + '<p class="sb-hint">Kept in this browser only. The 20 most recent are kept.</p>');
      $("sheetBody").addEventListener("click", function onClick(ev) {
        var del = ev.target.closest("[data-del]");
        if (del) {
          ev.stopPropagation();
          var id = del.getAttribute("data-del");
          if (id === S.projectId) { toast("That script is open", "bad"); return; }
          DB.del(id).then(function () { del.closest(".sb-stylerow").remove(); });
          return;
        }
        var row = ev.target.closest("[data-p]");
        if (!row) return;
        $("sheetBody").removeEventListener("click", onClick);
        var id2 = row.getAttribute("data-p");
        closeSheet();
        if (id2 === S.projectId) return;
        flushAutosave();
        DB.get(id2).then(function (p) { if (p) { loadProject(p); toast("Opened " + p.name); } });
      });
    }).catch(function () { toast("This browser does not allow local storage here", "bad"); });
  }

  // ---- styles ------------------------------------------------------------------------------------
  function swatch(st) {
    var p = A.parseColor(st.PrimaryColour), o = A.parseColor(st.OutlineColour);
    var oc = A.colorToCss(o), w = Math.min(2, Math.max(0, Number(st.Outline) || 0));
    var sh = w ? [[-1, -1], [1, -1], [-1, 1], [1, 1], [0, -1], [0, 1], [-1, 0], [1, 0]].map(function (d) { return d[0] * w + "px " + d[1] * w + "px 0 " + oc; }).join(",") : "none";
    return '<div class="sb-swatch" style="color:' + A.colorToCss(p) + ";font-family:'" + esc(st.Fontname) + "',sans-serif;" +
      (st.Bold ? "font-weight:700;" : "font-weight:400;") + (st.Italic ? "font-style:italic;" : "") + "text-shadow:" + sh + '">Aa</div>';
  }
  function renderStyles() {
    var body = $("stylesBody");
    if (!body.offsetParent && !isWide()) return;
    var counts = {};
    S.doc.events.forEach(function (e) { counts[e.style] = (counts[e.style] || 0) + 1; });
    body.innerHTML = '<div class="sb-row"><button class="sb-btn sb-primary sb-grow" data-sa="new">' + ic("plus") + "New style</button>" +
      '<button class="sb-btn" data-sa="import">' + ic("folder") + "Import…</button></div>" +
      '<div class="sb-card">' + S.doc.styles.map(function (st, i) {
        return '<div class="sb-stylerow" data-si="' + i + '">' + swatch(st) + '<div class="sb-stylemeta"><b>' + esc(st.Name) + "</b><span>" +
          esc(st.Fontname) + " " + A.fmtNum(st.Fontsize) + " · an" + st.Alignment + " · " + (counts[st.Name] || 0) + " line" + (counts[st.Name] === 1 ? "" : "s") + "</span></div>" +
          '<button class="sb-ibtn" data-sapply="' + i + '" title="Use for the selected lines" aria-label="Use for the selected lines">' + ic("select") + "</button></div>";
      }).join("") + "</div>" +
      (function () {
        var missing = Object.keys(counts).filter(function (n) { return styleNames().indexOf(n) === -1; });
        return missing.length ? '<p class="sb-hint" style="color:var(--pn-bad)">Lines use styles that do not exist: ' + missing.map(esc).join(", ") +
          ' — <button class="sb-btn sb-sm" data-sa="fixmissing">Create them</button></p>' : "";
      })();
  }
  function initStyles() {
    $("stylesBody").addEventListener("click", function (ev) {
      var ap = ev.target.closest("[data-sapply]");
      if (ap) {
        ev.stopPropagation();
        var name = S.doc.styles[Number(ap.getAttribute("data-sapply"))].Name;
        var ids = S.sel.size ? new Set(S.sel) : new Set([S.active]);
        change("Set style", function () { mapEvents(ids, function (e) { return A.withEvent(e, { style: name }); }); });
        toast(ids.size + " line" + (ids.size === 1 ? "" : "s") + " now use " + name);
        return;
      }
      var row = ev.target.closest("[data-si]");
      if (row) { openStyleEditor(Number(row.getAttribute("data-si"))); return; }
      var b = ev.target.closest("[data-sa]");
      if (!b) return;
      var a = b.getAttribute("data-sa");
      if (a === "new") openStyleEditor(-1);
      else if (a === "import") $("styleFile").click();
      else if (a === "fixmissing") {
        change("Create missing styles", function () {
          var have = styleNames(), add = [];
          S.doc.events.forEach(function (e) { if (have.indexOf(e.style) === -1 && add.indexOf(e.style) === -1) add.push(e.style); });
          S.doc.styles = S.doc.styles.concat(add.map(function (n) { var s = copyStyle(S.doc.styles[0] || A.defaultStyle()); s.Name = n; return s; }));
        });
      }
    });
    $("styleFile").addEventListener("change", function () {
      var f = this.files && this.files[0];
      this.value = "";
      if (!f) return;
      f.arrayBuffer().then(function (buf) { openStyleImport(A.parseAss(decodeText(buf)), f.name); });
    });
  }
  function copyStyle(s) { var c = {}; for (var k in s) c[k] = s[k]; return c; }
  function openStyleImport(src, name) {
    var have = styleNames();
    openSheet("Import styles from " + name, '<div class="sb-card">' + src.styles.map(function (st, i) {
      return '<label class="sb-stylerow"><input type="checkbox" data-imp="' + i + '" checked style="width:20px;height:20px">' + swatch(st) +
        '<div class="sb-stylemeta"><b>' + esc(st.Name) + "</b><span>" + esc(st.Fontname) + " " + A.fmtNum(st.Fontsize) +
        (have.indexOf(st.Name) !== -1 ? " · replaces the existing style" : "") + "</span></div></label>";
    }).join("") + "</div>" + (A.playRes(src).y !== A.playRes(S.doc).y ? '<label class="sb-check" style="margin-top:10px"><input type="checkbox" id="impScale" checked>Scale to this script\'s resolution (' + A.playRes(src).y + "p → " + A.playRes(S.doc).y + "p)</label>" : ""),
    [{ label: "Cancel" }, { label: "Import", primary: true, onClick: function () {
      var picked = qa("[data-imp]").filter(function (c) { return c.checked; }).map(function (c) { return src.styles[Number(c.getAttribute("data-imp"))]; });
      var scale = q("#impScale") && q("#impScale").checked;
      if (scale) {
        var dst = A.playRes(S.doc);
        var tmp = A.resample({ info: src.info, styles: picked, events: [], extras: [] }, dst.x, dst.y);
        picked = tmp.styles;
      }
      change("Import " + picked.length + " styles", function () {
        var list = S.doc.styles.slice();
        picked.forEach(function (st) {
          var i = list.map(function (s) { return s.Name; }).indexOf(st.Name);
          if (i === -1) list.push(copyStyle(st)); else list[i] = copyStyle(st);
        });
        S.doc.styles = list;
      });
      toast("Imported " + picked.length + " style" + (picked.length === 1 ? "" : "s"));
    } }]);
  }
  var COLOR_FIELDS = [["PrimaryColour", "Primary"], ["SecondaryColour", "Secondary (karaoke)"], ["OutlineColour", "Border"], ["BackColour", "Shadow"]];
  function openStyleEditor(i) {
    var isNew = i < 0;
    var st = isNew ? copyStyle(S.doc.styles[0] || A.defaultStyle()) : copyStyle(S.doc.styles[i]);
    if (isNew) { var n = 1, base = "New style"; st.Name = base; while (styleNames().indexOf(st.Name) !== -1) st.Name = base + " " + (++n); }
    var oldName = isNew ? null : st.Name;
    var fonts = {};
    ["Arial", "Verdana", "Tahoma", "Trebuchet MS", "Georgia", "Times New Roman", "Roboto", "Open Sans", "Gandhi Sans", "Noto Sans"].concat(S.doc.styles.map(function (s) { return s.Fontname; })).forEach(function (f) { fonts[f] = 1; });
    var colorRow = function (f) {
      var c = A.parseColor(st[f[0]]);
      return '<div class="sb-field"><label class="sb-label">' + f[1] + '</label><div class="sb-colorrow">' +
        '<input type="color" data-col="' + f[0] + '" value="' + A.colorToHex(c) + '">' +
        '<input type="range" min="0" max="255" data-alpha="' + f[0] + '" value="' + (255 - c.a) + '" aria-label="' + f[1] + ' opacity">' +
        '<span class="sb-mono" style="font-size:12.5px" data-alabel="' + f[0] + '">' + Math.round((255 - c.a) / 2.55) + "%</span></div></div>";
    };
    var body =
      '<canvas class="sb-stylepreview" id="stPrev"></canvas>' +
      '<div class="sb-field" style="margin-top:8px"><input class="sb-input" id="stSample" value="The quick brown fox · Ağaç Şiş Öğün" aria-label="Preview text"></div>' +
      '<div class="sb-stack">' +
      '<div class="sb-grid2">' + field("Name", '<input class="sb-input" data-f="Name" value="' + esc(st.Name) + '">') +
      field("Font", '<input class="sb-input" data-f="Fontname" list="fontList" value="' + esc(st.Fontname) + '"><datalist id="fontList">' + Object.keys(fonts).map(function (f) { return '<option value="' + esc(f) + '">'; }).join("") + "</datalist>") + "</div>" +
      '<div class="sb-grid2">' + field("Size", numInput("stSize", st.Fontsize, 'data-f="Fontsize" min="1" step="1"')) +
      '<div class="sb-field"><span class="sb-label">Emphasis</span><div class="sb-seg" id="stEmph">' +
      [["Bold", "B"], ["Italic", "I"], ["Underline", "U"], ["StrikeOut", "S"]].map(function (x) { return '<button type="button" data-toggle="' + x[0] + '"' + (st[x[0]] ? ' aria-pressed="true"' : "") + ' style="font-weight:700">' + x[1] + "</button>"; }).join("") +
      "</div></div></div>" +
      COLOR_FIELDS.map(colorRow).join("") +
      '<div class="sb-grid3">' + field("Border", numInput("stOutline", st.Outline, 'data-f="Outline" min="0" step="0.5"')) +
      field("Shadow", numInput("stShadow", st.Shadow, 'data-f="Shadow" min="0" step="0.5"')) +
      '<div class="sb-field"><span class="sb-label">Border style</span><select class="sb-select" data-f="BorderStyle"><option value="1"' + (st.BorderStyle !== 3 ? " selected" : "") + '>Outline</option><option value="3"' + (st.BorderStyle === 3 ? " selected" : "") + ">Opaque box</option></select></div></div>" +
      '<div class="sb-row" style="align-items:flex-start;gap:16px"><div class="sb-field"><span class="sb-label">Alignment</span><div class="sb-anpad" id="stAn">' +
      [7, 8, 9, 4, 5, 6, 1, 2, 3].map(function (a) { return '<button type="button" data-an="' + a + '"' + (st.Alignment === a ? ' aria-pressed="true"' : "") + ">" + a + "</button>"; }).join("") +
      '</div></div><div class="sb-grow sb-grid3">' + field("Margin L", numInput("stML", st.MarginL, 'data-f="MarginL" min="0"')) +
      field("Margin R", numInput("stMR", st.MarginR, 'data-f="MarginR" min="0"')) + field("Margin V", numInput("stMV", st.MarginV, 'data-f="MarginV" min="0"')) + "</div></div>" +
      '<details class="sb-more"><summary>Scale, spacing, angle</summary><div class="sb-grid2">' +
      field("Scale X %", numInput("stSX", st.ScaleX, 'data-f="ScaleX" min="1"')) + field("Scale Y %", numInput("stSY", st.ScaleY, 'data-f="ScaleY" min="1"')) +
      field("Spacing", numInput("stSp", st.Spacing, 'data-f="Spacing" step="0.5"')) + field("Angle", numInput("stAng", st.Angle, 'data-f="Angle"')) +
      field("Encoding", numInput("stEnc", st.Encoding, 'data-f="Encoding" min="0"')) + "</div></details>" +
      "</div>";
    var buttons = [];
    if (!isNew) {
      buttons.push({ label: ic("trash") + "Delete", danger: true, onClick: function () { deleteStyle(i); } });
      buttons.push({ label: ic("copy") + "Copy", onClick: function () {
        var c = copyStyle(st), n2 = 2;
        c.Name = st.Name + " copy"; while (styleNames().indexOf(c.Name) !== -1) c.Name = st.Name + " copy " + (n2++);
        change("Duplicate style", function () { var l = S.doc.styles.slice(); l.splice(i + 1, 0, c); S.doc.styles = l; });
        toast("Created " + c.Name);
      } });
    }
    buttons.push({ label: isNew ? "Create" : "Save", primary: true, onClick: function () {
      st.Name = String(st.Name || "").trim().replace(/,/g, ";");
      if (!st.Name) { toast("A style needs a name", "bad"); return false; }
      var clash = S.doc.styles.some(function (s, j) { return s.Name === st.Name && j !== i; });
      if (clash) { toast("Another style is already called " + st.Name, "bad"); return false; }
      change(isNew ? "New style" : "Edit style " + st.Name, function () {
        var l = S.doc.styles.slice();
        if (isNew) l.push(st); else l[i] = st;
        S.doc.styles = l;
        if (oldName && oldName !== st.Name) mapEvents(new Set(S.doc.events.filter(function (e) { return e.style === oldName; }).map(function (e) { return e.id; })), function (e) { return A.withEvent(e, { style: st.Name }); });
      });
    } });
    openSheet(isNew ? "New style" : "Style: " + st.Name, body, buttons);
    var prev = function () {
      var c = q("#stPrev");
      if (!c) return;
      var dpr = Math.min(2, window.devicePixelRatio || 1);
      c.width = Math.round(c.clientWidth * dpr); c.height = Math.round(c.clientHeight * dpr);
      var res = A.playRes(S.doc);
      // The strip shows a third of the frame's height, so text reads at roughly the size it will
      // have on a phone-width video rather than as a speck across the full script width.
      var ph = Math.max(40, Math.round(res.y * 0.34)), pw = Math.round(ph * c.width / Math.max(1, c.height));
      var fake = { info: [{ k: "PlayResX", v: String(pw) }, { k: "PlayResY", v: String(ph) }, { k: "ScaledBorderAndShadow", v: "yes" }],
        styles: [st], events: [A.withEvent(A.makeEvent({ start: 0, end: 1e9, style: st.Name }), { text: "{\\an5}" + q("#stSample").value.replace(/[{}]/g, "") })], extras: [] };
      R.render(c, fake, 1000, {});
    };
    var body2 = $("sheetBody");
    body2.addEventListener("input", function (ev) {
      var t = ev.target;
      var f = t.getAttribute("data-f");
      if (f) st[f] = f === "Name" || f === "Fontname" ? t.value : Number(t.value) || 0;
      var col = t.getAttribute("data-col"), al = t.getAttribute("data-alpha");
      if (col) { var c0 = A.parseColor(st[col]); st[col] = A.styleColor(A.hexToColor(t.value, c0.a)); }
      if (al) {
        var c1 = A.parseColor(st[al]); c1.a = 255 - Number(t.value); st[al] = A.styleColor(c1);
        q('[data-alabel="' + al + '"]').textContent = Math.round(Number(t.value) / 2.55) + "%";
      }
      if (f === "Fontname") R.clearMetrics();
      prev();
    });
    body2.addEventListener("change", function (ev) { var f = ev.target.getAttribute("data-f"); if (f === "BorderStyle") { st.BorderStyle = Number(ev.target.value); prev(); } });
    q("#stEmph").addEventListener("click", function (ev) {
      var b = ev.target.closest("[data-toggle]");
      if (!b) return;
      var k = b.getAttribute("data-toggle");
      st[k] = !st[k];
      if (st[k]) b.setAttribute("aria-pressed", "true"); else b.removeAttribute("aria-pressed");
      prev();
    });
    q("#stAn").addEventListener("click", function (ev) {
      var b = ev.target.closest("[data-an]");
      if (!b) return;
      st.Alignment = Number(b.getAttribute("data-an"));
      qa("#stAn button").forEach(function (x) { x.removeAttribute("aria-pressed"); });
      b.setAttribute("aria-pressed", "true");
    });
    requestAnimationFrame(prev);
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(prev);
  }
  function deleteStyle(i) {
    var st = S.doc.styles[i], used = S.doc.events.filter(function (e) { return e.style === st.Name; }).length;
    var others = S.doc.styles.filter(function (s, j) { return j !== i; });
    if (!used) {
      change("Delete style " + st.Name, function () { S.doc.styles = others.length ? others : [A.defaultStyle()]; });
      return;
    }
    setTimeout(function () {
      openSheet("Delete " + st.Name + "?", "<p>" + used + " line" + (used === 1 ? " uses" : "s use") + " this style. Move them to:</p>" +
        '<select class="sb-select" id="reassign">' + (others.length ? others : [A.defaultStyle()]).map(function (s) { return '<option value="' + esc(s.Name) + '">' + esc(s.Name) + "</option>"; }).join("") + "</select>",
        [{ label: "Cancel" }, { label: "Delete style", danger: true, onClick: function () {
          var to = q("#reassign").value;
          change("Delete style " + st.Name, function () {
            S.doc.styles = others.length ? others : [A.defaultStyle()];
            mapEvents(new Set(S.doc.events.filter(function (e) { return e.style === st.Name; }).map(function (e) { return e.id; })), function (e) { return A.withEvent(e, { style: to }); });
          });
        } }]);
    }, 50);
  }
  function openSetStyle() {
    openSheet("Style for " + S.sel.size + " line" + (S.sel.size === 1 ? "" : "s"), '<div class="sb-card">' + S.doc.styles.map(function (st, i) {
      return '<div class="sb-stylerow" data-pick="' + i + '">' + swatch(st) + '<div class="sb-stylemeta"><b>' + esc(st.Name) + "</b><span>" + esc(st.Fontname) + " " + A.fmtNum(st.Fontsize) + "</span></div></div>";
    }).join("") + "</div>");
    q(".sb-card").addEventListener("click", function (ev) {
      var r = ev.target.closest("[data-pick]");
      if (!r) return;
      var name = S.doc.styles[Number(r.getAttribute("data-pick"))].Name, ids = new Set(S.sel);
      change("Set style", function () { mapEvents(ids, function (e) { return A.withEvent(e, { style: name }); }); });
      closeSheet();
    });
  }

  // ---- tools ----------------------------------------------------------------------------------------
  var TOOLS = [
    ["hydra", "wand", "HYDRA tags", "Colours, borders, blur, scale, rotation — at the start, the cursor, as a transform or a gradient.", function () { openHydra(); }],
    ["select", "select", "Select lines", "By text, style, actor, CPS, length or duration; overlaps and comments.", function () { openSelect(); }],
    ["find", "replace", "Find & replace", "Plain or regular expressions, outside tags or in the raw line.", function () { openFind(); }],
    ["sort", "sort", "Sort lines", "By time, style, actor, effect, layer, text, CPS or length.", function () { openSort(); }],
    ["shift", "shift", "Shift times", "Move lines earlier or later by a time or a number of frames.", function () { openShift(); }],
    ["timing", "clock", "Timing post-processor", "Lead-in, lead-out, close small gaps, snap to frames.", function () { openPostTime(); }],
    ["reading", "reading", "Reading speed", "Lengthen lines to a target CPS without overlapping the next.", function () { openReading(); }],
    ["break", "brk", "Line breaker", "Balance long lines with \\N, rebalance or remove breaks.", function () { openBreaker(); }],
    ["clean", "broom", "Clean up", "Strip or tidy tags, remove notes, empty and duplicate lines.", function () { openClean(); }],
    ["fade", "fade", "Fade", "Add \\fad to the selected lines.", function () { openFade(); }],
    ["case", "abc", "Change case", "UPPER, lower, Title or Sentence case, tags untouched.", function () { openCase(); }],
    ["resample", "resize", "Resample resolution", "Rescale styles, positions and drawings to a new PlayRes.", function () { openResample(); }],
    ["qc", "flag", "Quality check", "Overlaps, fast lines, long lines, missing styles and more.", function () { openQC(); }],
    ["props", "info", "Script properties", "Title, resolution, wrap style, border scaling, colour matrix.", function () { openProps(); }]
  ];
  function renderTools() {
    $("toolsBody").innerHTML = '<div class="sb-tools">' + TOOLS.map(function (t) {
      return '<button class="sb-tool" data-tool="' + t[0] + '">' + ic(t[1]) + "<div><b>" + esc(t[2]) + "</b><span>" + esc(t[3]) + "</span></div></button>";
    }).join("") + '</div><p class="sb-hint">Tools act on the selected lines; pick "All lines" in a tool to run it over the whole script. Every tool can be undone.</p>';
    $("toolsBody").addEventListener("click", function (ev) {
      var b = ev.target.closest("[data-tool]");
      if (b) TOOLS.filter(function (t) { return t[0] === b.getAttribute("data-tool"); })[0][4]();
    });
  }
  function done(n, what) { toast(what + " — " + n + " line" + (n === 1 ? "" : "s")); }

  // HYDRA: the tag grid remembers what was last used, like the Lua script's config.
  var HYDRA_GROUPS = [
    ["Colours", ["c", "2c", "3c", "4c"]],
    ["Alpha", ["alpha", "1a", "3a", "4a"]],
    ["Border & shadow", ["bord", "shad", "xbord", "ybord", "xshad", "yshad", "blur", "be"]],
    ["Font & scale", ["fs", "fscx", "fscy", "fsp", "fn", "b", "i", "u", "s"]],
    ["Rotation & shear", ["frz", "frx", "fry", "fax", "fay"]],
    ["Layout", ["an", "q"]]
  ];
  function hydraMemory() {
    try { return JSON.parse(localStorage.getItem(HYDRA_KEY) || "{}"); } catch (e) { return {}; }
  }
  function defaultHydraValue(n) {
    var def = A.HYDRA_TAGS[n];
    if (def.kind === "color") return n === "3c" || n === "4c" ? "#000000" : "#ffffff";
    if (def.kind === "alpha") return 0;
    if (n === "fscx" || n === "fscy") return 100;
    if (n === "fs") return 48;
    if (n === "an") return 8;
    if (n === "b" || n === "i" || n === "u" || n === "s") return 1;
    if (n === "fn") return "Arial";
    if (n === "bord") return 2;
    if (n === "blur") return 0.6;
    return 0;
  }
  function hydraInput(n, which, v) {
    var def = A.HYDRA_TAGS[n], cls = which === "to" ? " sb-to" : "";
    var attr = 'data-h="' + n + '" data-w="' + which + '"';
    if (def.kind === "color") return '<input type="color" class="' + cls + '" ' + attr + ' value="' + esc(v) + '">';
    if (def.kind === "str") return '<input class="sb-input' + cls + '" ' + attr + ' value="' + esc(v) + '">';
    var step = def.kind === "int" || def.kind === "alpha" ? "1" : "0.1";
    return '<input class="sb-input' + cls + '" type="number" inputmode="decimal" step="' + step + '" ' + attr + ' value="' + esc(v) + '">';
  }
  function openHydra(mode) {
    var mem = hydraMemory(), on = mem.on || {}, from = mem.from || {}, to = mem.to || {};
    mode = mode || mem.mode || "start";
    var html = '<div class="sb-field"><span class="sb-label">Where</span>' + seg("hmode", [["start", "Line start"], ["cursor", "At cursor"], ["transform", "\\t"], ["gchar", "Grad. chars"], ["gline", "Grad. lines"]], mode) + "</div>" +
      '<div class="sb-grid3" id="hT"' + (mode === "transform" ? "" : " hidden") + ">" +
      field("t1 ms", numInput("hT1", mem.t1 === undefined ? "" : mem.t1, 'placeholder="line start"')) +
      field("t2 ms", numInput("hT2", mem.t2 === undefined ? "" : mem.t2, 'placeholder="line end"')) +
      field("Accel", numInput("hAcc", mem.accel === undefined ? 1 : mem.accel, 'step="0.1" min="0.01"')) + "</div>" +
      '<label class="sb-check" id="hSkipWrap"' + (mode === "gchar" ? "" : " hidden") + '><input type="checkbox" id="hSkip"' + (mem.skip === false ? "" : " checked") + ">Skip spaces in the gradient</label>" +
      '<p class="sb-hint" id="hHelp"></p>' +
      '<div class="sb-hydra" id="hGrid" data-grad="' + (mode === "gchar" || mode === "gline" ? "1" : "0") + '">' +
      HYDRA_GROUPS.map(function (g) {
        return '<h4 style="margin:12px 0 2px;font-size:12px;color:var(--pn-faint);text-transform:uppercase;letter-spacing:.06em">' + g[0] + "</h4>" +
          g[1].map(function (n) {
            var def = A.HYDRA_TAGS[n];
            var f = from[n] !== undefined ? from[n] : defaultHydraValue(n), t = to[n] !== undefined ? to[n] : defaultHydraValue(n);
            return '<div class="sb-hrow"><input type="checkbox" id="hon-' + n + '" data-hon="' + n + '"' + (on[n] ? " checked" : "") + ">" +
              '<label for="hon-' + n + '">' + esc(def.label) + " <small>\\" + n + "</small></label>" + hydraInput(n, "from", f) + hydraInput(n, "to", t) + "</div>";
          }).join("");
      }).join("") + "</div>" + scopeField();
    openSheet("HYDRA — tags", html, [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: applyHydra }]);
    var help = {
      start: "Sets the tags in each line's first override block, replacing the same tag already there.",
      cursor: "Inserts the tags where the cursor is in the active line (other selected lines get them at the start).",
      transform: "Adds \\t(t1,t2,accel,…) — leave t1/t2 empty to run over the whole line.",
      gchar: "Blends every ticked tag from the left value to the right value across each line's characters.",
      gline: "Blends from the left value on the first selected line to the right value on the last."
    };
    var sync = function () {
      var m = segVal("hmode");
      q("#hT").hidden = m !== "transform";
      q("#hSkipWrap").hidden = m !== "gchar";
      q("#hGrid").setAttribute("data-grad", m === "gchar" || m === "gline" ? "1" : "0");
      q("#hHelp").textContent = help[m];
    };
    q('[data-seg="hmode"]').addEventListener("segchange", sync);
    // Editing a value ticks its box: nobody types a border size they don't want applied.
    q("#hGrid").addEventListener("input", function (ev) {
      var n = ev.target.getAttribute("data-h");
      if (n) q('[data-hon="' + n + '"]').checked = true;
    });
    sync();
  }
  function applyHydra() {
    var mode = segVal("hmode"), on = {}, from = {}, to = {}, any = false;
    qa("[data-hon]").forEach(function (c) { var n = c.getAttribute("data-hon"); on[n] = c.checked; if (c.checked) any = true; });
    qa("[data-h]").forEach(function (inp) {
      var n = inp.getAttribute("data-h"), def = A.HYDRA_TAGS[n];
      var v = def.kind === "color" || def.kind === "str" ? inp.value : Number(inp.value);
      (inp.getAttribute("data-w") === "to" ? to : from)[n] = v;
    });
    var mem = { mode: mode, on: on, from: from, to: to, t1: q("#hT1").value, t2: q("#hT2").value, accel: q("#hAcc").value, skip: q("#hSkip").checked };
    try { localStorage.setItem(HYDRA_KEY, JSON.stringify(mem)); } catch (e) {}
    if (!any) { toast("Tick at least one tag", "bad"); return false; }
    var vals = {}, tos = {};
    Object.keys(on).forEach(function (n) { if (on[n]) { vals[n] = from[n]; tos[n] = to[n]; } });
    var ids = scopeIds(segVal("scope"));
    var order = S.doc.events.filter(function (e) { return ids.has(e.id); }).map(function (e) { return e.id; });
    var caretId = S.active, caret = S.caret;
    change("HYDRA", function () {
      mapEvents(ids, function (e) {
        var text = e.text;
        if (mode === "gchar") text = A.hydraGradientChars(text, vals, tos, mem.skip);
        else if (mode === "gline") {
          var k = order.indexOf(e.id), t = order.length > 1 ? k / (order.length - 1) : 0, blended = {};
          Object.keys(vals).forEach(function (n) { blended[n] = A.blend(n, vals[n], tos[n], t); });
          text = A.hydraApply(text, blended, { mode: "start" });
        } else if (mode === "cursor") {
          text = A.hydraApply(text, vals, e.id === caretId ? { mode: "cursor", offset: caret } : { mode: "start" });
        } else text = A.hydraApply(text, vals, { mode: mode, t1: mem.t1, t2: mem.t2, accel: mem.accel });
        return A.withEvent(e, { text: text });
      });
    });
    done(ids.size, "Tags applied");
  }

  var SELECT_FIELDS = [["visible", "Text (as seen)"], ["text", "Text (with tags)"], ["style", "Style"], ["actor", "Actor"], ["effect", "Effect"],
    ["layer", "Layer"], ["cps", "CPS"], ["length", "Line length"], ["duration", "Duration (ms)"], ["start", "Start (ms)"]];
  var SELECT_OPS = [["contains", "contains"], ["not", "does not contain"], ["equals", "is"], ["starts", "starts with"], ["ends", "ends with"],
    ["regex", "matches regex"], ["notregex", "doesn't match regex"], ["gt", ">"], ["ge", "≥"], ["lt", "<"], ["le", "≤"], ["eqnum", "= (number)"]];
  function openSelect() {
    var a = activeEvent();
    var html = '<div class="sb-field"><span class="sb-label">Quick picks</span><div class="sb-row">' +
      [["overlap", "Overlapping"], ["cps", "Over " + settings.cpsLimit + " CPS"], ["long", "Over " + settings.lenLimit + " chars"], ["comment", "Comments"],
        ["empty", "Empty"], ["samestyle", "Same style" + (a ? " (" + a.style + ")" : "")], ["sameactor", "Same actor"], ["zero", "Zero duration"]].map(function (p) {
        return '<button class="sb-btn sb-sm" data-quick="' + p[0] + '">' + esc(p[1]) + "</button>";
      }).join("") + "</div></div><hr class=\"sb-sep\">" +
      '<div class="sb-grid2">' + field("Field", '<select class="sb-select" id="selField">' + SELECT_FIELDS.map(function (f) { return '<option value="' + f[0] + '">' + f[1] + "</option>"; }).join("") + "</select>") +
      field("Condition", '<select class="sb-select" id="selOp">' + SELECT_OPS.map(function (f) { return '<option value="' + f[0] + '">' + f[1] + "</option>"; }).join("") + "</select>") + "</div>" +
      field("Value", '<input class="sb-input" id="selVal" autocomplete="off">') +
      '<label class="sb-check"><input type="checkbox" id="selCase">Match case</label>' +
      '<label class="sb-check"><input type="checkbox" id="selComments">Include comment lines</label>' +
      '<div class="sb-field"><span class="sb-label">Selection</span>' + seg("selMode", [["set", "Replace"], ["add", "Add"], ["sub", "Remove"], ["and", "Intersect"]], "set") + "</div>";
    openSheet("Select lines", html, [{ label: "Cancel" }, { label: "Select", primary: true, onClick: function () {
      var m = A.buildMatcher(q("#selOp").value, q("#selVal").value, q("#selCase").checked);
      if (!m) { toast("That regular expression is not valid", "bad"); return false; }
      var fieldName = q("#selField").value, withComments = q("#selComments").checked;
      applySelection(function (e) { return (withComments || !e.comment) && m(A.fieldValue(e, fieldName)); }, segVal("selMode"));
    } }]);
    q(".sb-row").addEventListener("click", function (ev) {
      var b = ev.target.closest("[data-quick]");
      if (!b) return;
      var k = b.getAttribute("data-quick"), ov = overlapSet(), act = activeEvent();
      var preds = {
        overlap: function (e) { return ov.has(e.id); },
        cps: function (e) { return !e.comment && info(e).cps > settings.cpsLimit; },
        long: function (e) { return !e.comment && info(e).len > settings.lenLimit; },
        comment: function (e) { return e.comment; },
        empty: function (e) { return !A.strippedText(e.text).trim() && !A.isDrawing(e.text); },
        samestyle: function (e) { return act && e.style === act.style; },
        sameactor: function (e) { return act && e.actor === act.actor; },
        zero: function (e) { return e.end <= e.start; }
      };
      closeSheet();
      applySelection(preds[k], "set");
    });
  }
  function applySelection(pred, mode) {
    var hit = new Set(S.doc.events.filter(pred).map(function (e) { return e.id; })), out;
    if (mode === "add") { out = new Set(S.sel); hit.forEach(function (id) { out.add(id); }); }
    else if (mode === "sub") { out = new Set(S.sel); hit.forEach(function (id) { out.delete(id); }); }
    else if (mode === "and") { out = new Set(); hit.forEach(function (id) { if (S.sel.has(id)) out.add(id); }); }
    else out = hit;
    S.sel = out;
    if (out.size) {
      var first = S.doc.events.filter(function (e) { return out.has(e.id); })[0];
      S.active = first.id;
      if (!isWide()) { S.selMode = out.size > 1; setView("lines"); }
      requestAnimationFrame(function () { ensureRowVisible(first.id); });
    }
    S.filter = ""; $("filter").value = "";
    refresh();
    toast(out.size + " line" + (out.size === 1 ? "" : "s") + " selected");
  }
  function openFind() {
    var html = field("Find", '<input class="sb-input" id="fFind" autocomplete="off">') +
      field("Replace with", '<input class="sb-input" id="fRep" autocomplete="off">', "With a regular expression, $1 inserts the first group.") +
      '<div class="sb-grid2">' + field("In", '<select class="sb-select" id="fIn"><option value="text">Text</option><option value="actor">Actor</option><option value="style">Style</option><option value="effect">Effect</option></select>') + "<div></div></div>" +
      '<label class="sb-check"><input type="checkbox" id="fRe">Regular expression</label>' +
      '<label class="sb-check"><input type="checkbox" id="fCase">Match case</label>' +
      '<label class="sb-check"><input type="checkbox" id="fRaw">Search inside tags too</label>' + scopeField(true);
    var build = function () {
      var src = q("#fFind").value;
      if (!src) { toast("Enter something to find", "bad"); return null; }
      var flags = "g" + (q("#fCase").checked ? "" : "i") + "u";
      try { return new RegExp(q("#fRe").checked ? src : src.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), flags); }
      catch (e) { toast("That regular expression is not valid", "bad"); return null; }
    };
    openSheet("Find & replace", html, [
      { label: "Find next", onClick: function () {
        var re = build();
        if (!re) return false;
        var fieldName = q("#fIn").value, raw = q("#fRaw").checked, n = S.doc.events.length, start = idx(S.active);
        for (var k = 1; k <= n; k++) {
          var e = S.doc.events[(start + k) % n];
          var hay = fieldName === "text" ? (raw ? e.text : A.strippedText(e.text)) : e[fieldName];
          re.lastIndex = 0;
          if (re.test(hay)) { setActive(e.id); toast("Line " + ((start + k) % n + 1)); return false; }
        }
        toast("Not found", "bad");
        return false;
      } },
      { label: "Replace all", primary: true, onClick: function () {
        var re = build();
        if (!re) return false;
        var fieldName = q("#fIn").value, raw = q("#fRaw").checked, rep = q("#fRep").value, count = 0;
        var ids = scopeIds(segVal("scope"));
        change("Replace all", function () {
          mapEvents(ids, function (e) {
            var before = e[fieldName], after = fieldName === "text" ? A.replaceIn(before, re, rep, raw) : before.replace(re, rep);
            if (after === before) return e;
            count++;
            var patch = {}; patch[fieldName] = after;
            return A.withEvent(e, patch);
          });
        });
        done(count, "Replaced");
      } }
    ]);
    setTimeout(function () { var f = q("#fFind"); if (f) f.focus(); }, 60);
  }
  var SORT_OPTS = [["start", "Start time"], ["end", "End time"], ["duration", "Duration"], ["style", "Style"], ["actor", "Actor"], ["effect", "Effect"],
    ["layer", "Layer"], ["text", "Text"], ["cps", "CPS"], ["length", "Line length"], ["comment", "Comments last"]];
  function openSort() {
    openSheet("Sort lines", field("Sort by", '<select class="sb-select" id="sKey">' + SORT_OPTS.map(function (o) { return '<option value="' + o[0] + '">' + o[1] + "</option>"; }).join("") + "</select>",
      "Sorting is stable: sort by time, then by style, and lines stay in time order within each style.") +
      '<div class="sb-field"><span class="sb-label">Order</span>' + seg("sOrd", [["asc", "Ascending"], ["desc", "Descending"]], "asc") + "</div>" + scopeField(true),
      [{ label: "Cancel" }, { label: "Sort", primary: true, onClick: function () { sortLines(q("#sKey").value, segVal("sOrd") === "desc", segVal("scope")); } }]);
  }
  function openShift() {
    openSheet("Shift times", '<div class="sb-grid2">' + field("By", '<input class="sb-input sb-mono" id="shAmt" value="0:00:01.00" inputmode="decimal">') +
      '<div class="sb-field"><span class="sb-label">Unit</span>' + seg("shUnit", [["time", "Time"], ["frames", "Frames"]], "time") + "</div></div>" +
      '<div class="sb-field"><span class="sb-label">Direction</span>' + seg("shDir", [["fwd", "Later"], ["back", "Earlier"]], "fwd") + "</div>" +
      '<div class="sb-field"><span class="sb-label">Move</span>' + seg("shWhich", [["both", "Start & end"], ["start", "Start"], ["end", "End"]], "both") + "</div>" +
      '<div class="sb-field"><span class="sb-label">Apply to</span>' + seg("scope", [["sel", "Selected (" + S.sel.size + ")"], ["later", "Selected & later"], ["all", "All lines"]], S.sel.size > 1 ? "sel" : "all") + "</div>" +
      '<p class="sb-hint">Frames use ' + A.fmtNum(fps(), 3) + " fps (Settings).</p>",
    [{ label: "Cancel" }, { label: "Shift", primary: true, onClick: function () {
      var unit = segVal("shUnit"), raw = q("#shAmt").value, ms;
      if (unit === "frames") ms = Math.round((Number(raw) || 0) * frameMs());
      else ms = A.parseLooseTime(raw);
      if (ms === null || !ms) { toast("Enter an amount", "bad"); return false; }
      if (segVal("shDir") === "back") ms = -ms;
      var which = segVal("shWhich"), ids = scopeIds(segVal("scope"));
      change("Shift times", function () { mapEvents(ids, function (e) { return A.shiftEvent(e, ms, which); }); });
      done(ids.size, "Shifted " + (ms > 0 ? "+" : "") + (ms / 1000).toFixed(3) + "s");
    } }]);
    q('[data-seg="shUnit"]').addEventListener("segchange", function (ev) { q("#shAmt").value = ev.detail === "frames" ? "1" : "0:00:01.00"; });
  }
  function openPostTime() {
    openSheet("Timing post-processor", '<div class="sb-grid2">' + field("Lead-in ms", numInput("ptIn", settings.leadIn, 'min="0"')) + field("Lead-out ms", numInput("ptOut", settings.leadOut, 'min="0"')) + "</div>" +
      '<div class="sb-grid2">' + field("Link gaps under ms", numInput("ptGap", 300, 'min="0"'), "Consecutive lines closer than this meet in the middle.") +
      field("Meeting point", '<input type="range" id="ptBias" min="0" max="100" value="50" style="width:100%;accent-color:var(--pn-k1-hi)">', "Left: keep the next line's start. Right: keep this line's end.") + "</div>" +
      '<label class="sb-check"><input type="checkbox" id="ptApplyLead" checked>Add lead-in and lead-out</label>' +
      '<label class="sb-check"><input type="checkbox" id="ptSame">Only link lines of the same style</label>' +
      '<label class="sb-check"><input type="checkbox" id="ptSnap">Snap to video frames (' + A.fmtNum(fps(), 3) + " fps)</label>" + scopeField(),
    [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: function () {
      var lead = q("#ptApplyLead").checked;
      settings.leadIn = Number(q("#ptIn").value) || 0; settings.leadOut = Number(q("#ptOut").value) || 0; saveSettings();
      var o = { leadIn: lead ? settings.leadIn : 0, leadOut: lead ? settings.leadOut : 0, threshold: Number(q("#ptGap").value) || 0,
        bias: 1 - Number(q("#ptBias").value) / 100, sameStyle: q("#ptSame").checked, snap: q("#ptSnap").checked, fps: fps() };
      var ids = scopeIds(segVal("scope"));
      change("Timing post-processor", function () {
        var picked = S.doc.events.filter(function (e) { return ids.has(e.id); });
        var out = A.postTime(picked, o), map = new Map();
        out.forEach(function (e) { map.set(e.id, e); });
        S.doc.events = S.doc.events.map(function (e) { return map.get(e.id) || e; });
      });
      done(ids.size, "Timing processed");
    } }]);
  }
  function openReading() {
    openSheet("Reading speed", '<div class="sb-grid2">' + field("Target CPS", numInput("rdCps", settings.cpsLimit, 'min="1" step="0.5"')) + field("Minimum ms", numInput("rdMin", 1000, 'min="0"')) + "</div>" +
      '<label class="sb-check"><input type="checkbox" id="rdOnlyFast" checked>Only lines that are too fast</label>' +
      '<label class="sb-check"><input type="checkbox" id="rdNoOv" checked>Never run into the next line</label>' + scopeField(),
    [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: function () {
      var target = Number(q("#rdCps").value) || settings.cpsLimit, min = Number(q("#rdMin").value) || 0;
      var onlyFast = q("#rdOnlyFast").checked, noOv = q("#rdNoOv").checked, ids = scopeIds(segVal("scope")), n = 0;
      change("Reading speed", function () {
        var sorted = S.doc.events.filter(function (e) { return !e.comment; }).slice().sort(function (a, b) { return a.start - b.start; });
        var nextStart = new Map();
        sorted.forEach(function (e, k) { var nx = sorted[k + 1]; if (nx && nx.start >= e.start) nextStart.set(e.id, nx.start); });
        mapEvents(ids, function (e) {
          if (e.comment || A.isDrawing(e.text)) return e;
          if (onlyFast && info(e).cps <= target && e.end - e.start >= min) return e;
          var out = A.durationFromCps(e, target, min);
          if (out.end < e.end && onlyFast) return e;
          if (noOv && nextStart.has(e.id)) out = A.withEvent(out, { end: Math.max(e.end, Math.min(out.end, nextStart.get(e.id))) });
          if (out.end !== e.end) n++;
          return out;
        });
      });
      done(n, "Durations changed");
    } }]);
  }
  function openBreaker() {
    openSheet("Line breaker", '<div class="sb-field"><span class="sb-label">Mode</span>' + seg("lbMode", [["add", "Break long lines"], ["force", "Rebalance all"], ["remove", "Remove breaks"]], "add") + "</div>" +
      field("Break lines longer than", numInput("lbMax", Math.min(settings.lenLimit, 42), 'min="10"'), "Breaks at the space nearest the middle, preferring a longer bottom line.") + scopeField(),
    [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: function () {
      var mode = segVal("lbMode"), max = Number(q("#lbMax").value) || 42, ids = scopeIds(segVal("scope")), n = 0;
      change("Line breaker", function () {
        mapEvents(ids, function (e) {
          if (A.isDrawing(e.text)) return e;
          var t = mode === "remove" ? e.text.replace(/\s*\\N\s*/g, " ") : mode === "force" ? A.autoBreak(e.text, max, A.visibleText(e.text).replace(/\n/g, " ").length > max) : A.autoBreak(e.text, max);
          if (t === e.text) return e;
          n++;
          return A.withEvent(e, { text: t });
        });
      });
      done(n, "Line breaks changed");
    } }]);
  }
  function openClean() {
    openSheet("Clean up", '<label class="sb-check"><input type="checkbox" id="clClean" checked>Tidy tags — merge <span class="sb-mono">}{</span>, drop empty and overridden tags</label>' +
      '<label class="sb-check"><input type="checkbox" id="clNotes">Remove notes (<span class="sb-mono">{comment}</span> blocks)</label>' +
      '<label class="sb-check"><input type="checkbox" id="clStrip">Strip every override tag</label>' +
      field("Strip only these tags", '<input class="sb-input sb-mono" id="clTags" placeholder="blur, be, fad" autocomplete="off">') +
      '<label class="sb-check"><input type="checkbox" id="clTrim" checked>Trim spaces around lines and \\N</label>' +
      '<label class="sb-check"><input type="checkbox" id="clEmpty">Delete empty lines</label>' +
      '<label class="sb-check"><input type="checkbox" id="clDup">Delete duplicate lines (same time, style and text)</label>' + scopeField(true),
    [{ label: "Cancel" }, { label: "Clean", primary: true, onClick: function () {
      var o = { clean: q("#clClean").checked, notes: q("#clNotes").checked, strip: q("#clStrip").checked, trim: q("#clTrim").checked,
        empty: q("#clEmpty").checked, dup: q("#clDup").checked,
        tags: q("#clTags").value.split(/[\s,\\]+/).filter(Boolean) };
      var ids = scopeIds(segVal("scope")), n = 0, removed = 0;
      change("Clean up", function () {
        mapEvents(ids, function (e) {
          var t = e.text;
          if (o.strip) t = A.stripTags(t);
          else if (o.tags.length) t = A.stripTags(t, o.tags);
          if (o.notes) t = A.stripComments(t);
          if (o.clean) t = A.cleanTags(t);
          if (o.trim) t = t.replace(/^(\{[^}]*\})?\s+/, "$1").replace(/\s+$/, "").replace(/\s*\\N\s*/g, "\\N").replace(/ {2,}/g, " ");
          if (t === e.text) return e;
          n++;
          return A.withEvent(e, { text: t });
        });
        if (o.empty || o.dup) {
          var seen = new Set();
          S.doc.events = S.doc.events.filter(function (e) {
            if (!ids.has(e.id)) return true;
            if (o.empty && !e.text.trim()) { removed++; return false; }
            if (o.dup) {
              var key = e.start + "|" + e.end + "|" + e.style + "|" + e.text + "|" + e.comment;
              if (seen.has(key)) { removed++; return false; }
              seen.add(key);
            }
            return true;
          });
        }
      });
      toast("Cleaned " + n + " line" + (n === 1 ? "" : "s") + (removed ? ", deleted " + removed : ""));
    } }]);
  }
  function openFade() {
    openSheet("Fade", '<div class="sb-grid2">' + field("Fade in ms", numInput("fdIn", 150, 'min="0"')) + field("Fade out ms", numInput("fdOut", 150, 'min="0"')) + "</div>" +
      '<label class="sb-check"><input type="checkbox" id="fdRemove">Remove fades instead</label>' + scopeField(),
    [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: function () {
      var ids = scopeIds(segVal("scope")), rm = q("#fdRemove").checked;
      var tag = A.makeTag("fad", Math.max(0, Number(q("#fdIn").value) || 0) + "," + Math.max(0, Number(q("#fdOut").value) || 0));
      change(rm ? "Remove fades" : "Fade", function () {
        mapEvents(ids, function (e) { return A.withEvent(e, { text: rm ? A.stripTags(e.text, ["fad"]) : A.setStartTags(e.text, [tag]) }); });
      });
      done(ids.size, rm ? "Fades removed" : "Fade added");
    } }]);
  }
  function openCase() {
    openSheet("Change case", '<div class="sb-field"><span class="sb-label">Case</span>' + seg("cMode", [["sentence", "Sentence"], ["title", "Title"], ["upper", "UPPER"], ["lower", "lower"]], "sentence") + "</div>" +
      '<p class="sb-hint">Only the visible text changes — tags and \\N are left alone.</p>' + scopeField(),
    [{ label: "Cancel" }, { label: "Apply", primary: true, onClick: function () {
      var mode = segVal("cMode"), ids = scopeIds(segVal("scope"));
      change("Change case", function () { mapEvents(ids, function (e) { return A.withEvent(e, { text: A.changeCase(e.text, mode) }); }); });
      done(ids.size, "Case changed");
    } }]);
  }
  function openResample() {
    var cur = A.playRes(S.doc);
    var vw = hasVideo() ? video.videoWidth : 0, vh = hasVideo() ? video.videoHeight : 0;
    openSheet("Resample resolution", "<p>Current script resolution: <b>" + cur.x + "×" + cur.y + "</b></p>" +
      '<div class="sb-row">' + [[1920, 1080], [1280, 720], [854, 480], [640, 360]].concat(vw ? [[vw, vh]] : []).map(function (r, k) {
        return '<button class="sb-btn sb-sm" data-res="' + r[0] + "x" + r[1] + '">' + r[0] + "×" + r[1] + (vw && k === 4 ? " (video)" : "") + "</button>";
      }).join("") + "</div>" +
      '<div class="sb-grid2" style="margin-top:12px">' + field("Width", numInput("rsX", cur.x, 'min="16"')) + field("Height", numInput("rsY", cur.y, 'min="16"')) + "</div>" +
      '<p class="sb-hint">Scales every style, margin, \\pos, \\move, \\org, \\clip, drawing, font size and border. Always runs on the whole script.</p>',
    [{ label: "Cancel" }, { label: "Resample", primary: true, onClick: function () {
      var x = Math.round(Number(q("#rsX").value)), y = Math.round(Number(q("#rsY").value));
      if (!(x > 0 && y > 0)) { toast("Enter a resolution", "bad"); return false; }
      if (x === cur.x && y === cur.y) return;
      change("Resample to " + x + "×" + y, function () {
        var r = A.resample(S.doc, x, y);
        S.doc = { info: r.info, styles: r.styles, events: r.events, extras: S.doc.extras };
      });
      fitStage();
      toast("Resampled to " + x + "×" + y);
    } }]);
    q(".sb-row").addEventListener("click", function (ev) {
      var b = ev.target.closest("[data-res]");
      if (!b) return;
      var p = b.getAttribute("data-res").split("x");
      q("#rsX").value = p[0]; q("#rsY").value = p[1];
    });
  }
  function openQC() {
    var issues = [], ov = overlapSet(), names = styleNames(), used = {};
    S.doc.events.forEach(function (e, i) {
      used[e.style] = 1;
      if (e.comment) return;
      var d = info(e), vis = A.strippedText(e.text).trim();
      if (e.end <= e.start) issues.push([i, "Zero or negative duration"]);
      if (!vis && !A.isDrawing(e.text)) issues.push([i, "Empty line"]);
      if (d.cps > settings.cpsLimit) issues.push([i, d.cps.toFixed(1) + " CPS — too fast to read"]);
      if (d.len > settings.lenLimit) issues.push([i, d.len + " characters on one line"]);
      if (ov.has(e.id)) issues.push([i, "Overlaps another line"]);
      if (names.indexOf(e.style) === -1) issues.push([i, "Style “" + e.style + "” does not exist"]);
      if (/\{[^}]*$/.test(e.text) || /^[^{]*\}/.test(e.text)) issues.push([i, "Unbalanced { } braces"]);
      if (/  /.test(vis)) issues.push([i, "Double space"]);
      if (e.end - e.start > 0 && e.end - e.start < 500 && vis) issues.push([i, "Shorter than half a second"]);
    });
    var unused = names.filter(function (n) { return !used[n]; });
    openSheet("Quality check", (issues.length ? '<p class="sb-hint" style="margin-top:0">' + issues.length + " issue" + (issues.length === 1 ? "" : "s") + " — tap one to go to the line.</p>" +
      '<ul class="sb-qc">' + issues.slice(0, 1000).map(function (x) {
        return '<li data-i="' + x[0] + '"><b>' + (x[0] + 1) + "</b><span>" + esc(x[1]) + " · " + esc(A.strippedText(S.doc.events[x[0]].text).slice(0, 60)) + "</span></li>";
      }).join("") + "</ul>" : '<div class="sb-empty"><b>Nothing to report</b>No overlaps, fast or long lines, or missing styles.</div>') +
      (unused.length ? '<p class="sb-hint">Unused styles: ' + unused.map(esc).join(", ") + "</p>" : "") +
      '<p class="sb-hint">Limits: ' + settings.cpsLimit + " CPS and " + settings.lenLimit + " characters per line (the same 50-character rule as pnass). Change them in Settings.</p>",
    issues.length ? [{ label: "Select all flagged lines", onClick: function () {
      var set = new Set(issues.map(function (x) { return S.doc.events[x[0]].id; }));
      applySelection(function (e) { return set.has(e.id); }, "set");
    } }] : []);
    var list = q(".sb-qc");
    if (list) list.addEventListener("click", function (ev) {
      var li = ev.target.closest("[data-i]");
      if (!li) return;
      var e = S.doc.events[Number(li.getAttribute("data-i"))];
      closeSheet();
      if (e) { setActive(e.id); if (!isWide()) setView("edit"); }
    });
  }
  var MATRICES = ["None", "TV.601", "TV.709", "PC.601", "PC.709", "TV.FCC", "PC.FCC", "TV.240M", "PC.240M"];
  function openProps() {
    var g = function (k, d) { var v = A.getInfo(S.doc, k); return v === null ? d : v; };
    var res = A.playRes(S.doc);
    openSheet("Script properties", field("Title", '<input class="sb-input" id="prTitle" value="' + esc(g("Title", "")) + '">') +
      '<div class="sb-grid2">' + field("Original script", '<input class="sb-input" id="prOrig" value="' + esc(g("Original Script", "")) + '">') +
      field("Translation", '<input class="sb-input" id="prTl" value="' + esc(g("Original Translation", "")) + '">') + "</div>" +
      '<div class="sb-grid2">' + field("Resolution X", numInput("prX", res.x, 'min="16"')) + field("Resolution Y", numInput("prY", res.y, 'min="16"')) + "</div>" +
      (hasVideo() ? '<button class="sb-btn sb-sm" id="prFromVideo">Use the video\'s ' + video.videoWidth + "×" + video.videoHeight + "</button>" : "") +
      '<p class="sb-hint">Changing the numbers here does not move anything — use Tools → Resample to rescale the script.</p>' +
      field("Wrap style", '<select class="sb-select" id="prWrap">' + [["0", "0 — smart, top line wider"], ["1", "1 — end-of-line"], ["2", "2 — no wrapping"], ["3", "3 — smart, bottom line wider"]].map(function (o) {
        return '<option value="' + o[0] + '"' + (g("WrapStyle", "0") === o[0] ? " selected" : "") + ">" + o[1] + "</option>";
      }).join("") + "</select>") +
      '<div class="sb-grid2">' + field("Scale border & shadow", '<select class="sb-select" id="prSbs"><option value="yes">Yes</option><option value="no"' + (String(g("ScaledBorderAndShadow", "yes")).toLowerCase() === "no" ? " selected" : "") + ">No</option></select>") +
      field("YCbCr matrix", '<select class="sb-select" id="prMat">' + MATRICES.map(function (m) { return "<option" + (g("YCbCr Matrix", "None") === m ? " selected" : "") + ">" + m + "</option>"; }).join("") + "</select>") + "</div>" +
      field("File name", '<input class="sb-input" id="prName" value="' + esc(S.name) + '">'),
    [{ label: "Cancel" }, { label: "Save", primary: true, onClick: function () {
      var vals = [["Title", q("#prTitle").value], ["Original Script", q("#prOrig").value || null], ["Original Translation", q("#prTl").value || null],
        ["PlayResX", Math.round(Number(q("#prX").value)) || res.x], ["PlayResY", Math.round(Number(q("#prY").value)) || res.y],
        ["WrapStyle", q("#prWrap").value], ["ScaledBorderAndShadow", q("#prSbs").value], ["YCbCr Matrix", q("#prMat").value]];
      var name = q("#prName").value.trim() || S.name;
      change("Script properties", function () {
        var docLike = { info: S.doc.info };
        vals.forEach(function (kv) { docLike.info = A.setInfo(docLike, kv[0], kv[1]); });
        S.doc.info = docLike.info;
      });
      S.name = /\.ass$/i.test(name) ? name : name + ".ass";
      fitStage(); renderBar();
    } }]);
    var fv = q("#prFromVideo");
    if (fv) fv.addEventListener("click", function () { q("#prX").value = video.videoWidth; q("#prY").value = video.videoHeight; });
  }
  function openSettings() {
    var pref = window.PN && PN.getThemePref ? PN.getThemePref() : "system";
    openSheet("Settings", '<div class="sb-grid2">' + field("Frame rate", '<select class="sb-select" id="stFps">' + [[24000 / 1001, "23.976"], [24, "24"], [25, "25"], [30000 / 1001, "29.97"], [30, "30"], [60000 / 1001, "59.94"], [60, "60"]].map(function (f) {
        return '<option value="' + f[0] + '"' + (Math.abs(f[0] - settings.fps) < 0.001 ? " selected" : "") + ">" + f[1] + " fps</option>";
      }).join("") + "</select>", "Detected from the video when it plays.") +
      field("Default line length ms", numInput("stDef", settings.defaultDur, 'min="100" step="100"')) + "</div>" +
      '<div class="sb-grid2">' + field("CPS limit", numInput("stCps", settings.cpsLimit, 'min="1" step="0.5"')) + field("Characters per line", numInput("stLen", settings.lenLimit, 'min="10"')) + "</div>" +
      '<label class="sb-check"><input type="checkbox" id="stEnter"' + (settings.enterNext ? " checked" : "") + ">Enter goes to the next line (Shift+Enter types \\N)</label>" +
      '<label class="sb-check"><input type="checkbox" id="stSeek"' + (settings.seekOnSelect ? " checked" : "") + ">Selecting a line jumps the video to it</label>" +
      '<label class="sb-check"><input type="checkbox" id="stSnap"' + (settings.snap ? " checked" : "") + ">Snap waveform drags to nearby lines, the playhead and frames</label>" +
      '<div class="sb-field" style="margin-top:8px"><span class="sb-label">Theme</span>' + seg("stTheme", [["system", "System"], ["dark", "Dark"], ["light", "Light"]], pref) + "</div>",
    [{ label: "Cancel" }, { label: "Save", primary: true, onClick: function () {
      settings.fps = Number(q("#stFps").value) || settings.fps;
      settings.defaultDur = Math.max(100, Number(q("#stDef").value) || 2000);
      settings.cpsLimit = Math.max(1, Number(q("#stCps").value) || 17);
      settings.lenLimit = Math.max(10, Number(q("#stLen").value) || 50);
      settings.enterNext = q("#stEnter").checked;
      settings.seekOnSelect = q("#stSeek").checked;
      settings.snap = q("#stSnap").checked;
      saveSettings();
      if (window.PN && PN.setTheme) PN.setTheme(segVal("stTheme"));
      derived = new WeakMap();
      refresh();
    } }]);
  }
  function openHelp() {
    var rows = [
      ["Tap a line", "Select it; tap it again to edit (phone)"], ["Long-press a line", "Start selecting several"],
      ["Enter in the text", "Next line (Shift+Enter: \\N)"], ["Waveform: drag a green/red bar", "Move the start/end"],
      ["Waveform: drag elsewhere", "Scroll; tap to seek; pinch or Ctrl+wheel to zoom"], ["\\pos button, then tap video", "Place the line"],
      ["Ctrl+Z / Ctrl+Y", "Undo / redo"], ["Ctrl+S", "Download .ass"], ["Ctrl+O", "Open a file"],
      ["Ctrl+↑ / Ctrl+↓", "Previous / next line"], ["Space (outside text)", "Play / pause"], ["← / → (outside text)", "Step a frame"],
      ["Ctrl+P", "Play the line"], ["Ctrl+3 / Ctrl+4", "Start / end at the playhead"], ["Ctrl+D", "Duplicate"],
      ["Delete (outside text)", "Delete the selected lines"], ["Ctrl+A (outside text)", "Select every visible line"], ["Ctrl+F", "Filter lines"]
    ];
    openSheet("Shortcuts & gestures", '<ul class="sb-qc">' + rows.map(function (r) { return "<li><b style=\"min-width:40%;font-family:var(--pn-body)\">" + esc(r[0]) + "</b><span style=\"white-space:normal\">" + esc(r[1]) + "</span></li>"; }).join("") + "</ul>" +
      '<p class="sb-hint">Everything stays on this device: scripts autosave in the browser, and nothing is uploaded. Download or share the .ass when you are done.</p>');
  }

  // ---- views --------------------------------------------------------------------------------------
  var VIEWS = [["lines", "list", "Lines"], ["edit", "edit", "Edit"], ["video", "video", "Video"], ["styles", "styles", "Styles"], ["tools", "tools", "Tools"]];
  function setView(v) {
    if (isWide() && (v === "styles" || v === "tools")) { setSide(v); return; }
    S.view = v;
    app.setAttribute("data-view", v);
    Array.prototype.forEach.call($("tabs").children, function (t) { t.setAttribute("aria-selected", t.getAttribute("data-view") === v ? "true" : "false"); });
    requestAnimationFrame(function () {
      fitStage();
      if (v === "lines") { measureRow(); renderList(); if (S.active !== null) ensureRowVisible(S.active); }
      if (v === "styles") renderStyles();
    });
  }
  function setSide(v) {
    S.side = v;
    app.setAttribute("data-side", v);
    Array.prototype.forEach.call(document.querySelectorAll(".sb-sidetabs button"), function (b) {
      if (b.getAttribute("data-side") === v) b.setAttribute("aria-pressed", "true"); else b.removeAttribute("aria-pressed");
    });
    if (v === "styles") renderStyles();
  }

  // ---- keyboard -------------------------------------------------------------------------------------
  function inField(t) { return t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable); }
  document.addEventListener("keydown", function (ev) {
    if (sheet.open) return;
    var mod = ev.ctrlKey || ev.metaKey, k = ev.key.toLowerCase(), field = inField(ev.target);
    if (mod && k === "z" && !ev.shiftKey) { if (field && ev.target.id !== "edText") return; ev.preventDefault(); undo(); return; }
    if (mod && (k === "y" || (k === "z" && ev.shiftKey))) { if (field && ev.target.id !== "edText") return; ev.preventDefault(); redo(); return; }
    if (mod && k === "s") { ev.preventDefault(); download("ass"); return; }
    if (mod && k === "o") { ev.preventDefault(); $("subFile").click(); return; }
    if (mod && k === "f") { ev.preventDefault(); setView("lines"); $("filter").focus(); return; }
    if (mod && k === "p") { ev.preventDefault(); playLine(); return; }
    if (mod && k === "d") { ev.preventDefault(); duplicateLines(); return; }
    if (mod && k === "3") { ev.preventDefault(); nudge("start="); return; }
    if (mod && k === "4") { ev.preventDefault(); nudge("end="); return; }
    if (mod && ev.key === "ArrowDown") { ev.preventDefault(); nextLine(false); return; }
    if (mod && ev.key === "ArrowUp") { ev.preventDefault(); prevLine(); return; }
    if (field) { if (ev.key === "Escape") ev.target.blur(); return; }
    if (ev.key === " ") { ev.preventDefault(); togglePlay(); }
    else if (ev.key === "ArrowLeft") { ev.preventDefault(); stepFrame(ev.shiftKey ? -10 : -1); }
    else if (ev.key === "ArrowRight") { ev.preventDefault(); stepFrame(ev.shiftKey ? 10 : 1); }
    else if (ev.key === "ArrowDown") { ev.preventDefault(); nextLine(false); }
    else if (ev.key === "ArrowUp") { ev.preventDefault(); prevLine(); }
    else if (ev.key === "Delete") { ev.preventDefault(); deleteLines(); }
    else if (mod && k === "a") { ev.preventDefault(); S.sel = new Set(rows.map(function (i) { return S.doc.events[i].id; })); refresh(); }
    else if (ev.key === "Enter") { ev.preventDefault(); setView("edit"); $("edText").focus(); }
    else if (ev.key === "Escape" && S.selMode) { S.selMode = false; refresh(); }
  });

  // ---- init -----------------------------------------------------------------------------------------
  function initChrome() {
    $("homeBtn").innerHTML = ic("home");
    $("menuBtn").innerHTML = ic("menu");
    $("undoBtn").innerHTML = ic("undo");
    $("redoBtn").innerHTML = ic("redo");
    $("saveBtn").innerHTML = ic("save");
    $("playBtn").innerHTML = ic("play");
    $("frameBack").innerHTML = ic("stepb");
    $("frameFwd").innerHTML = ic("stepf");
    $("mediaBtn").innerHTML = ic("film");
    $("vCollapse").innerHTML = ic("up");
    $("searchIcon").outerHTML = ic("search");
    $("sortBtn").innerHTML = ic("sort");
    $("selModeBtn").innerHTML = ic("select");
    $("addFab").innerHTML = ic("plus");
    $("prevLine").innerHTML = ic("left");
    $("nextLine").innerHTML = ic("right");
    $("commentBtn").innerHTML = ic("comment");
    $("sheetClose").innerHTML = ic("x");
    $("tabs").innerHTML = VIEWS.map(function (v) {
      return '<button class="sb-tab" role="tab" data-view="' + v[0] + '" aria-selected="' + (v[0] === "lines") + '">' + ic(v[1]) + "<span>" + v[2] + "</span></button>";
    }).join("");
    $("tabs").addEventListener("click", function (ev) { var t = ev.target.closest("[data-view]"); if (t) setView(t.getAttribute("data-view")); });
    Array.prototype.forEach.call(document.querySelectorAll(".sb-sidetabs button"), function (b) {
      b.addEventListener("click", function () { setSide(b.getAttribute("data-side")); });
    });
    $("menuBtn").addEventListener("click", openMenu);
    $("undoBtn").addEventListener("click", undo);
    $("redoBtn").addEventListener("click", redo);
    $("saveBtn").addEventListener("click", function () { if (navigator.canShare && !isWide() && matchMedia("(pointer: coarse)").matches) openSaveChoice(); else download("ass"); });
    $("prevLine").addEventListener("click", prevLine);
    $("nextLine").addEventListener("click", function () { nextLine(true); });
    $("commentBtn").addEventListener("click", function () { var id = S.active; S.sel = S.sel.has(id) ? S.sel : new Set([id]); toggleComment(); });
    $("sheetClose").addEventListener("click", closeSheet);
    $("subFile").addEventListener("change", function () { var f = this.files && this.files[0]; this.value = ""; if (f) openFile(f); });
    // Drag and drop anywhere: subtitles open as the script, media as the video.
    var depth = 0;
    document.addEventListener("dragenter", function (ev) { if (ev.dataTransfer && Array.prototype.indexOf.call(ev.dataTransfer.types, "Files") !== -1) { depth++; $("drop").hidden = false; } });
    document.addEventListener("dragleave", function () { depth = Math.max(0, depth - 1); if (!depth) $("drop").hidden = true; });
    document.addEventListener("dragover", function (ev) { ev.preventDefault(); });
    document.addEventListener("drop", function (ev) {
      ev.preventDefault(); depth = 0; $("drop").hidden = true;
      Array.prototype.forEach.call(ev.dataTransfer.files || [], openFile);
    });
    // The on-screen keyboard: hide the tab bar so the text box keeps its room.
    var maxH = window.innerHeight;
    var kb = function () {
      var vv = window.visualViewport, h = vv ? vv.height : window.innerHeight;
      maxH = Math.max(maxH, window.innerHeight, h);
      var up = !isWide() && inField(document.activeElement) && h < maxH * 0.78;
      app.setAttribute("data-kb", up ? "1" : "0");
    };
    if (window.visualViewport) window.visualViewport.addEventListener("resize", kb);
    document.addEventListener("focusin", function () { setTimeout(kb, 250); });
    document.addEventListener("focusout", function () { setTimeout(kb, 250); });
    window.addEventListener("resize", function () { measureRow(); renderList(); fitStage(); if (isWide() && (S.view === "styles" || S.view === "tools")) setView("lines"); });
    window.addEventListener("pagehide", flushAutosave);
    document.addEventListener("visibilitychange", function () { if (document.visibilityState === "hidden") flushAutosave(); });
    if (window.PN && PN.onTheme) PN.onTheme(function () { drawWave(); });
  }
  function openSaveChoice() {
    openSheet("Save", '<div class="sb-menu">' + menuItem("share", "Share .ass…", "share") + menuItem("save", "Download .ass", "save") + menuItem("save", "Export .srt", "srt") + "</div>");
    q(".sb-menu").addEventListener("click", function (ev) {
      var b = ev.target.closest("button[data-m]");
      if (!b) return;
      closeSheet();
      var m = b.getAttribute("data-m");
      setTimeout(function () { menuAction(m); }, 30);
    });
  }

  function start() {
    initChrome(); buildEditor(); initList(); initSelBar(); initMedia(); initWave(); initStyles(); renderTools();
    measureRow();
    paintWaveNote();
    var fresh = function () {
      S.active = S.doc.events[0].id; S.sel = new Set([S.active]);
      refresh();
    };
    var last = null;
    try { last = localStorage.getItem(LAST_KEY); } catch (e) {}
    if (last) {
      DB.get(last).then(function (p) {
        if (p) { loadProject(p); toast("Restored " + p.name); } else fresh();
      }).catch(fresh);
    } else fresh();
    if (!isWide()) setView("lines"); else setSide("tools");
  }
  start();
})();
