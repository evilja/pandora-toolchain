use std::fmt::Write;

use crate::libkagami::complex::overrides::ASSOverride;

// Appends one tag's text. Writing into the caller's buffer rather than
// returning a String matters because a typeset line can carry hundreds of
// tags and each one used to cost an allocation that was immediately copied
// into the line, then into the file.
pub fn write_override(out: &mut String, ov: &ASSOverride) {
    let _ = match ov {
        ASSOverride::BlockText(v)         => out.write_str(v),
        ASSOverride::Bold(v)              => write!(out, "b{}", *v as u8),
        ASSOverride::Italic(v)            => write!(out, "i{}", *v as u8),
        ASSOverride::Underline(v)         => write!(out, "u{}", *v as u8),
        ASSOverride::Strikeout(v)         => write!(out, "s{}", *v as u8),
        ASSOverride::Bord(v)              => write!(out, "bord{v}"),
        ASSOverride::Shad(v)              => write!(out, "shad{v}"),
        ASSOverride::Fn(v)                => write!(out, "fn{v}"),
        ASSOverride::Fs(v)                => write!(out, "fs{v}"),
        ASSOverride::Fsp(v)               => write!(out, "fsp{v}"),
        ASSOverride::Blur(v)              => write!(out, "blur{v}"),
        ASSOverride::Be(v)                => write!(out, "be{v}"),
        ASSOverride::Fscx(v)              => write!(out, "fscx{v}"),
        ASSOverride::Fscy(v)              => write!(out, "fscy{v}"),
        ASSOverride::Fsc(v)               => write!(out, "fsc{v}"),
        ASSOverride::Xbord(v)             => write!(out, "xbord{v}"),
        ASSOverride::Ybord(v)             => write!(out, "ybord{v}"),
        ASSOverride::Xshad(v)             => write!(out, "xshad{v}"),
        ASSOverride::Yshad(v)             => write!(out, "yshad{v}"),
        ASSOverride::Fax(v)               => write!(out, "fax{v}"),
        ASSOverride::Fay(v)               => write!(out, "fay{v}"),
        ASSOverride::Frx(v)               => write!(out, "frx{v}"),
        ASSOverride::Fry(v)               => write!(out, "fry{v}"),
        ASSOverride::Frz(v)               => write!(out, "frz{v}"),
        ASSOverride::Fr(v)                => write!(out, "fr{v}"),
        ASSOverride::Fad(a, b)            => write!(out, "fad({a},{b})"),
        ASSOverride::Fade(a,b,c,d,e,f,g)  => write!(out, "fade({a},{b},{c},{d},{e},{f},{g})"),
        ASSOverride::Pos(x, y)            => write!(out, "pos({x},{y})"),
        ASSOverride::Alpha(v)             => write!(out, "alpha&H{v:02X}&"),
        ASSOverride::AlphaI(v)            => write!(out, "1a&H{v:02X}&"),
        ASSOverride::AlphaII(v)           => write!(out, "2a&H{v:02X}&"),
        ASSOverride::AlphaIII(v)          => write!(out, "3a&H{v:02X}&"),
        ASSOverride::AlphaIV(v)           => write!(out, "4a&H{v:02X}&"),
        ASSOverride::ColorI(v)            => write!(out, "c&H{v:06X}&"),
        ASSOverride::ColorII(v)           => write!(out, "2c&H{v:06X}&"),
        ASSOverride::ColorIII(v)          => write!(out, "3c&H{v:06X}&"),
        ASSOverride::ColorIV(v)           => write!(out, "4c&H{v:06X}&"),
        ASSOverride::A(v)                 => write!(out, "a{v}"),
        ASSOverride::An(v)                => write!(out, "an{v}"),
        ASSOverride::P(v)                 => write!(out, "p{v}"),
        ASSOverride::ClipI(v)             => write!(out, "clip({v})"),
        ASSOverride::ClipII(s, v)         => write!(out, "clip({s},{v})"),
        ASSOverride::ClipRect(x0,y0,x1,y1)=> write!(out, "clip({x0},{y0},{x1},{y1})"),
        ASSOverride::IclipI(v)            => write!(out, "iclip({v})"),
        ASSOverride::IclipII(s, v)        => write!(out, "iclip({s},{v})"),
        ASSOverride::IclipRect(x0,y0,x1,y1)=> write!(out, "iclip({x0},{y0},{x1},{y1})"),
        ASSOverride::TransformI(v)        => write_transform(out, &[], v),
        ASSOverride::TransformII(a, v)    => write_transform(out, &[*a], v),
        ASSOverride::TransformIII(a,b,v)  => write_transform(out, &[*a, *b], v),
        ASSOverride::TransformIV(a,b,c,v) => write_transform(out, &[*a, *b, *c], v),
        ASSOverride::Fe(v)                => write!(out, "fe{v}"),
        ASSOverride::MoveI(a,b,c,d)       => write!(out, "move({a},{b},{c},{d})"),
        ASSOverride::MoveII(a,b,c,d,e,f)  => write!(out, "move({a},{b},{c},{d},{e},{f})"),
        ASSOverride::Org(x,y)             => write!(out, "org({x},{y})"),
        ASSOverride::Pbo(v)               => write!(out, "pbo{v}"),
        ASSOverride::Q(v)                 => write!(out, "q{v}"),
        ASSOverride::R(None)              => out.write_str("r"),
        ASSOverride::R(Some(s))           => write!(out, "r{s}"),
        ASSOverride::K(v)                 => write!(out, "k{v}"),
        ASSOverride::Kt(v)                => write!(out, "kt{v}"),
        ASSOverride::KSweep(v)            => write!(out, "K{v}"),
        ASSOverride::Kf(v)                => write!(out, "kf{v}"),
        ASSOverride::Ko(v)                => write!(out, "ko{v}"),
    };
}

fn write_transform(out: &mut String, leading: &[f32], tags: &[ASSOverride]) -> std::fmt::Result {
    out.write_str("t(")?;
    for value in leading {
        write!(out, "{value},")?;
    }
    write_overrides(out, tags);
    out.write_str(")")
}

pub fn write_overrides(out: &mut String, v: &[ASSOverride]) {
    for o in v {
        if !matches!(o, ASSOverride::BlockText(_)) {
            out.push('\\');
        }
        write_override(out, o);
    }
}

pub fn stringify_override(ov: &ASSOverride) -> String {
    let mut out = String::new();
    write_override(&mut out, ov);
    out
}

pub fn stringify_overrides(v: &[ASSOverride]) -> String {
    let mut out = String::new();
    write_overrides(&mut out, v);
    out
}

/// Debug formatter for tests — human readable variant names with values
pub fn fmt_override(ov: &ASSOverride) -> String {
    match ov {
        ASSOverride::BlockText(v)         => format!("BlockText({v:?})"),
        ASSOverride::Bold(v)              => format!("Bold({v})"),
        ASSOverride::Italic(v)            => format!("Italic({v})"),
        ASSOverride::Underline(v)         => format!("Underline({v})"),
        ASSOverride::Strikeout(v)         => format!("Strikeout({v})"),
        ASSOverride::Bord(v)              => format!("Bord({v})"),
        ASSOverride::Shad(v)              => format!("Shad({v})"),
        ASSOverride::Fn(v)                => format!("Fn({v:?})"),
        ASSOverride::Fs(v)                => format!("Fs({v})"),
        ASSOverride::Fsp(v)               => format!("Fsp({v})"),
        ASSOverride::Blur(v)              => format!("Blur({v})"),
        ASSOverride::Be(v)                => format!("Be({v})"),
        ASSOverride::Fscx(v)              => format!("Fscx({v})"),
        ASSOverride::Fscy(v)              => format!("Fscy({v})"),
        ASSOverride::Fsc(v)               => format!("Fsc({v})"),
        ASSOverride::Xbord(v)             => format!("Xbord({v})"),
        ASSOverride::Ybord(v)             => format!("Ybord({v})"),
        ASSOverride::Xshad(v)             => format!("Xshad({v})"),
        ASSOverride::Yshad(v)             => format!("Yshad({v})"),
        ASSOverride::Fax(v)               => format!("Fax({v})"),
        ASSOverride::Fay(v)               => format!("Fay({v})"),
        ASSOverride::Fr(v)                => format!("Fr({v})"),
        ASSOverride::Frx(v)               => format!("Frx({v})"),
        ASSOverride::Fry(v)               => format!("Fry({v})"),
        ASSOverride::Frz(v)               => format!("Frz({v})"),
        ASSOverride::Fad(a, b)            => format!("Fad({a}, {b})"),
        ASSOverride::Fade(a,b,c,d,e,f,g)  => format!("Fade({a},{b},{c},{d},{e},{f},{g})"),
        ASSOverride::Pos(x, y)            => format!("Pos({x}, {y})"),
        ASSOverride::Alpha(v)             => format!("Alpha({v:#010X})"),
        ASSOverride::AlphaI(v)            => format!("AlphaI({v:#010X})"),
        ASSOverride::AlphaII(v)           => format!("AlphaII({v:#010X})"),
        ASSOverride::AlphaIII(v)          => format!("AlphaIII({v:#010X})"),
        ASSOverride::AlphaIV(v)           => format!("AlphaIV({v:#010X})"),
        ASSOverride::ColorI(v)            => format!("ColorI({v:#010X})"),
        ASSOverride::ColorII(v)           => format!("ColorII({v:#010X})"),
        ASSOverride::ColorIII(v)          => format!("ColorIII({v:#010X})"),
        ASSOverride::ColorIV(v)           => format!("ColorIV({v:#010X})"),
        ASSOverride::A(v)                 => format!("A({v})"),
        ASSOverride::An(v)                => format!("An({v})"),
        ASSOverride::P(v)                 => format!("P({v})"),
        ASSOverride::ClipI(v)             => format!("ClipI({v:?})"),
        ASSOverride::ClipII(s, v)         => format!("ClipII({s}, {v:?})"),
        ASSOverride::ClipRect(x0,y0,x1,y1)=> format!("ClipRect({x0}, {y0}, {x1}, {y1})"),
        ASSOverride::IclipI(v)            => format!("IclipI({v:?})"),
        ASSOverride::IclipII(s, v)        => format!("IclipII({s}, {v:?})"),
        ASSOverride::IclipRect(x0,y0,x1,y1)=> format!("IclipRect({x0}, {y0}, {x1}, {y1})"),
        ASSOverride::TransformI(v)        => format!("TransformI([{}])", fmt_overrides(v)),
        ASSOverride::TransformII(a, v)    => format!("TransformII({a}, [{}])", fmt_overrides(v)),
        ASSOverride::TransformIII(a,b,v)  => format!("TransformIII({a}, {b}, [{}])", fmt_overrides(v)),
        ASSOverride::TransformIV(a,b,c,v) => format!("TransformIV({a}, {b}, {c}, [{}])", fmt_overrides(v)),
        ASSOverride::Fe(v)                => format!("Fe({v})"),
        ASSOverride::MoveI(a,b,c,d)       => format!("MoveI({a},{b},{c},{d})"),
        ASSOverride::MoveII(a,b,c,d,e,f)  => format!("MoveII({a},{b},{c},{d},{e},{f})"),
        ASSOverride::Org(x,y)             => format!("Org({x},{y})"),
        ASSOverride::Pbo(v)               => format!("Pbo({v})"),
        ASSOverride::Q(v)                 => format!("Q({v})"),
        ASSOverride::R(v)                 => format!("R({v:?})"),
        ASSOverride::K(v)                 => format!("K({v})"),
        ASSOverride::Kt(v)                => format!("Kt({v})"),
        ASSOverride::KSweep(v)            => format!("KSweep({v})"),
        ASSOverride::Kf(v)                => format!("Kf({v})"),
        ASSOverride::Ko(v)                => format!("Ko({v})"),
    }
}

pub fn fmt_overrides(v: &[ASSOverride]) -> String {
    v.iter().map(fmt_override).collect::<Vec<_>>().join(", ")
}
