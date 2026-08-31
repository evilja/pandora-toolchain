/* Pandora console shell — served at /console.js, shared by every console page.
   Owns the rail, the topbar, the theme, the bearer token, the API wrapper, and the phase
   meter. A page supplies its own .pn-content markup and calls PN.shell() once. */
(function (global) {
  "use strict";

  var TOKEN_KEY = "pandora_token";
  var THEME_KEY = "pandora_theme";

  // ---- icons (1.6px stroke, 24-box; inherit currentColor) -------------------
  var ICONS = {
    operations: '<path d="M3 12l9-8 9 8"/><path d="M5 10v10h14V10"/><path d="M10 20v-6h4v6"/>',
    jobs: '<rect x="3" y="7" width="18" height="13" rx="2"/><path d="M8 7V5a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><path d="M3 12h18"/>',
    encode: '<rect x="2" y="5" width="20" height="14" rx="2"/><path d="M2 9h20"/><path d="M6 5l2 4M11 5l2 4M16 5l2 4"/>',
    studio: '<rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>',
    repos: '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/><path d="M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>',
    trace: '<path d="M2 12h4l3 8 4-16 3 8h6"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
    chevron: '<path d="M6 9l6 6 6-6"/>',
    refresh: '<path d="M21 12a9 9 0 1 1-2.6-6.4"/><path d="M21 3v6h-6"/>',
    play: '<circle cx="12" cy="12" r="9"/><path d="M10 8.5l6 3.5-6 3.5z"/>',
    clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>',
    users: '<circle cx="9" cy="8" r="3.2"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0"/><path d="M17 5.3a3.2 3.2 0 0 1 0 5.4M18 14.6a6.5 6.5 0 0 1 3.5 5.4"/>',
    shield: '<path d="M12 3l7.5 3v5.5c0 4.4-3 8.3-7.5 9.5-4.5-1.2-7.5-5.1-7.5-9.5V6z"/><path d="M9 12l2 2 4-4"/>',
    check: '<circle cx="12" cy="12" r="9"/><path d="M8.5 12.5l2.5 2.5 4.5-5"/>',
    alert: '<path d="M12 3.5L21.5 20h-19z"/><path d="M12 10v4M12 17.2v.1"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 7.8v.1"/>',
    x: '<path d="M6 6l12 12M18 6L6 18"/>',
    download: '<path d="M12 3v12"/><path d="M7.5 10.5L12 15l4.5-4.5"/><path d="M4 20h16"/>',
    upload: '<path d="M12 21V9"/><path d="M7.5 13.5L12 9l4.5 4.5"/><path d="M4 4h16"/>',
    external: '<path d="M14 4h6v6"/><path d="M20 4l-8.5 8.5"/><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="M11 12l8-8 2 2-2 2 2 2-2 2-2-2-2 2z"/>',
    eye: '<path d="M2 12s3.6-6.5 10-6.5S22 12 22 12s-3.6 6.5-10 6.5S2 12 2 12z"/><circle cx="12" cy="12" r="2.8"/>',
    film: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M3 15h18M8 4v16M16 4v16"/>',
    hash: '<path d="M5 9h14M5 15h14M10 4L8 20M16 4l-2 16"/>',
    code: '<path d="M8.5 16.5L4 12l4.5-4.5"/><path d="M15.5 7.5L20 12l-4.5 4.5"/>',
    trash: '<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6 7l1 13h10l1-13"/>'
  };

  function icon(name, size) {
    var body = ICONS[name] || "";
    var s = size || 24;
    return '<svg viewBox="0 0 24 24" width="' + s + '" height="' + s + '" aria-hidden="true">' + body + "</svg>";
  }

  // ---- nav -----------------------------------------------------------------
  // Order follows the flow of a release: watch it, find one, make one, then the tools.
  var NAV = [
    { key: "operations", href: "/", label: "Operations", icon: "operations" },
    { key: "jobs", href: "/jobs", label: "Jobs", icon: "jobs" },
    { key: "encode", href: "/encode", label: "Encode", icon: "encode" },
    { key: "studio", href: "/studio", label: "Studio", icon: "studio" },
    { key: "repositories", href: "/git", label: "Repositories", icon: "repos" },
    { key: "trace", href: "/trace", label: "Trace Lab", icon: "trace" },
    { key: "settings", href: "/settings", label: "Settings", icon: "settings" }
  ];

  function esc(s) {
    return String(s === undefined || s === null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  // ---- theme ---------------------------------------------------------------
  var themeListeners = [];
  function storedTheme() {
    var t = null;
    try { t = localStorage.getItem(THEME_KEY); } catch (e) {}
    if (t === "light" || t === "dark" || t === "system") return t;
    return "system";
  }
  function resolveTheme(pref) {
    if (pref === "light" || pref === "dark") return pref;
    return (global.matchMedia && global.matchMedia("(prefers-color-scheme: dark)").matches) ? "dark" : "light";
  }
  function applyTheme(pref) {
    document.documentElement.setAttribute("data-theme", resolveTheme(pref));
    themeListeners.forEach(function (fn) { try { fn(pref); } catch (e) {} });
  }
  function setTheme(pref) {
    try { localStorage.setItem(THEME_KEY, pref); } catch (e) {}
    applyTheme(pref);
  }

  // ---- token ---------------------------------------------------------------
  var tokenListeners = [];
  function getToken() {
    try { return (localStorage.getItem(TOKEN_KEY) || "").trim(); } catch (e) { return ""; }
  }
  function setToken(v) {
    try { localStorage.setItem(TOKEN_KEY, v || ""); } catch (e) {}
    tokenListeners.forEach(function (fn) { try { fn(v || ""); } catch (e) {} });
  }
  function clearToken() {
    try { localStorage.removeItem(TOKEN_KEY); } catch (e) {}
    tokenListeners.forEach(function (fn) { try { fn(""); } catch (e) {} });
  }

  // ---- API -----------------------------------------------------------------
  function api(method, path, body) {
    var token = getToken();
    var opts = { method: method, headers: {}, cache: "no-store" };
    if (token) opts.headers["Authorization"] = "Bearer " + token;
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    if (method === "GET") path += (path.indexOf("?") === -1 ? "?" : "&") + "_=" + Date.now();
    return fetch(path, opts).then(function (resp) {
      var retryAfter = resp.headers.get("Retry-After");
      return resp.text().then(function (text) {
        var data;
        try { data = JSON.parse(text); } catch (e) { data = text; }
        return { status: resp.status, ok: resp.ok, data: data, retryAfter: retryAfter };
      });
    });
  }

  // 429: the API rate-limits write requests per token (default 30 / 60s). Retry-After is seconds.
  function rateNote(res) {
    if (!res || res.status !== 429) return "";
    var secs = parseInt(res.retryAfter, 10);
    return "Rate limit hit — too many requests on this token." +
      (secs > 0 ? " Try again in " + secs + "s." : "") +
      " Status polling is never throttled.";
  }

  function messageText(data, ok) {
    if (typeof data === "string" && data) return data;
    if (data && typeof data === "object") {
      if (data.error) return data.error;
      if (data.message) return data.message;
      if (data.detail) return data.detail;
    }
    return ok ? "Request completed." : "Request failed.";
  }

  // ---- pipeline ------------------------------------------------------------
  // Progress is one blue bar wherever it appears in a row, and a vertical stepper on a job's
  // detail. Green, amber and red are reserved for status, never for progress.
  var PHASES = ["Queue", "Download", "Encode", "Upload"];
  var PROBE_PHASES = ["Queue", "Probe"];
  var PHASE_ICONS = { Queue: "clock", Download: "download", Encode: "play", Upload: "upload", Probe: "search" };
  var TERMINAL = ["Uploaded", "Failed", "Declined", "Cancelled"];
  var PIPELINE_STAGES = ["Queued", "Downloading", "Downloaded", "Encoding", "Encoded", "Uploading", "Uploaded", "Probing", "Probed"];
  var PHASE_ACTIVE = { Queued: 0, Downloading: 1, Encoding: 2, Uploading: 3, Probing: 1 };
  var PHASE_DONE = { Queued: -1, Downloading: 0, Downloaded: 1, Encoding: 1, Encoded: 2, Uploading: 2, Uploaded: 3, Probing: 0, Probed: 1 };

  function pct(v) { return Math.max(0, Math.min(100, Math.round(Number(v) || 0))); }

  // How far through the whole pipeline a job is: finished phases plus the live one.
  function overallPercent(stage, progress) {
    if (stage === "Uploaded" || stage === "Probed") return 100;
    var probe = stage === "Probing" || stage === "Probed";
    var total = probe ? PROBE_PHASES.length : PHASES.length;
    var done = PHASE_DONE.hasOwnProperty(stage) ? PHASE_DONE[stage] + 1 : 0;
    var slot = PHASE_ACTIVE.hasOwnProperty(stage) ? PHASE_ACTIVE[stage] : -1;
    var live = 0;
    if (progress && typeof progress === "object" && slot >= 0) {
      var t = progress.type;
      if ((slot === 1 && (t === "download" || t === "probe")) ||
          (slot === 2 && t === "encode") ||
          (slot === 3 && (t === "upload" || t === "upload_all"))) live = pct(progress.percent) / 100;
      else if (slot === 0) live = 0;
    }
    return Math.min(100, Math.round(((done + live) / total) * 100));
  }

  function barTone(stage) {
    if (stage === "Failed" || stage === "Declined") return "bad";
    if (stage === "Cancelled") return "idle";
    if (stage === "Uploaded" || stage === "Probed") return "ok";
    return "";
  }

  // A row's progress: the stage name, a blue bar, and its percentage.
  function bar(stage, progress, withPercent) {
    var p = overallPercent(stage, progress);
    var tone = barTone(stage);
    return '<div class="pn-progcell">' +
      (withPercent ? '<span class="pn-progpct">' + p + "%</span>" : "") +
      '<span class="pn-bar"' + (tone ? ' data-tone="' + tone + '"' : "") +
      '><i style="width:' + p + '%"></i></span></div>';
  }

  // The job-detail pipeline: one row per phase with its own state.
  function stepper(stage, progress) {
    var probe = stage === "Probing" || stage === "Probed";
    var names = probe ? PROBE_PHASES : PHASES;
    var failed = stage === "Failed" || stage === "Declined";
    var cancelled = stage === "Cancelled";
    var finished = stage === "Uploaded" || stage === "Probed";
    var doneUpTo = PHASE_DONE.hasOwnProperty(stage) ? PHASE_DONE[stage] : -1;
    var slot = PHASE_ACTIVE.hasOwnProperty(stage) ? PHASE_ACTIVE[stage] : -1;

    var items = names.map(function (name, i) {
      var state = "todo", label = "Pending";
      if (finished) { state = "done"; label = "Completed"; }
      else if (failed && i === 0) { state = "bad"; label = "Failed"; }
      else if (cancelled && i === 0) { state = "bad"; label = "Cancelled"; }
      else if (failed || cancelled) { state = "todo"; label = "Not reached"; }
      else if (i <= doneUpTo) { state = "done"; label = "Completed"; }
      else if (i === slot) {
        state = "active";
        label = progress && progress.waiting ? "Waiting" : "In progress";
      }
      return '<li class="pn-step" data-state="' + state + '">' +
        '<span class="pn-stepicon">' + icon(state === "done" ? "check" : PHASE_ICONS[name] || "clock", 24) + "</span>" +
        '<span class="pn-stepbody"><span class="pn-stepname">' + esc(name) + "</span>" +
        '<span class="pn-stepstate"><span class="pn-dot"></span>' + esc(label) + "</span></span></li>";
    }).join("");
    return '<ul class="pn-steps">' + items + "</ul>";
  }

  // The route a submitted job will take, written the way the reference writes it.
  function routeText(names) {
    return (names || PHASES.slice(1)).join(" \u2192 ");
  }

  function toneOf(stage) {
    if (stage === "Uploaded") return "done";
    if (stage === "Failed" || stage === "Declined") return "bad";
    if (stage === "Cancelled") return "idle";
    if (stage === "Queued") return "queued";
    return "active";
  }
  function statusLabel(stage) {
    if (stage === "Uploaded") return "Completed";
    if (stage === "Queued") return "Queued";
    if (TERMINAL.indexOf(stage) !== -1) return stage;
    return "Active";
  }
  // The dot chip carries status; the stage name is shown in its own column beside it.
  function chip(stage, useStageName) {
    return '<span class="pn-chip" data-tone="' + toneOf(stage) + '">' +
      esc(useStageName ? stage : statusLabel(stage)) + "</span>";
  }

  function isTerminalStage(stage) { return TERMINAL.indexOf(stage) !== -1; }

  // ---- connection state ----------------------------------------------------
  var connEl = null;
  var identity = { kind: "none", label: "No token" };

  function paintConn(state, text) {
    if (!connEl) return;
    connEl.setAttribute("data-state", state);
    connEl.querySelector(".pn-conntext").textContent = text;
  }

  function paintIdentity() {
    var who = document.getElementById("pn-who");
    if (!who) return;
    var initials = identity.kind === "none" ? "—" : identity.label.slice(0, 2).toUpperCase();
    who.querySelector(".pn-avatar").textContent = initials;
    who.querySelector("b").textContent = identity.label;
  }

  // Probing what the token can reach is the only way to name it — the API has no
  // "who am I" route. Both probes are GETs, so neither counts against the write limit.
  function refreshIdentity() {
    if (!getToken()) {
      identity = { kind: "none", label: "No token" };
      paintIdentity();
      return fetch("/health", { cache: "no-store" })
        .then(function (r) { paintConn(r.ok ? "auth" : "down", r.ok ? "Needs a token" : "API unreachable"); return identity; })
        .catch(function () { paintConn("down", "API unreachable"); return identity; });
    }
    return api("GET", "/api/v1/jobs?status=ongoing").then(function (res) {
      if (res.status === 401) {
        identity = { kind: "invalid", label: "Rejected" };
        paintConn("auth", "Token rejected");
        paintIdentity();
        return identity;
      }
      if (!res.ok) {
        paintConn("down", "API error");
        return identity;
      }
      paintConn("ok", "API connected");
      return api("GET", "/api/v1/workers").then(function (w) {
        if (w.ok) { identity = { kind: "pnwitch", label: "PNwitch" }; paintIdentity(); return identity; }
        return api("GET", "/api/v1/git/attachments").then(function (g) {
          identity = g.ok ? { kind: "local", label: "Local" } : { kind: "plain", label: "Token" };
          paintIdentity();
          return identity;
        });
      });
    }).catch(function () {
      paintConn("down", "API unreachable");
      return identity;
    });
  }

  // ---- toast ---------------------------------------------------------------
  var toastEl = null, toastTimer = null;
  function toast(msg, tone) {
    if (!toastEl) {
      toastEl = document.createElement("div");
      toastEl.className = "pn-toast";
      toastEl.setAttribute("role", "status");
      document.body.appendChild(toastEl);
    }
    toastEl.textContent = msg;
    toastEl.setAttribute("data-tone", tone || "");
    toastEl.classList.add("pn-show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { toastEl.classList.remove("pn-show"); }, 3600);
  }

  // ---- shell ---------------------------------------------------------------
  function shell(opts) {
    opts = opts || {};
    var app = document.querySelector(".pn-app");
    var main = document.querySelector(".pn-main");
    if (!app || !main) return;

    var rail = document.createElement("aside");
    rail.className = "pn-rail";
    rail.innerHTML =
      '<a class="pn-wordmark" href="/"><span>PANDORA</span></a>' +
      '<nav class="pn-nav">' +
      NAV.map(function (n) {
        return '<a class="pn-navlink" href="' + n.href + '"' +
          (n.key === opts.page ? ' aria-current="page"' : "") + ">" +
          icon(n.icon) + "<span>" + esc(n.label) + "</span>" +
          '<span class="pn-navcount" data-count="' + n.key + '"></span></a>';
      }).join("") +
      "</nav>" +
      '<div class="pn-railfoot"><span class="pn-dot"></span><span id="pn-health">Checking systems…</span></div>';
    app.insertBefore(rail, app.firstChild);

    var top = document.createElement("header");
    top.className = "pn-top";
    top.innerHTML =
      '<h1 class="pn-title">' + esc(opts.title || "Pandora") + "</h1>" +
      (opts.actions || "") +
      '<div class="pn-topright">' +
      '<button class="pn-conn" id="pn-conn" type="button" data-state="auth" title="Recheck the API connection">' +
      '<span class="pn-dot"></span><span class="pn-conntext">Checking…</span></button>' +
      '<div class="pn-topsep"></div>' +
      '<a class="pn-who" id="pn-who" href="/settings" title="Connection settings">' +
      '<span class="pn-avatar">—</span><b>No token</b>' + icon("chevron", 14) + "</a>" +
      "</div>";
    main.insertBefore(top, main.firstChild);

    connEl = document.getElementById("pn-conn");
    connEl.addEventListener("click", function () { paintConn("auth", "Checking…"); refreshIdentity(); pingHealth(); });

    applyTheme(storedTheme());
    refreshIdentity();
    pingHealth();

    global.addEventListener("storage", function (e) {
      if (e.key === THEME_KEY) applyTheme(storedTheme());
      if (e.key === TOKEN_KEY) {
        tokenListeners.forEach(function (fn) { try { fn(getToken()); } catch (err) {} });
        refreshIdentity();
      }
    });
    if (global.matchMedia) {
      var mq = global.matchMedia("(prefers-color-scheme: dark)");
      var onSystem = function () { if (storedTheme() === "system") applyTheme("system"); };
      if (mq.addEventListener) mq.addEventListener("change", onSystem);
      else if (mq.addListener) mq.addListener(onSystem);
    }
  }

  function pingHealth() {
    var el = document.getElementById("pn-health");
    if (!el) return;
    fetch("/health", { cache: "no-store" }).then(function (r) {
      el.textContent = r.ok ? "All systems operational" : "API responded " + r.status;
      el.parentNode.setAttribute("data-state", r.ok ? "ok" : "down");
    }).catch(function () {
      el.textContent = "Cannot reach the API";
      el.parentNode.setAttribute("data-state", "down");
    });
  }

  // Live counts beside the rail labels — set by whichever page knows them.
  function navCount(key, value) {
    var el = document.querySelector('.pn-navcount[data-count="' + key + '"]');
    if (el) el.textContent = value === null || value === undefined || value === "" ? "" : String(value);
  }

  function formatDuration(secs) {
    secs = Math.max(0, Math.floor(Number(secs) || 0));
    var h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
    return String(h).padStart(2, "0") + ":" + String(m).padStart(2, "0") + ":" + String(s).padStart(2, "0");
  }
  function formatEta(secs) {
    var mins = Math.ceil(Math.max(0, Number(secs) || 0) / 60);
    if (mins < 60) return mins + "m";
    return Math.floor(mins / 60) + "h " + String(mins % 60).padStart(2, "0") + "m";
  }

  global.PN = {
    shell: shell,
    icon: icon,
    esc: esc,
    api: api,
    bar: bar,
    stepper: stepper,
    routeText: routeText,
    chip: chip,
    toneOf: toneOf,
    statusLabel: statusLabel,
    overallPercent: overallPercent,
    toast: toast,
    navCount: navCount,
    rateNote: rateNote,
    messageText: messageText,
    refreshIdentity: refreshIdentity,
    identity: function () { return identity; },
    pct: pct,
    formatDuration: formatDuration,
    formatEta: formatEta,
    isTerminalStage: isTerminalStage,
    PHASES: PHASES,
    PIPELINE_STAGES: PIPELINE_STAGES,
    TERMINAL: TERMINAL,
    getToken: getToken,
    setToken: setToken,
    clearToken: clearToken,
    onToken: function (fn) { tokenListeners.push(fn); },
    getThemePref: storedTheme,
    setTheme: setTheme,
    onTheme: function (fn) { themeListeners.push(fn); }
  };

  // Paint the stored theme before first paint, so no page flashes the wrong ground.
  applyTheme(storedTheme());
})(window);
