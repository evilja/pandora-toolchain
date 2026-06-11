use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::complex::helpers::{parse_parenthesized_args, parse_f32_prefix};

pub fn parse_clip_args(rest: &str, is_iclip: bool) -> Option<(ASSOverride, &str)> {
    let (args, after, _has_backslash_arg) = parse_parenthesized_args(rest)?;
    match args.len() {
        1 => {
            let drawing = args[0].to_string();
            Some((if is_iclip {
                ASSOverride::IclipI(drawing)
            } else {
                ASSOverride::ClipI(drawing)
            }, after))
        }
        2 => {
            let scale = parse_f32_prefix(args[0]).unwrap_or(0.0);
            let drawing = args[1].to_string();
            Some((if is_iclip {
                ASSOverride::IclipII(scale, drawing)
            } else {
                ASSOverride::ClipII(scale, drawing)
            }, after))
        }
        4 => {
            let x0 = parse_f32_prefix(args[0]).unwrap_or(0.0);
            let y0 = parse_f32_prefix(args[1]).unwrap_or(0.0);
            let x1 = parse_f32_prefix(args[2]).unwrap_or(0.0);
            let y1 = parse_f32_prefix(args[3]).unwrap_or(0.0);
            Some((if is_iclip {
                ASSOverride::IclipRect(x0, y0, x1, y1)
            } else {
                ASSOverride::ClipRect(x0, y0, x1, y1)
            }, after))
        }
        _ => None,
    }
}
