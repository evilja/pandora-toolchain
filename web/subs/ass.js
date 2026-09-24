/* Pandora Subs — the subtitle model, served at /subs/ass.js.
   Everything here is pure: parsing, serialising, time and colour maths, override-tag surgery and
   the automations (HYDRA-style tagging, sorting, shifting, line breaking, resampling). It touches
   no DOM, so `node web/subs/ass.test.js` exercises it without a browser.

   Event and style objects are treated as immutable: every edit returns a new object, and the
   editor's undo history keeps shallow copies of the arrays. Replacing an object instead of
   mutating it is what lets a hundred undo steps share one copy of an untouched line. */
(function (global) {
  "use strict";

  var STYLE_FIELDS = ["Name", "Fontname", "Fontsize", "PrimaryColour", "SecondaryColour", "OutlineColour",
    "BackColour", "Bold", "Italic", "Underline", "StrikeOut", "ScaleX", "ScaleY", "Spacing", "Angle",
    "BorderStyle", "Outline", "Shadow", "Alignment", "MarginL", "MarginR", "MarginV", "Encoding"];
  var EVENT_FIELDS = ["Layer", "Start", "End", "Style", "Name", "MarginL", "MarginR", "MarginV", "Effect", "Text"];
  var NUMERIC_STYLE = { Fontsize: 1, ScaleX: 1, ScaleY: 1, Spacing: 1, Angle: 1, BorderStyle: 1, Outline: 1,
    Shadow: 1, Alignment: 1, MarginL: 1, MarginR: 1, MarginV: 1, Encoding: 1 };
  var BOOL_STYLE = { Bold: 1, Italic: 1, Underline: 1, StrikeOut: 1 };

  var nextId = 1;
  function uid() { return nextId++; }

  // ---- numbers -------------------------------------------------------------
  // ASS files are hand-edited as often as generated, so numbers are written the short way:
  // 20 not 20.000, 0.5 not .50000.
  function fmtNum(n, places) {
    n = Number(n);
    if (!isFinite(n)) return "0";
    var p = places === undefined ? 3 : places;
    var s = n.toFixed(p);
    if (s.indexOf(".") !== -1) s = s.replace(/0+$/, "").replace(/\.$/, "");
    if (s === "-0") s = "0";
    return s;
  }
  function clamp(v, lo, hi) { return v < lo ? lo : v > hi ? hi : v; }

  // ---- time ----------------------------------------------------------------
  // Times are integer milliseconds in memory; ASS stores centiseconds, so the file is rounded
  // only when it is written.
  function parseTime(s) {
    var m = /^\s*(\d+):(\d{1,2}):(\d{1,2})(?:[.,](\d+))?\s*$/.exec(String(s));
    if (!m) return 0;
    var frac = m[4] ? Math.round(Number("0." + m[4]) * 1000) : 0;
    return ((Number(m[1]) * 60 + Number(m[2])) * 60 + Number(m[3])) * 1000 + frac;
  }
  function formatTime(ms) {
    var cs = Math.max(0, Math.round((Number(ms) || 0) / 10));
    var h = Math.floor(cs / 360000), m = Math.floor(cs / 6000) % 60, s = Math.floor(cs / 100) % 60, c = cs % 100;
    return h + ":" + pad2(m) + ":" + pad2(s) + "." + pad2(c);
  }
  function formatSrtTime(ms) {
    ms = Math.max(0, Math.round(Number(ms) || 0));
    var h = Math.floor(ms / 3600000), m = Math.floor(ms / 60000) % 60, s = Math.floor(ms / 1000) % 60;
    return pad2(h) + ":" + pad2(m) + ":" + pad2(s) + "," + String(ms % 1000).padStart(3, "0");
  }
  function pad2(n) { return n < 10 ? "0" + n : String(n); }
  // Loose input for a time field: "1:02.5", "62.5", "0:01:02.50" all mean the same thing.
  function parseLooseTime(s) {
    s = String(s || "").trim().replace(",", ".");
    if (!s) return null;
    var neg = s.charAt(0) === "-";
    if (neg) s = s.slice(1);
    var parts = s.split(":");
    if (parts.length > 3 || parts.some(function (p) { return !/^\d*(\.\d*)?$/.test(p) || p === ""; })) return null;
    var secs = 0;
    parts.forEach(function (p) { secs = secs * 60 + Number(p); });
    var ms = Math.round(secs * 1000);
    return neg ? -ms : ms;
  }

  // Constant-frame-rate frame maths, the way Aegisub does it for CFR video: frame n is shown from
  // n/fps until (n+1)/fps.
  function frameAt(ms, fps) { return Math.floor(ms * fps / 1000 + 1e-6); }
  function frameStart(n, fps) { return Math.ceil(n * 1000 / fps - 1e-6); }
  function snapToFrame(ms, fps) { return frameStart(Math.round(ms * fps / 1000), fps); }

  // ---- colour --------------------------------------------------------------
  // ASS writes colours as &HAABBGGRR (styles) or &HBBGGRR& (tags), alpha 00 = opaque.
  // SSA v4 files also write plain decimal integers, which are only told apart by the missing &H.
  function parseColor(s) {
    var str = String(s || "").trim();
    var isHex = /^&?h/i.test(str);
    var hex = str.replace(/[&hH]/g, "").trim();
    var n = isHex ? parseInt(hex || "0", 16) : parseInt(hex, 10) || 0;
    if (!isHex) hex = n.toString(16);
    if (!isFinite(n)) n = 0;
    n = n >>> 0;
    return { r: n & 255, g: (n >>> 8) & 255, b: (n >>> 16) & 255, a: hex.length > 6 ? (n >>> 24) & 255 : 0 };
  }
  function hex2(n) { return clamp(Math.round(n), 0, 255).toString(16).toUpperCase().padStart(2, "0"); }
  function styleColor(c) { return "&H" + hex2(c.a || 0) + hex2(c.b) + hex2(c.g) + hex2(c.r); }
  function tagColor(c) { return "&H" + hex2(c.b) + hex2(c.g) + hex2(c.r) + "&"; }
  function tagAlpha(a) { return "&H" + hex2(a) + "&"; }
  function parseAlpha(s) {
    var hex = String(s || "").replace(/[&hH]/g, "").trim();
    var n = parseInt(hex || "0", 16);
    return isFinite(n) ? clamp(n & 255, 0, 255) : 0;
  }
  function colorToCss(c, alphaOverride) {
    var a = alphaOverride === undefined ? c.a : alphaOverride;
    return "rgba(" + c.r + "," + c.g + "," + c.b + "," + fmtNum((255 - a) / 255, 3) + ")";
  }
  function colorToHex(c) {
    return "#" + [c.r, c.g, c.b].map(function (v) { return v.toString(16).padStart(2, "0"); }).join("");
  }
  function hexToColor(hex, a) {
    var m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(String(hex || ""));
    if (!m) return { r: 255, g: 255, b: 255, a: a || 0 };
    return { r: parseInt(m[1], 16), g: parseInt(m[2], 16), b: parseInt(m[3], 16), a: a || 0 };
  }

  // ---- defaults --------------------------------------------------------------
  function defaultStyle(name) {
    return {
      Name: name || "Default", Fontname: "Arial", Fontsize: 48,
      PrimaryColour: "&H00FFFFFF", SecondaryColour: "&H000000FF", OutlineColour: "&H00000000", BackColour: "&H00000000",
      Bold: false, Italic: false, Underline: false, StrikeOut: false,
      ScaleX: 100, ScaleY: 100, Spacing: 0, Angle: 0, BorderStyle: 1, Outline: 2, Shadow: 2,
      Alignment: 2, MarginL: 10, MarginR: 10, MarginV: 10, Encoding: 1
    };
  }
  function makeEvent(p) {
    p = p || {};
    return {
      id: uid(), comment: !!p.comment, layer: p.layer | 0, start: p.start | 0, end: p.end === undefined ? 2000 : p.end | 0,
      style: p.style || "Default", actor: p.actor || "", marginL: p.marginL | 0, marginR: p.marginR | 0,
      marginV: p.marginV | 0, effect: p.effect || "", text: p.text || ""
    };
  }
  function withEvent(ev, patch) {
    var out = {};
    for (var k in ev) out[k] = ev[k];
    for (var j in patch) out[j] = patch[j];
    return out;
  }
  function cloneEvent(ev) { var c = withEvent(ev, {}); c.id = uid(); return c; }
  function newDoc() {
    return {
      info: [
        { k: "Title", v: "New subtitles" }, { k: "ScriptType", v: "v4.00+" }, { k: "WrapStyle", v: "0" },
        { k: "ScaledBorderAndShadow", v: "yes" }, { k: "YCbCr Matrix", v: "TV.709" },
        { k: "PlayResX", v: "1920" }, { k: "PlayResY", v: "1080" }
      ],
      styles: [defaultStyle()],
      events: [makeEvent({ start: 0, end: 5000, text: "" })],
      extras: []
    };
  }

  function getInfo(doc, key) {
    var lk = key.toLowerCase();
    for (var i = 0; i < doc.info.length; i++) if (doc.info[i].k && doc.info[i].k.toLowerCase() === lk) return doc.info[i].v;
    return null;
  }
  function setInfo(doc, key, value) {
    var lk = key.toLowerCase(), info = doc.info.slice(), found = false;
    for (var i = 0; i < info.length; i++) {
      if (info[i].k && info[i].k.toLowerCase() === lk) {
        if (value === null || value === undefined) { info.splice(i, 1); i--; } else info[i] = { k: info[i].k, v: String(value) };
        found = true;
      }
    }
    if (!found && value !== null && value !== undefined) info.push({ k: key, v: String(value) });
    return info;
  }
  // A script with no PlayRes is rendered by libass at 384x288, so that is what we assume too.
  function playRes(doc) {
    var x = Number(getInfo(doc, "PlayResX")) || 0, y = Number(getInfo(doc, "PlayResY")) || 0;
    if (!x && !y) return { x: 384, y: 288 };
    if (!x) x = y === 1024 ? 1280 : Math.round(y * 4 / 3);
    if (!y) y = x === 1280 ? 1024 : Math.round(x * 3 / 4);
    return { x: x, y: y };
  }

  // ---- parse ---------------------------------------------------------------
  function splitFields(line, count) {
    var out = [], idx = 0;
    for (var i = 0; i < count - 1; i++) {
      var c = line.indexOf(",", idx);
      if (c === -1) { out.push(line.slice(idx)); idx = line.length; continue; }
      out.push(line.slice(idx, c)); idx = c + 1;
    }
    out.push(line.slice(idx));
    return out;
  }

  // SSA v4 numbered alignments the old way (1-3 bottom, +4 top, +8 middle).
  function legacyAlignment(a) {
    a = Number(a) || 2;
    var h = ((a - 1) & 3) + 1;
    if (a & 4) return h + 6;
    if (a & 8) return h + 3;
    return h;
  }

  function normaliseFieldName(f) {
    var n = f.trim();
    var map = { tertiarycolour: "OutlineColour", primarycolour: "PrimaryColour", secondarycolour: "SecondaryColour",
      outlinecolour: "OutlineColour", backcolour: "BackColour", fontname: "Fontname", fontsize: "Fontsize",
      marked: "Marked", alphalevel: "AlphaLevel" };
    return map[n.toLowerCase()] || n;
  }

  function parseStyle(fields, values, legacy) {
    var st = defaultStyle();
    fields.forEach(function (f, i) {
      var v = (values[i] || "").trim();
      if (f === "Name") st.Name = v.replace(/^\*/, "") || "Default";
      else if (BOOL_STYLE[f]) st[f] = v !== "0" && v !== "";
      else if (NUMERIC_STYLE[f]) st[f] = Number(v) || 0;
      else if (/Colour$/.test(f)) st[f] = styleColor(parseColor(v));
      else if (f in st) st[f] = v;
    });
    if (legacy) st.Alignment = legacyAlignment(st.Alignment);
    return st;
  }

  function parseAss(text) {
    text = String(text || "").replace(/^﻿/, "");
    var lines = text.split(/\r\n|\r|\n/);
    var doc = { info: [], styles: [], events: [], extras: [] };
    var section = "", styleFormat = null, eventFormat = null, legacy = false, extra = null;
    var seenStyles = false, seenEvents = false;
    for (var li = 0; li < lines.length; li++) {
      var raw = lines[li], line = raw.trim();
      var head = /^\[(.+)\]$/.exec(line);
      if (head) {
        section = head[1].trim().toLowerCase();
        extra = null;
        if (section === "v4 styles" || section === "v4+ styles" || section === "v4++ styles") {
          legacy = section === "v4 styles";
          seenStyles = true;
        } else if (section === "events") {
          seenEvents = true;
        } else if (section !== "script info") {
          extra = { name: head[1].trim(), lines: [], pos: seenEvents ? "post" : seenStyles ? "mid" : "pre" };
          doc.extras.push(extra);
        }
        continue;
      }
      if (extra) { extra.lines.push(raw); continue; }
      if (!line) continue;
      if (section === "script info") {
        if (line.charAt(0) === ";" || line.indexOf("!:") === 0) { doc.info.push({ k: null, v: line }); continue; }
        var c = line.indexOf(":");
        if (c === -1) continue;
        doc.info.push({ k: line.slice(0, c).trim(), v: line.slice(c + 1).trim() });
      } else if (section === "v4 styles" || section === "v4+ styles" || section === "v4++ styles") {
        var sm = /^([^:]+):\s?(.*)$/.exec(line);
        if (!sm) continue;
        if (sm[1] === "Format") styleFormat = sm[2].split(",").map(normaliseFieldName);
        else if (sm[1] === "Style") {
          var fmt = styleFormat || (legacy ? ["Name", "Fontname", "Fontsize", "PrimaryColour", "SecondaryColour", "OutlineColour", "BackColour", "Bold", "Italic", "BorderStyle", "Outline", "Shadow", "Alignment", "MarginL", "MarginR", "MarginV", "AlphaLevel", "Encoding"] : STYLE_FIELDS);
          doc.styles.push(parseStyle(fmt, splitFields(sm[2], fmt.length), legacy));
        }
      } else if (section === "events") {
        var em = /^([^:]+):\s?(.*)$/.exec(raw.replace(/^\s+/, ""));
        if (!em) continue;
        var kind = em[1].trim();
        if (kind === "Format") { eventFormat = em[2].split(",").map(function (f) { return f.trim(); }); continue; }
        if (kind !== "Dialogue" && kind !== "Comment") continue;
        var ef = eventFormat || EVENT_FIELDS;
        var vals = splitFields(em[2], ef.length);
        var ev = makeEvent({ comment: kind === "Comment" });
        ef.forEach(function (f, i) {
          var v = vals[i] === undefined ? "" : vals[i];
          switch (f) {
            case "Layer": ev.layer = parseInt(v, 10) || 0; break;
            case "Marked": break;
            case "Start": ev.start = parseTime(v); break;
            case "End": ev.end = parseTime(v); break;
            case "Style": ev.style = v.trim().replace(/^\*/, "") || "Default"; break;
            case "Name": case "Actor": ev.actor = v.trim(); break;
            case "MarginL": ev.marginL = parseInt(v, 10) || 0; break;
            case "MarginR": ev.marginR = parseInt(v, 10) || 0; break;
            case "MarginV": ev.marginV = parseInt(v, 10) || 0; break;
            case "Effect": ev.effect = v.trim(); break;
            case "Text": ev.text = v; break;
          }
        });
        doc.events.push(ev);
      }
    }
    doc.extras.forEach(function (x) { while (x.lines.length && !x.lines[x.lines.length - 1].trim()) x.lines.pop(); });
    if (!doc.styles.length) doc.styles.push(defaultStyle());
    if (!getInfo(doc, "ScriptType")) doc.info.unshift({ k: "ScriptType", v: "v4.00+" });
    else doc.info = setInfo(doc, "ScriptType", "v4.00+");
    return doc;
  }

  // ---- SRT / WebVTT ------------------------------------------------------------
  function htmlToAss(s) {
    return s
      .replace(/\r/g, "")
      .replace(/<\s*i\s*>/gi, "{\\i1}").replace(/<\s*\/\s*i\s*>/gi, "{\\i0}")
      .replace(/<\s*b\s*>/gi, "{\\b1}").replace(/<\s*\/\s*b\s*>/gi, "{\\b0}")
      .replace(/<\s*u\s*>/gi, "{\\u1}").replace(/<\s*\/\s*u\s*>/gi, "{\\u0}")
      .replace(/<\s*s\s*>/gi, "{\\s1}").replace(/<\s*\/\s*s\s*>/gi, "{\\s0}")
      .replace(/<\s*font[^>]*color\s*=\s*["']?#?([0-9a-f]{6})["']?[^>]*>/gi, function (_, hex) {
        return "{\\c" + tagColor(hexToColor(hex)) + "}";
      })
      .replace(/<\s*\/\s*font\s*>/gi, "{\\c}")
      .replace(/<[^>]+>/g, "")
      .replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&nbsp;/g, "\\h")
      .replace(/\n/g, "\\N")
      .replace(/\}\{/g, "");
  }

  function parseSrt(text, vtt) {
    var doc = newDoc();
    doc.events = [];
    doc.info = setInfo(doc, "Title", vtt ? "Imported WebVTT" : "Imported SRT");
    var blocks = String(text || "").replace(/^﻿/, "").replace(/\r\n|\r/g, "\n").split(/\n{2,}/);
    var re = /(\d+:)?(\d{1,2}):(\d{1,2})[.,](\d{1,3})\s*-->\s*(\d+:)?(\d{1,2}):(\d{1,2})[.,](\d{1,3})(.*)$/;
    blocks.forEach(function (block) {
      var lines = block.split("\n");
      for (var i = 0; i < lines.length; i++) {
        var m = re.exec(lines[i]);
        if (!m) continue;
        var t = function (h, mm, ss, ms) {
          return ((parseInt(h || "0", 10) * 60 + parseInt(mm, 10)) * 60 + parseInt(ss, 10)) * 1000 + parseInt((ms + "00").slice(0, 3), 10);
        };
        var body = lines.slice(i + 1).join("\n").replace(/\n+$/, "");
        var ev = makeEvent({ start: t(m[1], m[2], m[3], m[4]), end: t(m[5], m[6], m[7], m[8]), text: htmlToAss(body) });
        // WebVTT cue settings: only a top line position translates cleanly.
        if (vtt && /line:\s*(0|[0-9]%|1[0-9]%)/.test(m[9] || "")) ev.text = "{\\an8}" + ev.text;
        doc.events.push(ev);
        break;
      }
    });
    if (!doc.events.length) doc.events.push(makeEvent({ start: 0, end: 5000 }));
    return doc;
  }

  // Picks the parser from the content rather than the extension: pasted text has no extension.
  function parseAny(text, filename) {
    var t = String(text || "");
    var name = String(filename || "").toLowerCase();
    if (/^﻿?\s*WEBVTT/.test(t) || /\.vtt$/.test(name)) return parseSrt(t, true);
    if (/\[script info\]|\[events\]|\[v4\+? styles\]/i.test(t)) return parseAss(t);
    if (/-->/.test(t)) return parseSrt(t, false);
    return parseAss(t);
  }

  // ---- serialise ------------------------------------------------------------
  function styleLine(st) {
    return "Style: " + STYLE_FIELDS.map(function (f) {
      var v = st[f];
      if (BOOL_STYLE[f]) return v ? "-1" : "0";
      if (NUMERIC_STYLE[f]) return fmtNum(v, 3);
      return String(v === undefined ? "" : v).replace(/,/g, ";");
    }).join(",");
  }
  function eventLine(ev) {
    return (ev.comment ? "Comment: " : "Dialogue: ") + [
      ev.layer | 0, formatTime(ev.start), formatTime(ev.end), String(ev.style).replace(/,/g, ";"),
      String(ev.actor || "").replace(/,/g, ";"), ev.marginL | 0, ev.marginR | 0, ev.marginV | 0,
      String(ev.effect || "").replace(/,/g, ";"), String(ev.text || "").replace(/[\r\n]+/g, "\\N")
    ].join(",");
  }
  function serializeAss(doc) {
    var out = ["[Script Info]"];
    if (!doc.info.some(function (i) { return i.k === null && /Pandora Subs/.test(i.v); })) {
      out.push("; Script generated by Pandora Subs");
    }
    doc.info.forEach(function (i) { out.push(i.k === null ? i.v : i.k + ": " + i.v); });
    out.push("");
    var extras = function (pos) {
      doc.extras.filter(function (x) { return x.pos === pos; }).forEach(function (x) {
        out.push("[" + x.name + "]");
        x.lines.forEach(function (l) { out.push(l); });
        out.push("");
      });
    };
    extras("pre");
    out.push("[V4+ Styles]");
    out.push("Format: " + STYLE_FIELDS.join(", "));
    doc.styles.forEach(function (s) { out.push(styleLine(s)); });
    out.push("");
    extras("mid");
    out.push("[Events]");
    out.push("Format: " + EVENT_FIELDS.join(", "));
    doc.events.forEach(function (e) { out.push(eventLine(e)); });
    out.push("");
    extras("post");
    return out.join("\r\n");
  }

  // SRT keeps italics, bold, underline and a colour; everything else in ASS has no SRT spelling.
  function assToHtml(text) {
    var out = "", open = {};
    splitBlocks(text).forEach(function (seg) {
      if (seg.type === "text") {
        out += seg.text.replace(/\\N|\\n/g, "\n").replace(/\\h/g, " ");
        return;
      }
      if (seg.type !== "tags") return;
      parseTags(seg.text).forEach(function (t) {
        if (t.name === "i" || t.name === "b" || t.name === "u" || t.name === "s") {
          var on = t.args !== "" && t.args !== "0";
          if (on && !open[t.name]) { out += "<" + t.name + ">"; open[t.name] = true; }
          else if (!on && open[t.name]) { out += "</" + t.name + ">"; open[t.name] = false; }
        }
      });
    });
    ["s", "u", "b", "i"].forEach(function (n) { if (open[n]) out += "</" + n + ">"; });
    return out;
  }
  function serializeSrt(doc) {
    var n = 0;
    return doc.events
      .filter(function (e) { return !e.comment && !isDrawing(e.text) && visibleText(e.text).trim(); })
      .slice().sort(function (a, b) { return a.start - b.start || a.end - b.end; })
      .map(function (e) {
        n++;
        var pre = /\{[^}]*\\an8[^}]*\}/.test(e.text) ? "{\\an8}" : "";
        return n + "\r\n" + formatSrtTime(e.start) + " --> " + formatSrtTime(e.end) + "\r\n" + pre + assToHtml(e.text).replace(/\n/g, "\r\n") + "\r\n";
      }).join("\r\n");
  }

  // ---- override blocks ---------------------------------------------------------
  // A line's text is a run of segments: override blocks `{\...}`, comment blocks `{...}` that hold
  // no tag (the typesetter's notes), and plain text. An unmatched `{` is plain text, as in libass.
  function splitBlocks(text) {
    var segs = [], i = 0, s = String(text || "");
    while (i < s.length) {
      var open = s.indexOf("{", i);
      if (open === -1) { segs.push({ type: "text", text: s.slice(i), start: i }); break; }
      var close = s.indexOf("}", open);
      if (close === -1) { segs.push({ type: "text", text: s.slice(i), start: i }); break; }
      if (open > i) segs.push({ type: "text", text: s.slice(i, open), start: i });
      var inner = s.slice(open + 1, close);
      segs.push({ type: inner.indexOf("\\") !== -1 ? "tags" : "comment", text: inner, start: open });
      i = close + 1;
    }
    return segs;
  }

  // Longest spellings first so \fscx is not read as \fs + "cx".
  var TAG_NAMES = ["xbord", "ybord", "xshad", "yshad", "alpha", "iclip", "fscx", "fscy", "clip", "blur", "bord",
    "shad", "fade", "move", "pbo", "fsp", "fax", "fay", "frx", "fry", "frz", "fad", "pos", "org", "fn", "fs", "fr",
    "fe", "an", "be", "kf", "ko", "1c", "2c", "3c", "4c", "1a", "2a", "3a", "4a", "c", "a", "b", "i", "u", "s", "q",
    "r", "t", "k", "K", "p"];

  // Reads one block's inner text into tags. Parenthesised arguments are kept raw (a \t carries a
  // nested tag list), and whatever is not a tag — stray text inside a block — is kept as `junk`
  // so rewriting a block never silently drops what someone typed there.
  function parseTags(inner) {
    var tags = [], i = 0, s = String(inner || "");
    while (i < s.length) {
      if (s.charAt(i) !== "\\") {
        var nb = s.indexOf("\\", i);
        var junk = nb === -1 ? s.slice(i) : s.slice(i, nb);
        tags.push({ name: "", args: junk, raw: junk, junk: true });
        i = nb === -1 ? s.length : nb;
        continue;
      }
      var j = i + 1, name = null;
      for (var k = 0; k < TAG_NAMES.length; k++) {
        if (s.substr(j, TAG_NAMES[k].length) === TAG_NAMES[k]) { name = TAG_NAMES[k]; break; }
      }
      if (name === null) {
        var um = /^[a-zA-Z0-9]*/.exec(s.slice(j));
        name = um[0];
      }
      j += name.length;
      var args = "", paren = false;
      if (s.charAt(j) === "(") {
        var depth = 0, st = j;
        for (; j < s.length; j++) {
          if (s.charAt(j) === "(") depth++;
          else if (s.charAt(j) === ")") { depth--; if (depth === 0) { j++; break; } }
        }
        args = s.slice(st + 1, depth === 0 ? j - 1 : j);
        paren = true;
      } else {
        var next = s.indexOf("\\", j);
        args = next === -1 ? s.slice(j) : s.slice(j, next);
        j = next === -1 ? s.length : next;
      }
      tags.push({ name: name, args: paren ? args : args.trim(), paren: paren, raw: s.slice(i, j) });
      i = j;
    }
    return tags;
  }
  function tagsToString(tags) { return tags.map(function (t) { return t.raw; }).join(""); }
  function makeTag(name, value) {
    var v = value === undefined || value === null ? "" : String(value);
    var paren = /^(pos|move|org|fad|fade|clip|iclip|t)$/.test(name);
    var raw = "\\" + name + (paren ? "(" + v + ")" : v);
    return { name: name, args: v, paren: paren, raw: raw };
  }
  // Tags that override one another: setting \frz must drop a \fr, setting \an drops a \a.
  var FAMILY = { c: "1c", "1c": "1c", fr: "frz", frz: "frz", a: "an", an: "an", fad: "fad", fade: "fad",
    pos: "pos", move: "pos", clip: "clip", iclip: "clip" };
  function family(name) { return FAMILY[name] || name; }
  // For these the first occurrence in a line wins in libass and VSFilter; for everything else the last.
  var FIRST_WINS = { pos: 1, an: 1, org: 1, fad: 1 };

  function argsOf(tag) {
    return tag.paren ? tag.args.split(",").map(function (a) { return a.trim(); }) : [tag.args];
  }

  function isDrawing(text) {
    var d = false;
    splitBlocks(text).forEach(function (seg) {
      if (seg.type !== "tags") return;
      parseTags(seg.text).forEach(function (t) { if (t.name === "p") d = (Number(t.args) || 0) > 0; });
    });
    return d;
  }

  // What a viewer reads: blocks removed, \N as a newline and \h as a space.
  function visibleText(text) {
    return splitBlocks(text).filter(function (s) { return s.type === "text"; })
      .map(function (s) { return s.text; }).join("")
      .replace(/\\N/g, "\n").replace(/\\n/g, "\n").replace(/\\h/g, " ");
  }
  function strippedText(text) { return visibleText(text).replace(/\n/g, " "); }

  // Characters per second the way Aegisub counts them: letters and digits only, over the
  // line's duration. Returns 0 for a line too short to have a rate.
  function cps(ev) {
    if (isDrawing(ev.text)) return 0;
    var dur = (ev.end - ev.start) / 1000;
    if (dur <= 0) return 0;
    var chars = (visibleText(ev.text).match(/[\p{L}\p{N}]/gu) || []).length;
    return chars / dur;
  }
  // The longest visible line of an event, matching pnass's check (drawings are skipped).
  function maxLineLength(ev) {
    if (isDrawing(ev.text)) return 0;
    return visibleText(ev.text).split("\n").reduce(function (m, l) { return Math.max(m, Array.from(l).length); }, 0);
  }

  // ---- tag surgery ------------------------------------------------------------
  // Puts tags into the line's leading override block, replacing any of the same family (outside
  // \t), or opens a leading block when there is none.
  function setStartTags(text, tagList) {
    var segs = splitBlocks(text);
    var first = segs.length && segs[0].type === "tags" && segs[0].start === 0 ? segs[0] : null;
    var tags = first ? parseTags(first.text) : [];
    tagList.forEach(function (nt) {
      if (nt.name !== "t") {
        var fam = family(nt.name);
        tags = tags.filter(function (t) { return t.junk || family(t.name) !== fam; });
      }
      tags.push(nt);
    });
    var block = "{" + tagsToString(tags) + "}";
    return first ? block + text.slice(first.text.length + 2) : block + text;
  }

  // Removes every tag of the given families from every block (and inside \t, dropping a \t that
  // ends up empty). With no names, removes every override tag but keeps comment blocks.
  function stripTags(text, names) {
    var fams = names ? names.map(family) : null;
    var out = "";
    splitBlocks(text).forEach(function (seg) {
      if (seg.type === "text") { out += seg.text; return; }
      if (seg.type === "comment") { out += "{" + seg.text + "}"; return; }
      if (!fams) {
        var junk = parseTags(seg.text).filter(function (t) { return t.junk; });
        if (junk.length) out += "{" + tagsToString(junk) + "}";
        return;
      }
      var kept = parseTags(seg.text).map(function (t) {
        if (t.name === "t" && fams.indexOf("t") === -1) {
          var a = t.args, m = /^([^\\]*)(\\.*)$/.exec(a);
          if (!m) return t;
          var inner = parseTags(m[2]).filter(function (x) { return x.junk || fams.indexOf(family(x.name)) === -1; });
          if (!inner.some(function (x) { return !x.junk; })) return null;
          return makeTag("t", m[1] + tagsToString(inner));
        }
        return fams.indexOf(family(t.name)) === -1 ? t : null;
      }).filter(Boolean);
      if (kept.length) out += "{" + tagsToString(kept) + "}";
    });
    return out;
  }
  function stripComments(text) {
    return splitBlocks(text).map(function (seg) {
      if (seg.type === "comment") return "";
      if (seg.type === "tags") {
        var tags = parseTags(seg.text).filter(function (t) { return !t.junk; });
        return tags.length ? "{" + tagsToString(tags) + "}" : "";
      }
      return seg.text;
    }).join("");
  }

  // Merges adjacent override blocks, drops empty ones and removes tags a later (or, for \pos and
  // friends, an earlier) tag of the same family makes dead. Only within one block: a \c that
  // changes colour mid-line is doing work.
  function cleanTags(text) {
    var merged = String(text || "").replace(/\{\}/g, "");
    var prev;
    do { prev = merged; merged = merged.replace(/\{(\\[^{}]*)\}\{(\\[^{}]*)\}/g, "{$1$2}"); } while (merged !== prev);
    var seenFirst = {};
    return splitBlocks(merged).map(function (seg) {
      if (seg.type === "text") return seg.text;
      if (seg.type === "comment") return "{" + seg.text + "}";
      var tags = parseTags(seg.text);
      var keep = tags.map(function () { return true; });
      var lastIdx = {};
      tags.forEach(function (t, i) {
        if (t.junk || t.name === "t" || t.name === "r" || /^k/i.test(t.name) || t.name === "") return;
        var fam = family(t.name);
        if (FIRST_WINS[fam]) {
          if (seenFirst[fam]) keep[i] = false; else seenFirst[fam] = true;
          return;
        }
        if (lastIdx[fam] !== undefined) keep[lastIdx[fam]] = false;
        lastIdx[fam] = i;
      });
      var kept = tags.filter(function (_, i) { return keep[i]; });
      return kept.length ? "{" + tagsToString(kept) + "}" : "";
    }).join("");
  }

  // Inserts tags at a raw text offset. Inside or right after an override block the tags join that
  // block; otherwise a new block opens there.
  function insertTagsAt(text, offset, tagList) {
    text = String(text || "");
    offset = clamp(offset | 0, 0, text.length);
    var segs = splitBlocks(text);
    for (var i = 0; i < segs.length; i++) {
      var seg = segs[i];
      if (seg.type !== "tags") continue;
      var end = seg.start + seg.text.length + 2;
      if (offset > seg.start && offset <= end) {
        var tags = parseTags(seg.text);
        tagList.forEach(function (nt) {
          if (nt.name !== "t") {
            var fam = family(nt.name);
            tags = tags.filter(function (t) { return t.junk || family(t.name) !== fam; });
          }
          tags.push(nt);
        });
        return text.slice(0, seg.start) + "{" + tagsToString(tags) + "}" + text.slice(end);
      }
    }
    if (offset === 0) return setStartTags(text, tagList);
    return text.slice(0, offset) + "{" + tagsToString(tagList) + "}" + text.slice(offset);
  }

  // ---- HYDRA -------------------------------------------------------------------
  // The tag kinds the tagging tool knows, how to write a value and how to blend two of them.
  var HYDRA_TAGS = {
    c: { label: "Primary", kind: "color" }, "2c": { label: "Secondary", kind: "color" },
    "3c": { label: "Border colour", kind: "color" }, "4c": { label: "Shadow colour", kind: "color" },
    alpha: { label: "Alpha", kind: "alpha" }, "1a": { label: "Primary alpha", kind: "alpha" },
    "3a": { label: "Border alpha", kind: "alpha" }, "4a": { label: "Shadow alpha", kind: "alpha" },
    bord: { label: "Border", kind: "num" }, shad: { label: "Shadow", kind: "num" },
    xbord: { label: "X border", kind: "num" }, ybord: { label: "Y border", kind: "num" },
    xshad: { label: "X shadow", kind: "num" }, yshad: { label: "Y shadow", kind: "num" },
    blur: { label: "Blur", kind: "num" }, be: { label: "Blur edges", kind: "num" },
    fs: { label: "Font size", kind: "num" }, fscx: { label: "Scale X", kind: "num" }, fscy: { label: "Scale Y", kind: "num" },
    fsp: { label: "Spacing", kind: "num" }, frz: { label: "Rotate Z", kind: "num" }, frx: { label: "Rotate X", kind: "num" },
    fry: { label: "Rotate Y", kind: "num" }, fax: { label: "Shear X", kind: "num" }, fay: { label: "Shear Y", kind: "num" },
    an: { label: "Alignment", kind: "int", noblend: true }, q: { label: "Wrap style", kind: "int", noblend: true },
    b: { label: "Bold", kind: "int", noblend: true }, i: { label: "Italic", kind: "int", noblend: true },
    u: { label: "Underline", kind: "int", noblend: true }, s: { label: "Strikeout", kind: "int", noblend: true },
    fn: { label: "Font name", kind: "str", noblend: true }
  };

  function hydraValue(name, v) {
    var def = HYDRA_TAGS[name] || { kind: "num" };
    if (def.kind === "color") return tagColor(typeof v === "string" ? hexToColor(v) : v);
    if (def.kind === "alpha") return tagAlpha(typeof v === "string" && /^&?h/i.test(v) ? parseAlpha(v) : Number(v) || 0);
    if (def.kind === "int") return String(Math.round(Number(v) || 0));
    if (def.kind === "str") return String(v || "");
    return fmtNum(v, 2);
  }
  function blend(name, a, b, t) {
    var def = HYDRA_TAGS[name] || { kind: "num" };
    if (def.noblend) return t < 1 ? a : b;
    if (def.kind === "color") {
      var ca = typeof a === "string" ? hexToColor(a) : a, cb = typeof b === "string" ? hexToColor(b) : b;
      return { r: ca.r + (cb.r - ca.r) * t, g: ca.g + (cb.g - ca.g) * t, b: ca.b + (cb.b - ca.b) * t, a: 0 };
    }
    return Number(a) + (Number(b) - Number(a)) * t;
  }
  function hydraTags(values) {
    return Object.keys(values).map(function (n) { return makeTag(n, hydraValue(n, values[n])); });
  }

  // `values` is { tagName: value }; colours are "#rrggbb", alphas 0-255, the rest numbers.
  // mode: "start" (the leading block), "cursor" (at raw offset `offset`), or "transform" (a \t in
  // the leading block, timed by t1/t2/accel when given).
  function hydraApply(text, values, opts) {
    opts = opts || {};
    var tags = hydraTags(values);
    if (!tags.length) return text;
    if (opts.mode === "transform") {
      var head = "";
      if (opts.t1 !== undefined && opts.t1 !== "" && opts.t2 !== undefined && opts.t2 !== "") {
        head = Math.round(Number(opts.t1) || 0) + "," + Math.round(Number(opts.t2) || 0) + ",";
        if (opts.accel !== undefined && opts.accel !== "" && Number(opts.accel) !== 1) head += fmtNum(opts.accel, 2) + ",";
      }
      return setStartTags(text, [makeTag("t", head + tagsToString(tags))]);
    }
    if (opts.mode === "cursor") return insertTagsAt(text, opts.offset || 0, tags);
    return setStartTags(text, tags);
  }

  // Gradient by character: a block before every visible character, each value blended from
  // `from` to `to`. Escapes (\N, \h) are single characters that never get a block of their own
  // split through them, and spaces are skipped when `skipSpaces` so they don't eat a step.
  function hydraGradientChars(text, from, to, skipSpaces) {
    var names = Object.keys(from).filter(function (n) { return to[n] !== undefined; });
    if (!names.length) return text;
    var segs = splitBlocks(text), units = [];
    segs.forEach(function (seg, si) {
      if (seg.type !== "text") return;
      var chars = seg.text.match(/\\[Nnh]|[\uD800-\uDBFF][\uDC00-\uDFFF]|[\s\S]/g) || [];
      chars.forEach(function (ch) {
        var counts = !(ch === "\\N" || ch === "\\n") && !(skipSpaces && (/^\s$/.test(ch) || ch === "\\h"));
        units.push({ seg: si, ch: ch, counts: counts });
      });
    });
    var total = units.filter(function (u) { return u.counts; }).length;
    if (!total) return text;
    var k = 0, bySeg = {};
    units.forEach(function (u) {
      var piece = u.ch;
      if (u.counts) {
        var t = total === 1 ? 0 : k / (total - 1);
        var vals = {};
        names.forEach(function (n) { vals[n] = blend(n, from[n], to[n], t); });
        piece = "{" + tagsToString(hydraTags(vals)) + "}" + u.ch;
        k++;
      }
      bySeg[u.seg] = (bySeg[u.seg] || "") + piece;
    });
    return cleanTags(segs.map(function (seg, si) {
      if (seg.type === "text") return bySeg[si] || "";
      return "{" + seg.text + "}";
    }).join(""));
  }

  // ---- line operations ---------------------------------------------------------
  // Replaces the space closest to the visual middle with \N. Lines already broken, drawings and
  // lines no longer than `maxLen` are left alone.
  function autoBreak(text, maxLen, force) {
    if (isDrawing(text)) return text;
    var vis = visibleText(text);
    if (!force && (vis.indexOf("\n") !== -1 || Array.from(vis).length <= maxLen)) return text;
    if (force && vis.indexOf("\n") !== -1) {
      text = text.replace(/\s*\\[Nn]\s*/g, " ");
      vis = visibleText(text);
    }
    var total = Array.from(vis).length, candidates = [], vi = 0;
    splitBlocks(text).forEach(function (seg) {
      if (seg.type !== "text") return;
      var chars = seg.text.match(/\\[Nnh]|[\uD800-\uDBFF][\uDC00-\uDFFF]|[\s\S]/g) || [], raw = seg.start;
      chars.forEach(function (ch) {
        if (ch === " ") candidates.push({ raw: raw, vis: vi });
        raw += ch.length; vi += 1;
      });
    });
    if (!candidates.length) return text;
    var mid = total / 2, best = candidates[0];
    candidates.forEach(function (c) {
      // Ties go to the later space: a longer bottom line reads better than a longer top one.
      if (Math.abs(c.vis - mid) <= Math.abs(best.vis - mid)) best = c;
    });
    return text.slice(0, best.raw) + "\\N" + text.slice(best.raw + 1);
  }

  // Splits one event into two at a raw text offset. Time is divided in proportion to the visible
  // characters on each side unless `atMs` names the cut. The second half inherits the tags that
  // were active at the cut — the leading block's — so it does not suddenly lose its \pos.
  function splitEvent(ev, offset, atMs) {
    var text = ev.text;
    offset = clamp(offset, 0, text.length);
    var left = text.slice(0, offset).replace(/\s*\\N\s*$/, "").replace(/\s+$/, "");
    var right = text.slice(offset).replace(/^\s*\\N\s*/, "").replace(/^\s+/, "");
    var segs = splitBlocks(text);
    var lead = segs.length && segs[0].type === "tags" && segs[0].start === 0 && offset >= segs[0].text.length + 2
      ? "{" + segs[0].text + "}" : "";
    var lv = Array.from(visibleText(left)).length, rv = Array.from(visibleText(right)).length;
    var cut = atMs !== undefined && atMs !== null && atMs > ev.start && atMs < ev.end ? atMs
      : Math.round(ev.start + (ev.end - ev.start) * (lv + rv ? lv / (lv + rv) : 0.5));
    return [withEvent(ev, { text: left, end: cut }), withEvent(cloneEvent(ev), { text: cleanTags(lead + right), start: cut })];
  }

  // Joins events into the first: its style and fields, the union of their time, and the texts
  // joined by `sep` (\N or a space). With keepFirst the first line's text is kept alone.
  function joinEvents(list, sep, keepFirst) {
    if (!list.length) return null;
    var sorted = list.slice().sort(function (a, b) { return a.start - b.start; });
    var start = Math.min.apply(null, list.map(function (e) { return e.start; }));
    var end = Math.max.apply(null, list.map(function (e) { return e.end; }));
    var text = keepFirst ? sorted[0].text : sorted.map(function (e) { return e.text; }).filter(function (t) { return t !== ""; }).join(sep);
    return withEvent(sorted[0], { start: start, end: end, text: text });
  }

  // Sorting is stable: equal keys keep their current order, which is what makes sorting by style
  // and then by time (or the reverse) useful.
  var SORT_KEYS = {
    start: function (e) { return e.start; }, end: function (e) { return e.end; },
    duration: function (e) { return e.end - e.start; }, layer: function (e) { return e.layer; },
    style: function (e) { return e.style.toLowerCase(); }, actor: function (e) { return e.actor.toLowerCase(); },
    effect: function (e) { return e.effect.toLowerCase(); }, text: function (e) { return strippedText(e.text).toLowerCase(); },
    cps: cps, length: maxLineLength, comment: function (e) { return e.comment ? 1 : 0; }
  };
  function sortEvents(events, key, desc) {
    var fn = SORT_KEYS[key] || SORT_KEYS.start, dir = desc ? -1 : 1;
    return events.map(function (e, i) { return { e: e, i: i, k: fn(e) }; })
      .sort(function (a, b) {
        if (a.k < b.k) return -dir;
        if (a.k > b.k) return dir;
        return a.i - b.i;
      }).map(function (x) { return x.e; });
  }

  function shiftEvent(ev, ms, which) {
    var p = {};
    if (which !== "end") p.start = Math.max(0, ev.start + ms);
    if (which !== "start") p.end = Math.max(0, ev.end + ms);
    var out = withEvent(ev, p);
    if (out.end < out.start) out = withEvent(out, which === "start" ? { start: out.end } : { end: out.start });
    return out;
  }

  // Lines that share screen time with another non-comment line. Returned as a Set of ids.
  function overlaps(events) {
    var list = events.filter(function (e) { return !e.comment && e.end > e.start; })
      .slice().sort(function (a, b) { return a.start - b.start; });
    var hit = new Set(), active = [];
    list.forEach(function (e) {
      active = active.filter(function (a) { return a.end > e.start; });
      active.forEach(function (a) { hit.add(a.id); hit.add(e.id); });
      active.push(e);
    });
    return hit;
  }

  // Timing post-processing over a set of lines: lead-in and lead-out, then closing gaps (or
  // overlaps) shorter than `threshold` between consecutive lines, meeting at `bias` of the gap.
  function postTime(events, o) {
    o = o || {};
    var leadIn = Number(o.leadIn) || 0, leadOut = Number(o.leadOut) || 0;
    var threshold = Number(o.threshold) || 0, bias = o.bias === undefined ? 0.5 : clamp(Number(o.bias), 0, 1);
    var fps = Number(o.fps) || 0;
    var out = events.map(function (e) {
      if (e.comment) return e;
      return withEvent(e, { start: Math.max(0, e.start - leadIn), end: e.end + leadOut });
    });
    if (threshold > 0) {
      var order = out.map(function (e, i) { return i; }).filter(function (i) { return !out[i].comment; })
        .sort(function (a, b) { return out[a].start - out[b].start; });
      for (var k = 0; k + 1 < order.length; k++) {
        var a = out[order[k]], b = out[order[k + 1]];
        if (o.sameStyle && a.style !== b.style) continue;
        var gap = b.start - a.end;
        if (Math.abs(gap) <= threshold && b.start >= a.start) {
          var meet = Math.round(a.end + gap * bias);
          out[order[k]] = withEvent(a, { end: meet });
          out[order[k + 1]] = withEvent(b, { start: meet });
        }
      }
    }
    if (fps > 0 && o.snap) out = out.map(function (e) {
      return e.comment ? e : withEvent(e, { start: snapToFrame(e.start, fps), end: snapToFrame(e.end, fps) });
    });
    return out.map(function (e) { return e.end < e.start ? withEvent(e, { end: e.start }) : e; });
  }

  // Ends each line where reading it takes `targetCps`, never shorter than `minMs`, and never past
  // the next line's start when `noOverlap`.
  function durationFromCps(ev, targetCps, minMs) {
    var chars = (visibleText(ev.text).match(/[\p{L}\p{N}]/gu) || []).length;
    var need = Math.max(Number(minMs) || 0, Math.round(chars / Math.max(1, Number(targetCps) || 15) * 1000));
    return withEvent(ev, { end: ev.start + need });
  }

  function changeCase(text, mode) {
    var fn = mode === "upper" ? function (s) { return s.toUpperCase(); }
      : mode === "lower" ? function (s) { return s.toLowerCase(); }
      : null;
    var sentenceStart = true;
    return splitBlocks(text).map(function (seg) {
      if (seg.type !== "text") return "{" + seg.text + "}";
      // \N, \n and \h are escapes, not letters: split them out so casing cannot turn \N into \n.
      return seg.text.split(/(\\[Nnh])/).map(function (piece) {
        if (/^\\[Nnh]$/.test(piece)) return piece;
        if (fn) return fn(piece);
        if (mode === "title") return piece.toLowerCase().replace(/(^|[\s\-"'(\[])(\p{L})/gu, function (_, a, b) { return a + b.toUpperCase(); });
        // sentence
        var s = "";
        Array.from(piece.toLowerCase()).forEach(function (ch) {
          if (sentenceStart && /\p{L}/u.test(ch)) { s += ch.toUpperCase(); sentenceStart = false; }
          else s += ch;
          if (/[.!?]/.test(ch)) sentenceStart = true;
        });
        return s;
      }).join("");
    }).join("");
  }

  // ---- resampling ------------------------------------------------------------------
  function scaleDrawing(cmds, sx, sy) {
    var axis = 0;
    return cmds.replace(/[a-zA-Z]|-?\d*\.?\d+(?:e-?\d+)?/g, function (tok) {
      if (/^[a-zA-Z]$/.test(tok)) { axis = 0; return tok; }
      var v = Number(tok) * (axis % 2 === 0 ? sx : sy);
      axis++;
      return fmtNum(v, 2);
    });
  }
  function scaleTagArgs(t, sx, sy, sr) {
    var a = argsOf(t);
    var n = function (v, f) { return fmtNum(Number(v) * f, 2); };
    switch (t.name) {
      case "pos": case "org":
        return makeTag(t.name, n(a[0], sx) + "," + n(a[1], sy));
      case "move":
        return makeTag("move", [n(a[0], sx), n(a[1], sy), n(a[2], sx), n(a[3], sy)].concat(a.slice(4)).join(","));
      case "clip": case "iclip":
        if (a.length === 4) return makeTag(t.name, [n(a[0], sx), n(a[1], sy), n(a[2], sx), n(a[3], sy)].join(","));
        if (a.length === 2) return makeTag(t.name, a[0] + "," + scaleDrawing(a[1], sx, sy));
        return makeTag(t.name, scaleDrawing(a[0], sx, sy));
      case "fs": case "bord": case "shad": case "blur": case "be": case "ybord": case "yshad":
        if (t.args === "") return t;
        return makeTag(t.name, t.name === "be" ? String(Math.round(Number(t.args) * sr)) : n(t.args, t.name === "ybord" || t.name === "yshad" || t.name === "fs" ? sy : sr));
      case "xbord": case "xshad": case "fsp":
        if (t.args === "") return t;
        return makeTag(t.name, n(t.args, sx));
      case "t":
        var m = /^([^\\]*)(\\.*)$/.exec(t.args);
        if (!m) return t;
        return makeTag("t", m[1] + tagsToString(parseTags(m[2]).map(function (x) { return x.junk ? x : scaleTagArgs(x, sx, sy, sr); })));
    }
    return t;
  }
  // Rescales a script from one PlayRes to another: styles, margins, positions, clips and
  // drawings. Border/shadow/blur follow the vertical scale, as Aegisub's resampler does.
  function resample(doc, newX, newY) {
    var old = playRes(doc);
    var sx = newX / old.x, sy = newY / old.y, sr = sy;
    var styles = doc.styles.map(function (s) {
      var c = {};
      for (var k in s) c[k] = s[k];
      c.Fontsize = Number(fmtNum(s.Fontsize * sy, 2));
      c.Outline = Number(fmtNum(s.Outline * sr, 2));
      c.Shadow = Number(fmtNum(s.Shadow * sr, 2));
      c.Spacing = Number(fmtNum(s.Spacing * sx, 2));
      c.MarginL = Math.round(s.MarginL * sx); c.MarginR = Math.round(s.MarginR * sx); c.MarginV = Math.round(s.MarginV * sy);
      return c;
    });
    var events = doc.events.map(function (e) {
      var drawing = false;
      var text = splitBlocks(e.text).map(function (seg) {
        if (seg.type === "comment") return "{" + seg.text + "}";
        if (seg.type === "text") return drawing ? scaleDrawing(seg.text, sx, sy) : seg.text;
        var tags = parseTags(seg.text).map(function (t) {
          if (t.name === "p") drawing = (Number(t.args) || 0) > 0;
          return t.junk ? t : scaleTagArgs(t, sx, sy, sr);
        });
        return "{" + tagsToString(tags) + "}";
      }).join("");
      return withEvent(e, { text: text, marginL: Math.round(e.marginL * sx), marginR: Math.round(e.marginR * sx), marginV: Math.round(e.marginV * sy) });
    });
    var info = setInfo({ info: doc.info }, "PlayResX", newX);
    info = setInfo({ info: info }, "PlayResY", newY);
    return { info: info, styles: styles, events: events, extras: doc.extras };
  }

  // ---- find & select ------------------------------------------------------------------
  function fieldValue(e, field) {
    switch (field) {
      case "text": return e.text;
      case "visible": return strippedText(e.text);
      case "style": return e.style;
      case "actor": return e.actor;
      case "effect": return e.effect;
      case "layer": return e.layer;
      case "duration": return e.end - e.start;
      case "start": return e.start;
      case "end": return e.end;
      case "cps": return cps(e);
      case "length": return maxLineLength(e);
    }
    return "";
  }
  function buildMatcher(op, value, caseSensitive) {
    var v = String(value === undefined ? "" : value);
    if (op === "regex" || op === "notregex") {
      var re;
      try { re = new RegExp(v, caseSensitive ? "u" : "iu"); } catch (e) { return null; }
      return function (x) { var r = re.test(String(x)); return op === "regex" ? r : !r; };
    }
    if (op === "gt" || op === "lt" || op === "ge" || op === "le" || op === "eqnum") {
      var num = Number(v);
      return function (x) {
        x = Number(x);
        return op === "gt" ? x > num : op === "lt" ? x < num : op === "ge" ? x >= num : op === "le" ? x <= num : x === num;
      };
    }
    var norm = function (s) { return caseSensitive ? String(s) : String(s).toLowerCase(); };
    var nv = norm(v);
    return function (x) {
      var s = norm(x);
      switch (op) {
        case "equals": return s === nv;
        case "starts": return s.indexOf(nv) === 0;
        case "ends": return s.slice(-nv.length) === nv;
        case "not": return s.indexOf(nv) === -1;
        default: return s.indexOf(nv) !== -1;
      }
    };
  }

  // Find and replace over text outside override blocks, or over the raw line (tags included).
  function replaceIn(text, re, replacement, rawMode) {
    if (rawMode) return text.replace(re, replacement);
    return splitBlocks(text).map(function (seg) {
      return seg.type === "text" ? seg.text.replace(re, replacement) : "{" + seg.text + "}";
    }).join("");
  }

  global.ASS = {
    STYLE_FIELDS: STYLE_FIELDS, EVENT_FIELDS: EVENT_FIELDS, HYDRA_TAGS: HYDRA_TAGS, SORT_KEYS: SORT_KEYS,
    uid: uid, fmtNum: fmtNum, clamp: clamp,
    parseTime: parseTime, formatTime: formatTime, formatSrtTime: formatSrtTime, parseLooseTime: parseLooseTime,
    frameAt: frameAt, frameStart: frameStart, snapToFrame: snapToFrame,
    parseColor: parseColor, styleColor: styleColor, tagColor: tagColor, tagAlpha: tagAlpha, parseAlpha: parseAlpha,
    colorToCss: colorToCss, colorToHex: colorToHex, hexToColor: hexToColor,
    defaultStyle: defaultStyle, makeEvent: makeEvent, withEvent: withEvent, cloneEvent: cloneEvent, newDoc: newDoc,
    getInfo: getInfo, setInfo: setInfo, playRes: playRes,
    parseAss: parseAss, parseSrt: parseSrt, parseAny: parseAny, serializeAss: serializeAss, serializeSrt: serializeSrt,
    splitBlocks: splitBlocks, parseTags: parseTags, tagsToString: tagsToString, makeTag: makeTag, family: family, argsOf: argsOf,
    isDrawing: isDrawing, visibleText: visibleText, strippedText: strippedText, cps: cps, maxLineLength: maxLineLength,
    setStartTags: setStartTags, stripTags: stripTags, stripComments: stripComments, cleanTags: cleanTags, insertTagsAt: insertTagsAt,
    hydraApply: hydraApply, hydraGradientChars: hydraGradientChars, hydraValue: hydraValue, blend: blend,
    autoBreak: autoBreak, splitEvent: splitEvent, joinEvents: joinEvents, sortEvents: sortEvents, shiftEvent: shiftEvent,
    overlaps: overlaps, postTime: postTime, durationFromCps: durationFromCps, changeCase: changeCase,
    resample: resample, scaleDrawing: scaleDrawing,
    fieldValue: fieldValue, buildMatcher: buildMatcher, replaceIn: replaceIn
  };
  if (typeof module !== "undefined" && module.exports) module.exports = global.ASS;
})(typeof window !== "undefined" ? window : globalThis);
