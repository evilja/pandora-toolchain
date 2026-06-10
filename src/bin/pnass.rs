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

    let warning_event_count = sub.events.len();

    if let Some(merge_path) = args.merge.as_deref() {
        let mut secondary = SubstationAlpha::load(PathBuf::from(merge_path), false).await;
        let overlap: std::collections::HashSet<String> = style_names(&sub)
            .intersection(&style_names(&secondary))
            .cloned()
            .collect();
        if !overlap.is_empty() {
            rename_overlapping_styles(&mut secondary, &overlap);
        }
        append_sub(&mut sub, secondary);
    }

    let mut run_count: usize = 0;
    for (i, ev) in sub.events.iter().take(warning_event_count).enumerate() {
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

fn rename_overlapping_styles(sub: &mut SubstationAlpha, names: &std::collections::HashSet<String>) {
    use std::collections::HashMap;

    let mut mapping: HashMap<String, String> = HashMap::new();
    for style in &sub.v4p_styles {
        if names.contains(&style.name) {
            mapping.entry(style.name.clone()).or_insert_with(|| format!("pn-{}", random_suffix()));
        }
    }

    for ev in &mut sub.events {
        if let Some(new) = mapping.get(&ev.style) {
            ev.style = new.clone();
        }
    }

    for style in &mut sub.v4p_styles {
        if let Some(new) = mapping.get(&style.name) {
            style.name = new.clone();
        }
    }
}

fn style_names(sub: &SubstationAlpha) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut set: HashSet<String> = sub.v4p_styles.iter().map(|s| s.name.clone()).collect();
    for ev in &sub.events {
        if !ev.style.is_empty() {
            set.insert(ev.style.clone());
        }
    }
    set
}

fn append_sub(dst: &mut SubstationAlpha, src: SubstationAlpha) {
    dst.v4p_styles.extend(src.v4p_styles);
    dst.events.extend(src.events);
}
