use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::complex::helpers::{
    parse_bool_val, parse_f32_val, parse_hex_val,
    parse_parenthesized_args, parse_f32_prefix,
};
use crate::libkagami::complex::parse::parse_clip_args;

/// Parse the interior of \t(...).
/// Format: [\tag...] | [accel,\tag...] | [t1,t2,\tag...] | [t1,t2,accel,\tag...]
pub fn parse_transform(inner: &str) -> Option<ASSOverride> {
    let (args, _after, has_backslash_arg) = parse_parenthesized_args(inner)?;
    if !has_backslash_arg || args.is_empty() {
        return None;
    }

    let cnt = args.len().checked_sub(1)?;
    if cnt > 3 {
        return None;
    }

    let style_str = args[cnt];

    let (style_overrides, _) = parse_override_block_content(style_str);

    let ov = match cnt {
        0 => ASSOverride::TransformI(style_overrides),
        1 => ASSOverride::TransformII(parse_f32_prefix(args[0]).unwrap_or(0.0), style_overrides),
        2 => ASSOverride::TransformIII(
            parse_f32_prefix(args[0]).unwrap_or(0.0),
            parse_f32_prefix(args[1]).unwrap_or(0.0),
            style_overrides,
        ),
        _ => ASSOverride::TransformIV(
            parse_f32_prefix(args[0]).unwrap_or(0.0),
            parse_f32_prefix(args[1]).unwrap_or(0.0),
            parse_f32_prefix(args[2]).unwrap_or(0.0),
            style_overrides,
        ),
    };
    Some(ov)
}

fn first_arg<'a>(rest: &'a str) -> Option<(&'a str, &'a str)> {
    parse_parenthesized_args(rest)
        .map(|(args, after, _)| (args.first().copied().unwrap_or(""), after))
}

fn parse_f32_arg(rest: &str) -> (f32, &str) {
    match first_arg(rest) {
        Some((arg, after)) => (parse_f32_prefix(arg).unwrap_or(0.0), after),
        None => parse_f32_val(rest),
    }
}

fn parse_bool_arg(rest: &str) -> (bool, &str) {
    match first_arg(rest) {
        Some((arg, after)) => (parse_bool_val(arg).0, after),
        None => parse_bool_val(rest),
    }
}

fn parse_hex_arg(rest: &str) -> (u32, &str) {
    match first_arg(rest) {
        Some((arg, after)) => (parse_hex_val(arg).0, after),
        None => parse_hex_val(rest),
    }
}

fn parse_text_arg(rest: &str) -> (String, &str) {
    match first_arg(rest) {
        Some((arg, after)) => (arg.to_string(), after),
        None => {
            let end = rest.find('\\').unwrap_or(rest.len());
            (rest[..end].to_string(), &rest[end..])
        }
    }
}

fn parse_paren_numbers(rest: &str) -> Option<(Vec<f32>, &str)> {
    parse_parenthesized_args(rest)
        .map(|(args, after, _)| {
            let nums = args.iter()
                .map(|arg| parse_f32_prefix(arg).unwrap_or(0.0))
                .collect();
            (nums, after)
        })
}

fn empty_transform(ov: &ASSOverride) -> bool {
    matches!(ov,
        ASSOverride::TransformI(v)
        | ASSOverride::TransformII(_, v)
        | ASSOverride::TransformIII(_, _, v)
        | ASSOverride::TransformIV(_, _, _, v)
        if v.is_empty()
    )
}

/// Parse one override tag. `s` begins immediately after the leading backslash.
/// Returns (tag, bytes_consumed, is_malformed).
/// is_malformed = true means an unclosed paren was found; caller should drop
/// all subsequent tags in the block.
pub fn parse_one_tag(s: &str) -> Option<(ASSOverride, usize, bool)> {
    let orig_len = s.len();

    macro_rules! consumed {
        ($rest:expr) => { orig_len - $rest.len() };
    }
    macro_rules! try_flag {
        ($prefix:literal, $variant:expr) => {
            if let Some(rest) = s.strip_prefix($prefix) {
                let (val, rest2) = parse_bool_arg(rest);
                return Some(($variant(val), consumed!(rest2), false));
            }
        };
    }
    macro_rules! try_f32 {
        ($prefix:literal, $variant:expr) => {
            if let Some(rest) = s.strip_prefix($prefix) {
                let (val, rest2) = parse_f32_arg(rest);
                return Some(($variant(val), consumed!(rest2), false));
            }
        };
    }
    macro_rules! try_hex {
        ($prefix:literal, $ctor:expr) => {
            if let Some(rest) = s.strip_prefix($prefix) {
                let (val, rest2) = parse_hex_arg(rest);
                return Some(($ctor(val), consumed!(rest2), false));
            }
        };
    }

    // ── \fn — before anything else starting with 'f' ────────────────────────
    if let Some(rest) = s.strip_prefix("fn") {
        let (name, rest2) = parse_text_arg(rest);
        return Some((ASSOverride::Fn(name), consumed!(rest2), false));
    }

    // ── float tags — longest prefix first ───────────────────────────────────
    try_f32!("xbord", ASSOverride::Xbord);
    try_f32!("ybord", ASSOverride::Ybord);
    try_f32!("xshad", ASSOverride::Xshad);
    try_f32!("yshad", ASSOverride::Yshad);
    try_f32!("fscx",  ASSOverride::Fscx);
    try_f32!("fscy",  ASSOverride::Fscy);
    try_f32!("fsc",   ASSOverride::Fsc);
    try_f32!("fsp",   ASSOverride::Fsp);
    try_f32!("frx",   ASSOverride::Frx);
    try_f32!("fry",   ASSOverride::Fry);
    try_f32!("frz",   ASSOverride::Frz);
    try_f32!("fax",   ASSOverride::Fax);
    try_f32!("fay",   ASSOverride::Fay);
    try_f32!("fe",    ASSOverride::Fe);
    try_f32!("pbo",   ASSOverride::Pbo);
    try_f32!("fr",    ASSOverride::Frz);
    try_f32!("blur",  ASSOverride::Blur);
    try_f32!("bord",  ASSOverride::Bord);
    try_f32!("shad",  ASSOverride::Shad);
    try_f32!("be",    ASSOverride::Be);
    try_f32!("fs",    ASSOverride::Fs);

    // ── \an — before \alpha ──────────────────────────────────────────────────
    if let Some(rest) = s.strip_prefix("an") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::An(val as u8), consumed!(rest2), false));
    }

    // ── \q ───────────────────────────────────────────────────────────────────
    if let Some(rest) = s.strip_prefix("q") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::Q(val as u8), consumed!(rest2), false));
    }

    // ── \r — consumes until next \ like \fn ──────────────────────────────────
    if let Some(rest) = s.strip_prefix("r") {
        let end = rest.find('\\').unwrap_or(rest.len());
        let name = rest[..end].trim().to_string();
        let tag = ASSOverride::R(if name.is_empty() { None } else { Some(name) });
        return Some((tag, consumed!(&rest[end..]), false));
    }

    // ── alpha / color — longest prefix first ────────────────────────────────
    try_hex!("alpha", ASSOverride::Alpha);
    try_hex!("1a",    ASSOverride::AlphaI);
    try_hex!("2a",    ASSOverride::AlphaII);
    try_hex!("3a",    ASSOverride::AlphaIII);
    try_hex!("4a",    ASSOverride::AlphaIV);
    try_hex!("1c",    ASSOverride::ColorI);
    try_hex!("2c",    ASSOverride::ColorII);
    try_hex!("3c",    ASSOverride::ColorIII);
    try_hex!("4c",    ASSOverride::ColorIV);

    if let Some(rest) = s.strip_prefix("a") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::A(val as u8), consumed!(rest2), false));
    }

    // ── karaoke — ko/kf before k, K uppercase before k ───────────────────────
    if let Some(rest) = s.strip_prefix("kt") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::Kt(val as u32), consumed!(rest2), false));
    }
    if let Some(rest) = s.strip_prefix("ko") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::Ko(val as u32), consumed!(rest2), false));
    }
    if let Some(rest) = s.strip_prefix("kf") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::Kf(val as u32), consumed!(rest2), false));
    }
    if let Some(rest) = s.strip_prefix("K") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::KSweep(val as u32), consumed!(rest2), false));
    }
    if let Some(rest) = s.strip_prefix("k") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::K(val as u32), consumed!(rest2), false));
    }

    // ── \c — primary color alias, guard against "clip" ───────────────────────
    if s.starts_with('c') && !s.starts_with("clip") {
        let rest = &s[1..];
        let (val, rest2) = parse_hex_val(rest);
        return Some((ASSOverride::ColorI(val), consumed!(rest2), false));
    }

    // ── paren-based tags ─────────────────────────────────────────────────────

    if let Some(rest) = s.strip_prefix("fade") {
        if rest.starts_with('(') {
            match parse_paren_numbers(rest) {
                Some((n, after)) if n.len() == 2 => {
                    return Some((ASSOverride::Fad(n[0], n[1]), consumed!(after), false));
                }
                Some((n, after)) if n.len() == 7 => {
                    return Some((ASSOverride::Fade(n[0], n[1], n[2], n[3], n[4], n[5], n[6]), consumed!(after), false));
                }
                Some((_n, _after)) => return None,
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("move") {
        if rest.starts_with('(') {
            match parse_paren_numbers(rest) {
                Some((n, after)) if n.len() == 4 => {
                    return Some((ASSOverride::MoveI(n[0], n[1], n[2], n[3]), consumed!(after), false));
                }
                Some((n, after)) if n.len() == 6 => {
                    return Some((ASSOverride::MoveII(n[0], n[1], n[2], n[3], n[4], n[5]), consumed!(after), false));
                }
                Some((_n, _after)) => return None,
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("org") {
        if rest.starts_with('(') {
            match parse_paren_numbers(rest) {
                Some((n, after)) if n.len() == 2 => {
                    return Some((ASSOverride::Org(n[0], n[1]), consumed!(after), false));
                }
                Some((_n, _after)) => return None,
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("fad") {
        if rest.starts_with('(') {
            match parse_paren_numbers(rest) {
                Some((n, after)) if n.len() == 2 => {
                    return Some((ASSOverride::Fad(n[0], n[1]), consumed!(after), false));
                }
                Some((n, after)) if n.len() == 7 => {
                    return Some((ASSOverride::Fade(n[0], n[1], n[2], n[3], n[4], n[5], n[6]), consumed!(after), false));
                }
                Some((_n, _after)) => return None,
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("pos") {
        if rest.starts_with('(') {
            match parse_paren_numbers(rest) {
                Some((n, after)) if n.len() == 2 => {
                    return Some((ASSOverride::Pos(n[0], n[1]), consumed!(after), false));
                }
                Some((_n, _after)) => return None,
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("t") {
        if rest.starts_with('(') {
            match parse_parenthesized_args(rest) {
                Some((_args, after, _has_backslash_arg)) => {
                    let transform_source = &rest[..rest.len() - after.len()];
                    let ov = parse_transform(transform_source).unwrap_or_else(|| ASSOverride::TransformI(Vec::new()));
                    return Some((ov, consumed!(after), false));
                }
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("iclip") {
        if rest.starts_with('(') {
            match parse_clip_args(rest, true) {
                Some((ov, after)) => return Some((ov, consumed!(after), false)),
                None => return None,
            }
        }
    }

    if let Some(rest) = s.strip_prefix("clip") {
        if rest.starts_with('(') {
            match parse_clip_args(rest, false) {
                Some((ov, after)) => return Some((ov, consumed!(after), false)),
                None => return None,
            }
        }
    }

    // ── single-char / short tags — LAST ──────────────────────────────────────
    try_flag!("b", ASSOverride::Bold);
    try_flag!("i", ASSOverride::Italic);
    try_flag!("u", ASSOverride::Underline);
    try_flag!("s", ASSOverride::Strikeout);

    if let Some(rest) = s.strip_prefix("p") {
        let (val, rest2) = parse_f32_arg(rest);
        return Some((ASSOverride::P(val as u8), consumed!(rest2), false));
    }

    None
}

/// Parse the content of a `{...}` block (without the surrounding braces).
/// Returns (tags, malformed) where malformed = true means an unclosed paren
/// was encountered and remaining tags were dropped.
pub fn parse_override_block_content(mut s: &str) -> (Vec<ASSOverride>, bool) {
    let mut result = Vec::new();

    loop {
        let bs = match s.find('\\') {
            Some(i) => i,
            None => break,
        };
        if bs > 0 {
            result.push(ASSOverride::BlockText(s[..bs].to_string()));
        }
        s = &s[bs + 1..];

        if s.is_empty() {
            break;
        }

        match parse_one_tag(s) {
            None => {
                let next = s.find('\\').unwrap_or(s.len());
                s = &s[next..];
            }
            Some((tag, consumed, is_malformed)) => {
                if empty_transform(&tag) {
                    s = &s[consumed..];
                    continue;
                }
                result.push(tag);
                if is_malformed {
                    return (result, true);
                }
                s = &s[consumed..];
            }
        }
    }

    if !s.is_empty() {
        result.push(ASSOverride::BlockText(s.to_string()));
    }

    (result, false)
}
