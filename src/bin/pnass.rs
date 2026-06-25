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
    smart_layer: Option<u16>,

    #[arg(long)]
    split_signs: Option<String>,

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

    if let Some(n) = args.smart_layer {
        set_basic_text_layers(&mut sub, n);
    }

    if let Some(signs_path) = args.split_signs.as_deref() {
        if let Some(signs) = split_sign_events(&mut sub) {
            if signs.dump_to_file(PathBuf::from(signs_path)).await.is_err() {
                eprintln!("pnass: failed to write {}", signs_path);
                std::process::exit(1);
            }
        }
    }

    let warning_event_count = sub.events.len();
    let prune_styles = args.merge.is_some() || args.negkey.as_deref() == Some("PNassMerge");

    if let Some(merge_path) = args.merge.as_deref() {
        let mut secondary = SubstationAlpha::load(PathBuf::from(merge_path), true).await;
        fill_script_info_defaults(&mut secondary.script_info);
        if let Err(e) = normalize_merge_resolutions(&mut sub, &mut secondary) {
            println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                schema = [leaf, leaf], data = ["4", e]).unwrap());
            std::process::exit(1);
        }
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

    if prune_styles {
        prune_unused_styles(&mut sub);
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

fn normalize_merge_resolutions(primary: &mut SubstationAlpha, secondary: &mut SubstationAlpha) -> Result<(), String> {
    let px = primary.script_info.playresx;
    let py = primary.script_info.playresy;
    let sx = secondary.script_info.playresx;
    let sy = secondary.script_info.playresy;

    if px == sx && py == sy {
        return Ok(());
    }
    if px as u32 * sy as u32 != py as u32 * sx as u32 {
        return Err(format!(
            "ASS merge rejected: incompatible PlayRes ratios (input {}x{}, merge {}x{})",
            px, py, sx, sy
        ));
    }

    let primary_area = px as u32 * py as u32;
    let secondary_area = sx as u32 * sy as u32;
    if primary_area >= secondary_area {
        secondary.scale(px, py)
    } else {
        primary.scale(sx, sy)
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
        rename_style_refs_in_line(&mut ev.text, &mapping);
    }

    for style in &mut sub.v4p_styles {
        if let Some(new) = mapping.get(&style.name) {
            style.name = new.clone();
        }
    }
}

fn rename_style_refs_in_line(line: &mut ASSLine, mapping: &std::collections::HashMap<String, String>) {
    for item in &mut line.data {
        if let ASSText::Override(ov) = item {
            rename_style_refs_in_override(ov, mapping);
        }
    }
}

fn rename_style_refs_in_override(ov: &mut ASSOverride, mapping: &std::collections::HashMap<String, String>) {
    match ov {
        ASSOverride::R(Some(name)) => {
            if let Some(new) = mapping.get(name) {
                *name = new.clone();
            }
        }
        ASSOverride::TransformI(tags)
        | ASSOverride::TransformII(_, tags)
        | ASSOverride::TransformIII(_, _, tags)
        | ASSOverride::TransformIV(_, _, _, tags) => {
            for tag in tags {
                rename_style_refs_in_override(tag, mapping);
            }
        }
        _ => {}
    }
}

fn set_basic_text_layers(sub: &mut SubstationAlpha, layer: u16) {
    for ev in &mut sub.events {
        if event_has_only_basic_overrides(ev) {
            ev.layer = layer;
        }
    }
}

fn event_has_only_basic_overrides(ev: &pandora_toolchain::libkagami::core::Event) -> bool {
    if ev.style.to_lowercase().contains("sign") {
        return false;
    }
    ev.text.data.iter().all(|item| match item {
        ASSText::RawText(_) => true,
        ASSText::Override(ov) => is_basic_override(ov),
        ASSText::Drawing(_) => false,
    })
}

fn is_basic_override(ov: &ASSOverride) -> bool {
    matches!(
        ov,
        ASSOverride::Bold(_)
            | ASSOverride::Italic(_)
            | ASSOverride::Underline(_)
            | ASSOverride::Strikeout(_)
    )
}

fn split_sign_events(sub: &mut SubstationAlpha) -> Option<SubstationAlpha> {
    let mut kept = Vec::new();
    let mut signs = Vec::new();
    for ev in sub.events.drain(..) {
        if ev.style.to_lowercase().contains("sign") {
            signs.push(ev);
        } else {
            kept.push(ev);
        }
    }
    sub.events = kept;
    if signs.is_empty() {
        return None;
    }

    let sign_used = used_style_names_for_events(&signs);
    let tl_used = used_style_names(sub);
    let mut sign_styles = Vec::new();
    let mut tl_styles = Vec::new();
    for style in sub.v4p_styles.drain(..) {
        if sign_used.contains(&style.name) {
            if tl_used.contains(&style.name) {
                tl_styles.push(clone_style(&style));
            }
            sign_styles.push(style);
        } else if tl_used.contains(&style.name) {
            tl_styles.push(style);
        }
    }
    sub.v4p_styles = tl_styles;

    Some(SubstationAlpha {
        script_info: clone_script_info(&sub.script_info),
        v4p_styles: sign_styles,
        events: signs,
    })
}

fn clone_script_info(si: &ScriptInfo) -> ScriptInfo {
    ScriptInfo {
        title: si.title.clone(),
        script_type: si.script_type.clone(),
        wrap_style: si.wrap_style,
        scaled_border_and_shadow: si.scaled_border_and_shadow,
        playresx: si.playresx,
        playresy: si.playresy,
        ycbcr_matrix: si.ycbcr_matrix.clone(),
        layout_res_x: si.layout_res_x,
        layout_res_y: si.layout_res_y,
    }
}

fn clone_style(style: &pandora_toolchain::libkagami::core::V4pStyle) -> pandora_toolchain::libkagami::core::V4pStyle {
    pandora_toolchain::libkagami::core::V4pStyle {
        name: style.name.clone(),
        fontname: style.fontname.clone(),
        fontsize: style.fontsize,
        colours: style.colours.clone(),
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
    }
}

fn used_style_names_for_events(events: &[pandora_toolchain::libkagami::core::Event]) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut used = HashSet::new();
    for ev in events {
        if !ev.style.is_empty() {
            used.insert(ev.style.clone());
        }
        for item in &ev.text.data {
            collect_style_names_from_text(item, &mut used);
        }
    }
    used
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

fn prune_unused_styles(sub: &mut SubstationAlpha) {
    let used = used_style_names(sub);
    sub.v4p_styles.retain(|style| used.contains(&style.name));
}

fn used_style_names(sub: &SubstationAlpha) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut used = HashSet::new();
    for ev in &sub.events {
        if !ev.style.is_empty() {
            used.insert(ev.style.clone());
        }
        for item in &ev.text.data {
            collect_style_names_from_text(item, &mut used);
        }
    }
    used
}

fn collect_style_names_from_text(item: &ASSText, used: &mut std::collections::HashSet<String>) {
    if let ASSText::Override(ov) = item {
        collect_style_names_from_override(ov, used);
    }
}

fn collect_style_names_from_override(ov: &ASSOverride, used: &mut std::collections::HashSet<String>) {
    match ov {
        ASSOverride::R(Some(name)) if !name.is_empty() => {
            used.insert(name.clone());
        }
        ASSOverride::TransformI(tags)
        | ASSOverride::TransformII(_, tags)
        | ASSOverride::TransformIII(_, _, tags)
        | ASSOverride::TransformIV(_, _, _, tags) => {
            for tag in tags {
                collect_style_names_from_override(tag, used);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pandora_toolchain::libkagami::complex::types::{AssColour, AssTime};
    use pandora_toolchain::libkagami::core::{Event, V4pStyle};

    fn style(name: &str) -> V4pStyle {
        V4pStyle {
            name: name.to_string(),
            fontname: "Arial".to_string(),
            fontsize: 60,
            colours: [
                AssColour::opaque_white(),
                AssColour::opaque_white(),
                AssColour::transparent(),
                AssColour::transparent(),
            ],
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            scale_x: 100,
            scale_y: 100,
            spacing: 0.0,
            angle: 0.0,
            border_style: 1,
            outline: 0.0,
            shadow: 0.0,
            alignment: 2,
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            encoding: 1,
        }
    }

    fn event(style: &str, data: Vec<ASSText>) -> Event {
        Event {
            layer: 0,
            start: AssTime { hours: 0, minutes: 0, seconds: 0, centiseconds: 0 },
            end: AssTime { hours: 0, minutes: 0, seconds: 1, centiseconds: 0 },
            style: style.to_string(),
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            text: ASSLine { current_overrides: Vec::new(), data },
        }
    }

    fn sub_with_res(x: u16, y: u16) -> SubstationAlpha {
        SubstationAlpha {
            script_info: ScriptInfo {
                title: String::new(),
                script_type: "v4.00+".to_string(),
                wrap_style: 2,
                scaled_border_and_shadow: true,
                playresx: x,
                playresy: y,
                ycbcr_matrix: "TV.709".to_string(),
                layout_res_x: x,
                layout_res_y: y,
            },
            v4p_styles: vec![style("Default")],
            events: vec![event(
                "Default",
                vec![
                    ASSText::Override(ASSOverride::Pos(10.0, 20.0)),
                    ASSText::RawText("line".to_string()),
                ],
            )],
        }
    }

    #[test]
    fn prunes_styles_not_referenced_by_events() {
        let mut sub = SubstationAlpha {
            script_info: ScriptInfo {
                title: String::new(),
                script_type: String::new(),
                wrap_style: 0,
                scaled_border_and_shadow: false,
                playresx: 0,
                playresy: 0,
                ycbcr_matrix: String::new(),
                layout_res_x: 0,
                layout_res_y: 0,
            },
            v4p_styles: vec![style("Default"), style("Alt"), style("Unused")],
            events: vec![event(
                "Default",
                vec![
                    ASSText::Override(ASSOverride::R(Some("Alt".to_string()))),
                    ASSText::RawText("line".to_string()),
                ],
            )],
        };

        prune_unused_styles(&mut sub);

        let names: Vec<String> = sub.v4p_styles.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Default".to_string(), "Alt".to_string()]);
    }

    #[test]
    fn renames_secondary_reset_style_references() {
        let mut sub = SubstationAlpha {
            script_info: ScriptInfo {
                title: String::new(),
                script_type: String::new(),
                wrap_style: 0,
                scaled_border_and_shadow: false,
                playresx: 0,
                playresy: 0,
                ycbcr_matrix: String::new(),
                layout_res_x: 0,
                layout_res_y: 0,
            },
            v4p_styles: vec![style("Default"), style("Alt")],
            events: vec![event(
                "Default",
                vec![
                    ASSText::Override(ASSOverride::R(Some("Default".to_string()))),
                    ASSText::Override(ASSOverride::TransformI(vec![
                        ASSOverride::R(Some("Default".to_string())),
                    ])),
                ],
            )],
        };
        let overlap = std::collections::HashSet::from(["Default".to_string()]);

        rename_overlapping_styles(&mut sub, &overlap);

        let renamed = sub.v4p_styles.iter()
            .find(|style| style.name != "Alt")
            .map(|style| style.name.clone())
            .unwrap();
        assert_ne!(renamed, "Default");
        assert_eq!(sub.events[0].style, renamed);
        assert!(matches!(
            &sub.events[0].text.data[0],
            ASSText::Override(ASSOverride::R(Some(name))) if name == &renamed
        ));
        assert!(matches!(
            &sub.events[0].text.data[1],
            ASSText::Override(ASSOverride::TransformI(tags))
                if matches!(&tags[0], ASSOverride::R(Some(name)) if name == &renamed)
        ));
    }

    #[test]
    fn merge_resolution_scales_secondary_to_primary_when_primary_is_larger() {
        let mut primary = sub_with_res(1920, 1080);
        let mut secondary = sub_with_res(1280, 720);

        normalize_merge_resolutions(&mut primary, &mut secondary).unwrap();

        assert_eq!(primary.script_info.playresx, 1920);
        assert_eq!(primary.script_info.playresy, 1080);
        assert_eq!(secondary.script_info.playresx, 1920);
        assert_eq!(secondary.script_info.playresy, 1080);
        assert!(matches!(
            secondary.events[0].text.data[0],
            ASSText::Override(ASSOverride::Pos(x, y)) if x == 15.0 && y == 30.0
        ));
    }

    #[test]
    fn merge_resolution_scales_primary_to_secondary_when_secondary_is_larger() {
        let mut primary = sub_with_res(640, 480);
        let mut secondary = sub_with_res(1440, 1080);

        normalize_merge_resolutions(&mut primary, &mut secondary).unwrap();

        assert_eq!(primary.script_info.playresx, 1440);
        assert_eq!(primary.script_info.playresy, 1080);
        assert_eq!(secondary.script_info.playresx, 1440);
        assert_eq!(secondary.script_info.playresy, 1080);
        assert!(matches!(
            primary.events[0].text.data[0],
            ASSText::Override(ASSOverride::Pos(x, y)) if x == 22.5 && y == 45.0
        ));
    }

    #[test]
    fn merge_resolution_rejects_different_ratios() {
        let mut primary = sub_with_res(1440, 1080);
        let mut secondary = sub_with_res(1920, 1080);

        let err = normalize_merge_resolutions(&mut primary, &mut secondary).unwrap_err();

        assert!(err.contains("incompatible PlayRes ratios"));
        assert_eq!(primary.script_info.playresx, 1440);
        assert_eq!(secondary.script_info.playresx, 1920);
    }
}
