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
    if placement.opacity < 100 {
        // `format=rgba` first, because a logo that arrived without an alpha channel has nothing for
        // the mixer to scale and would come out fully opaque.
        logo_chain.push_str(&format!(
            ",format=rgba,colorchannelmixer=aa={:.3}",
            placement.opacity as f64 / 100.0
        ));
    }
    let (x, y) = placement.position.overlay_expressions(placement.margin);
    // `overlay` negotiates its own format, and given an alpha-carrying overlay it will pick one for
    // the main input too: a 10-bit chain measured as `yuva420p` on the way in, which both adds an
    // alpha channel no release wants and quietly drops the video to 8 bits — and libx265 then
    // refuses the stream outright with "does not support alpha layer encoding". Restating the
    // chain's own pixel format after the overlay pins both back.
    let output_format = trailing_pixel_format(base_filter).unwrap_or(FALLBACK_PIXEL_FORMAT);
    // `shortest=0` and `eof_action=repeat` keep a still image on screen for the whole episode: an
    // overlay input that ends after one frame would otherwise stop the encode at frame one.
    format!(
        "{base_filter}[pnbase];{logo_chain}[pnlogo];[pnbase][pnlogo]overlay={x}:{y}:eof_action=repeat:shortest=0,format={output_format}"
    )
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

    // These values are stored on disk and may be hand-edited, so they are clamped on the way out
    // rather than trusted: a job should encode with a sane logo, not fail.
    #[test]
    fn stored_placement_is_clamped_rather_than_trusted() {
        let wild = LogoPlacement {
            position: LogoPosition::BottomLeft,
            margin: 99_999,
            opacity: 0,
            width_percent: Some(200),
        };
        let safe = wild.sanitized();
        assert_eq!(safe.margin, MAX_LOGO_MARGIN);
        assert_eq!(safe.opacity, 1);
        assert_eq!(safe.width_percent, Some(MAX_LOGO_WIDTH_PERCENT));
        assert_eq!(safe.position, LogoPosition::BottomLeft);
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
