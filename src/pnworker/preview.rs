use crate::lib::image::{Align, Canvas, Color, Font, ImageError, ImageResult, TextOptions};
use crate::libkagami::complex::types::AssTime;
use crate::libkagami::core::{Event, Stamp, SubstationAlpha};
use std::collections::BTreeSet;

const TYPESET_JOIN_GAP_CS: u64 = 100;
pub const DEFAULT_COOLDOWN_CS: u64 = 9_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewShot {
    pub centiseconds: u64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Selected,
    DeferredCooldown,
    BackfilledCooldown,
    SkippedGap,
    OverQuota,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterVerdict {
    pub rank: usize,
    pub span: (u64, u64),
    pub shot_cs: u64,
    pub fn_lines: usize,
    pub drawing_lines: usize,
    pub lines: usize,
    pub max_tags: usize,
    pub outcome: Verdict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewSelection {
    pub shots: Vec<PreviewShot>,
    pub verdicts: Vec<ClusterVerdict>,
    pub ranking_log: String,
}

#[derive(Clone, Debug)]
struct ClusterMember {
    start_cs: u64,
    end_cs: u64,
    midpoint_cs: u64,
    has_drawing: bool,
    tag_count: usize,
}

#[derive(Clone, Debug)]
struct TypesetCluster {
    start_cs: u64,
    end_cs: u64,
    lines: usize,
    fn_lines: usize,
    drawing_lines: usize,
    max_tags: usize,
    heavy_mid_cs: Option<u64>,
    members: Vec<ClusterMember>,
}

#[derive(Clone, Copy)]
enum LabelWeight {
    Ranked(f64),
    Stamped,
}

pub fn select_preview_shots(
    ts: &SubstationAlpha,
    max: usize,
    min_gap_cs: u64,
) -> PreviewSelection {
    select_shots_with_stamps_and_cooldown(
        &[ts],
        Some(ts),
        max,
        min_gap_cs,
        DEFAULT_COOLDOWN_CS,
    )
}

// Every script supplies manual stamps; only the explicit TS supplies automatic candidates.
pub fn select_shots_with_stamps(
    scripts: &[&SubstationAlpha],
    ts: Option<&SubstationAlpha>,
    max: usize,
    min_gap_cs: u64,
) -> PreviewSelection {
    select_shots_with_stamps_and_cooldown(
        scripts,
        ts,
        max,
        min_gap_cs,
        DEFAULT_COOLDOWN_CS,
    )
}

pub fn select_shots_with_stamps_and_cooldown(
    scripts: &[&SubstationAlpha],
    ts: Option<&SubstationAlpha>,
    max: usize,
    min_gap_cs: u64,
    cooldown_cs: u64,
) -> PreviewSelection {
    let stamps = collect_stamps(scripts);
    if !stamps.is_empty() {
        let shots = stamps
            .into_iter()
            .take(max)
            .map(|stamp| stamp_shot(&stamp))
            .collect();
        let verdicts = Vec::new();
        let ranking_log = "manual stamps selected; automatic ranking skipped\n".to_string();
        return PreviewSelection {
            shots,
            verdicts,
            ranking_log,
        };
    }

    let Some(ts) = ts else {
        return PreviewSelection {
            shots: Vec::new(),
            verdicts: Vec::new(),
            ranking_log: format_ranking_table(&[]),
        };
    };
    select_clusters(ts, max, min_gap_cs, cooldown_cs)
}

fn collect_stamps(scripts: &[&SubstationAlpha]) -> Vec<Stamp> {
    let mut stamps = scripts
        .iter()
        .flat_map(|script| script.pandora_meta.stamps.iter().cloned())
        .collect::<Vec<_>>();
    stamps.sort_by_key(|stamp| {
        (
            stamp.start.total_centiseconds(),
            stamp.end.total_centiseconds(),
        )
    });
    let mut seen = BTreeSet::new();
    stamps.retain(|stamp| seen.insert(stamp_midpoint(stamp)));
    stamps
}

fn stamp_midpoint(stamp: &Stamp) -> u64 {
    let start = stamp.start.total_centiseconds();
    let end = stamp.end.total_centiseconds();
    if end <= start {
        start
    } else {
        start + (end - start) / 2
    }
}

fn stamp_shot(stamp: &Stamp) -> PreviewShot {
    let start = stamp.start.total_centiseconds();
    let end = stamp.end.total_centiseconds();
    let centiseconds = stamp_midpoint(stamp);
    PreviewShot {
        centiseconds,
        label: format_shot_label(
            centiseconds,
            end.saturating_sub(start),
            LabelWeight::Stamped,
            Some(&stamp.note),
        ),
    }
}

fn select_clusters(
    ts: &SubstationAlpha,
    max: usize,
    min_gap_cs: u64,
    cooldown_cs: u64,
) -> PreviewSelection {
    let mut ranked = typeset_clusters(&ts.events, TYPESET_JOIN_GAP_CS);
    ranked.sort_by(compare_clusters);

    let ranks = dense_ranks(&ranked);
    let weights = normalized_weights(&ranks);
    let mut outcomes = vec![Verdict::OverQuota; ranked.len()];
    let mut accepted = Vec::<usize>::new();
    let mut deferred = Vec::<usize>::new();

    for (idx, cluster) in ranked.iter().enumerate() {
        if accepted.len() >= max {
            continue;
        }
        let shot_cs = cluster.shot_cs();
        if accepted.iter().any(|accepted_idx| {
            ranked[*accepted_idx].shot_cs().abs_diff(shot_cs) < min_gap_cs
        }) {
            outcomes[idx] = Verdict::SkippedGap;
            continue;
        }
        if accepted.iter().any(|accepted_idx| {
            let accepted_cluster = &ranked[*accepted_idx];
            shot_cs > accepted_cluster.end_cs
                && shot_cs <= accepted_cluster.end_cs.saturating_add(cooldown_cs)
        }) {
            outcomes[idx] = Verdict::DeferredCooldown;
            deferred.push(idx);
            continue;
        }
        outcomes[idx] = Verdict::Selected;
        accepted.push(idx);
    }

    for idx in deferred {
        if accepted.len() >= max {
            break;
        }
        let shot_cs = ranked[idx].shot_cs();
        if accepted.iter().any(|accepted_idx| {
            ranked[*accepted_idx].shot_cs().abs_diff(shot_cs) < min_gap_cs
        }) {
            outcomes[idx] = Verdict::SkippedGap;
            continue;
        }
        outcomes[idx] = Verdict::BackfilledCooldown;
        accepted.push(idx);
    }

    let mut shots = accepted
        .into_iter()
        .map(|idx| {
            let cluster = &ranked[idx];
            let centiseconds = cluster.shot_cs();
            PreviewShot {
                centiseconds,
                label: format_shot_label(
                    centiseconds,
                    cluster.end_cs.saturating_sub(cluster.start_cs),
                    LabelWeight::Ranked(weights[idx]),
                    None,
                ),
            }
        })
        .collect::<Vec<_>>();
    shots.sort_by_key(|shot| shot.centiseconds);

    let verdicts = ranked
        .iter()
        .enumerate()
        .map(|(idx, cluster)| ClusterVerdict {
            rank: ranks[idx],
            span: (cluster.start_cs, cluster.end_cs),
            shot_cs: cluster.shot_cs(),
            fn_lines: cluster.fn_lines,
            drawing_lines: cluster.drawing_lines,
            lines: cluster.lines,
            max_tags: cluster.max_tags,
            outcome: outcomes[idx].clone(),
        })
        .collect::<Vec<_>>();
    let ranking_log = format_ranking_table(&verdicts);
    PreviewSelection {
        shots,
        verdicts,
        ranking_log,
    }
}

fn typeset_clusters(events: &[Event], join_gap_cs: u64) -> Vec<TypesetCluster> {
    let mut candidates = events
        .iter()
        .filter_map(|event| {
            let start_cs = event.start.total_centiseconds();
            let end_cs = event.end.total_centiseconds();
            (end_cs > start_cs).then_some((event, start_cs, end_cs))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, start, end)| (*start, *end));

    let mut clusters = Vec::<TypesetCluster>::new();
    for (event, start_cs, end_cs) in candidates {
        let member = ClusterMember {
            start_cs,
            end_cs,
            midpoint_cs: start_cs + (end_cs - start_cs) / 2,
            has_drawing: event.has_drawing(),
            tag_count: event.tag_count(),
        };
        let joins = clusters
            .last()
            .map(|cluster| start_cs <= cluster.end_cs.saturating_add(join_gap_cs))
            .unwrap_or(false);
        if joins {
            let cluster = clusters.last_mut().unwrap();
            cluster.end_cs = cluster.end_cs.max(end_cs);
            cluster.lines += 1;
            cluster.fn_lines += usize::from(event.has_fn());
            cluster.drawing_lines += usize::from(member.has_drawing);
            cluster.max_tags = cluster.max_tags.max(member.tag_count);
            cluster.members.push(member);
        } else {
            clusters.push(TypesetCluster {
                start_cs,
                end_cs,
                lines: 1,
                fn_lines: usize::from(event.has_fn()),
                drawing_lines: usize::from(member.has_drawing),
                max_tags: member.tag_count,
                heavy_mid_cs: None,
                members: vec![member],
            });
        }
    }
    for cluster in &mut clusters {
        cluster.finish_placement();
    }
    clusters
}

impl TypesetCluster {
    fn finish_placement(&mut self) {
        if self.drawing_lines == 0 && self.max_tags == 0 {
            return;
        }
        let center = self.start_cs + (self.end_cs - self.start_cs) / 2;
        self.heavy_mid_cs = self
            .members
            .iter()
            .max_by(|left, right| {
                (left.has_drawing, left.tag_count)
                    .cmp(&(right.has_drawing, right.tag_count))
                    .then_with(|| right.midpoint_cs.abs_diff(center).cmp(&left.midpoint_cs.abs_diff(center)))
                    .then_with(|| right.midpoint_cs.cmp(&left.midpoint_cs))
            })
            .map(|member| member.midpoint_cs);
    }

    fn shot_cs(&self) -> u64 {
        if let Some(midpoint) = self.heavy_mid_cs {
            return midpoint;
        }
        let center = self.start_cs + (self.end_cs - self.start_cs) / 2;
        if self
            .members
            .iter()
            .any(|member| member.start_cs < center && center < member.end_cs)
        {
            return center;
        }
        self.members
            .iter()
            .min_by_key(|member| (member.midpoint_cs.abs_diff(center), member.midpoint_cs))
            .map(|member| member.midpoint_cs)
            .unwrap_or(center)
    }

    fn duration(&self) -> u64 {
        self.end_cs.saturating_sub(self.start_cs)
    }
}

fn compare_clusters(left: &TypesetCluster, right: &TypesetCluster) -> std::cmp::Ordering {
    (right.fn_lines > 0)
        .cmp(&(left.fn_lines > 0))
        .then_with(|| right.drawing_lines.cmp(&left.drawing_lines))
        .then_with(|| right.lines.cmp(&left.lines))
        .then_with(|| right.max_tags.cmp(&left.max_tags))
        .then_with(|| right.duration().cmp(&left.duration()))
        .then_with(|| left.start_cs.cmp(&right.start_cs))
}

fn same_rank(left: &TypesetCluster, right: &TypesetCluster) -> bool {
    (left.fn_lines > 0) == (right.fn_lines > 0)
        && left.drawing_lines == right.drawing_lines
        && left.lines == right.lines
        && left.max_tags == right.max_tags
        && left.duration() == right.duration()
}

fn dense_ranks(clusters: &[TypesetCluster]) -> Vec<usize> {
    let mut ranks = Vec::with_capacity(clusters.len());
    let mut rank = 0;
    for (idx, cluster) in clusters.iter().enumerate() {
        if idx == 0 || !same_rank(&clusters[idx - 1], cluster) {
            rank += 1;
        }
        ranks.push(rank);
    }
    ranks
}

fn normalized_weights(ranks: &[usize]) -> Vec<f64> {
    let distinct = ranks.last().copied().unwrap_or(0);
    if distinct <= 1 {
        return vec![1.0; ranks.len()];
    }
    ranks
        .iter()
        .map(|rank| 1.0 - (*rank as f64 - 1.0) / (distinct as f64 - 1.0))
        .collect()
}

fn format_shot_label(
    shot_cs: u64,
    length_cs: u64,
    weight: LabelWeight,
    note: Option<&str>,
) -> String {
    let weight = match weight {
        LabelWeight::Ranked(value) => format!("{value:.2}"),
        LabelWeight::Stamped => "+".to_string(),
    };
    let mut label = format!(
        "{} · {:.1}s · {}",
        AssTime::from_centiseconds(shot_cs),
        length_cs as f64 / 100.0,
        weight
    );
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        label.push_str(" · ");
        label.push_str(&truncate_note(note, 40));
    }
    label
}

fn truncate_note(note: &str, max_chars: usize) -> String {
    if note.chars().count() <= max_chars {
        return note.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", note.chars().take(keep).collect::<String>())
}

pub fn format_ranking_table(verdicts: &[ClusterVerdict]) -> String {
    let mut out = String::from(
        "rank  span                           shot         fn  drw  lines  tags  outcome\n",
    );
    for verdict in verdicts {
        out.push_str(&format!(
            "{:<5} {:<30} {:<12} {:<3} {:<4} {:<6} {:<5} {}\n",
            verdict.rank,
            format!(
                "{}-{}",
                AssTime::from_centiseconds(verdict.span.0),
                AssTime::from_centiseconds(verdict.span.1)
            ),
            AssTime::from_centiseconds(verdict.shot_cs),
            if verdict.fn_lines > 0 { "y" } else { "n" },
            verdict.drawing_lines,
            verdict.lines,
            verdict.max_tags,
            verdict_label(&verdict.outcome),
        ));
    }
    out
}

fn verdict_label(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Selected => "selected",
        Verdict::DeferredCooldown => "deferred-cooldown",
        Verdict::BackfilledCooldown => "backfilled",
        Verdict::SkippedGap => "skipped-gap",
        Verdict::OverQuota => "over-quota",
    }
}

pub fn compose_preview(
    frame_png: &[u8],
    label: &str,
    watermark_font: &Font,
    label_font: &Font,
) -> ImageResult<Vec<u8>> {
    let mut canvas = Canvas::from_png_bytes(frame_png)?;
    let height = canvas.height() as f32;
    let width = canvas.width() as f32;
    let size = (height / 30.0).clamp(16.0, 48.0);
    let margin = (height / 60.0).clamp(4.0, 32.0);
    let shadow = 2.0_f32.max(size / 18.0);
    let line_height = 1.2;

    let label_shadow = TextOptions {
        x: margin + shadow,
        y: margin + shadow,
        size,
        color: Color::BLACK,
        align: Align::Left,
        max_width: None,
        line_height,
    };
    canvas.draw_text(label, label_font, &label_shadow)?;
    let label_text = TextOptions {
        x: margin,
        y: margin,
        size,
        color: Color::WHITE,
        align: Align::Left,
        max_width: None,
        line_height,
    };
    canvas.draw_text(label, label_font, &label_text)?;

    let watermark = "pandora tools";
    let watermark_y = height - margin - size * line_height;
    let watermark_shadow = TextOptions {
        x: width - margin + shadow,
        y: watermark_y + shadow,
        size,
        color: Color::BLACK,
        align: Align::Right,
        max_width: None,
        line_height,
    };
    canvas.draw_text(watermark, watermark_font, &watermark_shadow)?;
    let watermark_text = TextOptions {
        x: width - margin,
        y: watermark_y,
        size,
        color: Color {
            r: 255,
            g: 255,
            b: 255,
            a: 140,
        },
        align: Align::Right,
        max_width: None,
        line_height,
    };
    canvas.draw_text(watermark, watermark_font, &watermark_text)?;

    canvas.png_bytes()
}

pub fn merge_previews(frames: &[Vec<u8>]) -> ImageResult<Vec<u8>> {
    const GUTTER: u32 = 8;

    if frames.is_empty() {
        return Err(ImageError::Dimensions("preview frames must not be empty".to_string()));
    }
    let decoded = frames
        .iter()
        .map(|frame| Canvas::from_png_bytes(frame))
        .collect::<ImageResult<Vec<_>>>()?;
    let width = decoded.iter().map(Canvas::width).max().unwrap();
    let frames_height = decoded
        .iter()
        .try_fold(0u32, |height, frame| height.checked_add(frame.height()))
        .ok_or_else(|| ImageError::Dimensions("merged preview height overflowed".to_string()))?;
    let gutters = GUTTER
        .checked_mul(decoded.len().saturating_sub(1) as u32)
        .ok_or_else(|| ImageError::Dimensions("merged preview gutter height overflowed".to_string()))?;
    let height = frames_height
        .checked_add(gutters)
        .ok_or_else(|| ImageError::Dimensions("merged preview height overflowed".to_string()))?;
    let mut merged = Canvas::new(width, height, Color::BLACK)?;
    let mut y = 0u32;
    for frame in &decoded {
        let x = (width - frame.width()) / 2;
        merged.blit(frame, x, y);
        y += frame.height() + GUTTER;
    }
    merged.png_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::image::{Canvas, Color};
    use crate::libkagami::complex::types::AssColour;
    use crate::libkagami::core::{Event, PandoraMeta, ScriptInfo, Stamp, V4pStyle};
    use crate::libkagami::tags::ASSLine;

    fn test_sub(events: Vec<Event>) -> SubstationAlpha {
        SubstationAlpha {
            script_info: ScriptInfo {
                title: String::new(),
                script_type: "v4.00+".to_string(),
                wrap_style: 2,
                scaled_border_and_shadow: true,
                playresx: 640,
                playresy: 480,
                ycbcr_matrix: "TV.709".to_string(),
                layout_res_x: 640,
                layout_res_y: 480,
            },
            v4p_styles: vec![V4pStyle {
                name: "Default".to_string(),
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
                outline: 2.0,
                shadow: 2.0,
                alignment: 2,
                margin_l: 10,
                margin_r: 10,
                margin_v: 10,
                encoding: 1,
            }],
            events,
            comments: Vec::new(),
            pandora_meta: PandoraMeta::default(),
        }
    }

    fn event(start: u64, end: u64, text: &str) -> Event {
        Event {
            layer: 0,
            start: AssTime::from_centiseconds(start),
            end: AssTime::from_centiseconds(end),
            style: "Default".to_string(),
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            text: text.parse::<ASSLine>().unwrap(),
        }
    }

    fn stamp(start: u64, end: u64, note: &str) -> Stamp {
        Stamp {
            start: AssTime::from_centiseconds(start),
            end: AssTime::from_centiseconds(end),
            note: note.to_string(),
        }
    }

    #[test]
    fn plain_timed_lines_are_preview_candidates() {
        let sub = test_sub(vec![event(0, 200, "plain sign")]);
        let selection = select_preview_shots(&sub, 3, 1000);
        assert_eq!(selection.shots.len(), 1);
        assert_eq!(selection.shots[0].centiseconds, 100);
        assert!(selection.shots[0].label.ends_with("2.0s · 1.00"));
    }

    #[test]
    fn overlapping_stack_is_one_centered_cluster() {
        let sub = test_sub((0..10).map(|_| event(0, 400, "plain")).collect());
        let selection = select_preview_shots(&sub, 3, 1000);
        assert_eq!(selection.shots.len(), 1);
        assert_eq!(selection.shots[0].centiseconds, 200);
        assert_eq!(selection.verdicts[0].lines, 10);
    }

    #[test]
    fn cluster_join_tolerance_and_seam_snapping_are_applied() {
        let joined = test_sub(vec![event(0, 400, "a"), event(450, 900, "b")]);
        let clusters = typeset_clusters(&joined.events, TYPESET_JOIN_GAP_CS);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].shot_cs(), 675);

        let split = test_sub(vec![event(0, 400, "a"), event(501, 900, "b")]);
        assert_eq!(typeset_clusters(&split.events, TYPESET_JOIN_GAP_CS).len(), 2);
    }

    #[test]
    fn fn_presence_outranks_a_larger_plain_cluster() {
        let mut events = (0..10)
            .map(|_| event(0, 200, "plain"))
            .collect::<Vec<_>>();
        events.push(event(20_000, 20_200, r"{\fnFancy}a"));
        events.push(event(20_000, 20_200, "layer"));
        let selection = select_preview_shots(&test_sub(events), 1, 1000);
        assert_eq!(selection.shots[0].centiseconds, 20_100);
        assert!(selection.verdicts[0].fn_lines > 0);
    }

    #[test]
    fn denser_cluster_wins_the_contested_slot() {
        let sub = test_sub(vec![
            event(0, 200, "a"),
            event(20_000, 20_200, "b"),
            event(40_000, 40_200, "c1"),
            event(40_000, 40_200, "c2"),
            event(40_000, 40_200, "c3"),
        ]);
        let selection = select_preview_shots(&sub, 1, 1000);
        assert_eq!(selection.shots[0].centiseconds, 40_100);
        assert_eq!(selection.verdicts[0].lines, 3);
    }

    #[test]
    fn heavy_member_controls_cluster_placement() {
        let sub = test_sub(vec![
            event(0, 3000, "plain"),
            event(200, 600, r"{\pos(10,20)\fs40}heavy"),
        ]);
        let selection = select_preview_shots(&sub, 1, 1000);
        assert_eq!(selection.shots[0].centiseconds, 400);
    }

    #[test]
    fn hard_gap_is_measured_between_actual_shot_times() {
        let sub = test_sub(vec![
            event(0, 200, r"{\pos(1,2)}a"),
            event(500, 700, "b"),
        ]);
        let selection = select_preview_shots(&sub, 3, 1000);
        assert_eq!(selection.shots.len(), 1);
        assert!(selection
            .verdicts
            .iter()
            .any(|row| row.outcome == Verdict::SkippedGap));
    }

    #[test]
    fn cooldown_defers_then_backfills_only_when_needed() {
        let base = test_sub(vec![
            event(2900, 3100, "a"),
            event(5900, 6100, "b"),
            event(29_900, 30_100, "c"),
        ]);
        let selection = select_preview_shots(&base, 3, 1000);
        assert_eq!(
            selection
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![3000, 6000, 30_000]
        );
        assert!(selection
            .verdicts
            .iter()
            .any(|row| row.shot_cs == 6000 && row.outcome == Verdict::BackfilledCooldown));

        let spread = test_sub(vec![
            event(2900, 3100, "a"),
            event(5900, 6100, "b"),
            event(29_900, 30_100, "c"),
            event(47_900, 48_100, "d"),
        ]);
        let selection = select_preview_shots(&spread, 3, 1000);
        assert_eq!(
            selection
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![3000, 30_000, 48_000]
        );
    }

    #[test]
    fn configurable_cooldown_can_be_disabled() {
        let sub = test_sub(vec![
            event(2900, 3100, "a"),
            event(5900, 6100, "b"),
            event(29_900, 30_100, "c"),
        ]);
        let default = select_preview_shots(&sub, 2, 1000);
        assert_eq!(
            default
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![3000, 30_000]
        );

        let disabled = select_shots_with_stamps_and_cooldown(
            &[&sub],
            Some(&sub),
            2,
            1000,
            0,
        );
        assert_eq!(
            disabled
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![3000, 6000]
        );
    }

    #[test]
    fn stamps_across_scripts_are_authoritative_deduped_and_capped() {
        let mut tl = test_sub(vec![]);
        tl.pandora_meta.stamps = vec![stamp(100, 300, "TL note"), stamp(1000, 1200, "")];
        let mut ts = test_sub(vec![event(20_000, 20_200, r"{\fnFancy}auto")]);
        ts.pandora_meta.stamps = vec![
            stamp(200, 200, "same midpoint"),
            stamp(2000, 2200, "third"),
            stamp(3000, 3200, "over cap"),
        ];

        let selection = select_shots_with_stamps(&[&tl, &ts], Some(&ts), 3, 1000);
        assert_eq!(
            selection
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![200, 1100, 2100]
        );
        assert_eq!(selection.shots[0].label, "0:00:02.00 · 2.0s · + · TL note");
        assert!(selection.verdicts.is_empty());
    }

    #[test]
    fn tl_stamp_works_without_ts_and_midpoints_dedup_globally() {
        let mut tl = test_sub(vec![]);
        tl.pandora_meta.stamps = vec![
            stamp(0, 1000, "wide"),
            stamp(100, 200, "between"),
            stamp(400, 600, "duplicate midpoint"),
        ];

        let selection = select_shots_with_stamps(&[&tl], None, 3, 1000);
        assert_eq!(selection.shots.len(), 2);
        assert_eq!(
            selection
                .shots
                .iter()
                .map(|shot| shot.centiseconds)
                .collect::<Vec<_>>(),
            vec![500, 150]
        );
    }

    #[test]
    fn labels_report_dense_rank_weight_and_length() {
        let sub = test_sub(vec![
            event(0, 1450, r"{\fnFancy}top"),
            event(20_000, 20_200, r"{\p1}m 0 0 l 10 0 10 10{\p0}"),
            event(40_000, 40_200, "plain"),
        ]);
        let selection = select_preview_shots(&sub, 3, 1000);
        assert!(selection.shots[0].label.ends_with("14.5s · 1.00"));
        assert!(selection.shots[1].label.ends_with("2.0s · 0.50"));
        assert!(selection.shots[2].label.ends_with("2.0s · 0.00"));

        let tied = select_preview_shots(
            &test_sub(vec![event(0, 200, "a"), event(20_000, 20_200, "b")]),
            2,
            1000,
        );
        assert!(tied.shots.iter().all(|shot| shot.label.ends_with("1.00")));
    }

    #[test]
    fn ranking_table_is_auditable() {
        let selection = select_preview_shots(&test_sub(vec![event(0, 200, "plain")]), 1, 1000);
        assert!(selection.ranking_log.starts_with("rank  span"));
        assert!(selection.ranking_log.contains("0:00:00.00-0:00:02.00"));
        assert!(selection.ranking_log.contains("selected"));
    }

    #[test]
    fn compose_preview_draws_label_and_watermark() {
        let input = Canvas::new(320, 180, Color::BLACK)
            .unwrap()
            .png_bytes()
            .unwrap();
        let font = Font::fallback();
        let output = compose_preview(&input, "0:00:02.00 · 2.0s · 1.00", &font, &font).unwrap();
        let canvas = Canvas::from_png_bytes(&output).unwrap();

        let top_changed = (0..80).any(|x| {
            (0..40).any(|y| canvas.pixel_rgba(x, y).unwrap_or(Color::BLACK) != Color::BLACK)
        });
        let bottom_changed = (180..320).any(|x| {
            (130..180).any(|y| canvas.pixel_rgba(x, y).unwrap_or(Color::BLACK) != Color::BLACK)
        });
        assert!(top_changed);
        assert!(bottom_changed);
    }

    #[test]
    fn merge_previews_stacks_two_frames_with_a_gutter() {
        let red = Canvas::new(2, 2, Color { r: 255, g: 0, b: 0, a: 255 })
            .unwrap()
            .png_bytes()
            .unwrap();
        let blue = Canvas::new(2, 2, Color { r: 0, g: 0, b: 255, a: 255 })
            .unwrap()
            .png_bytes()
            .unwrap();

        let merged = Canvas::from_png_bytes(&merge_previews(&[red, blue]).unwrap()).unwrap();

        assert_eq!((merged.width(), merged.height()), (2, 12));
        assert_eq!(merged.pixel_rgba(0, 0).unwrap(), Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(merged.pixel_rgba(0, 10).unwrap(), Color { r: 0, g: 0, b: 255, a: 255 });
        assert_eq!(merged.pixel_rgba(0, 5).unwrap(), Color::BLACK);
    }

    #[test]
    fn merge_previews_centers_frames_with_different_widths() {
        let narrow = Canvas::new(2, 1, Color::WHITE).unwrap().png_bytes().unwrap();
        let wide = Canvas::new(4, 2, Color { r: 0, g: 255, b: 0, a: 255 })
            .unwrap()
            .png_bytes()
            .unwrap();

        let merged = Canvas::from_png_bytes(&merge_previews(&[narrow, wide]).unwrap()).unwrap();

        assert_eq!((merged.width(), merged.height()), (4, 11));
        assert_eq!(merged.pixel_rgba(1, 0).unwrap(), Color::WHITE);
        assert_eq!(merged.pixel_rgba(0, 0).unwrap(), Color::BLACK);
        assert_eq!(merged.pixel_rgba(3, 9).unwrap(), Color { r: 0, g: 255, b: 0, a: 255 });
    }
}
