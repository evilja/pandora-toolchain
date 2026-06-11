pub fn take_parens(s: &str) -> Option<(&str, &str)> {
    debug_assert!(s.starts_with('('));
    let inner_start = 1;
    let mut depth = 1usize;
    for (i, c) in s[inner_start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let inner = &s[inner_start..inner_start + i];
                    let rest = &s[inner_start + i + 1..];
                    return Some((inner, rest));
                }
            }
            _ => {}
        }
    }
    None // unclosed
}

pub fn parse_parenthesized_args(s: &str) -> Option<(Vec<&str>, &str, bool)> {
    if !s.starts_with('(') {
        return None;
    }
    let mut args = Vec::new();
    let mut q = 1usize;
    let mut has_backslash_arg = false;

    loop {
        while q < s.len() && s.as_bytes()[q] == b' ' {
            q += 1;
        }

        let start = q;
        let mut r = q;
        while r < s.len() {
            match s.as_bytes()[r] {
                b',' | b'\\' | b')' => break,
                _ => r += 1,
            }
        }

        if r < s.len() && s.as_bytes()[r] == b',' {
            push_arg(&mut args, &s[start..r]);
            q = r + 1;
            continue;
        }

        if r < s.len() && s.as_bytes()[r] == b'\\' {
            has_backslash_arg = true;
            match s[r..].find(')') {
                Some(paren) => r += paren,
                None => r = s.len(),
            }
        }

        push_arg(&mut args, &s[start..r]);
        q = r;
        if q < s.len() {
            q += 1;
        }
        break;
    }

    Some((args, &s[q..], has_backslash_arg))
}

fn push_arg<'a>(args: &mut Vec<&'a str>, arg: &'a str) {
    let trimmed = arg.trim_end_matches(' ');
    if !trimmed.is_empty() {
        args.push(trimmed);
    }
}

/// Read a boolean-ish integer (0/1).
/// Invariant: uppercase first char → false (treat as 0).
pub fn parse_bool_val(s: &str) -> (bool, &str) {
    if s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        return (false, s);
    }
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return (true, s);
    }
    let val = s[..end].parse::<u32>().unwrap_or(0) != 0;
    (val, &s[end..])
}

/// Read one f32.
/// Invariant: uppercase first char → 0, skip to next tag.
/// Invariant: space-separated extra values after the first are dropped.
pub fn parse_f32_val(s: &str) -> (f32, &str) {
    let s = s.trim_start_matches(' ');

    if s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        let end = s.find('\\').unwrap_or(s.len());
        return (0.0, &s[end..]);
    }

    let end = s
        .find(|c: char| c != '-' && c != '+' && c != '.' && !c.is_ascii_digit())
        .unwrap_or(s.len());
    let val = s[..end].parse::<f32>().unwrap_or(0.0);

    let rest = &s[end..];
    let rest = skip_to_next_tag(rest);
    (val, rest)
}

pub fn parse_f32_prefix(s: &str) -> Option<f32> {
    let s = s.trim_start_matches(' ');
    let end = s
        .find(|c: char| c != '-' && c != '+' && c != '.' && !c.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    s[..end].parse::<f32>().ok()
}

/// After consuming a value, if there is non-backslash/non-brace content remaining
/// (e.g. a second space-separated number), skip past it to the next tag boundary.
pub fn skip_to_next_tag(s: &str) -> &str {
    let trimmed = s.trim_start_matches(' ');
    if trimmed.is_empty() || trimmed.starts_with('\\') || trimmed.starts_with('}') {
        return s;
    }
    s.find('\\').map(|i| &s[i..]).unwrap_or(&s[s.len()..])
}

pub fn parse_ass_int_prefix(s: &str) -> Option<(u32, usize)> {
    let s_trimmed = s.trim_start_matches(' ');
    let leading_spaces = s.len() - s_trimmed.len();

    if let Some(rest) = s_trimmed.strip_prefix('&') {
        let (digits, offset) = match rest.strip_prefix('H').or_else(|| rest.strip_prefix('h')) {
            Some(hex) => (hex, 2),
            None => (rest, 1),
        };
        let end = digits
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(digits.len());
        if end == 0 {
            return None;
        }
        let val = u32::from_str_radix(&digits[..end], 16).ok()?;
        let trailing = digits[end..].starts_with('&') as usize;
        return Some((val, leading_spaces + offset + end + trailing));
    }

    if let Some(rest) = s_trimmed.strip_prefix("0x").or_else(|| s_trimmed.strip_prefix("0X")) {
        let end = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        let val = u32::from_str_radix(&rest[..end], 16).ok()?;
        return Some((val, leading_spaces + 2 + end));
    }

    let end = s_trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(s_trimmed.len());
    if end == 0 {
        return None;
    }
    let val = s_trimmed[..end].parse::<u32>().ok()?;
    Some((val, leading_spaces + end))
}

pub fn parse_hex_val(s: &str) -> (u32, &str) {
    match parse_ass_int_prefix(s) {
        Some((val, consumed)) => (val, &s[consumed..]),
        None => (0, s),
    }
}

pub fn parse_csv_f32s(s: &str) -> Vec<f32> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<f32>().ok())
        .collect()
}
