use serde::{Deserialize, Serialize};

pub const MAX_COLOR_COUNT: u16 = 512;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TracePreset {
    LogoUi,
    #[default]
    Illustration,
    Photo,
    Gradient,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TraceOptions {
    pub color_count: u16,
    pub preserve_gradients: bool,
    pub color_smoothing: u8,
    pub path_simplify: f32,
    pub curve_fit: f32,
    pub corner_threshold: f32,
    pub min_area: u32,
    pub alpha_threshold: u8,
    pub max_dimension: u32,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self::for_preset(TracePreset::default())
    }
}

impl TraceOptions {
    pub fn for_preset(preset: TracePreset) -> Self {
        match preset {
            TracePreset::LogoUi => Self {
                color_count: 24,
                preserve_gradients: false,
                color_smoothing: 2,
                path_simplify: 0.35,
                curve_fit: 0.0,
                corner_threshold: 25.0,
                min_area: 2,
                alpha_threshold: 8,
                max_dimension: 1024,
            },
            TracePreset::Illustration => Self {
                color_count: 12,
                preserve_gradients: false,
                color_smoothing: 1,
                path_simplify: 1.0,
                curve_fit: 0.65,
                corner_threshold: 55.0,
                min_area: 4,
                alpha_threshold: 8,
                max_dimension: 1024,
            },
            TracePreset::Photo => Self {
                color_count: 64,
                preserve_gradients: false,
                color_smoothing: 2,
                path_simplify: 0.75,
                curve_fit: 0.3,
                corner_threshold: 45.0,
                min_area: 6,
                alpha_threshold: 8,
                max_dimension: 1024,
            },
            TracePreset::Gradient => Self {
                color_count: 128,
                preserve_gradients: true,
                color_smoothing: 0,
                path_simplify: 0.5,
                curve_fit: 0.7,
                corner_threshold: 55.0,
                min_area: 8,
                alpha_threshold: 8,
                max_dimension: 1024,
            },
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_COLOR_COUNT).contains(&self.color_count) {
            return Err(format!(
                "color_count must be between 1 and {MAX_COLOR_COUNT}"
            ));
        }
        if self.color_smoothing > 4 {
            return Err("color_smoothing must be between 0 and 4".to_string());
        }
        if !self.path_simplify.is_finite() || !(0.0..=32.0).contains(&self.path_simplify) {
            return Err("path_simplify must be finite and between 0 and 32".to_string());
        }
        if !self.curve_fit.is_finite() || !(0.0..=1.0).contains(&self.curve_fit) {
            return Err("curve_fit must be finite and between 0 and 1".to_string());
        }
        if !self.corner_threshold.is_finite() || !(0.0..=180.0).contains(&self.corner_threshold) {
            return Err("corner_threshold must be finite and between 0 and 180".to_string());
        }
        if self.min_area > 1_000_000 {
            return Err("min_area must not exceed 1000000".to_string());
        }
        if self.max_dimension > crate::MAX_IMAGE_DIMENSION {
            return Err(format!(
                "max_dimension must be 0 or no greater than {}",
                crate::MAX_IMAGE_DIMENSION
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_count_accepts_512_and_rejects_values_above_it() {
        let mut options = TraceOptions::default();
        options.color_count = MAX_COLOR_COUNT;
        assert!(options.validate().is_ok());

        options.color_count = MAX_COLOR_COUNT + 1;
        assert_eq!(
            options.validate().unwrap_err(),
            "color_count must be between 1 and 512"
        );
    }

    #[test]
    fn presets_target_different_source_types() {
        let ui = TraceOptions::for_preset(TracePreset::LogoUi);
        let illustration = TraceOptions::for_preset(TracePreset::Illustration);
        let photo = TraceOptions::for_preset(TracePreset::Photo);
        let gradient = TraceOptions::for_preset(TracePreset::Gradient);

        assert_eq!(ui.curve_fit, 0.0);
        assert!(ui.path_simplify < illustration.path_simplify);
        assert!(photo.color_count > illustration.color_count);
        assert!(gradient.color_count > photo.color_count);
        assert!(gradient.curve_fit > photo.curve_fit);
        assert!(!ui.preserve_gradients);
        assert!(!illustration.preserve_gradients);
        assert!(!photo.preserve_gradients);
        assert!(gradient.preserve_gradients);
        assert!(ui.validate().is_ok());
        assert!(illustration.validate().is_ok());
        assert!(photo.validate().is_ok());
        assert!(gradient.validate().is_ok());
    }
}
