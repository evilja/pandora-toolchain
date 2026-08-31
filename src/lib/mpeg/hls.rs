// The names an HLS output publishes, and the ffmpeg options that produce them. Two places build
// HLS: the broker remuxes a finished MP4 when it publishes one, and pnmpeg writes the layout
// straight out of its final video/audio mux for a job whose server has made HLS the only release
// output. Both name their files here so a chunk the broker will serve cannot be spelled one way by
// the encoder and another by the route that has to recognise it.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsSegmentType {
    Ts,
    Fmp4,
}

impl HlsSegmentType {
    pub fn for_video_codec(codec: &str) -> Result<Self, String> {
        let codec = codec.trim().to_ascii_lowercase();
        if codec == "av1"
            || codec.starts_with("av1_")
            || matches!(codec.as_str(), "libaom-av1" | "libsvtav1")
        {
            return Ok(Self::Fmp4);
        }
        Ok(Self::Ts)
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Self::Ts => ".ts",
            Self::Fmp4 => ".m4s",
        }
    }

    pub fn init_suffix(self) -> Option<&'static str> {
        match self {
            Self::Ts => None,
            Self::Fmp4 => Some(".mp4"),
        }
    }
}

// What a server may spell its HLS output names with. `%uuid%` is a fresh v4 UUID, `%random%` six
// random hex characters, and `%res%` the published height as `720p`. The default keeps a UUID in
// every name — two releases from one server must not collide — and puts the resolution last so the
// name a viewer sees ends in something meaningful.
pub const DEFAULT_NAME_TEMPLATE: &str = "%uuid%_%random%_%res%";

// The suffix that separates a layout's media playlist from its master. A rendered name may not end
// in it, or the master would parse as some other layout's media playlist.
const VARIANT_SUFFIX: &str = "_variant";

// Every file in a layout sits below this one directory, whatever the release is called. It used to
// carry the resolution, which a name template may now leave out entirely; the path validators still
// admit the old spelling so a capability published before this change plays until it expires.
const CHUNK_DIRECTORY: &str = "chunk";

// Long enough for every variable at once and then some, short enough that `p<n>-<stem>.ts` stays
// well inside the 255 bytes a filesystem will accept for one name.
const MAX_STEM_LEN: usize = 180;
const MAX_TEMPLATE_LEN: usize = 160;

// One release's layout, every file in it named after the one stem the server's template produced:
// `<stem>.m3u8` is the master, `<stem>_variant.m3u8` the media playlist it points at, and the
// segments sit one directory down. MPEG-TS uses `chunk/p<n>-<stem>.ts`; fMP4/CMAF uses an
// `init-<stem>.mp4` plus `.m4s` media segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsNames {
    pub segment_type: HlsSegmentType,
    pub master: String,
    pub media: String,
    pub chunk_directory: String,
    pub chunk_base_url: String,
    pub chunk_pattern: String,
    pub init_segment: Option<String>,
}

// How long a segment is asked to be. Stream copy can only cut on a keyframe, so this is a target
// the encoder's IDR spacing rounds up: the muxer closes a segment at the first keyframe at or past
// this mark, and x264 places those on scene cuts rather than on a fixed grid.
const SEGMENT_SECONDS: &str = "4";

// The resolution a name is labelled with. A source whose height nothing could measure still has to
// be published; 1080p is the label the upload worker falls back to for the same reason, so the two
// stay consistent.
pub fn resolution_label(height: Option<u32>) -> String {
    format!("{}p", height.filter(|height| *height > 0).unwrap_or(1080))
}

// Substitutes the template's variables, refusing anything a public name may not carry. Literals are
// held to the same character set as the rendered name, so a template is rejected where an operator
// can still read the error rather than at the moment a release fails to publish.
fn expand(template: &str, resolution: &str, uuid: &str, random: &str) -> Result<String, String> {
    let mut out = String::with_capacity(template.len() + 48);
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        let (literal, tail) = rest.split_at(start);
        push_literal(literal, &mut out)?;
        let tail = &tail[1..];
        let end = tail
            .find('%')
            .ok_or_else(|| "`%` opens a variable that is never closed".to_string())?;
        match &tail[..end] {
            "uuid" => out.push_str(uuid),
            "random" => out.push_str(random),
            "res" => out.push_str(resolution),
            other => {
                return Err(format!(
                    "`%{other}%` is not a variable; use %uuid%, %random%, or %res%"
                ));
            }
        }
        rest = &tail[end + 1..];
    }
    push_literal(rest, &mut out)?;
    Ok(out)
}

fn push_literal(literal: &str, out: &mut String) -> Result<(), String> {
    for character in literal.chars() {
        if !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-') {
            return Err(format!(
                "`{character}` cannot appear in a file name; use letters, digits, `.`, `_`, or `-`"
            ));
        }
        out.push(character);
    }
    Ok(())
}

// Checks a template the way rendering one will, on a sample of what it produces, and hands back the
// trimmed template to store. Everything a name is not allowed to be is decided here rather than at
// publication, because a template that only fails on some heights or some UUIDs would be a release
// that fails long after the edit that caused it.
pub fn validate_name_template(template: &str) -> Result<String, String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("an HLS name template cannot be empty".to_string());
    }
    if template.len() > MAX_TEMPLATE_LEN {
        return Err(format!(
            "an HLS name template is at most {MAX_TEMPLATE_LEN} characters"
        ));
    }
    let sample = expand(template, "1080p", SAMPLE_UUID, SAMPLE_RANDOM)?;
    if !is_safe_stem(&sample) {
        return Err(format!(
            "`{template}` produces `{sample}`, which has to start with a letter or a digit and stay under {MAX_STEM_LEN} characters"
        ));
    }
    if sample.ends_with(VARIANT_SUFFIX) {
        return Err(format!(
            "an HLS name cannot end in `{VARIANT_SUFFIX}`; that suffix names the media playlist"
        ));
    }
    Ok(template.to_string())
}

const SAMPLE_UUID: &str = "00000000-0000-4000-8000-000000000000";
const SAMPLE_RANDOM: &str = "0f9c2a";

// The name this output's files are built from. `validate_name_template` has already refused
// anything unusable at `/edit`, but a hand-edited `meta.pandora` reaches this too, and publishing
// under a name the serving route would refuse is worse than publishing under the default one.
pub fn render_name_template(
    template: &str,
    height: Option<u32>,
    uuid: &str,
    random: &str,
) -> String {
    let resolution = resolution_label(height);
    let rendered = expand(template, &resolution, uuid, random)
        .ok()
        .filter(|name| is_safe_stem(name) && !name.ends_with(VARIANT_SUFFIX));
    match rendered {
        Some(name) => name,
        None => expand(DEFAULT_NAME_TEMPLATE, &resolution, uuid, random)
            .expect("the default HLS name template renders"),
    }
}

impl HlsNames {
    pub fn new(stem: &str, segment_type: HlsSegmentType) -> Self {
        Self {
            segment_type,
            master: format!("{stem}.m3u8"),
            media: format!("{stem}{VARIANT_SUFFIX}.m3u8"),
            chunk_pattern: format!("{CHUNK_DIRECTORY}/p%d-{stem}{}", segment_type.suffix()),
            init_segment: segment_type
                .init_suffix()
                .map(|suffix| format!("{CHUNK_DIRECTORY}/init-{stem}{suffix}")),
            chunk_base_url: format!("{CHUNK_DIRECTORY}/"),
            chunk_directory: CHUNK_DIRECTORY.to_string(),
        }
    }

    // The layout the server's own naming template asks for.
    pub fn from_template(
        template: &str,
        height: Option<u32>,
        uuid: &str,
        random: &str,
        segment_type: HlsSegmentType,
    ) -> Self {
        Self::new(
            &render_name_template(template, height, uuid, random),
            segment_type,
        )
    }

    // The rest of a layout from the one file that names it. The publisher adopts directories it did
    // not write — pnmpeg leaves one behind for a job that muxed its own HLS — and this is how it
    // learns what the encoder called them without being told separately. It cannot know the
    // template that produced the name, and does not need to: the stem is whatever is left once the
    // media playlist's own suffixes come off.
    pub fn from_media_filename(filename: &str, segment_type: HlsSegmentType) -> Option<Self> {
        let stem = filename.strip_suffix(".m3u8")?.strip_suffix(VARIANT_SUFFIX)?;
        if !is_safe_stem(stem) {
            return None;
        }
        let names = Self::new(stem, segment_type);
        (names.media == filename).then_some(names)
    }

    // The ffmpeg options that write this layout into `directory`, ending with the playlist itself.
    // The muxer records only a segment's basename in the playlist, so the chunk directory is put
    // back in front of every entry with the base URL — which stays relative however the files are
    // addressed, and is what keeps the published playlist portable. The muxer will not create the
    // directory its segments go in; that is the caller's job before the mux starts.
    pub fn muxer_args_in(&self, directory: &Path) -> Vec<String> {
        let segments = directory.join(&self.chunk_pattern);
        let playlist = directory.join(&self.media);
        let mut args = [
            "-start_number",
            "0",
            "-hls_time",
            SEGMENT_SECONDS,
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "vod",
            "-hls_flags",
            "independent_segments",
            "-hls_base_url",
            &self.chunk_base_url,
            "-hls_segment_filename",
            &segments.to_string_lossy(),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
        match self.segment_type {
            HlsSegmentType::Ts => {
                args.extend(["-hls_segment_type".to_string(), "mpegts".to_string()]);
            }
            HlsSegmentType::Fmp4 => {
                // Unlike `hls_segment_filename`, ffmpeg resolves this value against the media
                // playlist's directory. Passing an absolute path makes some ffmpeg versions join
                // the output directory twice, so the safe public relative name is also the correct
                // writer-side value.
                args.extend([
                    "-hls_segment_type".to_string(),
                    "fmp4".to_string(),
                    "-hls_fmp4_init_filename".to_string(),
                    self.init_segment.clone().expect("fMP4 has an init segment"),
                ]);
            }
        }
        args.push(playlist.to_string_lossy().to_string());
        args
    }

    pub fn owns_chunk_path(&self, resource: &str) -> bool {
        let Some((prefix, suffix)) = self.chunk_pattern.split_once("%d") else {
            return false;
        };
        let Some(index) = resource
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(suffix))
        else {
            return false;
        };
        is_chunk_path(resource, self.segment_type)
            && !index.is_empty()
            && index.bytes().all(|byte| byte.is_ascii_digit())
    }
}

// Playlist names are one part of the small HLS capability surface. Segment and init names have
// their own validators below; the broker refuses everything else before joining it onto a path.
pub fn is_playlist_filename(filename: &str) -> bool {
    let Some(stem) = filename.strip_suffix(".m3u8") else {
        return false;
    };
    is_safe_stem(stem.strip_suffix(VARIANT_SUFFIX).unwrap_or(stem))
}

// `chunk/p<n>-<stem>.ts` or `.m4s`. The one slash a public name is allowed to contain is the one
// this spells out, so a traversal or a nested path fails the shape rather than being stripped.
pub fn is_chunk_path(resource: &str, segment_type: HlsSegmentType) -> bool {
    let Some((directory, filename)) = resource.split_once('/') else {
        return false;
    };
    if !is_chunk_directory(directory) {
        return false;
    }
    let Some(rest) = filename
        .strip_suffix(segment_type.suffix())
        .and_then(|value| value.strip_prefix('p'))
    else {
        return false;
    };
    let Some((index, stem)) = rest.split_once('-') else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) && is_safe_stem(stem)
}

// `chunk/init-<stem>.mp4`. Only fMP4 layouts have this extra public resource.
pub fn is_init_path(resource: &str) -> bool {
    let Some((directory, filename)) = resource.split_once('/') else {
        return false;
    };
    if !is_chunk_directory(directory) {
        return false;
    }
    match filename
        .strip_suffix(".mp4")
        .and_then(|value| value.strip_prefix("init-"))
    {
        Some(stem) => is_safe_stem(stem),
        None => false,
    }
}

// `chunk`, or the `chunk-<height>p` a release published before names became configurable is still
// being served under.
fn is_chunk_directory(directory: &str) -> bool {
    directory == CHUNK_DIRECTORY
        || directory
            .strip_prefix("chunk-")
            .map(is_resolution)
            .unwrap_or(false)
}

fn is_resolution(value: &str) -> bool {
    match value.strip_suffix('p') {
        Some(height) => {
            !height.is_empty()
                && height.len() <= 5
                && height.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => false,
    }
}

// What every public name in a layout is built from, and the only thing standing between an
// operator's template and a path join: no separator, no relative component, nothing that reads as
// one, and nothing that would need escaping in a playlist or a URL.
fn is_safe_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= MAX_STEM_LEN
        && !stem.starts_with('.')
        && !stem.starts_with('-')
        && !stem.contains("..")
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "f28305c9-74d0-449e-920b-938155931dd6";
    const RANDOM: &str = "a1b2c3";

    fn default_names(height: Option<u32>, segment_type: HlsSegmentType) -> HlsNames {
        HlsNames::from_template(DEFAULT_NAME_TEMPLATE, height, ID, RANDOM, segment_type)
    }

    #[test]
    fn the_default_template_names_a_layout_after_the_uuid_the_random_and_the_height() {
        let names = default_names(Some(720), HlsSegmentType::Ts);
        let stem = format!("{ID}_{RANDOM}_720p");
        assert_eq!(names.master, format!("{stem}.m3u8"));
        assert_eq!(names.media, format!("{stem}_variant.m3u8"));
        assert_eq!(names.chunk_directory, "chunk");
        assert_eq!(names.chunk_base_url, "chunk/");
        assert_eq!(names.chunk_pattern, format!("chunk/p%d-{stem}.ts"));
        assert_eq!(names.init_segment, None);
        assert!(is_playlist_filename(&names.master));
        assert!(is_playlist_filename(&names.media));
        assert!(is_chunk_path(&format!("chunk/p0-{stem}.ts"), HlsSegmentType::Ts));
        // A height nothing could measure is still published, under the upload worker's fallback.
        assert_eq!(
            default_names(None, HlsSegmentType::Ts).master,
            format!("{ID}_{RANDOM}_1080p.m3u8")
        );
    }

    #[test]
    fn a_template_places_each_variable_where_the_server_asked_for_it() {
        let names = HlsNames::from_template("%res%_%uuid%", Some(480), ID, RANDOM, HlsSegmentType::Ts);
        assert_eq!(names.master, format!("480p_{ID}.m3u8"));
        assert_eq!(
            render_name_template("ep-%random%", Some(1080), ID, RANDOM),
            format!("ep-{RANDOM}")
        );
        // Nothing forces a variable in at all, and one may appear more than once.
        assert_eq!(render_name_template("static", Some(720), ID, RANDOM), "static");
        assert_eq!(
            render_name_template("%res%.%res%", Some(720), ID, RANDOM),
            "720p.720p"
        );
    }

    #[test]
    fn a_template_only_accepts_the_variables_and_characters_a_name_can_carry() {
        assert_eq!(
            validate_name_template("  %uuid%_%res%  ").as_deref(),
            Ok("%uuid%_%res%")
        );
        for template in [
            "",
            "   ",
            "%episode%",
            "%uuid",
            "name with spaces",
            "a/b-%uuid%",
            "../%uuid%",
            ".hidden-%uuid%",
            "-%uuid%",
            "%uuid%_variant",
            &"x".repeat(MAX_TEMPLATE_LEN + 1),
        ] {
            assert!(
                validate_name_template(template).is_err(),
                "`{template}` should be refused"
            );
        }
    }

    #[test]
    fn an_unusable_template_publishes_under_the_default_rather_than_an_unservable_name() {
        // Only reachable by hand-editing `meta.pandora`; `/edit` refuses all of these.
        for template in ["", "../%uuid%", "%episode%", "%uuid%_variant"] {
            let stem = render_name_template(template, Some(720), ID, RANDOM);
            assert_eq!(stem, format!("{ID}_{RANDOM}_720p"));
            assert!(is_playlist_filename(&format!("{stem}.m3u8")));
        }
    }

    #[test]
    fn a_media_playlist_names_the_layout_it_belongs_to() {
        let names = default_names(Some(1080), HlsSegmentType::Ts);
        assert_eq!(
            HlsNames::from_media_filename(&names.media, HlsSegmentType::Ts),
            Some(names.clone())
        );
        assert_eq!(
            HlsNames::from_media_filename(&names.master, HlsSegmentType::Ts),
            None
        );
        assert_eq!(
            HlsNames::from_media_filename("master.m3u8", HlsSegmentType::Ts),
            None
        );
        assert_eq!(
            HlsNames::from_media_filename("../x_variant.m3u8", HlsSegmentType::Ts),
            None
        );
        // A layout named by some other server's template is still adoptable: the stem is read off
        // the file rather than re-derived from a template the publisher does not know.
        assert_eq!(
            HlsNames::from_media_filename("Ep12.480p_variant.m3u8", HlsSegmentType::Ts)
                .map(|names| names.master),
            Some("Ep12.480p.m3u8".to_string())
        );
    }

    #[test]
    fn fmp4_names_and_muxer_args_include_the_init_segment() {
        let names = default_names(Some(1080), HlsSegmentType::Fmp4);
        let stem = format!("{ID}_{RANDOM}_1080p");
        assert_eq!(names.chunk_pattern, format!("chunk/p%d-{stem}.m4s"));
        assert_eq!(
            names.init_segment.as_deref(),
            Some(format!("chunk/init-{stem}.mp4").as_str())
        );
        assert!(is_init_path(names.init_segment.as_deref().unwrap()));
        assert!(names.owns_chunk_path(&format!("chunk/p42-{stem}.m4s")));
        assert!(!names.owns_chunk_path(&format!("chunk/p42-{ID}_{RANDOM}_720p.m4s")));

        let args = names.muxer_args_in(Path::new("/jobs/7/work/hls"));
        let position = |flag: &str| args.iter().position(|value| value == flag).unwrap();
        assert_eq!(args[position("-hls_segment_type") + 1], "fmp4");
        assert_eq!(
            args[position("-hls_fmp4_init_filename") + 1],
            format!("chunk/init-{stem}.mp4")
        );
    }

    #[test]
    fn the_muxer_writes_chunks_below_the_playlist_and_names_them_in_it() {
        let names = default_names(Some(720), HlsSegmentType::Ts);
        let args = names.muxer_args_in(Path::new("/jobs/7/work/hls"));
        let position = |flag: &str| args.iter().position(|value| value == flag).unwrap();
        assert_eq!(
            args[position("-hls_segment_filename") + 1],
            format!("/jobs/7/work/hls/{}", names.chunk_pattern)
        );
        // Deliberate, and the number players buffer around: the segment target is asserted here
        // rather than against the constant so that changing it has to be a decision.
        assert_eq!(args[position("-hls_time") + 1], "4");
        // Relative however the files themselves are addressed: this is what a player resolves
        // against the playlist's own URL, not a path on the encoder's disk.
        assert_eq!(args[position("-hls_base_url") + 1], names.chunk_base_url);
        assert_eq!(
            args.last().unwrap(),
            &format!("/jobs/7/work/hls/{}", names.media)
        );
        // A caller that runs the mux inside the directory addresses the same files by name.
        assert_eq!(
            default_names(Some(720), HlsSegmentType::Ts)
                .muxer_args_in(Path::new(""))
                .last(),
            Some(&names.media)
        );
    }

    #[test]
    fn public_names_stay_a_shape_and_never_a_path() {
        let stem = format!("{ID}_{RANDOM}_720p");
        assert!(is_playlist_filename("Ep12.480p.m3u8"));
        assert!(!is_playlist_filename(&format!("../{stem}.m3u8")));
        assert!(!is_playlist_filename(&format!("sub/{stem}.m3u8")));
        assert!(!is_playlist_filename(&stem));
        assert!(!is_playlist_filename(".m3u8"));
        assert!(!is_playlist_filename(".hidden.m3u8"));
        for path in [
            format!("chunk/../p0-{stem}.ts"),
            format!("chunk/px-{stem}.ts"),
            format!("chunk/sub/p0-{stem}.ts"),
            format!("chunk/p0-.ts"),
            format!("p0-{stem}.ts"),
            format!("chunks/p0-{stem}.ts"),
        ] {
            assert!(!is_chunk_path(&path, HlsSegmentType::Ts), "{path}");
        }
        assert!(!is_init_path(&format!("chunk/init-../{stem}.mp4")));
        assert!(!is_init_path(&format!("chunk/{stem}.mp4")));
    }

    #[test]
    fn a_layout_published_under_the_old_chunk_directory_still_serves() {
        // A capability minted before names became configurable keeps playing until it expires.
        assert!(is_chunk_path(&format!("chunk-720p/p0-720p_{ID}.ts"), HlsSegmentType::Ts));
        assert!(is_init_path(&format!("chunk-1080p/init-1080p_{ID}.mp4")));
        assert!(is_playlist_filename(&format!("720p_{ID}_variant.m3u8")));
        assert!(!is_chunk_path(&format!("chunk-720/p0-720p_{ID}.ts"), HlsSegmentType::Ts));
    }

    #[test]
    fn segment_type_selects_fmp4_for_av1_encoders() {
        assert_eq!(
            HlsSegmentType::for_video_codec("h264"),
            Ok(HlsSegmentType::Ts)
        );
        assert_eq!(
            HlsSegmentType::for_video_codec("av1"),
            Ok(HlsSegmentType::Fmp4)
        );
        assert_eq!(
            HlsSegmentType::for_video_codec("av1_nvenc"),
            Ok(HlsSegmentType::Fmp4)
        );
        assert_eq!(
            HlsSegmentType::for_video_codec("libsvtav1"),
            Ok(HlsSegmentType::Fmp4)
        );
    }
}
