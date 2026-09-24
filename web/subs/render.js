/* Pandora Subs — preview renderer, served at /subs/render.js.
   An approximation of libass on a 2D canvas, good enough to check timing, placement and
   typesetting on a phone: styles, \N and smart wrapping, alignment and margins, \pos/\move/\org,
   \fad/\fade, \t transforms, colours and alphas, borders, shadows, blur, scale, spacing,
   rotation, shear, rectangle and vector clips, drawings and \k karaoke. Fonts the device
   does not have fall back to the system sans-serif, so metrics are close rather than exact —
   the encode is still rendered by libass on the server. */
(function (global) {
  "use strict";
  var A = global.ASS;

  var metricCache = {};
  // ASS sizes a font by its full cell height (ascent + descent), CSS by its em. Measuring the
  // ratio per family keeps a 48 in the script looking like a 48 in the encode.
  function fontRatio(ctx, family) {
    if (metricCache[family]) return metricCache[family];
    ctx.save();
    ctx.font = '100px "' + family + '", sans-serif';
    var m = ctx.measureText("Hg");
    ctx.restore();
    var asc = m.fontBoundingBoxAscent, desc = m.fontBoundingBoxDescent;
    var r = asc && desc ? { cell: (asc + desc) / 100, asc: asc / 100 } : { cell: 1.15, asc: 0.905 };
    metricCache[family] = r;
    return r;
  }

  function styleState(st) {
    var c = function (s) { return A.parseColor(s); };
    var p = c(st.PrimaryColour), s2 = c(st.SecondaryColour), o = c(st.OutlineColour), b = c(st.BackColour);
    return {
      fn: st.Fontname, fs: Number(st.Fontsize) || 20, b: st.Bold ? 700 : 400, i: !!st.Italic, u: !!st.Underline, s: !!st.StrikeOut,
      c1: p, c2: s2, c3: o, c4: b, a1: p.a, a2: s2.a, a3: o.a, a4: b.a,
      xbord: Number(st.Outline) || 0, ybord: Number(st.Outline) || 0, xshad: Number(st.Shadow) || 0, yshad: Number(st.Shadow) || 0,
      blur: 0, be: 0, fscx: Number(st.ScaleX) || 100, fscy: Number(st.ScaleY) || 100, fsp: Number(st.Spacing) || 0,
      frz: Number(st.Angle) || 0, frx: 0, fry: 0, fax: 0, fay: 0, an: Number(st.Alignment) || 2, bstyle: Number(st.BorderStyle) || 1,
      p: 0, k: null
    };
  }
  function copyState(s) { var o = {}; for (var k in s) o[k] = s[k]; return o; }

  function num(v, d) { var n = parseFloat(v); return isFinite(n) ? n : d; }
  function lerp(a, b, t) { return a + (b - a) * t; }
  function lerpColor(a, b, t) { return { r: lerp(a.r, b.r, t), g: lerp(a.g, b.g, t), b: lerp(a.b, b.b, t), a: 0 }; }

  // Applies one tag to the running state. `base` is the style state (for bare resets), `ev` the
  // event-level record for tags that act on the whole line, and `tt` the time in the line for \t.
  function applyTag(t, st, base, ev, tt, styles) {
    var a = t.args, args = A.argsOf(t);
    switch (t.name) {
      case "b": st.b = a === "" ? base.b : (a === "1" ? 700 : a === "0" ? 400 : num(a, 400)); break;
      case "i": st.i = a === "" ? base.i : a !== "0"; break;
      case "u": st.u = a === "" ? base.u : a !== "0"; break;
      case "s": st.s = a === "" ? base.s : a !== "0"; break;
      case "fn": st.fn = a === "" ? base.fn : a; break;
      case "fs":
        if (a === "") st.fs = base.fs;
        else if (/^[+-]/.test(a)) st.fs = st.fs * (1 + num(a, 0) / 10);
        else st.fs = num(a, base.fs);
        break;
      case "fscx": st.fscx = a === "" ? base.fscx : num(a, 100); break;
      case "fscy": st.fscy = a === "" ? base.fscy : num(a, 100); break;
      case "fsp": st.fsp = a === "" ? base.fsp : num(a, 0); break;
      case "bord": st.xbord = st.ybord = a === "" ? base.xbord : Math.max(0, num(a, 0)); break;
      case "xbord": st.xbord = a === "" ? base.xbord : Math.max(0, num(a, 0)); break;
      case "ybord": st.ybord = a === "" ? base.ybord : Math.max(0, num(a, 0)); break;
      case "shad": st.xshad = st.yshad = a === "" ? base.xshad : num(a, 0); break;
      case "xshad": st.xshad = a === "" ? base.xshad : num(a, 0); break;
      case "yshad": st.yshad = a === "" ? base.yshad : num(a, 0); break;
      case "blur": st.blur = a === "" ? 0 : Math.max(0, num(a, 0)); break;
      case "be": st.be = a === "" ? 0 : Math.max(0, num(a, 0)); break;
      case "fr": case "frz": st.frz = a === "" ? base.frz : num(a, 0); break;
      case "frx": st.frx = a === "" ? 0 : num(a, 0); break;
      case "fry": st.fry = a === "" ? 0 : num(a, 0); break;
      case "fax": st.fax = a === "" ? 0 : num(a, 0); break;
      case "fay": st.fay = a === "" ? 0 : num(a, 0); break;
      case "c": case "1c": st.c1 = a === "" ? base.c1 : withA(A.parseColor(a), 0); break;
      case "2c": st.c2 = a === "" ? base.c2 : withA(A.parseColor(a), 0); break;
      case "3c": st.c3 = a === "" ? base.c3 : withA(A.parseColor(a), 0); break;
      case "4c": st.c4 = a === "" ? base.c4 : withA(A.parseColor(a), 0); break;
      case "alpha":
        var al = a === "" ? null : A.parseAlpha(a);
        st.a1 = al === null ? base.a1 : al; st.a2 = al === null ? base.a2 : al;
        st.a3 = al === null ? base.a3 : al; st.a4 = al === null ? base.a4 : al;
        break;
      case "1a": st.a1 = a === "" ? base.a1 : A.parseAlpha(a); break;
      case "2a": st.a2 = a === "" ? base.a2 : A.parseAlpha(a); break;
      case "3a": st.a3 = a === "" ? base.a3 : A.parseAlpha(a); break;
      case "4a": st.a4 = a === "" ? base.a4 : A.parseAlpha(a); break;
      case "an": if (!ev.anSet && num(a, 0) >= 1 && num(a, 0) <= 9) { ev.an = num(a, 2); ev.anSet = true; } break;
      case "a":
        if (!ev.anSet && a !== "") {
          var la = num(a, 2), h = ((la - 1) & 3) + 1;
          ev.an = la & 4 ? h + 6 : la & 8 ? h + 3 : h; ev.anSet = true;
        }
        break;
      case "q": ev.q = a === "" ? ev.q : num(a, 0); break;
      case "pos": if (!ev.pos && !ev.move && args.length >= 2) ev.pos = { x: num(args[0], 0), y: num(args[1], 0) }; break;
      case "move":
        if (!ev.pos && !ev.move && args.length >= 4) {
          ev.move = { x1: num(args[0], 0), y1: num(args[1], 0), x2: num(args[2], 0), y2: num(args[3], 0),
            t1: args.length >= 6 ? num(args[4], 0) : null, t2: args.length >= 6 ? num(args[5], 0) : null };
        }
        break;
      case "org": if (!ev.org && args.length >= 2) ev.org = { x: num(args[0], 0), y: num(args[1], 0) }; break;
      case "fad": if (!ev.fade && args.length >= 2) ev.fade = { simple: true, in: num(args[0], 0), out: num(args[1], 0) }; break;
      case "fade":
        if (!ev.fade && args.length >= 7) ev.fade = { a1: num(args[0], 0), a2: num(args[1], 0), a3: num(args[2], 0),
          t1: num(args[3], 0), t2: num(args[4], 0), t3: num(args[5], 0), t4: num(args[6], 0) };
        break;
      case "clip": case "iclip":
        if (args.length === 4) ev.clip = { inverse: t.name === "iclip", rect: args.map(function (x) { return num(x, 0); }) };
        else if (args.length) ev.clip = { inverse: t.name === "iclip", scale: args.length === 2 ? num(args[0], 1) : 1, path: args[args.length - 1] };
        break;
      case "p": st.p = Math.max(0, num(a, 0)); break;
      case "pbo": st.pbo = num(a, 0); break;
      case "r":
        var rs = a && styles[a] ? styleState(styles[a]) : base;
        var keep = { p: st.p, k: st.k };
        for (var key in rs) st[key] = rs[key];
        st.p = keep.p; st.k = keep.k;
        break;
      case "k": case "K": case "kf": case "ko":
        st.k = { dur: num(a, 0) * 10, kind: t.name };
        break;
      case "t": applyTransform(t, st, base, ev, tt, styles); break;
    }
  }
  function withA(c, a) { c.a = a; return c; }

  // \t([t1,t2,][accel,]\tags): blends the state from its current value towards the tags' values.
  function applyTransform(t, st, base, ev, tt, styles) {
    var m = /^([^\\]*)(\\.*)$/.exec(t.args);
    if (!m) return;
    var nums = m[1].split(",").map(function (s) { return s.trim(); }).filter(function (s) { return s !== ""; }).map(Number);
    var t1 = 0, t2 = ev.duration, accel = 1;
    if (nums.length === 1) accel = nums[0];
    else if (nums.length >= 2) { t1 = nums[0]; t2 = nums[1]; if (nums.length >= 3) accel = nums[2]; }
    var prog = tt <= t1 ? 0 : tt >= t2 ? 1 : Math.pow((tt - t1) / Math.max(1, t2 - t1), accel || 1);
    if (t2 <= t1) prog = tt >= t1 ? 1 : 0;
    var target = copyState(st), dummy = { anSet: true, pos: true, move: true, org: true, fade: true, clip: ev.clip };
    A.parseTags(m[2]).forEach(function (x) { if (!x.junk && x.name !== "t") applyTag(x, target, base, dummy, tt, styles); });
    ["fs", "fscx", "fscy", "fsp", "xbord", "ybord", "xshad", "yshad", "blur", "be", "frz", "frx", "fry", "fax", "fay", "a1", "a2", "a3", "a4"].forEach(function (k) {
      if (target[k] !== st[k]) st[k] = lerp(st[k], target[k], prog);
    });
    ["c1", "c2", "c3", "c4"].forEach(function (k) {
      if (target[k] !== st[k]) st[k] = lerpColor(st[k], target[k], prog);
    });
    if (dummy.clip && dummy.clip !== ev.clip && ev.clip && ev.clip.rect && dummy.clip.rect) {
      ev.clip = { inverse: ev.clip.inverse, rect: ev.clip.rect.map(function (v, i) { return lerp(v, dummy.clip.rect[i], prog); }) };
    }
  }

  // Parses a line into runs of text (or drawing) sharing one state, plus the line-level record.
  function layoutEvent(evt, styles, stylesByName, tt, doc) {
    var style = stylesByName[evt.style] || stylesByName.Default || doc.styles[0];
    var base = styleState(style);
    var st = copyState(base);
    var ev = { an: base.an, anSet: false, q: Number(A.getInfo(doc, "WrapStyle")) || 0, duration: evt.end - evt.start, bstyle: base.bstyle };
    var runs = [], kTime = 0;
    A.splitBlocks(evt.text).forEach(function (seg) {
      if (seg.type === "comment") return;
      if (seg.type === "tags") {
        A.parseTags(seg.text).forEach(function (t) {
          if (t.junk) return;
          applyTag(t, st, base, ev, tt, stylesByName);
        });
        return;
      }
      var k = null;
      if (st.k) { k = { start: kTime, end: kTime + st.k.dur, kind: st.k.kind }; kTime += st.k.dur; st.k = null; }
      runs.push({ text: seg.text, st: copyState(st), k: k });
    });
    return { runs: runs, ev: ev, style: style };
  }

  // Splits runs into lines at \N (and \n under \q2), then wraps words to the available width.
  function breakLines(ctx, runs, ev, maxW) {
    var lines = [[]];
    runs.forEach(function (r) {
      if (r.st.p > 0) { lines[lines.length - 1].push({ draw: r.text, st: r.st, k: r.k }); return; }
      var parts = r.text.split(ev.q === 2 ? /\\N|\\n/ : /\\N/);
      parts.forEach(function (p, i) {
        if (i > 0) lines.push([]);
        var t = p.replace(/\\h/g, " ");
        if (ev.q !== 2) t = t.replace(/\\n/g, " ");
        if (t) lines[lines.length - 1].push({ text: t, st: r.st, k: r.k });
      });
    });
    if (ev.q === 2) return lines;
    var out = [];
    lines.forEach(function (line) {
      var words = [];
      line.forEach(function (piece) {
        if (piece.draw !== undefined) { words.push(piece); return; }
        piece.text.split(/( +)/).forEach(function (w) { if (w) words.push({ text: w, st: piece.st, k: piece.k }); });
      });
      var cur = [], w = 0;
      words.forEach(function (word) {
        var ww = measure(ctx, word);
        var isSpace = word.text !== undefined && /^ +$/.test(word.text);
        if (cur.length && !isSpace && w + ww > maxW && ev.q !== 2) {
          while (cur.length && cur[cur.length - 1].text !== undefined && /^ +$/.test(cur[cur.length - 1].text)) cur.pop();
          out.push(cur); cur = []; w = 0;
        }
        if (!cur.length && isSpace) return;
        cur.push(word); w += ww;
      });
      out.push(cur);
    });
    return out;
  }

  function fontFor(ctx, st) {
    var ratio = fontRatio(ctx, st.fn);
    var px = st.fs / ratio.cell;
    return { css: (st.i ? "italic " : "") + (st.b >= 600 ? "bold " : st.b !== 400 ? st.b + " " : "") + px.toFixed(2) + 'px "' + st.fn + '", sans-serif', ratio: ratio };
  }
  function measure(ctx, piece) {
    if (piece.w !== undefined) return piece.w;
    var st = piece.st;
    if (piece.draw !== undefined) {
      var bb = drawingBox(piece.draw, st);
      piece.w = bb.w; piece.h = bb.h;
      return piece.w;
    }
    ctx.font = fontFor(ctx, st).css;
    var w = ctx.measureText(piece.text).width + st.fsp * Array.from(piece.text).length;
    piece.w = w * st.fscx / 100;
    return piece.w;
  }

  // Drawing commands (m, n, l, b, s, p, c) turned into a list of path operations.
  function parseDrawing(cmds, scaleExp) {
    var f = 1 / Math.pow(2, Math.max(0, scaleExp - 1));
    var toks = String(cmds).match(/[mnlbspc]|-?\d*\.?\d+(?:e-?\d+)?/gi) || [];
    var ops = [], cmd = null, nums = [];
    var flush = function () {
      if (!cmd) return;
      var need = cmd === "b" ? 6 : cmd === "m" || cmd === "n" || cmd === "l" || cmd === "s" || cmd === "p" ? 2 : 0;
      if (cmd === "c") { ops.push({ op: "z" }); cmd = null; return; }
      if (!need) return;
      for (var i = 0; i + need <= nums.length; i += need) {
        var pts = nums.slice(i, i + need).map(function (v) { return v * f; });
        if (cmd === "b") ops.push({ op: "b", p: pts });
        else ops.push({ op: cmd === "m" || cmd === "n" ? "m" : "l", p: pts });
      }
    };
    toks.forEach(function (t) {
      if (/^[a-z]$/i.test(t)) { flush(); cmd = t.toLowerCase(); nums = []; }
      else nums.push(Number(t));
    });
    flush();
    return ops;
  }
  function drawingBox(cmds, st) {
    var ops = parseDrawing(cmds, st.p), maxX = 0, maxY = 0, minX = 0, minY = 0;
    ops.forEach(function (o) {
      if (!o.p) return;
      for (var i = 0; i < o.p.length; i += 2) {
        maxX = Math.max(maxX, o.p[i]); maxY = Math.max(maxY, o.p[i + 1]);
        minX = Math.min(minX, o.p[i]); minY = Math.min(minY, o.p[i + 1]);
      }
    });
    return { w: (maxX - minX) * st.fscx / 100, h: (maxY - minY) * st.fscy / 100, minX: minX, minY: minY, ops: ops };
  }
  function tracePath(ctx, ops) {
    ctx.beginPath();
    ops.forEach(function (o) {
      if (o.op === "m") ctx.moveTo(o.p[0], o.p[1]);
      else if (o.op === "l") ctx.lineTo(o.p[0], o.p[1]);
      else if (o.op === "b") ctx.bezierCurveTo(o.p[0], o.p[1], o.p[2], o.p[3], o.p[4], o.p[5]);
      else if (o.op === "z") ctx.closePath();
    });
  }

  function fadeAlpha(ev, tt) {
    var f = ev.fade;
    if (!f) return 1;
    if (f.simple) {
      var a = 1;
      if (f.in > 0 && tt < f.in) a = Math.min(a, tt / f.in);
      if (f.out > 0 && tt > ev.duration - f.out) a = Math.min(a, (ev.duration - tt) / f.out);
      return Math.max(0, Math.min(1, a));
    }
    var al;
    if (tt < f.t1) al = f.a1;
    else if (tt < f.t2) al = lerp(f.a1, f.a2, (tt - f.t1) / Math.max(1, f.t2 - f.t1));
    else if (tt < f.t3) al = f.a2;
    else if (tt < f.t4) al = lerp(f.a2, f.a3, (tt - f.t3) / Math.max(1, f.t4 - f.t3));
    else al = f.a3;
    return 1 - Math.max(0, Math.min(255, al)) / 255;
  }
  function rgba(c, a, mul) {
    return "rgba(" + Math.round(c.r) + "," + Math.round(c.g) + "," + Math.round(c.b) + "," + ((255 - a) / 255 * mul).toFixed(3) + ")";
  }

  // Renders every line visible at `timeMs` onto `canvas`. `opts.highlight` names an event id to
  // outline (the line being edited), `opts.videoHeight` scales unscaled borders.
  function render(canvas, doc, timeMs, opts) {
    opts = opts || {};
    var ctx = canvas.getContext("2d");
    var W = canvas.width, H = canvas.height;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, W, H);
    if (!doc) return [];
    var res = A.playRes(doc);
    var sx = W / res.x, sy = H / res.y;
    var scaledBorders = String(A.getInfo(doc, "ScaledBorderAndShadow") || "").toLowerCase() !== "no";
    var borderScale = scaledBorders ? 1 : res.y / (opts.videoHeight || H / (window.devicePixelRatio || 1));
    var stylesByName = {};
    doc.styles.forEach(function (s) { stylesByName[s.Name] = s; });
    var active = [];
    doc.events.forEach(function (e, idx) {
      if (!e.comment && e.start <= timeMs && e.end > timeMs) active.push({ e: e, idx: idx });
    });
    active.sort(function (a, b) { return a.e.layer - b.e.layer || a.idx - b.idx; });
    var placed = [], boxes = [];
    active.forEach(function (item) {
      var e = item.e, tt = timeMs - e.start;
      var lay = layoutEvent(e, doc.styles, stylesByName, tt, doc);
      var ev = lay.ev, style = lay.style;
      var mL = e.marginL || style.MarginL, mR = e.marginR || style.MarginR, mV = e.marginV || style.MarginV;
      var positioned = !!(ev.pos || ev.move);
      var maxW = positioned ? Infinity : Math.max(10, res.x - mL - mR);
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      var lines = breakLines(ctx, lay.runs, ev, maxW);
      var metrics = lines.map(function (line) {
        var w = 0, h = 0, asc = 0;
        line.forEach(function (p) {
          w += measure(ctx, p);
          if (p.draw !== undefined) { h = Math.max(h, p.h); asc = Math.max(asc, p.h); }
          else {
            var r = fontRatio(ctx, p.st.fn);
            var fh = p.st.fs * p.st.fscy / 100;
            h = Math.max(h, fh); asc = Math.max(asc, fh * r.asc / r.cell);
          }
        });
        if (!line.length) { var bs = styleState(style); h = bs.fs * bs.fscy / 100; asc = h * 0.8; }
        return { w: w, h: h, asc: asc };
      });
      // A trailing empty line from a closing \N still takes up height only between lines.
      var blockW = metrics.reduce(function (m, x) { return Math.max(m, x.w); }, 0);
      var blockH = metrics.reduce(function (m, x) { return m + x.h; }, 0);
      var an = ev.an, hAl = (an - 1) % 3, vAl = an <= 3 ? "b" : an <= 6 ? "m" : "t";
      var ax, ay;
      if (ev.move) {
        var t1 = ev.move.t1 === null ? 0 : ev.move.t1, t2 = ev.move.t2 === null ? ev.duration : ev.move.t2;
        var k = tt <= t1 ? 0 : tt >= t2 ? 1 : (tt - t1) / Math.max(1, t2 - t1);
        ax = lerp(ev.move.x1, ev.move.x2, k); ay = lerp(ev.move.y1, ev.move.y2, k);
      } else if (ev.pos) { ax = ev.pos.x; ay = ev.pos.y; }
      else {
        ax = hAl === 0 ? mL : hAl === 1 ? (mL + res.x - mR) / 2 : res.x - mR;
        ay = vAl === "b" ? res.y - mV : vAl === "m" ? res.y / 2 : mV;
      }
      var top = vAl === "b" ? ay - blockH : vAl === "m" ? ay - blockH / 2 : ay;
      // Unpositioned lines that would overlap stack away from their edge, like libass collisions.
      if (!positioned) {
        var left = hAl === 0 ? ax : hAl === 1 ? ax - blockW / 2 : ax - blockW;
        var moved = true, guard = 0;
        while (moved && guard++ < 50) {
          moved = false;
          for (var i = 0; i < placed.length; i++) {
            var p = placed[i];
            if (p.v !== vAl) continue;
            var overlapX = left < p.x + p.w && left + blockW > p.x;
            var overlapY = top < p.y + p.h && top + blockH > p.y;
            if (overlapX && overlapY) {
              top = vAl === "t" ? p.y + p.h : p.y - blockH;
              moved = true;
            }
          }
        }
        placed.push({ x: left, y: top, w: blockW, h: blockH, v: vAl });
      }
      var first = lay.runs.length ? lay.runs[0].st : styleState(style);
      var fadeMul = fadeAlpha(ev, tt);
      var org = ev.org || { x: ax, y: ay };

      ctx.setTransform(sx, 0, 0, sy, 0, 0);
      ctx.save();
      if (ev.clip) {
        ctx.beginPath();
        if (ev.clip.rect) {
          var r = ev.clip.rect;
          if (ev.clip.inverse) { ctx.rect(0, 0, res.x, res.y); ctx.rect(r[0], r[3], r[2] - r[0], r[1] - r[3]); }
          else ctx.rect(r[0], r[1], r[2] - r[0], r[3] - r[1]);
          ctx.clip("evenodd");
        } else {
          var ops = parseDrawing(ev.clip.path, ev.clip.scale || 1);
          tracePath(ctx, ops);
          if (ev.clip.inverse) ctx.rect(0, 0, res.x, res.y);
          ctx.clip("evenodd");
        }
      }
      // Rotation and shear act on the whole line around its origin, as in libass.
      ctx.translate(org.x, org.y);
      if (first.frz) ctx.rotate(-first.frz * Math.PI / 180);
      var cx = Math.cos(first.fry * Math.PI / 180), cy = Math.cos(first.frx * Math.PI / 180);
      if (cx !== 1 || cy !== 1) ctx.scale(Math.abs(cx) < 0.02 ? 0.02 : cx, Math.abs(cy) < 0.02 ? 0.02 : cy);
      ctx.translate(-org.x, -org.y);
      if (first.fax || first.fay) {
        ctx.translate(ax, ay);
        ctx.transform(1, first.fay, first.fax, 1, 0, 0);
        ctx.translate(-ax, -ay);
      }

      // Where every piece goes, computed once and drawn in three passes.
      var drawList = [], y = top;
      lines.forEach(function (line, li) {
        var m = metrics[li];
        var x = hAl === 0 ? ax : hAl === 1 ? ax - m.w / 2 : ax - m.w;
        if (positioned) {
          var bl = hAl === 0 ? ax : hAl === 1 ? ax - blockW / 2 : ax - blockW;
          x = hAl === 0 ? bl : hAl === 1 ? bl + (blockW - m.w) / 2 : bl + blockW - m.w;
        }
        line.forEach(function (p) {
          drawList.push({ p: p, x: x, base: y + m.asc, top: y, lineH: m.h });
          x += measure(ctx, p);
        });
        y += m.h;
      });
      boxes.push({ id: e.id, x: Math.min.apply(null, drawList.map(function (d) { return d.x; }).concat([ax])),
        y: top, w: blockW, h: blockH });

      var passes = ["shadow", "border", "fill"];
      passes.forEach(function (pass) {
        drawList.forEach(function (d) {
          var st = d.p.st;
          var bx = st.xbord * borderScale, by = st.ybord * borderScale;
          var shx = st.xshad * borderScale, shy = st.yshad * borderScale;
          var blur = (st.blur + st.be * 0.6) * sy;
          var kColor = null;
          if (d.p.k) {
            var kPast = tt >= d.p.k.end, kIn = tt >= d.p.k.start;
            if (d.p.k.kind === "ko") { if (!kIn && pass === "border") return; }
            else if (d.p.k.kind === "kf" && kIn && !kPast) kColor = "sweep";
            else if (!kIn) kColor = "secondary";
          }
          ctx.save();
          if (blur > 0 && pass !== "fill" && "filter" in ctx) ctx.filter = "blur(" + blur.toFixed(1) + "px)";
          if (blur > 0 && pass === "fill" && bx === 0 && by === 0 && "filter" in ctx) ctx.filter = "blur(" + blur.toFixed(1) + "px)";
          var ox = pass === "shadow" ? shx : 0, oy = pass === "shadow" ? shy : 0;
          if (pass === "shadow" && !shx && !shy) { ctx.restore(); return; }
          if (ev.bstyle === 3) {
            if (pass === "fill") { /* the fill pass draws text below */ }
            else {
              ctx.fillStyle = pass === "shadow" ? rgba(st.c4, st.a4, fadeMul) : rgba(st.c3, st.a3, fadeMul);
              ctx.fillRect(d.x - bx + ox, d.top - by + oy, measure(ctx, d.p) + 2 * bx, d.lineH + 2 * by);
              ctx.restore();
              return;
            }
          }
          if (pass === "border" && bx <= 0 && by <= 0) { ctx.restore(); return; }
          var fillColor = pass === "shadow" ? rgba(st.c4, st.a4, fadeMul)
            : pass === "border" ? rgba(st.c3, st.a3, fadeMul)
            : kColor === "secondary" ? rgba(st.c2, st.a2, fadeMul) : rgba(st.c1, st.a1, fadeMul);
          if (d.p.draw !== undefined) {
            var bb = drawingBox(d.p.draw, st);
            ctx.translate(d.x + ox, d.base - bb.h + oy);
            ctx.scale(st.fscx / 100, st.fscy / 100);
            ctx.translate(-bb.minX, -bb.minY);
            tracePath(ctx, bb.ops);
            if (pass === "border") { ctx.lineWidth = 2 * Math.max(bx, by) * 100 / st.fscx; ctx.lineJoin = "round"; ctx.strokeStyle = fillColor; ctx.stroke(); }
            else { ctx.fillStyle = fillColor; ctx.fill(); }
            ctx.restore();
            return;
          }
          var f = fontFor(ctx, st);
          ctx.font = f.css;
          ctx.textBaseline = "alphabetic";
          ctx.translate(d.x + ox, d.base + oy);
          ctx.scale(st.fscx / 100, st.fscy / 100);
          var drawText = function (fn) {
            if (!st.fsp) { fn(d.p.text, 0); return; }
            var cx2 = 0;
            Array.from(d.p.text).forEach(function (ch) { fn(ch, cx2); cx2 += ctx.measureText(ch).width + st.fsp; });
          };
          if (pass === "border" || (pass === "shadow" && (bx > 0 || by > 0))) {
            ctx.lineJoin = "round"; ctx.miterLimit = 2;
            ctx.lineWidth = 2 * Math.max(bx, by) * 100 / Math.max(1, st.fscy);
            ctx.strokeStyle = fillColor;
            drawText(function (s, x) { ctx.strokeText(s, x, 0); });
          }
          if (pass !== "border") {
            if (kColor === "sweep") {
              var w = ctx.measureText(d.p.text).width, prog = (tt - d.p.k.start) / Math.max(1, d.p.k.end - d.p.k.start);
              var g = ctx.createLinearGradient(0, 0, w, 0);
              g.addColorStop(0, rgba(st.c1, st.a1, fadeMul)); g.addColorStop(prog, rgba(st.c1, st.a1, fadeMul));
              g.addColorStop(Math.min(1, prog + 0.001), rgba(st.c2, st.a2, fadeMul)); g.addColorStop(1, rgba(st.c2, st.a2, fadeMul));
              ctx.fillStyle = g;
            } else ctx.fillStyle = fillColor;
            drawText(function (s, x) { ctx.fillText(s, x, 0); });
            if (pass === "fill" && (st.u || st.s)) {
              var tw = ctx.measureText(d.p.text).width + st.fsp * Array.from(d.p.text).length, th = Math.max(1, st.fs / 18);
              if (st.u) ctx.fillRect(0, th * 1.5, tw, th);
              if (st.s) ctx.fillRect(0, -st.fs * 0.3, tw, th);
            }
          }
          ctx.restore();
        });
      });
      ctx.restore();
      if (opts.highlight === e.id && drawList.length) {
        ctx.save();
        ctx.setTransform(sx, 0, 0, sy, 0, 0);
        ctx.strokeStyle = "rgba(60,129,235,0.9)";
        ctx.setLineDash([6 / sx, 4 / sx]);
        ctx.lineWidth = 1.5 / sx;
        var b = boxes[boxes.length - 1];
        ctx.strokeRect(b.x - 4, b.y - 4, b.w + 8, b.h + 8);
        ctx.restore();
      }
    });
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    return boxes;
  }

  global.SubRender = { render: render, fontRatio: fontRatio, clearMetrics: function () { metricCache = {}; } };
})(window);
