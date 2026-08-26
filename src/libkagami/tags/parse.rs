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
///
/// Dispatch is on the first byte. Every candidate prefix is matched from
/// position 0, so a prefix can only match input that starts with its own first
/// byte — grouping by that byte and keeping each group's internal order is the
/// same parser as one flat chain, but `\b` no longer pays for the thirty-odd
/// comparisons that used to sit in front of it. Order *within* a group is
/// still load-bearing: longest prefix first (`fscx` before `fsc` before `fs`),
/// and the paren-guarded forms before the bare tag they'd otherwise shadow
/// (`pbo`/`pos` before `p`, `iclip` before `i`).
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
    macro_rules! try_int {
        ($prefix:literal, $variant:expr, $ty:ty) => {
            if let Some(rest) = s.strip_prefix($prefix) {
                let (val, rest2) = parse_f32_arg(rest);
                return Some(($variant(val as $ty), consumed!(rest2), false));
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
    macro_rules! try_paren_numbers {
        ($prefix:literal, $($count:literal => $build:expr),+ $(,)?) => {
            if let Some(rest) = s.strip_prefix($prefix) {
                if rest.starts_with('(') {
                    match parse_paren_numbers(rest) {
                        $(Some((n, after)) if n.len() == $count => {
                            #[allow(clippy::redundant_closure_call)]
                            return Some(($build(&n), consumed!(after), false));
                        })+
                        Some((_n, _after)) => return None,
                        None => return None,
                    }
                }
            }
        };
    }

    match *s.as_bytes().first()? {
        // ── \fn first, then the float tags, longest prefix first ────────────
        b'f' => {
            if let Some(rest) = s.strip_prefix("fn") {
                let (name, rest2) = parse_text_arg(rest);
                return Some((ASSOverride::Fn(name), consumed!(rest2), false));
            }
            try_f32!("fscx", ASSOverride::Fscx);
            try_f32!("fscy", ASSOverride::Fscy);
            try_f32!("fsc",  ASSOverride::Fsc);
            try_f32!("fsp",  ASSOverride::Fsp);
            try_f32!("frx",  ASSOverride::Frx);
            try_f32!("fry",  ASSOverride::Fry);
            try_f32!("frz",  ASSOverride::Frz);
            try_f32!("fax",  ASSOverride::Fax);
            try_f32!("fay",  ASSOverride::Fay);
            try_f32!("fe",   ASSOverride::Fe);
            try_f32!("fr",   ASSOverride::Frz);
            try_f32!("fs",   ASSOverride::Fs);
            try_paren_numbers!("fade",
                2 => |n: &[f32]| ASSOverride::Fad(n[0], n[1]),
                7 => |n: &[f32]| ASSOverride::Fade(n[0], n[1], n[2], n[3], n[4], n[5], n[6]),
            );
            try_paren_numbers!("fad",
                2 => |n: &[f32]| ASSOverride::Fad(n[0], n[1]),
                7 => |n: &[f32]| ASSOverride::Fade(n[0], n[1], n[2], n[3], n[4], n[5], n[6]),
            );
        }
        b'x' => {
            try_f32!("xbord", ASSOverride::Xbord);
            try_f32!("xshad", ASSOverride::Xshad);
        }
        b'y' => {
            try_f32!("ybord", ASSOverride::Ybord);
            try_f32!("yshad", ASSOverride::Yshad);
        }
        b'b' => {
            try_f32!("blur", ASSOverride::Blur);
            try_f32!("bord", ASSOverride::Bord);
            try_f32!("be",   ASSOverride::Be);
            try_flag!("b",   ASSOverride::Bold);
        }
        b's' => {
            try_f32!("shad", ASSOverride::Shad);
            try_flag!("s",   ASSOverride::Strikeout);
        }
        // ── \an before \alpha, both before bare \a ──────────────────────────
        b'a' => {
            try_int!("an", ASSOverride::An, u8);
            try_hex!("alpha", ASSOverride::Alpha);
            try_int!("a", ASSOverride::A, u8);
        }
        b'q' => {
            try_int!("q", ASSOverride::Q, u8);
        }
        // ── \r consumes until the next \, like \fn ──────────────────────────
        b'r' => {
            let rest = &s[1..];
            let end = rest.find('\\').unwrap_or(rest.len());
            let name = rest[..end].trim().to_string();
            let tag = ASSOverride::R(if name.is_empty() { None } else { Some(name) });
            return Some((tag, consumed!(&rest[end..]), false));
        }
        b'1' => {
            try_hex!("1a", ASSOverride::AlphaI);
            try_hex!("1c", ASSOverride::ColorI);
        }
        b'2' => {
            try_hex!("2a", ASSOverride::AlphaII);
            try_hex!("2c", ASSOverride::ColorII);
        }
        b'3' => {
            try_hex!("3a", ASSOverride::AlphaIII);
            try_hex!("3c", ASSOverride::ColorIII);
        }
        b'4' => {
            try_hex!("4a", ASSOverride::AlphaIV);
            try_hex!("4c", ASSOverride::ColorIV);
        }
        // ── karaoke — kt/ko/kf before bare k ────────────────────────────────
        b'k' => {
            try_int!("kt", ASSOverride::Kt, u32);
            try_int!("ko", ASSOverride::Ko, u32);
            try_int!("kf", ASSOverride::Kf, u32);
            try_int!("k",  ASSOverride::K,  u32);
        }
        b'K' => {
            try_int!("K", ASSOverride::KSweep, u32);
        }
        // ── \c is the primary colour alias; \clip is a different tag ────────
        b'c' => {
            if !s.starts_with("clip") {
                let (val, rest2) = parse_hex_val(&s[1..]);
                return Some((ASSOverride::ColorI(val), consumed!(rest2), false));
            }
            if let Some(rest) = s.strip_prefix("clip") {
                if rest.starts_with('(') {
                    match parse_clip_args(rest, false) {
                        Some((ov, after)) => return Some((ov, consumed!(after), false)),
                        None => return None,
                    }
                }
            }
        }
        b'm' => {
            try_paren_numbers!("move",
                4 => |n: &[f32]| ASSOverride::MoveI(n[0], n[1], n[2], n[3]),
                6 => |n: &[f32]| ASSOverride::MoveII(n[0], n[1], n[2], n[3], n[4], n[5]),
            );
        }
        b'o' => {
            try_paren_numbers!("org", 2 => |n: &[f32]| ASSOverride::Org(n[0], n[1]));
        }
        b'p' => {
            try_f32!("pbo", ASSOverride::Pbo);
            try_paren_numbers!("pos", 2 => |n: &[f32]| ASSOverride::Pos(n[0], n[1]));
            try_int!("p", ASSOverride::P, u8);
        }
        b't' => {
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
        }
        b'i' => {
            if let Some(rest) = s.strip_prefix("iclip") {
                if rest.starts_with('(') {
                    match parse_clip_args(rest, true) {
                        Some((ov, after)) => return Some((ov, consumed!(after), false)),
                        None => return None,
                    }
                }
            }
            try_flag!("i", ASSOverride::Italic);
        }
        b'u' => {
            try_flag!("u", ASSOverride::Underline);
        }
        _ => {}
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

    (result, false)
}
