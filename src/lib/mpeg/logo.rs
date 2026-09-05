use serde::{Deserialize, Serialize};

// The image watermark: a logo burned into the video by the encoder, beside the ASS watermark that
// pnass injects into the subtitle. The two are deliberately separate — an ASS watermark is text
// libass draws and can be styled, animated and timed per event; a logo is a picture ffmpeg composits
// over every frame — and a server may configure either, both, or neither.
//
// This is the whole of what travels: the bytes are handled beside it, and everything here is what
// decides where those bytes land on the frame. It lives in the library rather than in pnmpeg so
// that the Discord command, the job snapshot, the link spec and the encoder all name the same
// anchors; a position the bot accepted and the encoder did not understand would be a release that
// silently came out with the logo in the wrong corner.

// Where the logo sits, as a nine-point anchor grid. Named rather than free x/y because the useful
// answer is almost always a corner, and an anchor survives a change of resolution while a pixel
// coordinate does not.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LogoPosition {
    TopLeft,
    TopCenter,
    #[default]
    TopRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

pub const LOGO_POSITIONS: [LogoPosition; 9] = [
    LogoPosition::TopLeft,
    LogoPosition::TopCenter,
    LogoPosition::TopRight,
    LogoPosition::MiddleLeft,
    LogoPosition::MiddleCenter,
    LogoPosition::MiddleRight,
    LogoPosition::BottomLeft,
    LogoPosition::BottomCenter,
    LogoPosition::BottomRight,
];

impl LogoPosition {
    // The value written to `logo.toml` and carried in a link spec. Kebab-case because that is what
    // the serde rename produces, and the two must not disagree.
    pub fn name(self) -> &'static str {
        match self {
            LogoPosition::TopLeft => "top-left",
            LogoPosition::TopCenter => "top-center",
            LogoPosition::TopRight => "top-right",
            LogoPosition::MiddleLeft => "middle-left",
            LogoPosition::MiddleCenter => "middle-center",
            LogoPosition::MiddleRight => "middle-right",
            LogoPosition::BottomLeft => "bottom-left",
            LogoPosition::BottomCenter => "bottom-center",
            LogoPosition::BottomRight => "bottom-right",
        }
    }

    // Underscores and spaces are accepted alongside the canonical hyphen because an operator typing
    // a position by hand into an API payload should not have to guess which separator was chosen.
    pub fn from_name(value: &str) -> Option<Self> {
        let normalised = value.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        LOGO_POSITIONS
            .into_iter()
            .find(|position| position.name() == normalised)
    }

    // The `overlay` x and y expressions for this anchor at the given margin. `W`/`H` are the frame's
    // dimensions and `w`/`h` the logo's, so one expression is correct at every resolution and needs
    // nothing probed.
    pub fn overlay_expressions(self, margin: u32) -> (String, String) {
        let x = match self {
            LogoPosition::TopLeft | LogoPosition::MiddleLeft | LogoPosition::BottomLeft => {
                margin.to_string()
            }
            LogoPosition::TopCenter | LogoPosition::MiddleCenter | LogoPosition::BottomCenter => {
                "(W-w)/2".to_string()
            }
            LogoPosition::TopRight | LogoPosition::MiddleRight | LogoPosition::BottomRight => {
                format!("W-w-{margin}")
            }
        };
        let y = match self {
            LogoPosition::TopLeft | LogoPosition::TopCenter | LogoPosition::TopRight => {
                margin.to_string()
            }
            LogoPosition::MiddleLeft | LogoPosition::MiddleCenter | LogoPosition::MiddleRight => {
                "(H-h)/2".to_string()
            }
            LogoPosition::BottomLeft | LogoPosition::BottomCenter | LogoPosition::BottomRight => {
                format!("H-h-{margin}")
            }
        };
        (x, y)
    }
}

pub const DEFAULT_LOGO_MARGIN: u32 = 24;
pub const MAX_LOGO_MARGIN: u32 = 2000;
pub const DEFAULT_LOGO_OPACITY: u8 = 100;
// Below a few percent of the frame the logo is a smudge, and above half of it the thing is not a
// watermark any more. The range exists so a typed percentage cannot produce a filter that fails or
// an encode nobody wants to ship.
pub const MIN_LOGO_WIDTH_PERCENT: u8 = 1;
pub const MAX_LOGO_WIDTH_PERCENT: u8 = 50;

// The longest interval a period may name. Nothing this encodes is longer than an episode, and a
// period longer than the video is a logo that is drawn once at the start and never again — which is
// still a coherent thing to ask for, so the cap is generous rather than tight.
pub const MAX_LOGO_PERIOD_SECONDS: u32 = 6 * 60 * 60;

// How long the logo takes to arrive and to leave. Derived rather than configured: a quarter of the
// visible window, capped at a second, is long enough to read as a fade and short enough that a
// twenty-second appearance is still twenty seconds of logo.
pub const LOGO_FADE_SECONDS: f64 = 1.0;

// The fade is drawn as this many discrete alpha levels rather than a continuous ramp. ffmpeg has no
// time-varying alpha for a still overlay — `colorchannelmixer` takes a number, not an expression,
// and the per-pixel filter that does take one (`geq`) needs the logo turned into a looping video
// stream that `overlay` then has to fast-forward through from zero on every chunk. A stack of
// `overlay`s at fixed alphas, each switched on for the slice of the ramp it stands for, costs
// nothing when it is off and works identically in a chunk that starts twenty minutes in.
pub const LOGO_FADE_STEPS: u32 = 8;

// A logo that appears in bursts instead of sitting on every frame: `every_seconds` from the start of
// one appearance to the start of the next, `visible_seconds` on screen each time. Stored as two
// counts rather than as the `5m:20s` an operator types, because the encoder, the link spec and the
// forwarding key all want the numbers and none of them should have to parse a duration.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoPeriod {
    pub every_seconds: u32,
    pub visible_seconds: u32,
}

impl LogoPeriod {
    // `<every>:<visible>`, each side a duration like `5m`, `90s` or `1h30m`. Rejected rather than
    // clamped, because a period is typed by hand and an operator who wrote `20s:5m` meant the other
    // order and should be told so rather than handed a logo that never leaves.
    pub fn parse(value: &str) -> Result<LogoPeriod, String> {
        let value = value.trim();
        let Some((every, visible)) = value.split_once(':') else {
            return Err(format!(
                "`{value}` is not a period. It is written as `<every>:<visible>` — `5m:20s` shows the logo for twenty seconds every five minutes."
            ));
        };
        let duration = |part: &str| {
            parse_duration_seconds(part).ok_or_else(|| {
                format!(
                    "`{}` is not a duration. Write it as `5m`, `20s`, `1h30m`, or a plain number of seconds.",
                    part.trim()
                )
            })
        };
        let period = LogoPeriod {
            every_seconds: duration(every)?,
            visible_seconds: duration(visible)?,
        };
        period.validate()?;
        Ok(period)
    }

    fn validate(&self) -> Result<(), String> {
        if self.visible_seconds == 0 {
            return Err(
                "a period that shows the logo for no time at all would hide it entirely; that is what `clear` is for."
                    .to_string(),
            );
        }
        if self.every_seconds > MAX_LOGO_PERIOD_SECONDS || self.visible_seconds > MAX_LOGO_PERIOD_SECONDS {
            return Err(format!(
                "a period is at most {}; `{}` is longer than anything this encodes.",
                format_duration_seconds(MAX_LOGO_PERIOD_SECONDS),
                self.label()
            ));
        }
        if self.visible_seconds >= self.every_seconds {
            return Err(format!(
                "`{}` keeps the logo on screen for as long as it waits, so it would never leave. The visible half has to be shorter than the interval, or use `off` to draw it on every frame.",
                self.label()
            ));
        }
        Ok(())
    }

    // What an operator typed, spelled back the one way. This is the label the command echoes and the
    // value the forwarding key hashes, so two servers that wrote `300:20` and `5m:20s` are one.
    pub fn label(&self) -> String {
        format!(
            "{}:{}",
            format_duration_seconds(self.every_seconds),
            format_duration_seconds(self.visible_seconds)
        )
    }

    // The ramp at each end of an appearance. Capped at a quarter of the window so that a short one
    // still spends half its time at full alpha instead of being nothing but fade.
    pub fn fade_seconds(&self) -> f64 {
        (self.visible_seconds as f64 / 4.0).min(LOGO_FADE_SECONDS)
    }

    // The `enable` expression for the overlay drawn at `step`/`LOGO_FADE_STEPS` of the logo's alpha:
    // the slice of the ramp that rounds to that level, on the way in and again on the way out.
    //
    // `t` is the frame's own timestamp, so the pattern is anchored to the video rather than to the
    // run — which is what keeps a parallel chunk that starts twenty minutes in showing the logo at
    // the same moments the linear encode of the same episode would.
    pub fn fade_step_enable(&self, step: u32) -> String {
        let fade = self.fade_seconds();
        // The ramp reaches `level`/steps at `(level-0.5)*fade/steps` in, and leaves it the same
        // distance before the end; rounding to the nearest level is what the half is.
        let entry = |level: f64| (level - 0.5) * fade / LOGO_FADE_STEPS as f64;
        let visible = self.visible_seconds as f64;
        let elapsed = format!("mod(t,{})", self.every_seconds);
        let (opens, closes) = (entry(step as f64), visible - entry(step as f64));
        if step >= LOGO_FADE_STEPS {
            return format!("gte({elapsed},{opens:.3})*lte({elapsed},{closes:.3})");
        }
        let (next_opens, next_closes) = (entry(step as f64 + 1.0), visible - entry(step as f64 + 1.0));
        format!(
            "gte({elapsed},{opens:.3})*lt({elapsed},{next_opens:.3})+gt({elapsed},{next_closes:.3})*lte({elapsed},{closes:.3})"
        )
    }
}

// A duration written the way a person writes one: `1h30m`, `5m`, `90s`, or a bare number of seconds.
// Units may be combined and a trailing bare number is seconds, so `1m30` is the same as `90s`.
pub fn parse_duration_seconds(value: &str) -> Option<u32> {
    let value = value.trim().to_ascii_lowercase();
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut saw_digit = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            saw_digit = true;
            continue;
        }
        let unit: u64 = match ch {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        // A unit with no number in front of it is a typo, not a zero.
        let count = digits.parse::<u64>().ok()?;
        digits.clear();
        total = total.checked_add(count.checked_mul(unit)?)?;
    }
    if !digits.is_empty() {
        total = total.checked_add(digits.parse::<u64>().ok()?)?;
    }
    if !saw_digit {
        return None;
    }
    u32::try_from(total).ok()
}

// The canonical spelling of a duration, smallest number of parts that says it. Zero is `0s` rather
// than the empty string, so a value always reads as a duration.
pub fn format_duration_seconds(seconds: u32) -> String {
    let mut out = String::new();
    if seconds / 3600 > 0 {
        out.push_str(&format!("{}h", seconds / 3600));
    }
    if (seconds % 3600) / 60 > 0 {
        out.push_str(&format!("{}m", (seconds % 3600) / 60));
    }
    if seconds % 60 > 0 || out.is_empty() {
        out.push_str(&format!("{}s", seconds % 60));
    }
    out
}

// How the logo is drawn, without the bytes. `width_percent` is a share of the *output* frame width;
// `None` keeps the uploaded image's own pixel size, which is what an operator who already prepared
// the file at the right scale wants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoPlacement {
    #[serde(default)]
    pub position: LogoPosition,
    #[serde(default = "default_margin")]
    pub margin: u32,
    #[serde(default = "default_opacity")]
    pub opacity: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_percent: Option<u8>,
    // Absent is the logo on every frame, which is what a watermark usually is. A period makes it a
    // recurring burst instead, faded in and out at each end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<LogoPeriod>,
}

fn default_margin() -> u32 {
    DEFAULT_LOGO_MARGIN
}

fn default_opacity() -> u8 {
    DEFAULT_LOGO_OPACITY
}

impl Default for LogoPlacement {
    fn default() -> Self {
        LogoPlacement {
            position: LogoPosition::default(),
            margin: DEFAULT_LOGO_MARGIN,
            opacity: DEFAULT_LOGO_OPACITY,
            width_percent: None,
            period: None,
        }
    }
}

impl LogoPlacement {
    // Clamps rather than rejects, because this runs on values that have already been stored: a
    // hand-edited `logo.toml` should encode with a sane logo, not fail the job.
    pub fn sanitized(&self) -> LogoPlacement {
        LogoPlacement {
            position: self.position,
            margin: self.margin.min(MAX_LOGO_MARGIN),
            opacity: self.opacity.clamp(1, 100),
            width_percent: self
                .width_percent
                .map(|value| value.clamp(MIN_LOGO_WIDTH_PERCENT, MAX_LOGO_WIDTH_PERCENT)),
            // A period that does not describe a burst — no visible time, or as much of it as the
            // interval — is dropped back to the logo on every frame rather than clamped into some
            // other cadence nobody asked for.
            period: self.period.filter(|period| period.validate().is_ok()),
        }
    }
}

// A server's configured logo, with the bytes that get written beside the job. This is what a job
// snapshots and what travels to a linked node — the coordinator's own path to the file means
// nothing on another machine, so the bytes move and the node writes its own copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerLogo {
    pub bytes: Vec<u8>,
    // Normalised to one of `LOGO_EXTENSIONS`, because ffmpeg's `movie` source picks its demuxer by
    // the file name and a logo written without an extension is one it will not open.
    pub extension: String,
    pub placement: LogoPlacement,
}

impl ServerLogo {
    pub fn file_name(&self) -> String {
        format!("server_logo.{}", self.extension)
    }
}

// `logo.toml` beside the image. The placement is a nested table rather than flattened scalars so the
// file stays readable when an operator opens it, and so adding a field later cannot collide with
// `file`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogoConfig {
    // The image's file name, relative to the same directory as this file. A name rather than a path
    // because nothing outside that directory may be addressed.
    pub file: String,
    #[serde(default)]
    pub placement: LogoPlacement,
}

// A filter-graph value ffmpeg will read back as one token. The same escaping `pnmpeg` applies to a
// subtitle path, for the same reason: a Windows path or a file with a comma in it would otherwise
// end the argument early.
pub fn escape_filter_value(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' | '\'' | ':' | ',' | '[' | ']' | ';' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

// Appends the logo overlay to a `-vf` chain that already ends in whatever the preset produces.
//
// The preset's chain is left exactly as it was and its output is labelled, so a preset that scales,
// tone-maps, or writes 10-bit still does all of that first and the logo is composited onto the frame
// that is actually being encoded — which is also why `frame_width` is the *output* width and not the
// source's. The logo arrives through `movie`, a source filter, rather than as a second `-i`: a
// simple filtergraph takes one decoder input, and adding a real input would mean rewriting every
// preset's `-map` arguments and the three encode paths that share them.
//
// `frame_width` is only consulted for a percentage width; without one the image's own size is used
// and nothing needs to be known about the frame at all.
pub fn compose_logo_filter(
    base_filter: &str,
    logo_path: &str,
    placement: &LogoPlacement,
    frame_width: Option<u32>,
) -> String {
    let placement = placement.sanitized();
    let mut logo_chain = format!("movie={}", escape_filter_value(logo_path));
    if let Some(width) = placement
        .width_percent
        .and_then(|percent| frame_width.map(|frame| logo_pixel_width(frame, percent)))
    {
        // `-1` keeps the aspect ratio; `force_original_aspect_ratio` is not needed because only one
        // dimension is being pinned. An odd width is fine — the logo is an overlay, not an encoded
        // stream, so it has no chroma-subsampling constraint of its own.
        logo_chain.push_str(&format!(",scale={width}:-1"));
    }
    let (x, y) = placement.position.overlay_expressions(placement.margin);
    // `shortest=0` and `eof_action=repeat` keep a still image on screen for the whole episode: an
    // overlay input that ends after one frame would otherwise stop the encode at frame one.
    let overlay = format!("overlay={x}:{y}:eof_action=repeat:shortest=0");
    // `overlay` negotiates its own format, and given an alpha-carrying overlay it will pick one for
    // the main input too: a 10-bit chain measured as `yuva420p` on the way in, which both adds an
    // alpha channel no release wants and quietly drops the video to 8 bits — and libx265 then
    // refuses the stream outright with "does not support alpha layer encoding". Restating the
    // chain's own pixel format after the overlay pins both back.
    let output_format = trailing_pixel_format(base_filter).unwrap_or(FALLBACK_PIXEL_FORMAT);

    let Some(period) = placement.period else {
        if placement.opacity < 100 {
            // `format=rgba` first, because a logo that arrived without an alpha channel has nothing
            // for the mixer to scale and would come out fully opaque.
            logo_chain.push_str(&format!(
                ",format=rgba,colorchannelmixer=aa={:.3}",
                placement.opacity as f64 / 100.0
            ));
        }
        return format!("{base_filter}[pnbase];{logo_chain}[pnlogo];[pnbase][pnlogo]{overlay},format={output_format}");
    };

    // One copy of the picture per rung of the fade, each mixed to its own alpha and switched on for
    // the slice of the ramp that rounds to it. Only ever one of them is enabled at a time, and a
    // disabled `overlay` hands the frame straight through, so the whole stack costs nothing during
    // the minutes the logo is not on screen. See `LOGO_FADE_STEPS` for why it is built this way
    // rather than with a time-varying alpha.
    logo_chain.push_str(",format=rgba");
    let mut graph = format!("{base_filter}[pnbase];{logo_chain},split={LOGO_FADE_STEPS}");
    for step in 1..=LOGO_FADE_STEPS {
        graph.push_str(&format!("[pnraw{step}]"));
    }
    graph.push(';');
    for step in 1..=LOGO_FADE_STEPS {
        graph.push_str(&format!(
            "[pnraw{step}]colorchannelmixer=aa={:.3}[pnlogo{step}];",
            placement.opacity as f64 / 100.0 * step as f64 / LOGO_FADE_STEPS as f64
        ));
    }
    for step in 1..=LOGO_FADE_STEPS {
        let input = match step {
            1 => "pnbase".to_string(),
            _ => format!("pnstep{}", step - 1),
        };
        // The expression is quoted, which is what lets the commas inside `mod(t,...)` survive the
        // filtergraph parser splitting arguments on them.
        graph.push_str(&format!(
            "[{input}][pnlogo{step}]{overlay}:enable='{}'",
            period.fade_step_enable(step)
        ));
        if step < LOGO_FADE_STEPS {
            graph.push_str(&format!("[pnstep{step}];"));
        }
    }
    graph.push_str(&format!(",format={output_format}"));
    graph
}

// What a chain that declares no format of its own is pinned to after the overlay. Every preset this
// build ships ends in a `format=`, so this only covers a preset file that left one out — and for
// one of those, 8-bit 4:2:0 is what `overlay` was going to produce anyway, minus the alpha channel
// that would have gone with it. A preset file that wants 10-bit has to say so, which it already had
// to for the encoder to receive it.
const FALLBACK_PIXEL_FORMAT: &str = "yuv420p";

// The pixel format a filter chain ends up in, read off the last `format=` filter in it. Only a
// `format=` that starts a filter counts, so a `format=rgba` buried in some other filter's arguments
// is not mistaken for the chain's own output.
pub fn trailing_pixel_format(chain: &str) -> Option<&str> {
    let bytes = chain.as_bytes();
    let mut found = None;
    let mut search = 0;
    while let Some(offset) = chain[search..].find("format=") {
        let start = search + offset;
        search = start + "format=".len();
        let starts_filter = start == 0
            || matches!(bytes[start - 1], b',' | b';' | b']');
        if !starts_filter {
            continue;
        }
        let value = &chain[search..];
        let end = value
            .find(|ch| matches!(ch, ',' | ';' | '[' | ':'))
            .unwrap_or(value.len());
        let value = &value[..end];
        // A pixel format is a bare token. Anything else is some other filter's `format=` argument
        // spelled in a way this cannot read, and guessing at it would pin the wrong format.
        if !value.is_empty()
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            found = Some(value);
        }
    }
    found
}

// The logo's pixel width for a percentage of the frame, never below one pixel — a percentage that
// rounded to zero would produce a `scale=0:-1` ffmpeg refuses.
pub fn logo_pixel_width(frame_width: u32, percent: u8) -> u32 {
    ((frame_width as u64 * percent as u64 + 50) / 100).max(1) as u32
}

// The file extensions a logo may be uploaded under, and the container ffmpeg's `movie` source reads
// them back as. PNG is first because it is the only one of the three that carries an alpha channel,
// which is what a watermark almost always wants.
pub const LOGO_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

pub fn logo_extension_is_supported(extension: &str) -> bool {
    let extension = extension.trim().trim_start_matches('.').to_ascii_lowercase();
    LOGO_EXTENSIONS.contains(&extension.as_str())
}

// The extension an uploaded logo is stored under, taken from its file name. `jpeg` is normalised to
// `jpg` so one server never ends up with two files that are the same picture.
pub fn logo_extension_from_filename(filename: &str) -> Option<&'static str> {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.trim().to_ascii_lowercase())?;
    match extension.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "webp" => Some("webp"),
        _ => None,
    }
}

// The image format the bytes actually are, from their signature. Content decides rather than the
// uploaded file name: ffmpeg's `movie` source picks its demuxer from the extension, so a PNG saved
// as `.jpg` would be opened as a JPEG and fail at encode time, long after anyone could connect it
// to the upload. `None` means nothing this build can burn in.
pub fn detect_logo_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every anchor has to produce an expression ffmpeg evaluates against the frame, not a baked
    // pixel coordinate: the same stored placement is used for a 1080p and a 480p release.
    #[test]
    fn every_anchor_places_itself_relative_to_the_frame() {
        for position in LOGO_POSITIONS {
            let (x, y) = position.overlay_expressions(30);
            assert!(!x.is_empty() && !y.is_empty(), "{position:?}");
            let name = position.name();
            if name.ends_with("-right") {
                assert_eq!(x, "W-w-30");
            }
            if name.ends_with("-left") {
                assert_eq!(x, "30");
            }
            if name.ends_with("-center") {
                assert_eq!(x, "(W-w)/2");
            }
            if name.starts_with("top-") {
                assert_eq!(y, "30");
            }
            if name.starts_with("bottom-") {
                assert_eq!(y, "H-h-30");
            }
            if name.starts_with("middle-") {
                assert_eq!(y, "(H-h)/2");
            }
        }
    }

    // The name is what reaches a node and what is written to disk, so parsing has to accept back
    // everything rendering produces — a round trip that failed would decline every leased job.
    #[test]
    fn position_names_round_trip() {
        for position in LOGO_POSITIONS {
            assert_eq!(LogoPosition::from_name(position.name()), Some(position));
        }
        assert_eq!(
            LogoPosition::from_name(" TOP_RIGHT "),
            Some(LogoPosition::TopRight)
        );
        assert_eq!(
            LogoPosition::from_name("bottom left"),
            Some(LogoPosition::BottomLeft)
        );
        assert_eq!(LogoPosition::from_name("middle"), None);
    }

    // The preset's own chain must reach the encoder unchanged and run first, or a scaling preset
    // would composite the logo at the source resolution and then shrink it with the picture.
    #[test]
    fn the_preset_chain_runs_before_the_overlay() {
        let filter = compose_logo_filter(
            "scale=-2:'min(720,ih)',ass=INPUTFILEASS,format=yuv420p",
            "DB/config/1/logo.png",
            &LogoPlacement::default(),
            None,
        );
        assert!(
            filter.starts_with("scale=-2:'min(720,ih)',ass=INPUTFILEASS,format=yuv420p[pnbase];"),
            "{filter}"
        );
        assert!(filter.contains("movie=DB/config/1/logo.png[pnlogo]"), "{filter}");
        assert!(filter.contains("[pnbase][pnlogo]overlay=W-w-24:24"), "{filter}");
        // The chain's own pixel format is restated, or the overlay picks one with alpha in it.
        assert!(filter.ends_with(",format=yuv420p"), "{filter}");
        // A still image ends after one frame; without these the encode would stop with it.
        assert!(filter.contains("eof_action=repeat:shortest=0"), "{filter}");
        // No percentage was asked for, so the image keeps the size it was uploaded at.
        assert!(!filter.contains("scale=") || !filter.contains("[pnlogo]scale"), "{filter}");
    }

    // `overlay` given an alpha overlay negotiates an alpha format for the main input too, which on
    // a 10-bit chain measured as `yuva420p`: alpha added and the video silently dropped to 8 bits,
    // which libx265 then refuses outright. The chain's own format has to be restated after it.
    #[test]
    fn the_chains_pixel_format_survives_the_overlay() {
        let ten_bit = compose_logo_filter(
            "scale=1920:1080:flags=lanczos,ass=INPUTFILEASS,format=yuv420p10le",
            "logo.png",
            &LogoPlacement::default(),
            None,
        );
        assert!(ten_bit.ends_with(",format=yuv420p10le"), "{ten_bit}");

        // The logo chain's own `format=rgba` is an intermediate step, not the output format.
        let translucent = compose_logo_filter(
            "ass=A,format=yuv420p",
            "logo.png",
            &LogoPlacement { opacity: 50, ..LogoPlacement::default() },
            None,
        );
        assert!(translucent.ends_with(",format=yuv420p"), "{translucent}");

        // A chain that declares nothing is pinned to what the overlay would have produced anyway,
        // minus the alpha channel.
        let bare = compose_logo_filter("ass=A", "logo.png", &LogoPlacement::default(), None);
        assert!(bare.ends_with(",format=yuv420p"), "{bare}");
    }

    #[test]
    fn only_a_format_filter_counts_as_the_chains_format() {
        assert_eq!(trailing_pixel_format("ass=A,format=yuv420p"), Some("yuv420p"));
        assert_eq!(trailing_pixel_format("format=nv12,scale=2:2"), Some("nv12"));
        // The last one wins: that is the one the frame reaches the encoder in.
        assert_eq!(
            trailing_pixel_format("format=rgba,scale=2:2,format=yuv420p10le"),
            Some("yuv420p10le")
        );
        // Some other filter's `format=` argument is not the chain's output format.
        assert_eq!(trailing_pixel_format("subtitles=a.ass:format=rgb"), None);
        assert_eq!(trailing_pixel_format("ass=A"), None);
        assert_eq!(trailing_pixel_format(""), None);
    }

    #[test]
    fn a_percentage_width_is_resolved_against_the_output_frame() {
        let placement = LogoPlacement {
            width_percent: Some(10),
            ..LogoPlacement::default()
        };
        let filter = compose_logo_filter("ass=INPUTFILEASS", "logo.png", &placement, Some(1920));
        assert!(filter.contains("movie=logo.png,scale=192:-1[pnlogo]"), "{filter}");

        // Without a known frame width there is nothing to take a percentage of, so the image's own
        // size is used rather than a guess.
        let unknown = compose_logo_filter("ass=INPUTFILEASS", "logo.png", &placement, None);
        assert!(!unknown.contains("scale="), "{unknown}");

        // A percentage that would round to zero pixels is still one pixel; `scale=0:-1` fails.
        assert_eq!(logo_pixel_width(10, 1), 1);
        assert_eq!(logo_pixel_width(1920, 10), 192);
        assert_eq!(logo_pixel_width(1280, 15), 192);
    }

    // A logo with no alpha channel has nothing for the mixer to scale, so the conversion has to come
    // first or a translucent watermark would come out fully opaque.
    #[test]
    fn opacity_converts_before_it_mixes_and_is_omitted_when_full() {
        let placement = LogoPlacement {
            opacity: 40,
            ..LogoPlacement::default()
        };
        let filter = compose_logo_filter("ass=A", "logo.png", &placement, None);
        let mixer = filter.find("colorchannelmixer=aa=0.400").expect("mixer");
        assert!(filter[..mixer].contains("format=rgba"), "{filter}");

        let opaque = compose_logo_filter("ass=A", "logo.png", &LogoPlacement::default(), None);
        assert!(!opaque.contains("colorchannelmixer"), "{opaque}");
    }

    // The syntax an operator types is `<every>:<visible>`, and both sides accept the units a person
    // reaches for. Everything that parses has to spell itself back the one way, because the label is
    // what the encoder is handed and what the forwarding key hashes.
    #[test]
    fn a_period_is_read_the_way_it_is_written() {
        let period = LogoPeriod::parse("5m:20s").unwrap();
        assert_eq!(period.every_seconds, 300);
        assert_eq!(period.visible_seconds, 20);
        assert_eq!(period.label(), "5m:20s");

        // `300:20` and `5m:20s` are the same cadence, so they must not look like two to anything
        // downstream that compares labels.
        assert_eq!(LogoPeriod::parse(" 300 : 20 ").unwrap().label(), "5m:20s");
        assert_eq!(LogoPeriod::parse("1h30m:90s").unwrap().label(), "1h30m:1m30s");

        assert_eq!(parse_duration_seconds("1m30"), Some(90));
        assert_eq!(parse_duration_seconds("2h"), Some(7200));
        assert_eq!(parse_duration_seconds("s"), None);
        assert_eq!(parse_duration_seconds(""), None);
        assert_eq!(parse_duration_seconds("5 minutes"), None);
        assert_eq!(format_duration_seconds(0), "0s");
        assert_eq!(format_duration_seconds(3600), "1h");
    }

    // A period typed by hand is refused rather than clamped: the operator who reversed the halves
    // meant the other order, and silently giving them a logo that never leaves hides that.
    #[test]
    fn a_cadence_that_describes_no_burst_is_refused() {
        assert!(LogoPeriod::parse("20s:5m").is_err(), "visible longer than the interval");
        assert!(LogoPeriod::parse("5m:5m").is_err(), "visible for the whole interval");
        assert!(LogoPeriod::parse("5m:0s").is_err(), "never visible");
        assert!(LogoPeriod::parse("5m").is_err(), "no colon at all");
        assert!(LogoPeriod::parse("12h:1m").is_err(), "longer than the cap");
        assert!(LogoPeriod::parse("5m:20s").is_ok());
    }

    // The fade is drawn as a stack of fixed-alpha overlays, and the whole thing only works if
    // exactly one rung is enabled at any instant: two enabled at once would composite the logo
    // twice and show it darker than either rung asks for.
    #[test]
    fn exactly_one_rung_of_the_fade_is_lit_at_a_time() {
        let period = LogoPeriod { every_seconds: 300, visible_seconds: 20 };
        let fade = period.fade_seconds();
        assert_eq!(fade, 1.0, "a twenty-second window takes the full second");

        // The enable expressions are arithmetic on `mod(t,every)`, so they can be evaluated here the
        // same way ffmpeg evaluates them.
        let lit = |elapsed: f64| -> Vec<u32> {
            (1..=LOGO_FADE_STEPS)
                .filter(|step| {
                    let entry = |level: f64| (level - 0.5) * fade / LOGO_FADE_STEPS as f64;
                    let (opens, closes) = (entry(*step as f64), 20.0 - entry(*step as f64));
                    if *step >= LOGO_FADE_STEPS {
                        return elapsed >= opens && elapsed <= closes;
                    }
                    let next = entry(*step as f64 + 1.0);
                    (elapsed >= opens && elapsed < next)
                        || (elapsed > 20.0 - next && elapsed <= closes)
                })
                .collect()
        };
        let mut sampled = 0;
        let mut ever_full = false;
        let mut ever_dark = false;
        for tick in 0..3000 {
            let elapsed = tick as f64 / 100.0;
            let on = lit(elapsed);
            assert!(on.len() <= 1, "{elapsed}s lights {on:?}");
            match on.first() {
                None => ever_dark = true,
                Some(&step) => {
                    sampled += 1;
                    ever_full |= step == LOGO_FADE_STEPS;
                }
            }
        }
        assert!(ever_full, "the logo reaches full alpha inside the window");
        assert!(ever_dark, "the logo leaves the frame between appearances");
        // Twenty of every thirty sampled seconds, minus the sliver at each end where the ramp
        // rounds to nothing.
        assert!((1900..=2000).contains(&sampled), "{sampled} lit samples of 3000");

        // Both directions of the ramp are covered: rising early in the window, falling late.
        assert_eq!(lit(0.4), lit(19.6), "the fade out mirrors the fade in");
        assert!(lit(0.02).is_empty(), "the ramp starts at nothing");
    }

    // A short appearance must not be all ramp: the fade is capped at a quarter of the window so the
    // logo still spends half its time at the alpha it was configured with.
    #[test]
    fn a_short_appearance_keeps_most_of_itself_at_full_alpha() {
        assert_eq!(LogoPeriod { every_seconds: 60, visible_seconds: 2 }.fade_seconds(), 0.5);
        assert_eq!(LogoPeriod { every_seconds: 60, visible_seconds: 30 }.fade_seconds(), 1.0);
    }

    // The stack is one `overlay` per rung, chained, with the picture split rather than decoded
    // eight times — and every rung has to carry the still-image options, or the encode stops at the
    // first frame the moment that rung is the one drawing.
    #[test]
    fn a_period_builds_a_faded_stack_of_overlays() {
        let placement = LogoPlacement {
            opacity: 50,
            period: Some(LogoPeriod { every_seconds: 300, visible_seconds: 20 }),
            ..LogoPlacement::default()
        };
        let filter = compose_logo_filter("ass=A,format=yuv420p10le", "logo.png", &placement, None);
        assert!(filter.starts_with("ass=A,format=yuv420p10le[pnbase];"), "{filter}");
        assert!(
            filter.contains(&format!("movie=logo.png,format=rgba,split={LOGO_FADE_STEPS}[pnraw1]")),
            "{filter}"
        );
        assert_eq!(
            filter.matches("eof_action=repeat:shortest=0").count(),
            LOGO_FADE_STEPS as usize,
            "{filter}"
        );
        assert_eq!(filter.matches("enable=").count(), LOGO_FADE_STEPS as usize, "{filter}");
        // The top rung is the configured opacity; the rest are fractions of it on the way there.
        assert!(filter.contains(&format!("[pnraw{LOGO_FADE_STEPS}]colorchannelmixer=aa=0.500")), "{filter}");
        assert!(filter.contains("[pnraw1]colorchannelmixer=aa=0.062"), "{filter}");
        // The pattern is anchored to the frame's own timestamp, which is what keeps a parallel chunk
        // starting twenty minutes in showing the logo at the same moments a linear encode would.
        assert!(filter.contains("mod(t,300)"), "{filter}");
        // Quoted, or the commas inside `mod()` would end the overlay's argument list early.
        assert!(filter.contains("enable='gte(mod(t,300)"), "{filter}");
        // And the chain still comes out of the stack in the format it went in as.
        assert!(filter.ends_with(",format=yuv420p10le"), "{filter}");

        // With no period nothing about the graph changes: one source, one overlay, no enable.
        let steady = compose_logo_filter("ass=A,format=yuv420p", "logo.png", &LogoPlacement::default(), None);
        assert!(!steady.contains("enable="), "{steady}");
        assert!(!steady.contains("split="), "{steady}");
    }

    // These values are stored on disk and may be hand-edited, so they are clamped on the way out
    // rather than trusted: a job should encode with a sane logo, not fail.
    #[test]
    fn stored_placement_is_clamped_rather_than_trusted() {
        let wild = LogoPlacement {
            position: LogoPosition::BottomLeft,
            margin: 99_999,
            opacity: 0,
            width_percent: Some(200),
            // On screen for longer than it waits: a cadence that describes no burst at all.
            period: Some(LogoPeriod { every_seconds: 10, visible_seconds: 60 }),
        };
        let safe = wild.sanitized();
        assert_eq!(safe.margin, MAX_LOGO_MARGIN);
        assert_eq!(safe.opacity, 1);
        assert_eq!(safe.width_percent, Some(MAX_LOGO_WIDTH_PERCENT));
        assert_eq!(safe.position, LogoPosition::BottomLeft);
        assert_eq!(safe.period, None, "an impossible cadence falls back to every frame");
    }

    // A path with a comma or a colon in it would end the filter argument early and produce an
    // ffmpeg error that names neither the logo nor the job.
    #[test]
    fn a_logo_path_is_escaped_into_the_graph() {
        let filter = compose_logo_filter(
            "ass=A",
            "C:\\work,logos\\a'b.png",
            &LogoPlacement::default(),
            None,
        );
        assert!(
            filter.contains("movie=C\\:\\\\work\\,logos\\\\a\\'b.png"),
            "{filter}"
        );
    }

    // ffmpeg's `movie` source opens the file by its extension, so a PNG stored as `.jpg` fails at
    // encode time — hours after the upload it came from, with nothing connecting the two.
    #[test]
    fn the_stored_format_comes_from_the_bytes_not_the_file_name() {
        assert_eq!(detect_logo_format(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(detect_logo_format(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]), Some("jpg"));
        assert_eq!(detect_logo_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(detect_logo_format(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(detect_logo_format(b"GIF89a"), None);
        assert_eq!(detect_logo_format(b"<svg"), None);
        assert_eq!(detect_logo_format(b""), None);
        // Short enough to fall inside a signature is not the same as matching one.
        assert_eq!(detect_logo_format(b"RIFF"), None);
    }

    #[test]
    fn only_container_formats_ffmpeg_reads_back_are_accepted() {
        assert_eq!(logo_extension_from_filename("logo.PNG"), Some("png"));
        assert_eq!(logo_extension_from_filename("logo.jpeg"), Some("jpg"));
        assert_eq!(logo_extension_from_filename("logo.jpg"), Some("jpg"));
        assert_eq!(logo_extension_from_filename("a.b.webp"), Some("webp"));
        assert_eq!(logo_extension_from_filename("logo.ass"), None);
        assert_eq!(logo_extension_from_filename("logo"), None);
        assert!(logo_extension_is_supported(".PNG"));
        assert!(!logo_extension_is_supported("gif"));
    }
}
