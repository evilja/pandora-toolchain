use std::collections::HashSet;
use std::mem::{discriminant, Discriminant};
use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::drawing::parse::Drawing;
use crate::libkagami::tags::parse::parse_override_block_content;
use crate::libkagami::tags::transform::{apply_same_tag_after_transform, transform_inner_tags};
use crate::libkagami::tags::state::{already_active, upsert_override, is_first_wins, same_override_kind, is_repeatable_effect};
use crate::libkagami::tags::stringify::stringify_override;

pub mod parse;
pub mod stringify;
pub mod state;
pub mod transform;

#[derive(Clone)]
pub enum ASSText {
    Override(ASSOverride),
    Drawing(Drawing),
    RawText(String),
}

#[derive(Clone)]
pub struct ASSLine {
    pub current_overrides: Vec<ASSOverride>,
    pub data: Vec<ASSText>,
}

impl ASSLine {
    pub fn from_str_store(s: &str, start: Vec<ASSOverride>) -> Self {
        if has_star_in_override_block(s) {
            return Self { current_overrides: start, data: vec![ASSText::RawText(s.to_string())] };
        }

        let mut data: Vec<ASSText> = Vec::new();
        let mut current_overrides: Vec<ASSOverride> = start.clone();
        let mut transformed_since_tag: HashSet<Discriminant<ASSOverride>> = HashSet::new();
        let mut raw_buf = String::new();
        let mut drawing_mode = start.iter()
            .rev()
            .find_map(|ov| if let ASSOverride::P(v) = ov { Some(*v) } else { None })
            .unwrap_or(0);

        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if !raw_buf.is_empty() {
                    push_text(&mut data, std::mem::take(&mut raw_buf), drawing_mode);
                }

                if find_block_end(bytes, i + 1).is_none() {
                    raw_buf.push('{');
                    i += 1;
                    continue;
                }

                let block_start = i + 1;
                let block_end = find_block_end(bytes, block_start).unwrap();

                let block_content = &s[block_start..block_end];
                let (tags, _) = parse_override_block_content(block_content);
                let tags = apply_same_tag_after_transform(tags);

                for tag in tags {
                    if matches!(&tag, ASSOverride::BlockText(_)) {
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    mark_transform_tags(&tag, &mut transformed_since_tag);
                    if let ASSOverride::R(ref name) = tag {
                        if name.is_none() {
                            // bare \r — reset to style baseline
                            current_overrides = start.clone();
                            drawing_mode = start.iter()
                                .rev()
                                .find_map(|ov| if let ASSOverride::P(v) = ov { Some(*v) } else { None })
                                .unwrap_or(0);
                        } else {
                            // named \r — can't resolve style here, just clear
                            current_overrides.clear();
                            drawing_mode = 0;
                        }
                        transformed_since_tag.clear();
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    if let ASSOverride::P(v) = &tag {
                        drawing_mode = *v;
                    }
                    let tag_disc = discriminant(&tag);
                    if is_repeatable_effect(&tag) {
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    if already_active(&current_overrides, &tag) && !transformed_since_tag.contains(&tag_disc) {
                        continue;
                    }
                    if is_first_wins(&tag) {
                        if let Some(existing) = current_overrides.iter().find(|c| same_override_kind(c, &tag)) {
                            // suppress only if the existing value came from an explicit tag, not the style base
                            if !start.iter().any(|s| s == existing) {
                                continue;
                            }
                        }
                    }
                    upsert_override(&mut current_overrides, tag.clone());
                    transformed_since_tag.remove(&tag_disc);
                    data.push(ASSText::Override(tag));
                }

                i = block_end + 1;
            } else {
                if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1] == b'{' || bytes[i + 1] == b'}') {
                    raw_buf.push('\\');
                    raw_buf.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                raw_buf.push_str(&s[i..i + ch_len]);
                i += ch_len;
            }
        }

        if !raw_buf.is_empty() {
            push_text(&mut data, raw_buf, drawing_mode);
        }

        trim_tags_without_text_after(&mut data);
        current_overrides = final_overrides(&data, &start);

        Self { current_overrides, data }
    }
}

impl std::str::FromStr for ASSLine {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if has_star_in_override_block(s) {
            return Ok(Self { current_overrides: Vec::new(), data: vec![ASSText::RawText(s.to_string())] });
        }

        let mut data: Vec<ASSText> = Vec::new();
        let mut current_overrides: Vec<ASSOverride> = Vec::new();
        let mut transformed_since_tag: HashSet<Discriminant<ASSOverride>> = HashSet::new();
        let mut raw_buf = String::new();
        let mut drawing_mode = 0;

        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if !raw_buf.is_empty() {
                    push_text(&mut data, std::mem::take(&mut raw_buf), drawing_mode);
                }

                if find_block_end(bytes, i + 1).is_none() {
                    raw_buf.push('{');
                    i += 1;
                    continue;
                }

                let block_start = i + 1;
                let block_end = find_block_end(bytes, block_start).unwrap();

                let block_content = &s[block_start..block_end];
                let (tags, _) = parse_override_block_content(block_content);
                let tags = apply_same_tag_after_transform(tags);

                for tag in tags {
                    if matches!(&tag, ASSOverride::BlockText(_)) {
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    mark_transform_tags(&tag, &mut transformed_since_tag);
                    if matches!(&tag, ASSOverride::R(_)) {
                        current_overrides.clear();
                        drawing_mode = 0;
                        transformed_since_tag.clear();
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    if let ASSOverride::P(v) = &tag {
                        drawing_mode = *v;
                    }
                    let tag_disc = discriminant(&tag);
                    if is_repeatable_effect(&tag) {
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    if already_active(&current_overrides, &tag) && !transformed_since_tag.contains(&tag_disc) {
                        continue;
                    }
                    if is_first_wins(&tag) {
                        if current_overrides.iter().any(|c| same_override_kind(c, &tag)) {
                            continue;
                        }
                    }
                    upsert_override(&mut current_overrides, tag.clone());
                    transformed_since_tag.remove(&tag_disc);
                    data.push(ASSText::Override(tag));
                }

                i = block_end + 1;
            } else {
                if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1] == b'{' || bytes[i + 1] == b'}') {
                    raw_buf.push('\\');
                    raw_buf.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                raw_buf.push_str(&s[i..i + ch_len]);
                i += ch_len;
            }
        }

        if !raw_buf.is_empty() {
            push_text(&mut data, raw_buf, drawing_mode);
        }

        trim_tags_without_text_after(&mut data);
        current_overrides = final_overrides(&data, &[]);

        Ok(Self { current_overrides, data })
    }
}

impl ASSLine {
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < self.data.len() {
            if matches!(self.data[i], ASSText::Override(_)) {
                out.push('{');
                while i < self.data.len() {
                    if let ASSText::Override(ov) = &self.data[i] {
                        if !matches!(ov, ASSOverride::BlockText(_)) {
                            out.push('\\');
                        }
                        out.push_str(&stringify_override(ov));
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push('}');
            } else if let ASSText::RawText(t) = &self.data[i] {
                out.push_str(t);
                i += 1;
            } else if let ASSText::Drawing(d) = &self.data[i] {
                out.push_str(&d.stringify());
                i += 1;
            }
        }
        out
    }

    // Explicit tags inside raw fallback blocks intentionally report no signals.
    pub fn has_fn(&self) -> bool {
        self.data.iter().any(|item| match item {
            ASSText::Override(ov) => override_has_fn(ov),
            _ => false,
        })
    }

    // Parsed drawings and positive drawing modes both count as vector work.
    pub fn has_drawing(&self) -> bool {
        self.data.iter().any(|item| match item {
            ASSText::Drawing(_) => true,
            ASSText::Override(ov) => override_has_drawing(ov),
            ASSText::RawText(_) => false,
        })
    }

    // A transform counts once itself and once for each recursively nested tag.
    pub fn tag_count(&self) -> usize {
        self.data.iter().map(|item| match item {
            ASSText::Override(ov) => override_tag_count(ov),
            _ => 0,
        }).sum()
    }
}

fn transform_tags(ov: &ASSOverride) -> Option<&[ASSOverride]> {
    match ov {
        ASSOverride::TransformI(tags) => Some(tags),
        ASSOverride::TransformII(_, tags) => Some(tags),
        ASSOverride::TransformIII(_, _, tags) => Some(tags),
        ASSOverride::TransformIV(_, _, _, tags) => Some(tags),
        _ => None,
    }
}

fn override_has_fn(ov: &ASSOverride) -> bool {
    matches!(ov, ASSOverride::Fn(_))
        || transform_tags(ov)
            .map(|tags| tags.iter().any(override_has_fn))
            .unwrap_or(false)
}

fn override_has_drawing(ov: &ASSOverride) -> bool {
    matches!(ov, ASSOverride::P(value) if *value > 0)
        || transform_tags(ov)
            .map(|tags| tags.iter().any(override_has_drawing))
            .unwrap_or(false)
}

fn override_tag_count(ov: &ASSOverride) -> usize {
    1 + transform_tags(ov)
        .map(|tags| tags.iter().map(override_tag_count).sum::<usize>())
        .unwrap_or(0)
}

fn push_text(data: &mut Vec<ASSText>, text: String, drawing_mode: u8) {
    if drawing_mode > 0 {
        let drawing: Drawing = text.parse().unwrap();
        if !drawing.commands.is_empty() {
            data.push(ASSText::Drawing(drawing));
            return;
        }
    }
    data.push(ASSText::RawText(text));
}

fn find_block_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() {
        if bytes[j] == b'\\' && j + 1 < bytes.len() && bytes[j + 1] == b'{' {
            j += 2;
            continue;
        }
        if bytes[j] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn mark_transform_tags(ov: &ASSOverride, transformed: &mut HashSet<Discriminant<ASSOverride>>) {
    if let Some(tags) = transform_inner_tags(ov) {
        for tag in tags {
            transformed.insert(discriminant(tag));
        }
    }
}

fn trim_tags_without_text_after(data: &mut Vec<ASSText>) {
    let Some(last_text) = data.iter().rposition(|item| matches!(item, ASSText::RawText(_) | ASSText::Drawing(_))) else {
        data.clear();
        return;
    };
    data.truncate(last_text + 1);
}

fn final_overrides(data: &[ASSText], start: &[ASSOverride]) -> Vec<ASSOverride> {
    let mut current = start.to_vec();
    for item in data {
        let ASSText::Override(ov) = item else {
            continue;
        };
        match ov {
            ASSOverride::BlockText(_) => {}
            ASSOverride::R(None) => current = start.to_vec(),
            ASSOverride::R(Some(_)) => current.clear(),
            _ if is_repeatable_effect(ov) => {}
            _ => upsert_override(&mut current, ov.clone()),
        }
    }
    current
}

fn has_star_in_override_block(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            i += 2;
            continue;
        }
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let Some(end) = find_block_end(bytes, i + 1) else {
            i += 1;
            continue;
        };
        if s[i + 1..end].contains('*') {
            return true;
        }
        i = end + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libkagami::tags::stringify::fmt_override;

    fn print_line(label: &str, line: &ASSLine) {
        println!("\n── {label} ──");
        for item in &line.data {
            match item {
                ASSText::RawText(t) => println!("  RawText({t:?})"),
                ASSText::Override(ov) => println!("  Override({})", fmt_override(ov)),
                ASSText::Drawing(d) => println!("  Drawing({})", d.stringify()),
            }
        }
        println!("  current_overrides: [{}]",
            line.current_overrides.iter().map(fmt_override).collect::<Vec<_>>().join(", "));
    }

    #[test]
    fn test_same_value_deduped() {
        let line: ASSLine = r"{\b1}Hello{\b1}World".parse().unwrap();
        print_line("same_value_deduped", &line);
        let bold_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true))))
            .count();
        assert_eq!(bold_count, 1);
    }

    #[test]
    fn test_different_value_not_deduped() {
        let line: ASSLine = r"{\b1}Hello{\b0}World".parse().unwrap();
        print_line("different_value_not_deduped", &line);
        let bold_on  = line.data.iter().filter(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true)))).count();
        let bold_off = line.data.iter().filter(|t| matches!(t, ASSText::Override(ASSOverride::Bold(false)))).count();
        assert_eq!(bold_on,  1);
        assert_eq!(bold_off, 1);
    }

    #[test]
    fn test_multiple_tags_same_block_deduped() {
        let line: ASSLine = r"{\fs50\fs50}Hello".parse().unwrap();
        print_line("multiple_tags_same_block_deduped", &line);
        let fs_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::Fs(_))))
            .count();
        assert_eq!(fs_count, 1);
    }

    #[test]
    fn test_same_tag_different_blocks_deduped() {
        let line: ASSLine = r"{\pos(100,200)}Hello{\pos(100,200)}World".parse().unwrap();
        print_line("same_tag_different_blocks_deduped", &line);
        let pos_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::Pos(_, _))))
            .count();
        assert_eq!(pos_count, 1);
    }

    #[test]
    fn test_same_tag_different_value_not_deduped() {
        let line: ASSLine = r"{\pos(100,200)}Hello{\pos(300,400)}World".parse().unwrap();
        print_line("same_tag_different_value_not_deduped", &line);
        let pos_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::Pos(_, _))))
            .count();
        assert_eq!(pos_count, 1);
    }

    #[test]
    fn test_transform_dropped_when_raw_tag_follows_same_variant() {
        let line: ASSLine = r"{\t(1,100,\bord15)\bord3}Hello".parse().unwrap();
        print_line("transform_dropped_raw_tag_follows", &line);
        let has_transform = line.data.iter()
            .any(|t| matches!(t, ASSText::Override(ASSOverride::TransformIII(_, _, _))));
        let bord_val = line.data.iter()
            .find_map(|t| if let ASSText::Override(ASSOverride::Bord(v)) = t { Some(*v) } else { None });
        assert!(!has_transform);
        assert_eq!(bord_val, Some(3.0));
    }

    #[test]
    fn test_transform_kept_when_no_raw_tag_follows() {
        let line: ASSLine = r"{\t(1,100,\bord15)}Hello".parse().unwrap();
        print_line("transform_kept_no_raw_tag_follows", &line);
        let has_transform = line.data.iter()
            .any(|t| matches!(t, ASSText::Override(ASSOverride::TransformIII(_, _, _))));
        assert!(has_transform);
    }

    #[test]
    fn test_transform_multi_tag_partial_drop() {
        let line: ASSLine = r"{\t(1,100,\bord15\fs20)\t(1,100,\frz90)\fs50}Hello".parse().unwrap();
        print_line("transform_multi_tag_partial_drop", &line);
        let transform_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::TransformIII(_, _, _))))
            .count();
        assert_eq!(transform_count, 2);
        let fs_val = line.data.iter()
            .find_map(|t| if let ASSText::Override(ASSOverride::Fs(v)) = t { Some(*v) } else { None });
        assert_eq!(fs_val, Some(50.0));
    }

    #[test]
    fn test_current_overrides_reflects_final_state() {
        let line: ASSLine = r"{\fs50}Hello{\fs80}World".parse().unwrap();
        print_line("current_overrides_final_state", &line);
        let fs = line.current_overrides.iter()
            .find_map(|o| if let ASSOverride::Fs(v) = o { Some(*v) } else { None });
        assert_eq!(fs, Some(80.0));
        let fs_count = line.current_overrides.iter()
            .filter(|o| matches!(o, ASSOverride::Fs(_)))
            .count();
        assert_eq!(fs_count, 1);
    }

    #[test]
    fn test_raw_braces_are_escaped_on_stringify() {
        let line: ASSLine = r"\{literal\}".parse().unwrap();
        assert_eq!(line.stringify(), r"\{literal\}");
    }

    #[test]
    fn test_raw_unescaped_braces_are_not_escaped_on_stringify() {
        let line = ASSLine {
            current_overrides: Vec::new(),
            data: vec![ASSText::RawText(r"{\b1}raw".to_string())],
        };
        assert_eq!(line.stringify(), r"{\b1}raw");
    }

    #[test]
    fn test_drawing_mode_can_change_later() {
        let line: ASSLine = r"{\p1}m 0 0 l 10 10{\p0}text".parse().unwrap();
        let p_values: Vec<u8> = line.data.iter()
            .filter_map(|t| if let ASSText::Override(ASSOverride::P(v)) = t { Some(*v) } else { None })
            .collect();
        assert_eq!(p_values, vec![1, 0]);
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Drawing(_))));
        assert_eq!(line.stringify(), r"{\p1}m 0 0 l 10 10{\p0}text");
    }

    #[test]
    fn test_nested_block_keeps_tags_before_first_close() {
        let line: ASSLine = r"{\b1{oops}\i1}text".parse().unwrap();
        let bold_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true))))
            .count();
        assert_eq!(bold_count, 1);
        assert!(line.data.iter().any(|t| matches!(t, ASSText::RawText(s) if s == r"\i1}text")));
    }

    #[test]
    fn test_missing_closing_paren_is_tolerated() {
        let line: ASSLine = r"{\pos(10,20\b1}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Pos(10.0, 20.0)))));
        assert!(!line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true)))));
    }

    #[test]
    fn test_nested_parens_close_at_first_paren() {
        let line: ASSLine = r"{\pos(10,(20),30)\b1}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Pos(10.0, 0.0)))));
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true)))));
    }

    #[test]
    fn test_clip_rect_parses_four_args() {
        let line: ASSLine = r"{\clip(0,1,100,101)}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::ClipRect(0.0, 1.0, 100.0, 101.0)))));
    }

    #[test]
    fn test_parenthesized_simple_tag_arg() {
        let line: ASSLine = r"{\fs(42)}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Fs(42.0)))));
    }

    #[test]
    fn test_legacy_alignment_tag_is_preserved() {
        let line: ASSLine = r"{\a5}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::A(5)))));
        assert_eq!(line.stringify(), r"{\a5}text");
    }

    #[test]
    fn test_legacy_alignment_conflicts_with_an() {
        let line: ASSLine = r"{\a5\an7}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::A(5)))));
        assert!(!line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::An(7)))));
        assert_eq!(line.stringify(), r"{\a5}text");
    }

    #[test]
    fn test_kt_tag_is_preserved() {
        let line: ASSLine = r"{\kt30}text".parse().unwrap();
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Kt(30)))));
        assert_eq!(line.stringify(), r"{\kt30}text");
    }

    #[test]
    fn test_repeated_karaoke_tags_are_preserved() {
        let line: ASSLine = r"{\k20}a{\k20}b{\kt30\kf20}c".parse().unwrap();
        let k_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::K(20))))
            .count();
        assert_eq!(k_count, 2);
        assert_eq!(line.stringify(), r"{\k20}a{\k20}b{\kt30\kf20}c");
    }

    #[test]
    fn test_raw_tag_after_transform_affecting_same_property_is_kept() {
        let line: ASSLine = r"{\c&HFFFFFF&\t(0,2500,\c&H5F5FFF&)}A{\c&HFFFFFF&}B".parse().unwrap();
        let white_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::ColorI(0xFFFFFF))))
            .count();
        assert_eq!(white_count, 2);
        assert_eq!(
            line.stringify(),
            r"{\c&HFFFFFF&\t(0,2500,\c&H5F5FFF&)}A{\c&HFFFFFF&}B"
        );
    }

    #[test]
    fn test_same_raw_tag_dedupes_after_transform_reset() {
        let line: ASSLine = r"{\c&HFFFFFF&\t(0,2500,\c&H5F5FFF&)}A{\c&HFFFFFF&}B{\c&HFFFFFF&\t(0,2500,\c&H5F5FFF&)}C".parse().unwrap();
        let white_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::ColorI(0xFFFFFF))))
            .count();
        assert_eq!(white_count, 2);
        assert_eq!(
            line.stringify(),
            r"{\c&HFFFFFF&\t(0,2500,\c&H5F5FFF&)}A{\c&HFFFFFF&}B{\t(0,2500,\c&H5F5FFF&)}C"
        );
    }

    #[test]
    fn test_star_in_override_block_preserves_line() {
        let line: ASSLine = r"I {*\c&H8F889F&\3c&H8F889F&}l".parse().unwrap();
        assert_eq!(line.stringify(), r"I {*\c&H8F889F&\3c&H8F889F&}l");
        assert!(matches!(line.data.as_slice(), [ASSText::RawText(s)] if s == r"I {*\c&H8F889F&\3c&H8F889F&}l"));
    }

    #[test]
    fn test_non_tag_override_block_text_is_discarded() {
        let line: ASSLine = r"{x\c&HEEEEEE&}A{\c&HB2D5DE&}B{\c&HEEEEEE&}C{\c&HEEEEEE&}D".parse().unwrap();
        let light_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::ColorI(0xEEEEEE))))
            .count();
        assert_eq!(light_count, 2);
        assert_eq!(
            line.stringify(),
            r"{\c&HEEEEEE&}A{\c&HB2D5DE&}B{\c&HEEEEEE&}CD"
        );
    }

    #[test]
    fn test_trailing_tags_without_text_are_removed() {
        let line: ASSLine = r"{\b1}Hello{\i1}{\fs40}".parse().unwrap();
        assert_eq!(line.stringify(), r"{\b1}Hello");
        assert!(line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Bold(true)))));
        assert!(!line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Italic(true)))));
        assert!(!line.data.iter().any(|t| matches!(t, ASSText::Override(ASSOverride::Fs(40.0)))));
        assert!(!line.current_overrides.iter().any(|t| matches!(t, ASSOverride::Italic(true))));
        assert!(!line.current_overrides.iter().any(|t| matches!(t, ASSOverride::Fs(40.0))));
    }

    #[test]
    fn test_only_tags_are_removed() {
        let line: ASSLine = r"{\b1}{\fs40}".parse().unwrap();
        assert_eq!(line.stringify(), "");
        assert!(line.data.is_empty());
        assert!(line.current_overrides.is_empty());
    }

    #[test]
    fn test_trailing_tags_reset_to_style_baseline_after_store() {
        let line = ASSLine::from_str_store(
            r"{\fs80}Hello{\fnTrailing}",
            vec![ASSOverride::Fn("DefaultFont".to_string()), ASSOverride::Fs(20.0)],
        );
        assert_eq!(line.stringify(), r"{\fs80}Hello");
        assert!(line.current_overrides.iter().any(|t| matches!(t, ASSOverride::Fn(name) if name == "DefaultFont")));
        assert!(line.current_overrides.iter().any(|t| matches!(t, ASSOverride::Fs(80.0))));
        assert!(!line.current_overrides.iter().any(|t| matches!(t, ASSOverride::Fn(name) if name == "Trailing")));
    }

    #[test]
    fn structured_weight_counts_explicit_tags_only() {
        let line: ASSLine = r"{\pos(10,20)\fs40}text".parse().unwrap();
        assert_eq!(line.tag_count(), 2);
        assert!(!line.has_fn());

        let transformed: ASSLine = r"{\t(\fs40\1c&HFFFFFF&)}text".parse().unwrap();
        assert_eq!(transformed.tag_count(), 3);

        let plain: ASSLine = r"foo\Nbar\hbaz".parse().unwrap();
        assert_eq!(plain.tag_count(), 0);
        assert!(!plain.has_fn());
        assert!(!plain.has_drawing());
    }

    #[test]
    fn structured_weight_detects_drawings_and_transformed_fonts() {
        let drawing: ASSLine = r"{\p1}m 0 0 l 10 0 10 10{\p0}".parse().unwrap();
        assert!(drawing.has_drawing());

        let transformed_fn: ASSLine = r"{\t(\fnTypeset Font)}text".parse().unwrap();
        assert!(transformed_fn.has_fn());
        assert_eq!(transformed_fn.tag_count(), 2);
    }

    #[test]
    fn structured_weight_raw_fallback_has_no_signals() {
        let line: ASSLine = r"I {*\fnIgnored\p1\fs40}m 0 0 l 10 10".parse().unwrap();

        assert!(!line.has_fn());
        assert!(!line.has_drawing());
        assert_eq!(line.tag_count(), 0);
    }
}
