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

// One release's layout: `720p_<uuid>.m3u8` is the master, `720p_<uuid>_variant.m3u8` the media
// playlist it points at, and the segments sit one directory down. MPEG-TS uses
// `chunk-720p/p<n>-<uuid>.ts`; fMP4/CMAF uses an `init-<uuid>.mp4` plus `.m4s` media segments.
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

impl HlsNames {
    // A source whose height nothing could measure still has to be published; 1080p is the label the
    // upload worker falls back to for the same reason, so the two stay consistent.
    pub fn new(height: Option<u32>, id: &str, segment_type: HlsSegmentType) -> Self {
        let resolution = format!("{}p", height.filter(|height| *height > 0).unwrap_or(1080));
        let chunk_directory = format!("chunk-{resolution}");
        Self {
            segment_type,
            master: format!("{resolution}_{id}.m3u8"),
            media: format!("{resolution}_{id}_variant.m3u8"),
            chunk_pattern: format!("{chunk_directory}/p%d-{id}{}", segment_type.suffix()),
            init_segment: segment_type
                .init_suffix()
                .map(|suffix| format!("{chunk_directory}/init-{id}{suffix}")),
            chunk_base_url: format!("{chunk_directory}/"),
            chunk_directory,
        }
    }

    // The rest of a layout from the one file that names it. The publisher adopts directories it did
    // not write — pnmpeg leaves one behind for a job that muxed its own HLS — and this is how it
    // learns what the encoder called them without being told separately.
    pub fn from_media_filename(filename: &str, segment_type: HlsSegmentType) -> Option<Self> {
        let stem = filename.strip_suffix(".m3u8")?.strip_suffix("_variant")?;
        let (resolution, id) = stem.split_once('_')?;
        let height = resolution.strip_suffix('p')?.parse::<u32>().ok()?;
        if !is_uuid(id) {
            return None;
        }
        let names = Self::new(Some(height), id, segment_type);
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
    let stem = stem.strip_suffix("_variant").unwrap_or(stem);
    match stem.split_once('_') {
        Some((resolution, id)) => is_resolution(resolution) && is_uuid(id),
        None => false,
    }
}

// `chunk-720p/p<n>-<uuid>.ts` or `.m4s`. The one slash a public name is allowed to contain is the
// one this spells out, so a traversal or a nested path fails the shape rather than being stripped.
pub fn is_chunk_path(resource: &str, segment_type: HlsSegmentType) -> bool {
    let Some((directory, filename)) = resource.split_once('/') else {
        return false;
    };
    let Some(resolution) = directory.strip_prefix("chunk-") else {
        return false;
    };
    let Some(rest) = filename
        .strip_suffix(segment_type.suffix())
        .and_then(|value| value.strip_prefix('p'))
    else {
        return false;
    };
    let Some((index, id)) = rest.split_once('-') else {
        return false;
    };
    is_resolution(resolution)
        && !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && is_uuid(id)
}

// `chunk-720p/init-<uuid>.mp4`. Only fMP4 layouts have this extra public resource.
pub fn is_init_path(resource: &str) -> bool {
    let Some((directory, filename)) = resource.split_once('/') else {
        return false;
    };
    let Some(resolution) = directory.strip_prefix("chunk-") else {
        return false;
    };
    let Some(id) = filename
        .strip_suffix(".mp4")
        .and_then(|value| value.strip_prefix("init-"))
    else {
        return false;
    };
    is_resolution(resolution) && is_uuid(id)
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

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(position, byte)| {
            if matches!(position, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "f28305c9-74d0-449e-920b-938155931dd6";

    #[test]
    fn names_are_derived_from_the_output_height_and_one_id() {
        let names = HlsNames::new(Some(720), ID, HlsSegmentType::Ts);
        assert_eq!(names.master, format!("720p_{ID}.m3u8"));
        assert_eq!(names.media, format!("720p_{ID}_variant.m3u8"));
        assert_eq!(names.chunk_directory, "chunk-720p");
        assert_eq!(names.chunk_base_url, "chunk-720p/");
        assert_eq!(names.chunk_pattern, format!("chunk-720p/p%d-{ID}.ts"));
        assert_eq!(names.init_segment, None);
        assert!(is_playlist_filename(&names.master));
        assert!(is_playlist_filename(&names.media));
        assert!(is_chunk_path(
            &format!("chunk-720p/p0-{ID}.ts"),
            HlsSegmentType::Ts,
        ));
        // A height nothing could measure is still published, under the upload worker's fallback.
        assert_eq!(
            HlsNames::new(None, ID, HlsSegmentType::Ts).master,
            format!("1080p_{ID}.m3u8")
        );
    }

    #[test]
    fn a_media_playlist_names_the_layout_it_belongs_to() {
        let names = HlsNames::new(Some(1080), ID, HlsSegmentType::Ts);
        assert_eq!(
            HlsNames::from_media_filename(&names.media, HlsSegmentType::Ts),
            Some(names)
        );
        assert_eq!(
            HlsNames::from_media_filename(&format!("1080p_{ID}.m3u8"), HlsSegmentType::Ts),
            None
        );
        assert_eq!(
            HlsNames::from_media_filename("master.m3u8", HlsSegmentType::Ts),
            None
        );
        assert_eq!(
            HlsNames::from_media_filename(
                &format!("1080p_{}_variant.m3u8", ID.to_uppercase()),
                HlsSegmentType::Ts
            ),
            None
        );
        assert_eq!(
            HlsNames::from_media_filename(&format!("1080_{ID}_variant.m3u8"), HlsSegmentType::Ts),
            None
        );
    }

    #[test]
    fn fmp4_names_and_muxer_args_include_the_init_segment() {
        let names = HlsNames::new(Some(1080), ID, HlsSegmentType::Fmp4);
        assert_eq!(names.chunk_pattern, format!("chunk-1080p/p%d-{ID}.m4s"));
        assert_eq!(
            names.init_segment.as_deref(),
            Some(format!("chunk-1080p/init-{ID}.mp4").as_str())
        );
        assert!(is_init_path(names.init_segment.as_deref().unwrap()));
        assert!(names.owns_chunk_path(&format!("chunk-1080p/p42-{ID}.m4s")));
        assert!(!names.owns_chunk_path(&format!("chunk-720p/p42-{ID}.m4s")));

        let args = names.muxer_args_in(Path::new("/jobs/7/work/hls"));
        let position = |flag: &str| args.iter().position(|value| value == flag).unwrap();
        assert_eq!(args[position("-hls_segment_type") + 1], "fmp4");
        assert_eq!(
            args[position("-hls_fmp4_init_filename") + 1],
            format!("chunk-1080p/init-{ID}.mp4")
        );
    }

    #[test]
    fn the_muxer_writes_chunks_below_the_playlist_and_names_them_in_it() {
        let names = HlsNames::new(Some(720), ID, HlsSegmentType::Ts);
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
            HlsNames::new(Some(720), ID, HlsSegmentType::Ts)
                .muxer_args_in(Path::new(""))
                .last(),
            Some(&names.media)
        );
    }

    #[test]
    fn public_names_are_a_small_fixed_surface() {
        assert!(!is_playlist_filename("master.m3u8"));
        assert!(!is_playlist_filename(&format!("../720p_{ID}.m3u8")));
        assert!(!is_playlist_filename(&format!("720p_{ID}")));
        assert!(!is_playlist_filename(&format!("720_{ID}.m3u8")));
        assert!(!is_playlist_filename("720p_not-a-uuid.m3u8"));
        assert!(!is_playlist_filename(&format!(
            "720p_{}.m3u8",
            ID.to_uppercase()
        )));
        for path in [
            format!("chunk-720p/../p0-{ID}.ts"),
            format!("chunk-720p/p0-{ID}.ts.ts"),
            format!("chunk-720p/px-{ID}.ts"),
            format!("chunk-720p/sub/p0-{ID}.ts"),
            format!("p0-{ID}.ts"),
        ] {
            assert!(!is_chunk_path(&path, HlsSegmentType::Ts));
        }
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
