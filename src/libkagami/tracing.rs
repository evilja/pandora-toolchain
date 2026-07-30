use kagami_trace::{Segment, Trace};

use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::complex::types::{AssColour, AssTime};
use crate::libkagami::core::{Event, PandoraMeta, ScriptInfo, SubstationAlpha, V4pStyle};
use crate::libkagami::drawing::parse::{Drawing, DrawingCommand};
use crate::libkagami::tags::{ASSLine, ASSText};

#[derive(Clone, Debug)]
pub struct TraceAssOptions {
    pub title: String,
    pub style: String,
    pub actor: String,
    pub start: AssTime,
    pub end: AssTime,
    pub base_layer: u16,
    pub seam_overlap: f32,
}

impl Default for TraceAssOptions {
    fn default() -> Self {
        Self {
            title: "Kagami image trace".to_string(),
            style: "Kagami Trace".to_string(),
            actor: "kagami-trace".to_string(),
            start: AssTime::from_centiseconds(0),
            end: AssTime::from_centiseconds(500),
            base_layer: 0,
            seam_overlap: 0.5,
        }
    }
}

pub fn parse_trace_json(input: &str) -> Result<Trace, String> {
    let trace = Trace::from_json(input).map_err(|error| format!("invalid trace JSON: {error}"))?;
    trace.validate()?;
    Ok(trace)
}

pub fn trace_json_to_ass(
    input: &str,
    options: &TraceAssOptions,
) -> Result<SubstationAlpha, String> {
    let trace = parse_trace_json(input)?;
    trace_to_ass(&trace, options)
}

pub fn trace_to_ass(trace: &Trace, options: &TraceAssOptions) -> Result<SubstationAlpha, String> {
    trace.validate()?;
    if trace.width > u16::MAX as u32 || trace.height > u16::MAX as u32 {
        return Err(format!(
            "trace dimensions {}x{} exceed ASS PlayRes limits",
            trace.width, trace.height
        ));
    }
    if options.end.total_centiseconds() <= options.start.total_centiseconds() {
        return Err("ASS trace end time must be after its start time".to_string());
    }
    validate_ass_field("title", &options.title, false)?;
    validate_ass_field("style", &options.style, true)?;
    validate_ass_field("actor", &options.actor, true)?;
    if !options.seam_overlap.is_finite() || !(0.0..=4.0).contains(&options.seam_overlap) {
        return Err("ASS trace seam overlap must be finite and between 0 and 4".to_string());
    }
    let final_offset = u16::try_from(trace.layers.len().saturating_sub(1))
        .map_err(|_| "trace contains too many layers for ASS".to_string())?;
    options
        .base_layer
        .checked_add(final_offset)
        .ok_or_else(|| "trace contains too many layers for ASS".to_string())?;

    let mut events = Vec::with_capacity(trace.layers.len());
    for (index, layer) in trace.layers.iter().enumerate() {
        let color = trace
            .color(layer)
            .ok_or_else(|| format!("trace layer {} has an invalid palette index", index))?;
        let drawing = layer_drawing(layer);
        if drawing.commands.is_empty() {
            continue;
        }
        let color_value = ((color.b as u32) << 16) | ((color.g as u32) << 8) | color.r as u32;
        let alpha_value = 255u32 - color.a as u32;
        let overrides = vec![
            ASSOverride::An(7),
            ASSOverride::Pos(0.0, 0.0),
            ASSOverride::Bord(options.seam_overlap),
            ASSOverride::Shad(0.0),
            ASSOverride::ColorI(color_value),
            ASSOverride::ColorIII(color_value),
            ASSOverride::AlphaI(alpha_value),
            ASSOverride::AlphaIII(alpha_value),
            ASSOverride::P(1),
        ];
        let mut data: Vec<ASSText> = overrides.iter().cloned().map(ASSText::Override).collect();
        data.push(ASSText::Drawing(drawing));
        events.push(Event {
            layer: options.base_layer + index as u16,
            start: options.start,
            end: options.end,
            style: options.style.clone(),
            name: options.actor.clone(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            text: ASSLine {
                current_overrides: overrides,
                data,
            },
        });
    }

    Ok(SubstationAlpha {
        script_info: ScriptInfo {
            title: options.title.clone(),
            script_type: "v4.00+".to_string(),
            wrap_style: 2,
            scaled_border_and_shadow: true,
            playresx: trace.width as u16,
            playresy: trace.height as u16,
            ycbcr_matrix: "TV.709".to_string(),
            layout_res_x: trace.width as u16,
            layout_res_y: trace.height as u16,
        },
        v4p_styles: vec![trace_style(&options.style)],
        events,
        comments: Vec::new(),
        pandora_meta: PandoraMeta::default(),
    })
}

fn layer_drawing(layer: &kagami_trace::TraceLayer) -> Drawing {
    let mut commands = Vec::new();
    for path in &layer.paths {
        commands.push(DrawingCommand::Move(path.start.x, path.start.y));
        for segment in &path.segments {
            match segment {
                Segment::Line { to } => commands.push(DrawingCommand::Line(to.x, to.y)),
                Segment::Cubic {
                    control_1,
                    control_2,
                    to,
                } => commands.push(DrawingCommand::CubicBezier(
                    control_1.x,
                    control_1.y,
                    control_2.x,
                    control_2.y,
                    to.x,
                    to.y,
                )),
            }
        }
    }
    Drawing { commands }
}

fn trace_style(name: &str) -> V4pStyle {
    V4pStyle {
        name: name.to_string(),
        fontname: "Arial".to_string(),
        fontsize: 20,
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
        alignment: 7,
        margin_l: 0,
        margin_r: 0,
        margin_v: 0,
        encoding: 1,
    }
}

fn validate_ass_field(label: &str, value: &str, reject_comma: bool) -> Result<(), String> {
    if value.contains('\n') || value.contains('\r') || (reject_comma && value.contains(',')) {
        return Err(format!(
            "ASS trace {label} contains an unsupported separator"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kagami_trace::{
        Color, Path, Point, TRACE_SCHEMA_VERSION, TraceLayer, TraceOptions, trace_rgba,
    };

    use super::*;

    fn traced_colors() -> Trace {
        let pixels = [
            255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 128, 0, 255, 0, 128,
        ];
        trace_rgba(
            2,
            2,
            &pixels,
            &TraceOptions {
                color_count: 2,
                preserve_gradients: false,
                color_smoothing: 0,
                path_simplify: 0.0,
                curve_fit: 0.0,
                corner_threshold: 55.0,
                min_area: 1,
                alpha_threshold: 0,
                max_dimension: 0,
            },
        )
        .unwrap()
    }

    #[test]
    fn trace_json_parses_and_converts_to_ass_drawings() {
        let trace = traced_colors();
        let json = trace.to_json().unwrap();
        let ass = trace_json_to_ass(&json, &TraceAssOptions::default()).unwrap();
        let output = ass.stringify();

        assert_eq!(ass.script_info.playresx, 2);
        assert_eq!(ass.script_info.playresy, 2);
        assert_eq!(ass.events.len(), 2);
        assert!(output.contains(r"\p1"));
        assert!(output.contains(r"\c&H0000FF&"));
        assert!(output.contains(r"\c&H00FF00&"));
        assert!(output.contains(r"\1a&H7F&"));
        assert!(output.contains(r"\bord0.5"));
        assert!(output.contains(r"\3c&H0000FF&"));
        assert!(output.contains(r"\3a&H7F&"));
        assert!(output.contains("m "));
    }

    #[test]
    fn fitted_curves_become_one_persistent_ass_bezier_run() {
        let trace = Trace {
            schema_version: TRACE_SCHEMA_VERSION,
            width: 100,
            height: 100,
            sampled_width: 100,
            sampled_height: 100,
            palette: vec![Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            }],
            layers: vec![TraceLayer {
                palette_index: 0,
                sample_pixel_count: 1_000,
                paths: vec![Path {
                    start: Point { x: 0.0, y: 50.0 },
                    segments: vec![
                        Segment::Cubic {
                            control_1: Point { x: 0.0, y: 22.0 },
                            control_2: Point { x: 22.0, y: 0.0 },
                            to: Point { x: 50.0, y: 0.0 },
                        },
                        Segment::Cubic {
                            control_1: Point { x: 78.0, y: 0.0 },
                            control_2: Point { x: 100.0, y: 22.0 },
                            to: Point { x: 100.0, y: 50.0 },
                        },
                        Segment::Cubic {
                            control_1: Point { x: 100.0, y: 78.0 },
                            control_2: Point { x: 78.0, y: 100.0 },
                            to: Point { x: 50.0, y: 100.0 },
                        },
                        Segment::Cubic {
                            control_1: Point { x: 22.0, y: 100.0 },
                            control_2: Point { x: 0.0, y: 78.0 },
                            to: Point { x: 0.0, y: 50.0 },
                        },
                    ],
                    hole: false,
                    area: 7_850.0,
                }],
            }],
        };
        let output = trace_to_ass(&trace, &TraceAssOptions::default())
            .unwrap()
            .stringify();

        assert!(output.contains("m 0 50 b 0 22 22 0 50 0 78 0"));
        assert_eq!(output.matches(" b ").count(), 1);
    }

    #[test]
    fn malformed_or_future_trace_json_is_rejected() {
        assert!(parse_trace_json("not json").is_err());

        let mut value = serde_json::to_value(traced_colors()).unwrap();
        value["schema_version"] = serde_json::json!(99);
        assert!(parse_trace_json(&value.to_string()).is_err());
    }

    #[test]
    fn invalid_ass_options_are_rejected() {
        let mut options = TraceAssOptions::default();
        options.style = "bad,style".to_string();
        assert!(trace_to_ass(&traced_colors(), &options).is_err());

        options.style = "Kagami Trace".to_string();
        options.seam_overlap = f32::NAN;
        assert!(trace_to_ass(&traced_colors(), &options).is_err());
    }
}
