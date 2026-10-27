//! Box measurement: the padding, ascent and line height a caption block needs
//! at one font size, and whether it fits the requested line budget.

use crate::config::{Fit, Style};

use super::layout::{
    align_block, clip_lines, ink_extent, layout_block, space_gap, BlockLayout, Faces, PROBE_ASC,
    PROBE_DESC,
};

/// The measured geometry of a caption block at one font size.
pub(super) struct Metrics {
    pub(super) px: f32,
    pub(super) pad_x: f32,
    pub(super) content_w: f32,
    pub(super) ascent: f32,
    pub(super) line_h: f32,
    pub(super) block: BlockLayout,
    pub(super) box_w: f32,
    pub(super) box_h: f32,
    pub(super) text_top: f32,
    pub(super) box_content_h: f32,
    pub(super) content_h: f32,
}

pub(super) fn build_metrics(
    faces: &Faces,
    style: &Style,
    display: &[String],
    px: f32,
    frame_w: u32,
    frame_h: u32,
    clip: bool,
) -> Metrics {
    let scale = style.highlight.scale.max(1.0);
    let pad_x = style.box_style.padding_x * px;
    let pad_y = style.box_style.padding_y * px;
    let fixed_w = style
        .layout
        .box_width_pct
        .map(|p| frame_w as f32 * p / 100.0);
    let fixed_h = style
        .layout
        .box_height_pct
        .map(|p| frame_h as f32 * p / 100.0);
    let max_w = fixed_w
        .map(|w| (w - 2.0 * pad_x).max(1.0))
        .unwrap_or_else(|| {
            (frame_w as f32 * style.layout.max_width_pct / 100.0 - 2.0 * pad_x).max(1.0)
        });
    let one_line = style.layout.fit == Fit::Shrink;
    let break_w = if one_line { f32::INFINITY } else { max_w };
    let max_lines = if one_line {
        1
    } else {
        style.layout.max_lines.max(1)
    };
    let spacing = style.font.letter_spacing * px;
    let gap = space_gap(faces, px, spacing);
    let mut block = layout_block(
        faces,
        style,
        display,
        px,
        break_w,
        max_lines,
        style.layout.max_words_per_line,
    );
    let ascent = ink_extent(&faces.regular, PROBE_ASC, px * scale, true);
    let descent = ink_extent(&faces.regular, PROBE_DESC, px * scale, false);
    let line_h = style.layout.line_height * px;

    if clip {
        let max_fit = if let Some(bh) = fixed_h {
            let avail = (bh - 2.0 * pad_y).max(0.0);
            if avail + 0.5 < ascent + descent {
                1
            } else {
                1 + ((avail - ascent - descent) / line_h).floor().max(0.0) as usize
            }
        } else {
            max_lines
        };
        clip_lines(&mut block, gap, max_w, max_fit.min(max_lines));
    }

    let line_count = block.lines.len();
    let content_h = if line_count == 0 {
        ascent + descent
    } else {
        ascent + descent + (line_count - 1) as f32 * line_h
    };
    let reserve = style.layout.reserve_lines.max(line_count).max(1);
    let box_h =
        fixed_h.unwrap_or_else(|| ascent + descent + (reserve - 1) as f32 * line_h + 2.0 * pad_y);
    let hug_w = if clip {
        max_w + 2.0 * pad_x
    } else {
        block.width + 2.0 * pad_x
    };
    let box_w = fixed_w.unwrap_or(hug_w);
    let align_w = (box_w - 2.0 * pad_x).max(0.0);
    align_block(&mut block, style.layout.align, align_w);
    let box_content_h = (box_h - 2.0 * pad_y).max(0.0);
    let text_top = pad_y + (box_content_h - content_h).max(0.0) / 2.0;

    Metrics {
        px,
        pad_x,
        content_w: max_w,
        ascent,
        line_h,
        block,
        box_w,
        box_h,
        text_top,
        box_content_h,
        content_h,
    }
}

pub(super) fn wrap_fits(m: &Metrics, max_lines: usize) -> bool {
    m.block.lines.len() <= max_lines
        && m.block.width <= m.content_w + 0.5
        && m.content_h <= m.box_content_h + 0.5
}
