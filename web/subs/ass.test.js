// Tests for the Pandora Subs model. Run with `node web/subs/ass.test.js`; exits non-zero on failure.
"use strict";
var assert = require("assert");
var ASS = require("./ass.js");

var failures = 0, passed = 0;
function test(name, fn) {
  try { fn(); passed++; } catch (e) { failures++; console.error("FAIL " + name + "\n  " + (e && e.message)); }
}

var SAMPLE = [
  "﻿[Script Info]",
  "; a comment",
  "Title: Sample",
  "ScriptType: v4.00+",
  "PlayResX: 1280",
  "PlayResY: 720",
  "",
  "[Aegisub Project Garbage]",
  "Video File: ep01.mkv",
  "",
  "[V4+ Styles]",
  "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
  "Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,2.5,1,2,20,20,30,1",
  "Style: Sign,Verdana,36,&H0000FFFF,&H000000FF,&H00101010,&H00000000,0,-1,0,0,100,100,0,0,1,3,0,8,10,10,10,1",
  "",
  "[Events]",
  "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
  "Dialogue: 0,0:00:01.00,0:00:03.50,Default,Alice,0,0,0,,Hello, world!",
  "Comment: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,{note} a comment line",
  "Dialogue: 1,0:00:05.00,0:00:06.00,Sign,,0,0,0,,{\\pos(640,100)\\c&H0000FF&}Sign, with comma",
  "",
  "[Aegisub Extradata]",
  "Data: 1,foo,e1"
].join("\n");

test("parse ASS sections, styles and events", function () {
  var doc = ASS.parseAss(SAMPLE);
  assert.strictEqual(ASS.getInfo(doc, "Title"), "Sample");
  assert.strictEqual(doc.styles.length, 2);
  assert.strictEqual(doc.styles[0].Bold, true);
  assert.strictEqual(doc.styles[0].Outline, 2.5);
  assert.strictEqual(doc.styles[1].Alignment, 8);
  assert.strictEqual(doc.events.length, 3);
  assert.strictEqual(doc.events[0].text, "Hello, world!");
  assert.strictEqual(doc.events[0].actor, "Alice");
  assert.strictEqual(doc.events[0].start, 1000);
  assert.strictEqual(doc.events[0].end, 3500);
  assert.strictEqual(doc.events[1].comment, true);
  assert.strictEqual(doc.events[2].layer, 1);
  assert.strictEqual(doc.extras.length, 2);
  assert.strictEqual(doc.extras[0].pos, "pre");
  assert.strictEqual(doc.extras[1].pos, "post");
});

test("serialise round-trips byte-for-byte on the second pass", function () {
  var once = ASS.serializeAss(ASS.parseAss(SAMPLE));
  var twice = ASS.serializeAss(ASS.parseAss(once));
  assert.strictEqual(once, twice);
  assert.ok(once.indexOf("Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,2.5,1,2,20,20,30,1") !== -1);
  assert.ok(once.indexOf("Dialogue: 1,0:00:05.00,0:00:06.00,Sign,,0,0,0,,{\\pos(640,100)\\c&H0000FF&}Sign, with comma") !== -1);
  assert.ok(once.indexOf("[Aegisub Project Garbage]") < once.indexOf("[V4+ Styles]"));
  assert.ok(once.indexOf("[Aegisub Extradata]") > once.indexOf("[Events]"));
});

test("SSA v4 styles map legacy alignment", function () {
  var ssa = "[Script Info]\nScriptType: v4.00\n\n[V4 Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding\nStyle: Top,Arial,20,16777215,255,0,0,0,0,1,2,2,6,10,10,10,0,0\n\n[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: Marked=0,0:00:00.00,0:00:01.00,Top,,0000,0000,0000,,Hi";
  var doc = ASS.parseAss(ssa);
  assert.strictEqual(doc.styles[0].Alignment, 8);
  assert.strictEqual(doc.styles[0].PrimaryColour, "&H00FFFFFF");
  assert.strictEqual(doc.events[0].text, "Hi");
  assert.strictEqual(ASS.getInfo(doc, "ScriptType"), "v4.00+");
});

test("time parsing and formatting", function () {
  assert.strictEqual(ASS.parseTime("1:02:03.45"), 3723450);
  assert.strictEqual(ASS.formatTime(3723454), "1:02:03.45");
  assert.strictEqual(ASS.formatTime(3723455), "1:02:03.46");
  assert.strictEqual(ASS.parseLooseTime("1:02.5"), 62500);
  assert.strictEqual(ASS.parseLooseTime("62.5"), 62500);
  assert.strictEqual(ASS.parseLooseTime("-0.5"), -500);
  assert.strictEqual(ASS.parseLooseTime("abc"), null);
  assert.strictEqual(ASS.formatSrtTime(3723456), "01:02:03,456");
});

test("frame maths at 23.976", function () {
  var fps = 24000 / 1001;
  assert.strictEqual(ASS.frameAt(0, fps), 0);
  assert.strictEqual(ASS.frameStart(24, fps), 1001);
  assert.strictEqual(ASS.frameAt(1001, fps), 24);
  assert.strictEqual(ASS.snapToFrame(1010, fps), 1001);
});

test("colours", function () {
  var c = ASS.parseColor("&H80FF8000");
  assert.deepStrictEqual(c, { r: 0, g: 128, b: 255, a: 128 });
  assert.strictEqual(ASS.styleColor(c), "&H80FF8000");
  assert.strictEqual(ASS.tagColor(ASS.hexToColor("#ff0000")), "&H0000FF&");
  assert.strictEqual(ASS.colorToHex(ASS.parseColor("&H0000FF&")), "#ff0000");
});

test("SRT import converts tags and SRT export converts back", function () {
  var srt = "1\n00:00:01,000 --> 00:00:02,500\n<i>Hello</i>\nthere\n\n2\n00:00:03,000 --> 00:00:04,000\n<font color=\"#00ff00\">Green</font>\n";
  var doc = ASS.parseAny(srt, "x.srt");
  assert.strictEqual(doc.events.length, 2);
  assert.strictEqual(doc.events[0].text, "{\\i1}Hello{\\i0}\\Nthere");
  assert.strictEqual(doc.events[0].end, 2500);
  assert.strictEqual(doc.events[1].text, "{\\c&H00FF00&}Green{\\c}");
  var out = ASS.serializeSrt(doc);
  assert.ok(out.indexOf("00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i>\r\nthere") !== -1, out);
});

test("WebVTT import", function () {
  var vtt = "WEBVTT\n\n00:01.000 --> 00:02.000 line:0%\nTop\n\nid\n00:00:03.000 --> 00:00:04.000\nBottom";
  var doc = ASS.parseAny(vtt);
  assert.strictEqual(doc.events.length, 2);
  assert.strictEqual(doc.events[0].text, "{\\an8}Top");
  assert.strictEqual(doc.events[1].start, 3000);
});

test("tag parsing distinguishes long and short names", function () {
  var tags = ASS.parseTags("\\fscx120\\fs40\\bord2\\be1\\blur0.5\\an8\\alpha&H80&\\t(0,500,\\frz30)\\fnComic Sans\\r");
  assert.deepStrictEqual(tags.map(function (t) { return t.name; }), ["fscx", "fs", "bord", "be", "blur", "an", "alpha", "t", "fn", "r"]);
  assert.strictEqual(tags[7].args, "0,500,\\frz30");
  assert.strictEqual(tags[8].args, "Comic Sans");
});

test("visible text, CPS and line length", function () {
  var ev = ASS.makeEvent({ start: 0, end: 2000, text: "{\\i1}Hello{\\i0}\\Nworld{note}!" });
  assert.strictEqual(ASS.visibleText(ev.text), "Hello\nworld!");
  assert.strictEqual(ASS.cps(ev), 5);
  assert.strictEqual(ASS.maxLineLength(ev), 6);
  assert.strictEqual(ASS.maxLineLength(ASS.makeEvent({ text: "{\\p1}m 0 0 l 100 0 100 100" })), 0);
});

test("setStartTags replaces the same family and opens a block when needed", function () {
  assert.strictEqual(ASS.setStartTags("Hi", [ASS.makeTag("bord", "3")]), "{\\bord3}Hi");
  assert.strictEqual(ASS.setStartTags("{\\bord1\\c&H0000FF&}Hi", [ASS.makeTag("bord", "3")]), "{\\c&H0000FF&\\bord3}Hi");
  assert.strictEqual(ASS.setStartTags("{\\1c&H0000FF&}Hi", [ASS.makeTag("c", "&HFFFFFF&")]), "{\\c&HFFFFFF&}Hi");
  assert.strictEqual(ASS.setStartTags("{\\move(1,2,3,4)}Hi", [ASS.makeTag("pos", "5,6")]), "{\\pos(5,6)}Hi");
  assert.strictEqual(ASS.setStartTags("{note}Hi", [ASS.makeTag("i", "1")]), "{\\i1}{note}Hi");
});

test("stripTags removes named tags, including inside \\t", function () {
  assert.strictEqual(ASS.stripTags("{\\bord2\\t(\\bord4\\blur2)}A{\\bord1}B", ["bord"]), "{\\t(\\blur2)}AB");
  assert.strictEqual(ASS.stripTags("{\\t(\\bord4)}A", ["bord"]), "A");
  assert.strictEqual(ASS.stripTags("{\\i1}A{note}B"), "A{note}B");
  assert.strictEqual(ASS.stripComments("{\\i1}A{note}B"), "{\\i1}AB");
});

test("cleanTags merges blocks and removes dead tags", function () {
  assert.strictEqual(ASS.cleanTags("{\\bord1}{\\bord2\\i1}{}Hi"), "{\\bord2\\i1}Hi");
  assert.strictEqual(ASS.cleanTags("{\\pos(1,2)\\pos(3,4)}Hi"), "{\\pos(1,2)}Hi");
  assert.strictEqual(ASS.cleanTags("{\\pos(1,2)}A{\\pos(3,4)\\i1}B"), "{\\pos(1,2)}A{\\i1}B");
  assert.strictEqual(ASS.cleanTags("{\\c&H0000FF&}A{\\c&HFF0000&}B"), "{\\c&H0000FF&}A{\\c&HFF0000&}B");
});

test("insertTagsAt joins an adjacent block or opens one", function () {
  assert.strictEqual(ASS.insertTagsAt("Hello world", 6, [ASS.makeTag("i", "1")]), "Hello {\\i1}world");
  assert.strictEqual(ASS.insertTagsAt("{\\b1}Hello", 5, [ASS.makeTag("i", "1")]), "{\\b1\\i1}Hello");
  assert.strictEqual(ASS.insertTagsAt("Hello", 0, [ASS.makeTag("i", "1")]), "{\\i1}Hello");
});

test("HYDRA apply modes", function () {
  assert.strictEqual(ASS.hydraApply("Hi", { c: "#ff0000", bord: 3 }), "{\\c&H0000FF&\\bord3}Hi");
  assert.strictEqual(ASS.hydraApply("Hi", { frz: 30 }, { mode: "transform", t1: 0, t2: 500 }), "{\\t(0,500,\\frz30)}Hi");
  assert.strictEqual(ASS.hydraApply("Hi", { frz: 30 }, { mode: "transform", t1: 0, t2: 500, accel: 2 }), "{\\t(0,500,2,\\frz30)}Hi");
  assert.strictEqual(ASS.hydraApply("Hi", { frz: 30 }, { mode: "transform" }), "{\\t(\\frz30)}Hi");
  assert.strictEqual(ASS.hydraApply("Hi there", { i: 1 }, { mode: "cursor", offset: 3 }), "Hi {\\i1}there");
  assert.strictEqual(ASS.hydraApply("Hi", { alpha: 128 }), "{\\alpha&H80&}Hi");
});

test("HYDRA gradient by character", function () {
  var out = ASS.hydraGradientChars("{\\pos(1,1)}ab c", { bord: 0 }, { bord: 2 }, true);
  assert.strictEqual(out, "{\\pos(1,1)\\bord0}a{\\bord1}b {\\bord2}c");
  var col = ASS.hydraGradientChars("AB", { c: "#000000" }, { c: "#ffffff" }, false);
  assert.strictEqual(col, "{\\c&H000000&}A{\\c&HFFFFFF&}B");
  var br = ASS.hydraGradientChars("A\\NB", { fs: 10 }, { fs: 20 }, true);
  assert.strictEqual(br, "{\\fs10}A\\N{\\fs20}B");
});

test("autoBreak balances at the middle space", function () {
  assert.strictEqual(ASS.autoBreak("one two three four", 10), "one two\\Nthree four");
  assert.strictEqual(ASS.autoBreak("short", 10), "short");
  assert.strictEqual(ASS.autoBreak("{\\i1}alpha beta gamma", 5), "{\\i1}alpha beta\\Ngamma");
  assert.strictEqual(ASS.autoBreak("a b\\Nc d e f g", 3, true), "a b c d\\Ne f g");
});

test("splitEvent divides time by characters and keeps the leading block", function () {
  var ev = ASS.makeEvent({ start: 0, end: 1000, text: "{\\pos(1,2)}abc def" });
  var parts = ASS.splitEvent(ev, ev.text.indexOf("def"));
  assert.strictEqual(parts[0].text, "{\\pos(1,2)}abc");
  assert.strictEqual(parts[1].text, "{\\pos(1,2)}def");
  assert.strictEqual(parts[0].end, 500);
  assert.strictEqual(parts[1].start, 500);
  assert.notStrictEqual(parts[0].id, parts[1].id);
  var atTime = ASS.splitEvent(ev, 13, 800);
  assert.strictEqual(atTime[0].end, 800);
});

test("joinEvents unions time", function () {
  var a = ASS.makeEvent({ start: 1000, end: 2000, text: "A" }), b = ASS.makeEvent({ start: 0, end: 1500, text: "B" });
  var j = ASS.joinEvents([a, b], "\\N");
  assert.strictEqual(j.text, "B\\NA");
  assert.strictEqual(j.start, 0);
  assert.strictEqual(j.end, 2000);
});

test("sortEvents is stable and supports descending", function () {
  var e = [
    ASS.makeEvent({ start: 3, style: "B", text: "x" }), ASS.makeEvent({ start: 1, style: "A", text: "y" }),
    ASS.makeEvent({ start: 2, style: "B", text: "z" })
  ];
  assert.deepStrictEqual(ASS.sortEvents(e, "start").map(function (x) { return x.start; }), [1, 2, 3]);
  assert.deepStrictEqual(ASS.sortEvents(e, "style").map(function (x) { return x.text; }), ["y", "x", "z"]);
  assert.deepStrictEqual(ASS.sortEvents(e, "style", true).map(function (x) { return x.text; }), ["x", "z", "y"]);
});

test("overlaps and postTime", function () {
  var a = ASS.makeEvent({ start: 0, end: 1000 }), b = ASS.makeEvent({ start: 900, end: 2000 }), c = ASS.makeEvent({ start: 2100, end: 3000 });
  var hit = ASS.overlaps([a, b, c]);
  assert.ok(hit.has(a.id) && hit.has(b.id) && !hit.has(c.id));
  var out = ASS.postTime([a, b, c], { threshold: 200, bias: 0.5 });
  assert.strictEqual(out[0].end, 950);
  assert.strictEqual(out[1].start, 950);
  assert.strictEqual(out[1].end, 2050);
  assert.strictEqual(out[2].start, 2050);
  var lead = ASS.postTime([c], { leadIn: 100, leadOut: 200 });
  assert.strictEqual(lead[0].start, 2000);
  assert.strictEqual(lead[0].end, 3200);
});

test("shiftEvent never goes negative or inverts", function () {
  var e = ASS.makeEvent({ start: 500, end: 1000 });
  assert.strictEqual(ASS.shiftEvent(e, -1000).start, 0);
  var s = ASS.shiftEvent(e, 800, "start");
  assert.strictEqual(s.start, 1000);
  assert.strictEqual(s.end, 1000);
});

test("changeCase keeps tags and escapes intact", function () {
  assert.strictEqual(ASS.changeCase("{\\i1}hello\\Nworld", "upper"), "{\\i1}HELLO\\NWORLD");
  assert.strictEqual(ASS.changeCase("HELLO there. how ARE you", "sentence"), "Hello there. How are you");
  assert.strictEqual(ASS.changeCase("the quick fox", "title"), "The Quick Fox");
});

test("resample scales styles, tags, clips and drawings", function () {
  var doc = ASS.parseAss(SAMPLE);
  doc.events = doc.events.concat([ASS.makeEvent({ text: "{\\clip(0,0,640,360)\\fs36\\bord2}x" }), ASS.makeEvent({ text: "{\\p1}m 0 0 l 1280 720" })]);
  var r = ASS.resample(doc, 1920, 1080);
  assert.strictEqual(ASS.getInfo(r, "PlayResX"), "1920");
  assert.strictEqual(r.styles[0].Fontsize, 72);
  assert.strictEqual(r.styles[0].MarginV, 45);
  assert.strictEqual(r.events[2].text, "{\\pos(960,150)\\c&H0000FF&}Sign, with comma");
  assert.strictEqual(r.events[3].text, "{\\clip(0,0,960,540)\\fs54\\bord3}x");
  assert.strictEqual(r.events[4].text, "{\\p1}m 0 0 l 1920 1080");
});

test("matchers and replace outside tags", function () {
  assert.ok(ASS.buildMatcher("contains", "ell")("Hello"));
  assert.ok(!ASS.buildMatcher("contains", "ELL", true)("Hello"));
  assert.ok(ASS.buildMatcher("gt", "10")(12));
  assert.ok(ASS.buildMatcher("regex", "^h")("Hello"));
  assert.strictEqual(ASS.buildMatcher("regex", "("), null);
  assert.strictEqual(ASS.replaceIn("{\\c&H0000FF&}c", /c/g, "x"), "{\\c&H0000FF&}x");
  assert.strictEqual(ASS.replaceIn("{\\c&H0000FF&}c", /c/g, "x", true), "{\\x&H0000FF&}x");
});

test("durationFromCps", function () {
  var e = ASS.makeEvent({ start: 1000, end: 1100, text: "abcdefghijklmno" });
  assert.strictEqual(ASS.durationFromCps(e, 15, 500).end, 2000);
  assert.strictEqual(ASS.durationFromCps(ASS.withEvent(e, { text: "a" }), 15, 500).end, 1500);
});

console.log(passed + " passed, " + failures + " failed");
process.exit(failures ? 1 : 0);
