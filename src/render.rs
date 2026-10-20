//! Render caption "states": one PNG per (chunk, highlighted word).
//!
//! A chunk of 3 words produces 3 PNGs — identical text, different word bright.
//! ffmpeg shows each PNG only during its word's time window.
//!
//! PNGs are **tight crops** of the caption box (not full-frame): the export
//! step positions them with percentage-based overlay expressions. Full-frame
//! PNGs at 1080x1920 would cost ~8 MB each (~4.5 GB RSS for 550 states); a
//! tight crop costs ~300 KB and composites identically.

use crate::chunk::Chunk;
use crate::config::{self, Align, Anchor, Color, Easing, Fit, MotionStyle, PopupEffect, Style};
use anyhow::{bail, Context, Result};
use fontdue::{Font, FontSettings};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// One rendered highlight state with the time window it should be visible.
#[derive(Debug, Clone)]
pub struct CaptionState {
    pub png: PathBuf,
    pub start_secs: f64,
    pub end_secs: f64,
    /// Top edge of the caption box inside the state PNG, in pixels.
    pub box_top: f32,
    /// Height of the caption box, in pixels.
    pub box_h: f32,
    /// Which edge of the box `position_pct` refers to for this state.
    pub anchor: Anchor,
    /// When the containing popup first appeared (for popup motion).
    pub popup_start_secs: f64,
    /// Font size in pixels at render resolution (for slide distances).
    pub font_px: f32,
}

/// A font family loaded at a given pixel size: regular + bold faces plus the
/// font-wide ink metrics used to size the box.
///
/// Public so the GUI preview can keep one loaded per (family, size) and
/// re-render caption states without re-reading the TTFs every frame.
pub struct Faces {
    regular: Font,
    bold: Font,
}

/// Caption glyph size in pixels for a style on a frame of `frame_h` pixels.
pub fn font_px(style: &Style, frame_h: u32) -> f32 {
    style.font.size_pct / 100.0 * frame_h as f32
}

/// Straight-alpha blend of the caption `src` (non-premultiplied RGBA) onto the
/// fully-opaque `dst` frame at (ox, oy), clipped to the destination.
///
/// Shared by the GUI preview and the raw-pipe exporter so both composite
/// exactly like the state PNGs on screen.
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
pub fn blend_straight(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    ox: i32,
    oy: i32,
) {
    let (dw, dh) = (dw as i32, dh as i32);
    for sy in 0..sh as i32 {
        let dy = oy + sy;
        if dy < 0 || dy >= dh {
            continue;
        }
        for sx in 0..sw as i32 {
            let dx = ox + sx;
            if dx < 0 || dx >= dw {
                continue;
            }
            let si = (sy as usize * sw as usize + sx as usize) * 4;
            let a = src[si + 3] as u32;
            if a == 0 {
                continue;
            }
            let di = (dy as usize * dw as usize + dx as usize) * 4;
            if a == 255 {
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
                continue;
            }
            let inv = 255 - a;
            for c in 0..3 {
                let s = src[si + c] as u32;
                let d = dst[di + c] as u32;
                dst[di + c] = ((s * a + d * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// A whole-popup transform: scale, offset and alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupTransform {
    pub scale: f32,
    pub dx: f32,
    pub dy: f32,
    pub alpha: f32,
}

impl PopupTransform {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        dx: 0.0,
        dy: 0.0,
        alpha: 1.0,
    };

    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-4
            && self.dx.abs() < 0.01
            && self.dy.abs() < 0.01
            && (self.alpha - 1.0).abs() < 1e-4
    }
}

fn ease(easing: Easing, p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => p,
        Easing::EaseOut => 1.0 - (1.0 - p).powi(3),
        Easing::EaseInOut => p * p * (3.0 - 2.0 * p),
        Easing::Spring => {
            const C1: f32 = 1.701_58;
            let c3 = C1 + 1.0;
            1.0 + c3 * (p - 1.0).powi(3) + C1 * (p - 1.0).powi(2)
        }
    }
}

/// The transform for a popup at `phase` (0 = just appeared, 1 = settled).
/// `font_px` converts the slide distance (in em) to pixels.
pub fn popup_transform(motion: &MotionStyle, phase: f32, font_px: f32) -> PopupTransform {
    if motion.popup == PopupEffect::None || motion.duration_ms == 0 || phase >= 1.0 {
        return PopupTransform::IDENTITY;
    }
    let eased = ease(motion.easing, phase);
    let amount = motion.intensity.max(0.0);
    let travel = amount * font_px * (1.0 - eased);
    match motion.popup {
        PopupEffect::None => PopupTransform::IDENTITY,
        PopupEffect::Pop => PopupTransform {
            scale: (1.0 - amount * (1.0 - eased)).max(0.05),
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideUp => PopupTransform {
            dy: travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideDown => PopupTransform {
            dy: -travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideLeft => PopupTransform {
            dx: travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideRight => PopupTransform {
            dx: -travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::Fade => PopupTransform {
            alpha: eased.clamp(0.0, 1.0),
            ..PopupTransform::IDENTITY
        },
    }
}

/// Bilinear sample of a straight-alpha RGBA source into premultiplied RGBA.
fn sample_premul(src: &[u8], sw: usize, sh: usize, x: i32, y: i32) -> (f32, f32, f32, f32) {
    if x < 0 || y < 0 || x as usize >= sw || y as usize >= sh {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let i = (y as usize * sw + x as usize) * 4;
    let a = src[i + 3] as f32;
    (
        src[i] as f32 * a / 255.0,
        src[i + 1] as f32 * a / 255.0,
        src[i + 2] as f32 * a / 255.0,
        a,
    )
}

/// Blit `src` with a scale/offset/alpha transform around a pivot. Scaling is
/// done with bilinear filtering over premultiplied samples so transparent
/// edges do not fringe.
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
pub fn blend_transformed(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    x: i32,
    y: i32,
    pivot_x: f32,
    pivot_y: f32,
    transform: PopupTransform,
) {
    if transform.is_identity() {
        blend_straight(dst, dw, dh, src, sw, sh, x, y);
        return;
    }
    let scale = transform.scale.max(0.01);
    let tx = x as f32 + pivot_x * (1.0 - scale) + transform.dx;
    let ty = y as f32 + pivot_y * (1.0 - scale) + transform.dy;
    let tw = sw as f32 * scale;
    let th = sh as f32 * scale;
    if tw <= 0.0 || th <= 0.0 {
        return;
    }
    let dx0 = tx.floor().max(0.0) as i32;
    let dy0 = ty.floor().max(0.0) as i32;
    let dx1 = (tx + tw).ceil().min(dw as f32) as i32;
    let dy1 = (ty + th).ceil().min(dh as f32) as i32;
    let alpha_mul = transform.alpha.clamp(0.0, 1.0);
    if alpha_mul <= 0.0 {
        return;
    }

    for py in dy0..dy1 {
        let sy = (py as f32 + 0.5 - ty) / scale - 0.5;
        let y0 = sy.floor() as i32;
        let fy = sy - y0 as f32;
        for px in dx0..dx1 {
            let sx = (px as f32 + 0.5 - tx) / scale - 0.5;
            let x0 = sx.floor() as i32;
            let fx = sx - x0 as f32;
            let w00 = (1.0 - fx) * (1.0 - fy);
            let w10 = fx * (1.0 - fy);
            let w01 = (1.0 - fx) * fy;
            let w11 = fx * fy;
            let (r00, g00, b00, a00) = sample_premul(src, sw as usize, sh as usize, x0, y0);
            let (r10, g10, b10, a10) = sample_premul(src, sw as usize, sh as usize, x0 + 1, y0);
            let (r01, g01, b01, a01) = sample_premul(src, sw as usize, sh as usize, x0, y0 + 1);
            let (r11, g11, b11, a11) = sample_premul(src, sw as usize, sh as usize, x0 + 1, y0 + 1);
            let a = (a00 * w00 + a10 * w10 + a01 * w01 + a11 * w11) * alpha_mul;
            if a <= 0.0 {
                continue;
            }
            let r = (r00 * w00 + r10 * w10 + r01 * w01 + r11 * w11) * alpha_mul;
            let g = (g00 * w00 + g10 * w10 + g01 * w01 + g11 * w11) * alpha_mul;
            let b = (b00 * w00 + b10 * w10 + b01 * w01 + b11 * w11) * alpha_mul;
            let a_u = a.round().clamp(0.0, 255.0) as u32;
            let inv = 255 - a_u;
            let di = (py as usize * dw as usize + px as usize) * 4;
            dst[di] =
                (r.round().clamp(0.0, 255.0) as u32 + dst[di] as u32 * inv / 255).min(255) as u8;
            dst[di + 1] = (g.round().clamp(0.0, 255.0) as u32 + dst[di + 1] as u32 * inv / 255)
                .min(255) as u8;
            dst[di + 2] = (b.round().clamp(0.0, 255.0) as u32 + dst[di + 2] as u32 * inv / 255)
                .min(255) as u8;
            // The frame is opaque; keep its alpha.
            dst[di + 3] = 255;
        }
    }
}

/// Probe strings covering ascenders/caps and descenders.
const PROBE_ASC: &str = "bdfhklH";
const PROBE_DESC: &str = "gjpqyQ";

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
fn ink_extent(font: &Font, probe: &str, px: f32, up: bool) -> f32 {
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
struct LineLayout {
    /// Indices into the display words.
    words: Vec<usize>,
    /// Left edge of each word, relative to the line start.
    slots: Vec<f32>,
    /// Width of each word's slot (parallel to `words`).
    word_widths: Vec<f32>,
    /// Width of the line including inter-word gaps.
    width: f32,
    /// Left offset of this line inside the box content area (alignment).
    offset: f32,
}

/// All lines for one caption state.
struct BlockLayout {
    lines: Vec<LineLayout>,
    /// Width of the widest line.
    width: f32,
}

fn word_advance(font: &Font, text: &str, px: f32, spacing_px: f32) -> f32 {
    let mut chars = 0usize;
    let mut width = 0.0f32;
    for c in text.chars() {
        width += font.metrics(c, px).advance_width;
        chars += 1;
    }
    width + spacing_px * chars.saturating_sub(1) as f32
}

fn space_gap(faces: &Faces, px: f32, spacing_px: f32) -> f32 {
    faces.regular.metrics(' ', px).advance_width * 1.15 + spacing_px
}

/// Apply the style's text transforms before measuring or drawing.
fn display_text(text: &str, style: &Style) -> String {
    if style.font.uppercase {
        text.to_uppercase()
    } else {
        text.to_string()
    }
}

/// Which face a word uses in this state.
fn face_for<'a>(faces: &'a Faces, style: &Style, active: bool) -> &'a Font {
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
fn word_slot_width(faces: &Faces, style: &Style, word: &str, px: f32) -> f32 {
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

fn line_width(widths: &[f32], gap: f32, i: usize, j: usize) -> f32 {
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
fn break_lines(
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

fn layout_block(
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

fn align_block(block: &mut BlockLayout, align: Align, align_w: f32) {
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
fn clip_lines(block: &mut BlockLayout, gap: f32, max_width: f32, max_lines: usize) {
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

/// Multiply a colour's alpha by a 0..=1 opacity.
fn with_opacity(color: Color, opacity: f32) -> Color {
    Color {
        a: (color.a as f32 * opacity.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, 255.0) as u8,
        ..color
    }
}

/// A separable box blur over an 8-bit coverage mask, with clamped edges.
fn box_blur(src: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    if radius == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    fn clamp(i: isize, n: usize) -> usize {
        i.clamp(0, n.saturating_sub(1) as isize) as usize
    }
    let win = (radius * 2 + 1) as u32;
    let r = radius as isize;
    let mut tmp = vec![0u8; w * h];
    let mut dst = vec![0u8; w * h];

    for y in 0..h {
        let row = y * w;
        let mut sum = 0u32;
        for i in -r..=r {
            sum += src[row + clamp(i, w)] as u32;
        }
        for x in 0..w {
            tmp[row + x] = (sum / win) as u8;
            let sub = clamp(x as isize - r, w);
            let add = clamp(x as isize + r + 1, w);
            sum = sum + src[row + add] as u32 - src[row + sub] as u32;
        }
    }
    for x in 0..w {
        let mut sum = 0u32;
        for i in -r..=r {
            sum += tmp[clamp(i, h) * w + x] as u32;
        }
        for y in 0..h {
            dst[y * w + x] = (sum / win) as u8;
            let sub = clamp(y as isize - r, h);
            let add = clamp(y as isize + r + 1, h);
            sum = sum + tmp[add * w + x] as u32 - tmp[sub * w + x] as u32;
        }
    }
    dst
}

/// Distance (in px) from each pixel to the nearest covered pixel, using a
/// two-pass chamfer approximation. Used to build outlines without a vector
/// glyph API.
fn distance_transform(mask: &[u8], w: usize, h: usize) -> Vec<f32> {
    use std::f32::consts::SQRT_2;
    const INF: f32 = 1.0e9;
    const D: f32 = SQRT_2;
    let mut d = vec![INF; w * h];
    for (i, m) in mask.iter().enumerate() {
        if *m > 8 {
            d[i] = 0.0;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut best = d[i];
            if x > 0 {
                best = best.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                best = best.min(d[i - w] + 1.0);
                if x > 0 {
                    best = best.min(d[i - w - 1] + D);
                }
                if x + 1 < w {
                    best = best.min(d[i - w + 1] + D);
                }
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut best = d[i];
            if x + 1 < w {
                best = best.min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                best = best.min(d[i + w] + 1.0);
                if x > 0 {
                    best = best.min(d[i + w - 1] + D);
                }
                if x + 1 < w {
                    best = best.min(d[i + w + 1] + D);
                }
            }
            d[i] = best;
        }
    }
    d
}

/// Extra room (em) a word's glow/shadow/outline needs around its glyphs.
fn word_mask_pad_em(style: &Style) -> f32 {
    let mut e = 0.0f32;
    if style.text.outline.enabled {
        e += style.text.outline.width;
    }
    if style.text.shadow.enabled {
        e += style.text.shadow.blur * 2.0
            + style
                .text
                .shadow
                .offset_x
                .abs()
                .max(style.text.shadow.offset_y.abs());
    }
    if style.text.glow.enabled {
        e += style.text.glow.radius;
    }
    e + 0.06
}

/// Extra room (em) the canvas needs for effects that can escape the text box.
fn canvas_effect_margin_em(style: &Style) -> f32 {
    let mut e = word_mask_pad_em(style);
    if style.highlight.pill.enabled {
        e = e.max(
            style
                .highlight
                .pill
                .padding_x
                .max(style.highlight.pill.padding_y)
                + 0.04,
        );
    }
    if style.highlight.underline.enabled {
        e = e.max(style.highlight.underline.offset + style.highlight.underline.width + 0.02);
    }
    if e <= 0.06 {
        0.0
    } else {
        e
    }
}

struct WordMask {
    mask: Vec<u8>,
    w: usize,
    h: usize,
    pad: f32,
    ascent: f32,
}

/// Rasterize one word into a padded coverage mask, ready for effects.
fn word_mask(face: &Font, style: &Style, text: &str, px: f32) -> WordMask {
    let pad = (word_mask_pad_em(style) * px).ceil() + 2.0;
    let ascent = ink_extent(face, PROBE_ASC, px, true);
    let descent = ink_extent(face, PROBE_DESC, px, false);
    let spacing = style.font.letter_spacing * px;
    let width = word_advance(face, text, px, spacing);
    let w = (width + 2.0 * pad).ceil().max(1.0) as usize;
    let h = (ascent + descent + 2.0 * pad).ceil().max(1.0) as usize;
    let baseline = pad + ascent;
    let mut mask = vec![0u8; w * h];

    let chars: Vec<char> = text.chars().collect();
    let mut pen = pad;
    for (ci, c) in chars.iter().enumerate() {
        let advance = face.metrics(*c, px).advance_width;
        let (m, coverage) = face.rasterize(*c, px);
        if m.width > 0 && m.height > 0 {
            let ox = (pen + m.xmin as f32).round() as i32;
            let oy = (baseline - m.ymin as f32 - m.height as f32).round() as i32;
            for gy in 0..m.height {
                let py = oy + gy as i32;
                if py < 0 || py as usize >= h {
                    continue;
                }
                for gx in 0..m.width {
                    let px_x = ox + gx as i32;
                    if px_x < 0 || px_x as usize >= w {
                        continue;
                    }
                    let idx = py as usize * w + px_x as usize;
                    mask[idx] = mask[idx].max(coverage[gy * m.width + gx]);
                }
            }
        }
        pen += advance;
        if ci + 1 < chars.len() {
            pen += spacing;
        }
    }

    WordMask {
        mask,
        w,
        h,
        pad,
        ascent,
    }
}

/// Fill a rounded rect (r = 0 for a plain rect) over the premultiplied canvas.
fn fill_shape(
    pixmap: &mut tiny_skia::Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    color: Color,
) {
    if color.a == 0 {
        return;
    }
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        paint.blend_mode = tiny_skia::BlendMode::SourceOver;
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
}

/// Approximate a rounded rect with quadratic Bézier corners.
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Blend one rasterized glyph (fontdue coverage mask) onto a premultiplied
/// RGBA buffer.
#[allow(clippy::too_many_arguments)]
fn blend_glyph(
    data: &mut [u8],
    canvas_w: usize,
    canvas_h: usize,
    origin_x: i32,
    origin_y: i32,
    coverage: &[u8],
    gw: usize,
    gh: usize,
    color: Color,
) {
    let color_a = color.a as u32;
    for gy in 0..gh {
        let py = origin_y + gy as i32;
        if py < 0 || py as usize >= canvas_h {
            continue;
        }
        for gx in 0..gw {
            let px = origin_x + gx as i32;
            if px < 0 || px as usize >= canvas_w {
                continue;
            }
            let cov = coverage[gy * gw + gx] as u32;
            if cov == 0 {
                continue;
            }
            // Effective source alpha = coverage * color alpha.
            let a = cov * color_a / 255;
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            let idx = (py as usize * canvas_w + px as usize) * 4;
            let sr = (color.r as u32 * a + 127) / 255;
            let sg = (color.g as u32 * a + 127) / 255;
            let sb = (color.b as u32 * a + 127) / 255;

            let dr = data[idx] as u32;
            let dg = data[idx + 1] as u32;
            let db = data[idx + 2] as u32;
            let da = data[idx + 3] as u32;

            data[idx] = (sr + (dr * inv + 127) / 255).min(255) as u8;
            data[idx + 1] = (sg + (dg * inv + 127) / 255).min(255) as u8;
            data[idx + 2] = (sb + (db * inv + 127) / 255).min(255) as u8;
            data[idx + 3] = (a + (da * inv + 127) / 255).min(255) as u8;
        }
    }
}

/// Render one caption state for `words` with `active` highlighted into a
/// straight-alpha RGBA buffer. Returns `(pixels, width, height)`.
///
/// `frame_w` bounds the box: oversized chunks get their font shrunk until they
/// fit 92% of the frame width.
///
/// This is the pixel source for both the export's state PNGs and the GUI's
/// live preview — preview and output are rendered by the same code.
/// A rendered caption state: straight-alpha pixels plus the geometry needed to
/// place the box in a frame.
pub struct RenderedCaption {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Top edge of the caption box inside the canvas, in pixels.
    pub box_top: f32,
    /// Height of the caption box, in pixels.
    pub box_h: f32,
}

/// The measured geometry of a caption block at one font size.
struct Metrics {
    px: f32,
    pad_x: f32,
    content_w: f32,
    ascent: f32,
    line_h: f32,
    block: BlockLayout,
    box_w: f32,
    box_h: f32,
    text_top: f32,
    box_content_h: f32,
    content_h: f32,
}

fn build_metrics(
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

fn wrap_fits(m: &Metrics, max_lines: usize) -> bool {
    m.block.lines.len() <= max_lines
        && m.block.width <= m.content_w + 0.5
        && m.content_h <= m.box_content_h + 0.5
}

/// Render one caption state for `words` with `active` highlighted.
///
/// `frame_w` bounds the box and `frame_h` sizes fixed-height boxes. Oversized
/// `Shrink` chunks get their font reduced to fit; `Wrap` keeps the chosen size
/// and wraps, shrinking only as a last resort; `Clip` drops what does not fit.
///
/// This is the pixel source for both the export's state PNGs and the GUI's
/// live preview — preview and output are rendered by the same code.
pub fn render_state_rgba(
    faces: &Faces,
    style: &Style,
    words: &[String],
    active: usize,
    font_px: f32,
    frame_w: u32,
    frame_h: u32,
) -> Result<RenderedCaption> {
    let display: Vec<String> = words.iter().map(|w| display_text(w, style)).collect();
    let scale = style.highlight.scale.max(1.0);
    let active = active.min(display.len().saturating_sub(1));
    let min_px = font_px * 0.4;

    let m = match style.layout.fit {
        Fit::Shrink => {
            let mut m = build_metrics(faces, style, &display, font_px, frame_w, frame_h, false);
            if m.block.width > m.content_w && m.content_w > 0.0 {
                let px = (font_px * (m.content_w / m.block.width)).max(min_px);
                m = build_metrics(faces, style, &display, px, frame_w, frame_h, false);
            }
            m
        }
        Fit::Wrap => {
            let max_lines = style.layout.max_lines.max(1);
            let mut px = font_px;
            let mut m = build_metrics(faces, style, &display, px, frame_w, frame_h, false);
            while !wrap_fits(&m, max_lines) && px > min_px + 0.01 {
                px = (px * 0.94).max(min_px);
                m = build_metrics(faces, style, &display, px, frame_w, frame_h, false);
            }
            if !wrap_fits(&m, max_lines) {
                m = build_metrics(faces, style, &display, px, frame_w, frame_h, true);
            }
            m
        }
        Fit::Clip => build_metrics(faces, style, &display, font_px, frame_w, frame_h, true),
    };

    let px = m.px;
    let pad_x = m.pad_x;
    let box_w = m.box_w;
    let box_h = m.box_h;
    let ascent = m.ascent;

    // --- canvas with room for the shadow, border and text effects ---
    let shadow_margin = if style.box_style.shadow {
        (0.30
            + (style.box_style.shadow_blur - 0.14).max(0.0) * 1.5
            + (style.box_style.shadow_offset_y - 0.10).max(0.0))
        .max(0.30)
            * px
    } else {
        0.0
    };
    let border_margin = if style.box_style.border_width > 0.0 {
        style.box_style.border_width * px * 0.5 + 1.0
    } else {
        0.0
    };
    let effect_margin = canvas_effect_margin_em(style) * px;
    let margin = shadow_margin.max(border_margin).max(effect_margin);
    let canvas_w = (box_w + 2.0 * margin).ceil().max(1.0) as usize;
    let canvas_h = (box_h + 2.0 * margin).ceil().max(1.0) as usize;
    let box_x = margin;
    let box_y = margin;

    let mut pixmap =
        tiny_skia::Pixmap::new(canvas_w as u32, canvas_h as u32).context("pixmap alloc")?;

    // --- box shadow: stacked expanding rounded rects, faking a blur ---
    if style.box_style.shadow && style.box_style.shadow_opacity > 0.0 {
        const LAYERS: f32 = 5.0;
        for i in 0..LAYERS as usize {
            let t = (i + 1) as f32 / LAYERS;
            let grow = style.box_style.shadow_blur * px * t;
            let drop = style.box_style.shadow_offset_y * px * t;
            let base_alpha = (18.0 * (1.0 - t)) as u8 + 4;
            let color_alpha = style.box_style.shadow_color.a as f32 / 255.0;
            let alpha = (base_alpha as f32 * style.box_style.shadow_opacity * color_alpha)
                .round()
                .clamp(0.0, 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let radius = if style.box_style.rounded {
                style.box_style.radius_frac * box_h + grow
            } else {
                0.0
            };
            if let Some(path) = rounded_rect_path(
                box_x - grow,
                box_y - grow + drop,
                box_w + 2.0 * grow,
                box_h + 2.0 * grow,
                radius,
            ) {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(
                    style.box_style.shadow_color.r,
                    style.box_style.shadow_color.g,
                    style.box_style.shadow_color.b,
                    alpha,
                );
                paint.blend_mode = tiny_skia::BlendMode::SourceOver;
                paint.anti_alias = true;
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
    }

    // --- box fill + border ---
    let radius = if style.box_style.rounded {
        style.box_style.radius_frac * box_h
    } else {
        0.0
    };
    let box_alpha = (style.box_style.color.a as f32 * style.box_style.opacity).round() as u8;
    if let Some(path) = rounded_rect_path(box_x, box_y, box_w, box_h, radius) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(
            style.box_style.color.r,
            style.box_style.color.g,
            style.box_style.color.b,
            box_alpha,
        );
        // Source: the box replaces the shadow beneath it instead of stacking.
        paint.blend_mode = tiny_skia::BlendMode::Source;
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );

        if style.box_style.border_width > 0.0 {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(
                style.box_style.border_color.r,
                style.box_style.border_color.g,
                style.box_style.border_color.b,
                style.box_style.border_color.a,
            );
            paint.blend_mode = tiny_skia::BlendMode::SourceOver;
            paint.anti_alias = true;
            let stroke = tiny_skia::Stroke {
                width: style.box_style.border_width * px,
                ..Default::default()
            };
            pixmap.stroke_path(
                &path,
                &paint,
                &stroke,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    // --- active word geometry (pill/underline use the real word, not the
    // reserved slot) ---
    let text_top = box_y + m.text_top;
    let line_h = m.line_h;
    let baseline0 = text_top + ascent;
    let active_face = face_for(faces, style, true);
    let active_px = px * scale;
    let active_text = display.get(active).map(String::as_str).unwrap_or("");
    let active_spacing = style.font.letter_spacing * active_px;
    let active_w = word_advance(active_face, active_text, active_px, active_spacing);
    let active_asc = ink_extent(active_face, PROBE_ASC, active_px, true);
    let active_desc = ink_extent(active_face, PROBE_DESC, active_px, false);

    let active_pos = m.block.lines.iter().enumerate().find_map(|(li, line)| {
        line.words
            .iter()
            .position(|&w| w == active)
            .map(|k| (li, k))
    });
    let (active_x, active_baseline) = match active_pos {
        Some((li, k)) => {
            let line = &m.block.lines[li];
            (
                box_x + pad_x + line.offset + line.slots[k],
                baseline0 + li as f32 * line_h,
            )
        }
        None => (box_x + pad_x, baseline0),
    };
    let has_active = active_pos.is_some();

    if has_active && style.highlight.pill.enabled {
        let pill = &style.highlight.pill;
        let px_x = pill.padding_x * active_px;
        let px_y = pill.padding_y * active_px;
        let h = active_asc + active_desc + 2.0 * px_y;
        fill_shape(
            &mut pixmap,
            active_x - px_x,
            active_baseline - active_asc - px_y,
            active_w + 2.0 * px_x,
            h,
            pill.radius_frac * h,
            pill.color,
        );
    }

    let effects =
        style.text.outline.enabled || style.text.shadow.enabled || style.text.glow.enabled;
    let scaled = (scale - 1.0).abs() > 1e-4;
    let data = pixmap.data_mut();

    if effects || scaled {
        struct Prepared {
            wm: WordMask,
            x0: i32,
            y0: i32,
            word_px: f32,
            color: Color,
        }
        let mut prepared = Vec::with_capacity(display.len());
        for (li, line) in m.block.lines.iter().enumerate() {
            let line_baseline = baseline0 + li as f32 * line_h;
            for (k, &wi) in line.words.iter().enumerate() {
                let is_active = wi == active;
                let face = face_for(faces, style, is_active);
                let word_px = if is_active { active_px } else { px };
                let wm = word_mask(face, style, &display[wi], word_px);
                prepared.push(Prepared {
                    x0: (box_x + pad_x + line.offset + line.slots[k] - wm.pad).round() as i32,
                    y0: (line_baseline - wm.ascent - wm.pad).round() as i32,
                    wm,
                    word_px,
                    color: with_opacity(
                        if is_active {
                            style.text.highlight
                        } else {
                            style.text.dim
                        },
                        style.text.opacity,
                    ),
                });
            }
        }

        // Effects of every word first, so no word's fill darkens a neighbour's
        // shadow or outline.
        for pw in &prepared {
            if style.text.glow.enabled {
                let r = (style.text.glow.radius * pw.word_px).round().max(0.0) as usize;
                let blurred = box_blur(&pw.wm.mask, pw.wm.w, pw.wm.h, r);
                blend_glyph(
                    data,
                    canvas_w,
                    canvas_h,
                    pw.x0,
                    pw.y0,
                    &blurred,
                    pw.wm.w,
                    pw.wm.h,
                    with_opacity(style.text.glow.color, style.text.opacity),
                );
            }
            if style.text.shadow.enabled {
                let r = (style.text.shadow.blur * pw.word_px).round().max(0.0) as usize;
                let blurred = box_blur(&pw.wm.mask, pw.wm.w, pw.wm.h, r);
                let ox = (style.text.shadow.offset_x * pw.word_px).round() as i32;
                let oy = (style.text.shadow.offset_y * pw.word_px).round() as i32;
                blend_glyph(
                    data,
                    canvas_w,
                    canvas_h,
                    pw.x0 + ox,
                    pw.y0 + oy,
                    &blurred,
                    pw.wm.w,
                    pw.wm.h,
                    with_opacity(style.text.shadow.color, style.text.opacity),
                );
            }
            if style.text.outline.enabled {
                let dist = distance_transform(&pw.wm.mask, pw.wm.w, pw.wm.h);
                let width = style.text.outline.width * pw.word_px;
                let mut outline = vec![0u8; pw.wm.mask.len()];
                for (j, d) in dist.iter().enumerate() {
                    let a = (width + 0.5 - *d).clamp(0.0, 1.0);
                    outline[j] = (a * 255.0).round() as u8;
                }
                blend_glyph(
                    data,
                    canvas_w,
                    canvas_h,
                    pw.x0,
                    pw.y0,
                    &outline,
                    pw.wm.w,
                    pw.wm.h,
                    with_opacity(style.text.outline.color, style.text.opacity),
                );
            }
        }
        for pw in &prepared {
            blend_glyph(
                data,
                canvas_w,
                canvas_h,
                pw.x0,
                pw.y0,
                &pw.wm.mask,
                pw.wm.w,
                pw.wm.h,
                pw.color,
            );
        }
    } else {
        // Fast path: no glyph effects and no scale, so the original per-glyph
        // composition stays bit-identical for existing styles.
        for (li, line) in m.block.lines.iter().enumerate() {
            let line_baseline = baseline0 + li as f32 * line_h;
            for (k, &wi) in line.words.iter().enumerate() {
                let is_active = wi == active;
                let font = face_for(faces, style, is_active);
                let color = with_opacity(
                    if is_active {
                        style.text.highlight
                    } else {
                        style.text.dim
                    },
                    style.text.opacity,
                );
                let spacing = style.font.letter_spacing * px;
                let chars: Vec<char> = display[wi].chars().collect();
                let mut pen = box_x + pad_x + line.offset + line.slots[k];
                for (ci, c) in chars.iter().enumerate() {
                    let advance = font.metrics(*c, px).advance_width;
                    let (m, coverage) = font.rasterize(*c, px);
                    if m.width > 0 && m.height > 0 {
                        let ox = (pen + m.xmin as f32).round() as i32;
                        let oy = (line_baseline - m.ymin as f32 - m.height as f32).round() as i32;
                        blend_glyph(
                            data, canvas_w, canvas_h, ox, oy, &coverage, m.width, m.height, color,
                        );
                    }
                    pen += advance;
                    if ci + 1 < chars.len() {
                        pen += spacing;
                    }
                }
            }
        }
    }

    if has_active && style.highlight.underline.enabled {
        let u = &style.highlight.underline;
        let h = (u.width * active_px).max(1.0);
        fill_shape(
            &mut pixmap,
            active_x,
            active_baseline + active_desc + u.offset * active_px,
            active_w,
            h,
            0.0,
            with_opacity(u.color, style.text.opacity),
        );
    }

    // --- unpremultiply + encode ---
    let mut straight = pixmap.take();
    #[allow(clippy::manual_checked_ops)] // guarded by `a > 0`
    for p in straight.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a > 0 {
            p[0] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    Ok(RenderedCaption {
        rgba: straight,
        width: canvas_w as u32,
        height: canvas_h as u32,
        box_top: box_y,
        box_h,
    })
}

/// Render one state and save it as a PNG. Returns the box's placement
/// metadata within the PNG.
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
fn render_state(
    faces: &Faces,
    style: &Style,
    words: &[String],
    active: usize,
    font_px: f32,
    frame_w: u32,
    frame_h: u32,
    out: &Path,
) -> Result<(f32, f32)> {
    let RenderedCaption {
        rgba,
        width,
        height,
        box_top,
        box_h,
    } = render_state_rgba(faces, style, words, active, font_px, frame_w, frame_h)?;
    let img =
        image::RgbaImage::from_raw(width, height, rgba).context("RGBA buffer size mismatch")?;
    img.save(out)
        .with_context(|| format!("failed to write {}", out.display()))?;
    Ok((box_top, box_h))
}

/// Render every state of every chunk into `out_dir`.
///
/// `on_state` receives 0..=1 after each state (for the GUI's export progress).
/// Returns states in chronological order with their visibility windows.
/// Render every caption state for `chunks` into `out_dir`.
///
/// `cancel` is polled once per state; when it is set the pass stops with a
/// "cancelled" error (part-rendered PNGs are left to the caller's temp dir).
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
pub fn render_states(
    style: &Style,
    chunks: &[Chunk],
    frame_w: u32,
    frame_h: u32,
    out_dir: &Path,
    cancel: &AtomicBool,
    mut on_state: impl FnMut(f32),
) -> Result<Vec<CaptionState>> {
    let mut style = style.clone();
    style.sanitize();

    // Checked before the fonts are loaded so a cancel that arrives before the
    // pass starts doesn't pay for it.
    if cancel.load(Ordering::Relaxed) {
        bail!("export cancelled");
    }

    let font_px = font_px(&style, frame_h);
    let faces = Faces::load(&style, font_px)?;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let total: usize = chunks.iter().map(|c| c.words.len()).sum();
    eprintln!(
        "      font={}  size={font_px:.0}px  states={total}",
        style.font.family
    );

    let mut states = Vec::with_capacity(total);
    let mut done = 0usize;
    for (ci, chunk) in chunks.iter().enumerate() {
        let words: Vec<String> = chunk.words.iter().map(|w| w.text.clone()).collect();
        let windows = chunk.state_windows();
        for (k, &(start, end)) in windows.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                bail!("export cancelled");
            }
            let path = out_dir.join(format!("c{ci:04}_s{k:02}.png"));
            let (box_top, box_h) =
                render_state(&faces, &style, &words, k, font_px, frame_w, frame_h, &path)
                    .with_context(|| format!("failed to render state {k} of chunk {ci}"))?;
            states.push(CaptionState {
                png: path,
                start_secs: start,
                end_secs: end,
                box_top,
                box_h,
                anchor: style.layout.anchor,
                popup_start_secs: chunk.start_secs,
                font_px,
            });
            done += 1;
            if total > 0 {
                on_state(done as f32 / total as f32);
            }
        }
    }
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancel that lands before the pass starts stops it immediately, and the
    /// message says "cancelled" so the GUI can tell it from a failure.
    #[test]
    fn a_cancelled_pass_stops_without_rendering() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let err = render_states(
            &Style::default(),
            &[],
            200,
            200,
            dir.path(),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("cancelled"), "{err:#}");
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "nothing should be written"
        );
    }

    /// With nothing to render and no cancel, the pass is a clean no-op.
    #[test]
    fn an_empty_pass_renders_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let states = render_states(
            &Style::default(),
            &[],
            200,
            200,
            dir.path(),
            &cancel,
            |_| {},
        )
        .unwrap();
        assert!(states.is_empty());
    }

    /// FNV-1a, stable across toolchains (unlike `DefaultHasher`).
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn render_test_style(style: &Style, words: &[&str], active: usize) -> (Vec<u8>, u32, u32) {
        let px = font_px(style, 1920);
        let faces = Faces::load(style, px).unwrap();
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        let cap = render_state_rgba(&faces, style, &words, active, px, 1080, 1920).unwrap();
        (cap.rgba, cap.width, cap.height)
    }

    fn no_box_style() -> Style {
        let mut s = Style::default();
        s.box_style.opacity = 0.0;
        s.box_style.shadow = false;
        s
    }

    fn visible(rgba: &[u8]) -> usize {
        rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 8).count()
    }

    #[test]
    fn outline_extends_beyond_the_caption() {
        let base = no_box_style();
        let (b, bw, bh) = render_test_style(&base, &["Make", "every", "word"], 1);
        let mut outlined = base.clone();
        outlined.text.outline.enabled = true;
        outlined.text.outline.width = 0.12;
        let (o, ow, oh) = render_test_style(&outlined, &["Make", "every", "word"], 1);
        assert!(ow > bw && oh > bh, "effect margin should grow the canvas");
        assert!(visible(&o) > visible(&b), "outline adds visible pixels");
        assert_ne!(fnv1a(&o), fnv1a(&b));
    }

    #[test]
    fn pill_adds_a_plate_behind_the_active_word() {
        let base = no_box_style();
        let (b, ..) = render_test_style(&base, &["Make", "every", "word"], 1);
        let mut pill = base.clone();
        pill.highlight.pill.enabled = true;
        pill.highlight.pill.color = Color::new(255, 40, 40, 255);
        let (p, ..) = render_test_style(&pill, &["Make", "every", "word"], 1);
        assert!(
            visible(&p) > visible(&b) + 100,
            "pill should be a large plate"
        );
    }

    #[test]
    fn uppercase_and_letter_spacing_change_the_rendering() {
        let base = no_box_style();
        let (b, bw, _) = render_test_style(&base, &["make", "every", "word"], 1);

        let mut upper = base.clone();
        upper.font.uppercase = true;
        let (u, ..) = render_test_style(&upper, &["make", "every", "word"], 1);
        assert_ne!(fnv1a(&u), fnv1a(&b));

        let mut spaced = base.clone();
        spaced.font.letter_spacing = 0.10;
        let (_, sw, _) = render_test_style(&spaced, &["make", "every", "word"], 1);
        assert!(sw > bw, "tracking should widen the caption");
    }

    #[test]
    fn active_scale_widens_and_tallens_the_box() {
        let base = no_box_style();
        let (_, bw, bh) = render_test_style(&base, &["Make", "every", "word"], 1);
        let mut scaled = base.clone();
        scaled.highlight.scale = 1.20;
        let (_, sw, sh) = render_test_style(&scaled, &["Make", "every", "word"], 1);
        assert!(sw > bw);
        assert!(sh >= bh);
    }

    #[test]
    fn box_border_grows_the_canvas() {
        let mut base = no_box_style();
        base.box_style.opacity = 1.0;
        let (_, bw, bh) = render_test_style(&base, &["Make", "every", "word"], 1);
        let mut bordered = base.clone();
        bordered.box_style.border_width = 0.10;
        bordered.box_style.border_color = Color::new(255, 0, 0, 255);
        let (_, w, h) = render_test_style(&bordered, &["Make", "every", "word"], 1);
        assert!(w > bw && h > bh, "border margin should grow the canvas");
    }

    #[test]
    fn text_opacity_scales_the_glyph_alpha() {
        let mut style = no_box_style();
        style.text.opacity = 0.5;
        let (rgba, ..) = render_test_style(&style, &["Make", "every", "word"], 1);
        let max = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3])
            .max()
            .unwrap_or(0);
        assert!(max > 0 && max <= 128, "max alpha was {max}");
    }

    #[test]
    fn break_lines_balances_the_slack() {
        let widths = vec![10.0, 10.0, 10.0, 10.0];
        // 3 words fit (34 <= 35), so the minimum is two lines; the balanced
        // split is 2 + 2 rather than 3 + 1.
        assert_eq!(break_lines(&widths, 2.0, 35.0, 3, 0), vec![(0, 2), (2, 4)]);
        // A word cap is honoured even with plenty of horizontal room.
        assert_eq!(
            break_lines(&widths, 2.0, 1000.0, 3, 2),
            vec![(0, 2), (2, 4)]
        );
        // One line is a hard cap.
        assert_eq!(break_lines(&widths, 2.0, 35.0, 1, 0), vec![(0, 4)]);
        // An oversized single word still gets a line.
        assert_eq!(break_lines(&[100.0], 2.0, 50.0, 2, 0), vec![(0, 1)]);
        assert!(break_lines(&[], 2.0, 50.0, 2, 0).is_empty());
    }

    #[test]
    fn wrap_makes_a_taller_block_than_one_line() {
        let mut wrapped = no_box_style();
        wrapped.layout.fit = Fit::Wrap;
        wrapped.layout.max_lines = 2;
        wrapped.layout.max_width_pct = 40.0;
        wrapped.font.size_pct = 2.5;

        let mut one_line = wrapped.clone();
        one_line.layout.fit = Fit::Shrink;
        one_line.layout.max_width_pct = 92.0;

        let words = ["another", "long", "caption", "line"];
        let (_, _, h_wrap) = render_test_style(&wrapped, &words, 0);
        let (_, _, h_one) = render_test_style(&one_line, &words, 0);
        assert!(h_wrap > h_one, "wrapped={h_wrap} one={h_one}");
    }

    #[test]
    fn reserve_lines_keeps_the_box_height_stable() {
        let mut short = no_box_style();
        short.layout.reserve_lines = 2;
        short.font.size_pct = 3.0;
        let (_, _, h_short) = render_test_style(&short, &["short"], 0);

        let mut wrapped = short.clone();
        wrapped.layout.fit = Fit::Wrap;
        wrapped.layout.max_lines = 2;
        wrapped.layout.max_width_pct = 40.0;
        let (_, _, h_wrapped) = render_test_style(&wrapped, &["a", "much", "caption"], 0);
        assert_eq!(h_short, h_wrapped);
    }

    #[test]
    fn fixed_box_dimensions_are_honoured() {
        let mut style = no_box_style();
        style.layout.fit = Fit::Wrap;
        style.layout.max_lines = 2;
        style.layout.box_width_pct = Some(50.0);
        style.layout.box_height_pct = Some(10.0);
        style.font.size_pct = 3.0;
        let (_, w, h) = render_test_style(&style, &["one", "two", "three", "four"], 0);
        // 1080 * 50% = 540; 1920 * 10% = 192. No shadow/effects, so margin = 0.
        assert!((w as f32 - 540.0).abs() < 2.0, "width was {w}");
        assert!((h as f32 - 192.0).abs() < 2.0, "height was {h}");
    }

    #[test]
    fn clip_renders_without_overflowing() {
        let mut style = no_box_style();
        style.layout.fit = Fit::Clip;
        style.layout.max_lines = 1;
        style.layout.max_width_pct = 20.0;
        style.font.size_pct = 2.0;
        let (rgba, w, _) = render_test_style(&style, &["one", "two", "three", "four"], 0);
        assert!(w <= 216 + 2, "clip box width was {w}");
        assert!(visible(&rgba) > 0);
    }

    #[test]
    fn popup_transforms_are_static_once_settled() {
        let mut m = MotionStyle {
            popup: PopupEffect::Pop,
            duration_ms: 180,
            easing: Easing::EaseOut,
            intensity: 0.2,
        };
        assert!(popup_transform(&m, 1.0, 50.0).is_identity());
        assert!((popup_transform(&m, 0.0, 50.0).scale - 0.8).abs() < 1e-4);

        m.popup = PopupEffect::SlideUp;
        assert!((popup_transform(&m, 0.0, 50.0).dy - 10.0).abs() < 1e-4);

        m.popup = PopupEffect::Fade;
        assert_eq!(popup_transform(&m, 0.0, 50.0).alpha, 0.0);

        // Spring overshoots above 1 before settling.
        m.popup = PopupEffect::Pop;
        m.easing = Easing::Spring;
        let t = popup_transform(&m, 0.7, 50.0);
        assert!(t.scale > 1.0, "spring scale was {}", t.scale);
    }

    #[test]
    fn popup_motion_progresses_monotonically() {
        let pop = MotionStyle {
            popup: PopupEffect::Pop,
            duration_ms: 300,
            easing: Easing::EaseOut,
            intensity: 0.25,
        };
        let scales: Vec<f32> = (0..=10)
            .map(|i| popup_transform(&pop, i as f32 / 10.0, 50.0).scale)
            .collect();
        assert!(scales.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert!((scales[10] - 1.0).abs() < 1e-4);

        let slide = MotionStyle {
            popup: PopupEffect::SlideUp,
            duration_ms: 300,
            easing: Easing::EaseOut,
            intensity: 0.6,
        };
        let offsets: Vec<f32> = (0..=10)
            .map(|i| popup_transform(&slide, i as f32 / 10.0, 50.0).dy)
            .collect();
        assert!(offsets.windows(2).all(|w| w[1] <= w[0] + 1e-6));
        assert!(offsets[10].abs() < 1e-4);

        let fade = MotionStyle {
            popup: PopupEffect::Fade,
            duration_ms: 300,
            easing: Easing::EaseOut,
            intensity: 0.0,
        };
        let alphas: Vec<f32> = (0..=10)
            .map(|i| popup_transform(&fade, i as f32 / 10.0, 50.0).alpha)
            .collect();
        assert!(alphas.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert!((alphas[10] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn transformed_blit_offsets_fades_and_scales() {
        // Offset a single red pixel by 5px.
        let src = [255u8, 0, 0, 255];
        let mut dst = [0u8, 0, 0, 255].repeat(8 * 8);
        let shifted = PopupTransform {
            dx: 5.0,
            ..PopupTransform::IDENTITY
        };
        blend_transformed(&mut dst, 8, 8, &src, 1, 1, 0, 0, 0.0, 0.0, shifted);
        // Pixel (x = 5, y = 0).
        let center = 5 * 4;
        assert_eq!(&dst[center..center + 3], &[255, 0, 0]);
        let origin = 0;
        assert_eq!(&dst[origin..origin + 3], &[0, 0, 0]);

        // A 50% alpha fade lands at roughly half red.
        let mut faded = [0u8, 0, 0, 255].repeat(8 * 8);
        let half = PopupTransform {
            alpha: 0.5,
            ..PopupTransform::IDENTITY
        };
        blend_transformed(&mut faded, 8, 8, &src, 1, 1, 0, 0, 0.0, 0.0, half);
        assert!((faded[0] as i32 - 128).abs() <= 1, "red was {}", faded[0]);

        // A 2x scale around the centre of a 2x2 red block fills its interior.
        let block = [255u8, 0, 0, 255].repeat(4);
        let mut scaled = [0u8, 0, 0, 255].repeat(8 * 8);
        let big = PopupTransform {
            scale: 2.0,
            ..PopupTransform::IDENTITY
        };
        blend_transformed(&mut scaled, 8, 8, &block, 2, 2, 0, 0, 1.0, 1.0, big);
        // Pixel (x = 1, y = 1).
        let interior = (8 + 1) * 4;
        assert_eq!(&scaled[interior..interior + 3], &[255, 0, 0]);
    }

    #[test]
    fn box_blur_spreads_a_single_pixel() {
        let mut m = vec![0u8; 9 * 9];
        m[4 * 9 + 4] = 255;
        let b = box_blur(&m, 9, 9, 1);
        assert!(b[4 * 9 + 4] > 0 && b[4 * 9 + 4] < 255);
        assert!(b[4 * 9 + 3] > 0);
        assert!(b[3 * 9 + 4] > 0);
    }

    #[test]
    fn distance_transform_measures_pixels_from_the_glyph() {
        let mut m = vec![0u8; 9 * 9];
        m[4 * 9 + 4] = 255;
        let d = distance_transform(&m, 9, 9);
        assert_eq!(d[4 * 9 + 4], 0.0);
        assert!((d[4 * 9 + 5] - 1.0).abs() < 1e-4);
        assert!((d[3 * 9 + 3] - std::f32::consts::SQRT_2).abs() < 1e-4);
    }

    /// The default look is the compatibility baseline: the v2→v3 config
    /// migration and every later style addition must leave it pixel-identical.
    /// If this fails after an intentional visual change, update the hash.
    #[test]
    fn default_style_pixels_are_stable() {
        let style = Style::default();
        let px = font_px(&style, 1920);
        let faces = Faces::load(&style, px).unwrap();
        let words = vec!["Make".to_string(), "every".to_string(), "word".to_string()];
        let cap = render_state_rgba(&faces, &style, &words, 1, px, 1080, 1920).unwrap();
        let (rgba, w, h) = (cap.rgba, cap.width, cap.height);
        assert_eq!((w, h), (713, 143));
        assert_eq!(
            fnv1a(&rgba),
            0x87f2_60ff_c735_2900,
            "default caption pixels changed"
        );
    }
}
