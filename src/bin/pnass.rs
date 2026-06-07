use clap::Parser;
use pandora_toolchain::libkagami::core::{ScriptInfo, SubstationAlpha};
use pandora_toolchain::libkagami::complex::overrides::ASSOverride;
use pandora_toolchain::libkagami::tags::{ASSLine, ASSText};
use pandora_toolchain::libpnprotocol::core::{Protocol, Schema, ToolInfo};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "pnass",
    version = "0.1.1",
    about = "Pandora Toolchain ASS standardiser",
    long_about = None
)]
struct Args {
    #[arg(long)]
    input: String,

    #[arg(long)]
    output: String,

    #[arg(long)]
    merge: Option<String>,

    #[arg(long)]
    set_layer: Option<u16>,

    #[arg(long)]
    title: Option<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(
        ToolInfo {
            tool: args.negotiator.as_deref().unwrap_or("PNass"),
            build: args.negver.as_deref().unwrap_or("0.1.1"),
            proto: 1,
        },
        ToolInfo { tool: "PNass", build: "0.1.1", proto: 1 },
        args.negkey.clone().unwrap_or_else(|| "PNassCLI".to_string()),
    );

    let mut sub = SubstationAlpha::load(PathBuf::from(&args.input), true).await;

    if let Some(t) = args.title {
        sub.script_info.title = t;
    }
    fill_script_info_defaults(&mut sub.script_info);

    if let Some(n) = args.set_layer {
        for ev in &mut sub.events {
            ev.layer = n;
        }
    }

    if let Some(merge_path) = args.merge.as_deref() {
        let mut secondary = SubstationAlpha::load(PathBuf::from(merge_path), false).await;
        migrate_styles(&mut sub);
        migrate_styles(&mut secondary);
        append_sub(&mut sub, secondary);
    }

    let mut run_count: usize = 0;
    for (i, ev) in sub.events.iter().enumerate() {
        let is_drawing = ev.text.data.iter().any(|item| matches!(item, ASSText::Override(ASSOverride::P(1))));
        if is_drawing {
            continue;
        }
        let lines = visible_lines(&ev.text);
        let has_warning = lines.iter().any(|l| l.chars().count() > 50);
        if has_warning {
            if run_count == 0 {
                for line in lines.iter().filter(|l| l.chars().count() > 50) {
                    let prefixed = format!("{}: {}", i + 1, line);
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf], data = ["4", prefixed]).unwrap());
                }
            }
            run_count += 1;
        } else if run_count > 1 {
            let more = format!("{} more similar warnings", run_count - 1);
            println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                schema = [leaf, leaf], data = ["4", more]).unwrap());
            run_count = 0;
        } else {
            run_count = 0;
        }
    }
    if run_count > 1 {
        let more = format!("{} more similar warnings", run_count - 1);
        println!("{}", pn_emit!(protocol = proto, negkey = &neg,
            schema = [leaf, leaf], data = ["4", more]).unwrap());
    }

    if sub.dump_to_file(PathBuf::from(&args.output)).await.is_err() {
        eprintln!("pnass: failed to write {}", args.output);
        std::process::exit(1);
    }
}

fn visible_lines(line: &ASSLine) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for item in &line.data {
        if let ASSText::RawText(t) = item {
            let chars: Vec<char> = t.chars().collect();
            let mut k = 0;
            while k < chars.len() {
                if chars[k] == '\\' && k + 1 < chars.len() && chars[k + 1] == 'N' {
                    lines.push(std::mem::take(&mut current));
                    k += 2;
                    continue;
                }
                current.push(chars[k]);
                k += 1;
            }
        }
    }
    lines.push(current);
    lines
}

fn fill_script_info_defaults(si: &mut ScriptInfo) {
    if si.script_type.is_empty() {
        si.script_type = "v4.00+".to_string();
    }
    if si.wrap_style != 2 {
        si.wrap_style = 2;
    }
    if !si.scaled_border_and_shadow {
        si.scaled_border_and_shadow = true;
    }
    if si.playresx == 0 {
        si.playresx = 1920;
    }
    if si.playresy == 0 {
        si.playresy = 1080;
    }
    if si.ycbcr_matrix.is_empty() {
        si.ycbcr_matrix = "TV.709".to_string();
    }
    if si.layout_res_x == 0 {
        si.layout_res_x = si.playresx;
    }
    if si.layout_res_y == 0 {
        si.layout_res_y = si.playresy;
    }
}

fn random_suffix() -> String {
    const ALPH: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut state: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    if state == 0 {
        state = 0x9E3779B97F4A7C15;
    }
    let mut out = String::with_capacity(10);
    for _ in 0..10 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(ALPH[(state as usize) % ALPH.len()] as char);
    }
    out
}

fn migrate_styles(sub: &mut SubstationAlpha) {
    use std::collections::HashSet;

    let mut referenced: HashSet<String> = HashSet::new();
    for ev in &sub.events {
        if !ev.style.is_empty() {
            referenced.insert(ev.style.clone());
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut mapping: Vec<(String, String, pandora_toolchain::libkagami::core::V4pStyle)> = Vec::new();

    for style in &sub.v4p_styles {
        if !referenced.contains(&style.name) && seen.contains(&style.name) {
            continue;
        }
        if seen.contains(&style.name) {
            continue;
        }
        let new_name = format!("pn-{}", random_suffix());
        seen.insert(style.name.clone());
        mapping.push((style.name.clone(), new_name, pandora_toolchain::libkagami::core::V4pStyle {
            name: String::new(),
            fontname: style.fontname.clone(),
            fontsize: style.fontsize,
            colours: style.colours,
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
            strikeout: style.strikeout,
            scale_x: style.scale_x,
            scale_y: style.scale_y,
            spacing: style.spacing,
            angle: style.angle,
            border_style: style.border_style,
            outline: style.outline,
            shadow: style.shadow,
            alignment: style.alignment,
            margin_l: style.margin_l,
            margin_r: style.margin_r,
            margin_v: style.margin_v,
            encoding: style.encoding,
        }));
    }

    for ev in &mut sub.events {
        for (old, new, _) in &mapping {
            if ev.style == *old {
                ev.style = new.clone();
                break;
            }
        }
    }

    for (_, new_name, mut style) in mapping {
        style.name = new_name;
        sub.v4p_styles.push(style);
    }
}

fn append_sub(dst: &mut SubstationAlpha, src: SubstationAlpha) {
    dst.v4p_styles.extend(src.v4p_styles);
    dst.events.extend(src.events);
}
