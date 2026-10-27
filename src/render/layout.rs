//! Font loading and text layout: which face to draw, and how the words of a
//! chunk break into lines inside the caption box.

use crate::config::{self, Align, Style};
use anyhow::Result;
use fontdue::{Font, FontSettings};

/// A font family loaded at a given pixel size: regular + bold faces plus the
/// font-wide ink metrics used to size the box.
///
/// Public so the GUI preview can keep one loaded per (family, size) and
/// re-render caption states without re-reading the TTFs every frame.
pub struct Faces {
    pub(super) regular: Font,
    pub(super) bold: Font,
}

/// Caption glyph size in pixels for a style on a frame of `frame_h` pixels.
pub fn font_px(style: &Style, frame_h: u32) -> f32 {
    style.font.size_pct / 100.0 * frame_h as f32
}

/// Probe strings covering ascenders/caps and descenders.
pub(super) const PROBE_ASC: &str = "bdfhklH";

pub(super) const PROBE_DESC: &str = "gjpqyQ";

impl Faces {
    pub fn load(style: &Style, px: f32) -> Result<Faces> {
        let (dir, _) = config::find_fonts_dir()?;
        let (regular_path, bold_path) = config::font_paths(&dir, &style.font.family)?;
        let regular = fontdue::Font::from_bytes(
            config::load_font_bytes(&regular_path)?,
            FontSettings::default(),
        )
        .map_err(|e| anyhow::anyhow!("failed to parse font {}: {e}", regular_path.display()))?;
        let bold = fontdue::Font::from_bytes(
            config::load_font_bytes(&bold_path)?,
            FontSettings::default(),
        )
        .map_err(|e| anyhow::anyhow!("failed to parse font {}: {e}", bold_path.display()))?;
        let _ = px;
        Ok(Faces { regular, bold })
    }
}

/// Max ink extent over a probe string: upward (`up = true`) or downward.
///
/// Real ink bounds keep the box hugging the text the way the reference
/// screenshot does — some fonts ship generous line gaps that would balloon it.
pub(super) fn ink_extent(font: &Font, probe: &str, px: f32, up: bool) -> f32 {
    let mut extent = 0f32;
    for c in probe.chars() {
        let m = font.metrics(c, px);
        let e = if up {
            m.ymin as f32 + m.height as f32
        } else {
            -(m.ymin as f32)
        };
        extent = extent.max(e);
    }
    extent.max(px * 0.1)
}

/// One line of a caption block.
pub(super) struct LineLayout {
    /// Indices into the display words.
    pub(super) words: Vec<usize>,
    /// Left edge of each word, relative to the line start.
    pub(super) slots: Vec<f32>,
    /// Width of each word's slot (parallel to `words`).
    pub(super) word_widths: Vec<f32>,
    /// Width of the line including inter-word gaps.
    pub(super) width: f32,
    /// Left offset of this line inside the box content area (alignment).
    pub(super) offset: f32,
}

/// All lines for one caption state.
pub(super) struct BlockLayout {
    pub(super) lines: Vec<LineLayout>,
    /// Width of the widest line.
    pub(super) width: f32,
}

pub(super) fn word_advance(font: &Font, text: &str, px: f32, spacing_px: f32) -> f32 {
    let mut chars = 0usize;
    let mut width = 0.0f32;
    for c in text.chars() {
        width += font.metrics(c, px).advance_width;
        chars += 1;
    }
    width + spacing_px * chars.saturating_sub(1) as f32
}

pub(super) fn space_gap(faces: &Faces, px: f32, spacing_px: f32) -> f32 {
    faces.regular.metrics(' ', px).advance_width * 1.15 + spacing_px
}

/// Apply the style's text transforms before measuring or drawing.
pub(super) fn display_text(text: &str, style: &Style) -> String {
    if style.font.uppercase {
        text.to_uppercase()
    } else {
        text.to_string()
    }
}

/// Which face a word uses in this state.
pub(super) fn face_for<'a>(faces: &'a Faces, style: &Style, active: bool) -> &'a Font {
    let bold = if active {
        style.font.active_bold
    } else {
        style.font.dim_bold
    };
    if bold {
        &faces.bold
    } else {
        &faces.regular
    }
}

/// Width reserved for one word: the widest of the regular, bold and scaled
/// active faces, so changing the highlight never reflows the line.
pub(super) fn word_slot_width(faces: &Faces, style: &Style, word: &str, px: f32) -> f32 {
    let spacing = style.font.letter_spacing * px;
    let scale = style.highlight.scale.max(1.0);
    let regular = word_advance(&faces.regular, word, px, spacing);
    let bold = word_advance(&faces.bold, word, px, spacing);
    let active_face = face_for(faces, style, true);
    let active = word_advance(
        active_face,
        word,
        px * scale,
        style.font.letter_spacing * px * scale,
    );
    regular.max(bold).max(active)
}

pub(super) fn line_width(widths: &[f32], gap: f32, i: usize, j: usize) -> f32 {
    let mut total = 0.0f32;
    for (n, w) in widths[i..j].iter().enumerate() {
        total += *w;
        if n + 1 < j - i {
            total += gap;
        }
    }
    total
}

/// Break words into at most `max_lines` lines. The minimum line count is found
/// greedily, then a small DP balances the slack across those lines.
/// `max_words = 0` means no per-line word cap.
pub(super) fn break_lines(
    widths: &[f32],
    gap: f32,
    max_width: f32,
    max_lines: usize,
    max_words: usize,
) -> Vec<(usize, usize)> {
    let n = widths.len();
    if n == 0 {
        return Vec::new();
    }
    if !max_width.is_finite() {
        return vec![(0, n)];
    }
    let max_lines = max_lines.max(1);
    let max_width = max_width.max(1.0);

    // Minimum line count under the word cap and width. Oversized single words
    // are allowed to occupy their own line.
    let mut min_lines = 1usize;
    let mut cur_w = widths[0];
    let mut cur_n = 1usize;
    for w in widths.iter().skip(1) {
        let over_w = cur_w + gap + *w > max_width + 0.01;
        let over_n = max_words > 0 && cur_n >= max_words;
        if over_w || over_n {
            min_lines += 1;
            cur_w = *w;
            cur_n = 1;
        } else {
            cur_w += gap + *w;
            cur_n += 1;
        }
    }
    let k = min_lines.min(max_lines).max(1);

    // DP: cost[l][j] = best cost for the first j words in l lines.
    const INF: f32 = 1.0e18;
    let mut cost = vec![vec![INF; n + 1]; k + 1];
    let mut prev = vec![vec![0usize; n + 1]; k + 1];
    cost[0][0] = 0.0;
    for l in 1..=k {
        for j in 1..=n {
            for i in 0..j {
                if cost[l - 1][i] >= INF {
                    continue;
                }
                let count = j - i;
                if max_words > 0 && count > max_words {
                    continue;
                }
                let w = line_width(widths, gap, i, j);
                let overflow = (w - max_width).max(0.0);
                let slack = (max_width - w).max(0.0);
                // Strongly prefer no overflow, then balance the slack.
                let line_cost = slack * slack + overflow * overflow * 100.0 + overflow * 1.0e6;
                let c = cost[l - 1][i] + line_cost;
                if c < cost[l][j] {
                    cost[l][j] = c;
                    prev[l][j] = i;
                }
            }
        }
    }

    if cost[k][n] < INF {
        let mut out = Vec::with_capacity(k);
        let mut j = n;
        for l in (1..=k).rev() {
            let i = prev[l][j];
            out.push((i, j));
            j = i;
        }
        out.reverse();
        return out;
    }

    // Fallback: divide evenly, respecting the word cap.
    let mut out = Vec::with_capacity(k);
    let mut start = 0usize;
    for l in 0..k {
        let remaining = k - l;
        let mut end = if remaining == 1 {
            n
        } else {
            start + ((n - start) / remaining).max(1)
        };
        if max_words > 0 {
            end = end.min(start + max_words);
        }
        if end <= start {
            end = (start + 1).min(n);
        }
        out.push((start, end));
        start = end;
    }
    out
}

pub(super) fn layout_block(
    faces: &Faces,
    style: &Style,
    words: &[String],
    px: f32,
    content_w: f32,
    max_lines: usize,
    max_words: usize,
) -> BlockLayout {
    let spacing = style.font.letter_spacing * px;
    let gap = space_gap(faces, px, spacing);
    let widths: Vec<f32> = words
        .iter()
        .map(|w| word_slot_width(faces, style, w, px))
        .collect();
    let ranges = if words.is_empty() {
        Vec::new()
    } else {
        break_lines(&widths, gap, content_w, max_lines, max_words)
    };

    let mut lines = Vec::with_capacity(ranges.len());
    let mut block_w = 0.0f32;
    for (i, j) in ranges {
        let mut slots = Vec::with_capacity(j - i);
        let mut line_words = Vec::with_capacity(j - i);
        let mut line_widths = Vec::with_capacity(j - i);
        let mut x = 0.0f32;
        for (n, w) in widths[i..j].iter().enumerate() {
            slots.push(x);
            line_words.push(i + n);
            line_widths.push(*w);
            x += *w;
            if n + 1 < j - i {
                x += gap;
            }
        }
        block_w = block_w.max(x);
        lines.push(LineLayout {
            words: line_words,
            slots,
            word_widths: line_widths,
            width: x,
            offset: 0.0,
        });
    }
    BlockLayout {
        lines,
        width: block_w,
    }
}

pub(super) fn align_block(block: &mut BlockLayout, align: Align, align_w: f32) {
    for line in &mut block.lines {
        let slack = (align_w - line.width).max(0.0);
        line.offset = match align {
            Align::Left => 0.0,
            Align::Center => slack / 2.0,
            Align::Right => slack,
        };
    }
}

/// Drop words and lines that do not fit in a bounded (Clip) block.
pub(super) fn clip_lines(block: &mut BlockLayout, gap: f32, max_width: f32, max_lines: usize) {
    block.lines.truncate(max_lines.max(1));
    let mut widest = 0.0f32;
    for line in &mut block.lines {
        let mut words = Vec::new();
        let mut slots = Vec::new();
        let mut widths = Vec::new();
        let mut x = 0.0f32;
        for ((&wi, &ww), &slot) in line.words.iter().zip(&line.word_widths).zip(&line.slots) {
            let next_w = if words.is_empty() { ww } else { x + gap + ww };
            if next_w > max_width + 0.01 {
                break;
            }
            words.push(wi);
            slots.push(slot);
            widths.push(ww);
            x = next_w;
        }
        line.words = words;
        line.slots = slots;
        line.word_widths = widths;
        line.width = x;
        widest = widest.max(x);
    }
    block.lines.retain(|l| !l.words.is_empty());
    block.width = widest;
}
