use serde::{Deserialize, Serialize};

pub const TRACE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub fn hex_rgb(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Segment {
    Line {
        to: Point,
    },
    Cubic {
        control_1: Point,
        control_2: Point,
        to: Point,
    },
}

impl Segment {
    pub fn end(&self) -> Point {
        match self {
            Segment::Line { to } | Segment::Cubic { to, .. } => *to,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Path {
    pub start: Point,
    pub segments: Vec<Segment>,
    pub hole: bool,
    pub area: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TraceLayer {
    pub palette_index: u16,
    pub sample_pixel_count: u64,
    pub paths: Vec<Path>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Trace {
    pub schema_version: u16,
    pub width: u32,
    pub height: u32,
    pub sampled_width: u32,
    pub sampled_height: u32,
    pub palette: Vec<Color>,
    pub layers: Vec<TraceLayer>,
}

impl Trace {
    pub(crate) fn new(
        width: u32,
        height: u32,
        sampled_width: u32,
        sampled_height: u32,
        palette: Vec<Color>,
        layers: Vec<TraceLayer>,
    ) -> Self {
        Self {
            schema_version: TRACE_SCHEMA_VERSION,
            width,
            height,
            sampled_width,
            sampled_height,
            palette,
            layers,
        }
    }

    pub fn color(&self, layer: &TraceLayer) -> Option<Color> {
        self.palette.get(layer.palette_index as usize).copied()
    }

    pub fn path_count(&self) -> usize {
        self.layers.iter().map(|layer| layer.paths.len()).sum()
    }

    pub fn segment_count(&self) -> usize {
        self.layers
            .iter()
            .flat_map(|layer| &layer.paths)
            .map(|path| path.segments.len())
            .sum()
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != TRACE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported trace schema version {} (expected {})",
                self.schema_version, TRACE_SCHEMA_VERSION
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.sampled_width == 0
            || self.sampled_height == 0
        {
            return Err("trace dimensions must be non-zero".to_string());
        }
        for layer in &self.layers {
            if layer.palette_index as usize >= self.palette.len() {
                return Err(format!(
                    "palette index {} is out of range",
                    layer.palette_index
                ));
            }
            for path in &layer.paths {
                validate_point(path.start)?;
                if path.segments.is_empty() {
                    return Err("trace paths must contain at least one segment".to_string());
                }
                if !path.area.is_finite() || path.area <= 0.0 {
                    return Err("trace path area must be positive and finite".to_string());
                }
                for segment in &path.segments {
                    match segment {
                        Segment::Line { to } => validate_point(*to)?,
                        Segment::Cubic {
                            control_1,
                            control_2,
                            to,
                        } => {
                            validate_point(*control_1)?;
                            validate_point(*control_2)?;
                            validate_point(*to)?;
                        }
                    }
                }
                let end = path.segments.last().unwrap().end();
                if (end.x - path.start.x).abs() > 0.001 || (end.y - path.start.y).abs() > 0.001 {
                    return Err("trace paths must end at their starting point".to_string());
                }
            }
        }
        Ok(())
    }
}

fn validate_point(point: Point) -> Result<(), String> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return Err("trace points must be finite".to_string());
    }
    Ok(())
}
