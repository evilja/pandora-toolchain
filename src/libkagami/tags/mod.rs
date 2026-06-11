use std::collections::HashSet;
use std::mem::{discriminant, Discriminant};
use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::tags::parse::parse_override_block_content;
use crate::libkagami::tags::transform::{apply_same_tag_after_transform, transform_inner_tags};
use crate::libkagami::tags::state::{already_active, upsert_override, is_first_wins};
use crate::libkagami::tags::stringify::stringify_override;

pub mod parse;
pub mod stringify;
pub mod state;
pub mod transform;

pub enum ASSText {
    Override(ASSOverride),
    RawText(String),
}

pub struct ASSLine {
    pub current_overrides: Vec<ASSOverride>,
    pub data: Vec<ASSText>,
}

impl ASSLine {
    pub fn from_str_store(s: &str, start: Vec<ASSOverride>) -> Self {
        let mut data: Vec<ASSText> = Vec::new();
        let mut current_overrides: Vec<ASSOverride> = start.clone();
        let mut transformed_since_tag: HashSet<Discriminant<ASSOverride>> = HashSet::new();
        let preserve_boundaries = has_override_block_text(s);
        let mut raw_buf = String::new();

        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if !raw_buf.is_empty() {
                    data.push(ASSText::RawText(std::mem::take(&mut raw_buf)));
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
                        } else {
                            // named \r — can't resolve style here, just clear
                            current_overrides.clear();
                        }
                        transformed_since_tag.clear();
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    let tag_disc = discriminant(&tag);
                    if !preserve_boundaries && already_active(&current_overrides, &tag) && !transformed_since_tag.contains(&tag_disc) {
                        continue;
                    }
                    if is_first_wins(&tag) {
                        if let Some(existing) = current_overrides.iter().find(|c| discriminant(*c) == discriminant(&tag)) {
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
            data.push(ASSText::RawText(raw_buf));
        }

        Self { current_overrides, data }
    }
}

impl std::str::FromStr for ASSLine {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut data: Vec<ASSText> = Vec::new();
        let mut current_overrides: Vec<ASSOverride> = Vec::new();
        let mut transformed_since_tag: HashSet<Discriminant<ASSOverride>> = HashSet::new();
        let preserve_boundaries = has_override_block_text(s);
        let mut raw_buf = String::new();

        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if !raw_buf.is_empty() {
                    data.push(ASSText::RawText(std::mem::take(&mut raw_buf)));
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
                        transformed_since_tag.clear();
                        data.push(ASSText::Override(tag));
                        continue;
                    }
                    let tag_disc = discriminant(&tag);
                    if !preserve_boundaries && already_active(&current_overrides, &tag) && !transformed_since_tag.contains(&tag_disc) {
                        continue;
                    }
                    if is_first_wins(&tag) {
                        if current_overrides.iter().any(|c| discriminant(c) == discriminant(&tag)) {
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
            data.push(ASSText::RawText(raw_buf));
        }

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
            }
        }
        out
    }
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

fn has_override_block_text(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let Some(end) = find_block_end(bytes, i + 1) else {
            i += 1;
            continue;
        };
        let (tags, _) = parse_override_block_content(&s[i + 1..end]);
        if tags.iter().any(|tag| matches!(tag, ASSOverride::BlockText(text) if !text.is_empty())) {
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
    fn test_override_block_text_is_preserved() {
        let line: ASSLine = r"I {*\c&H8F889F&\3c&H8F889F&}l".parse().unwrap();
        assert_eq!(line.stringify(), r"I {*\c&H8F889F&\3c&H8F889F&}l");
    }

    #[test]
    fn test_override_block_text_disables_line_dedup() {
        let line: ASSLine = r"{*\c&HEEEEEE&}A{\c&HB2D5DE&}B{\c&HEEEEEE&}C{\c&HEEEEEE&}D".parse().unwrap();
        let light_count = line.data.iter()
            .filter(|t| matches!(t, ASSText::Override(ASSOverride::ColorI(0xEEEEEE))))
            .count();
        assert_eq!(light_count, 3);
        assert_eq!(
            line.stringify(),
            r"{*\c&HEEEEEE&}A{\c&HB2D5DE&}B{\c&HEEEEEE&}C{\c&HEEEEEE&}D"
        );
    }
}
