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
use crate::config::{self, Color, Style};
use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};
use std::path::{Path, PathBuf};

/// One rendered highlight state with the time window it should be visible.
#[derive(Debug, Clone)]
pub struct CaptionState {
    pub png: PathBuf,
    pub start_secs: f64,
    pub end_secs: f64,
}

/// A font family loaded at a given pixel size: regular + bold faces plus the
/// font-wide ink metrics used to size the box.
///
/// Public so the GUI preview can keep one loaded per (family, size) and
/// re-render caption states without re-reading the TTFs every frame.
pub struct Faces {
    regular: Font,
    bold: Font,
    /// Distance from baseline up to the tallest ascender/cap (px).
    ascent: f32,
    /// Distance from baseline down to the lowest descender (px).
    descent: f32,
}

/// Caption glyph size in pixels for a style on a frame of `frame_h` pixels.
pub fn font_px(style: &Style, frame_h: u32) -> f32 {
    style.font_size_pct / 100.0 * frame_h as f32
}

/// Straight-alpha blend of the caption `src` (non-premultiplied RGBA) onto the
/// fully-opaque `dst` frame at (ox, oy), clipped to the destination.
///
/// Shared by the GUI preview and the raw-pipe exporter so both composite
/// exactly like the state PNGs on screen.
pub fn blend_straight(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32, ox: i32, oy: i32) {
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

/// Probe strings covering ascenders/caps and descenders.
const PROBE_ASC: &str = "bdfhklH";
const PROBE_DESC: &str = "gjpqyQ";

impl Faces {
    pub fn load(style: &Style, px: f32) -> Result<Faces> {
        let (dir, _) = config::find_fonts_dir()?;
        let (regular_path, bold_path) = config::font_paths(&dir, &style.font_family)?;
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
        Ok(Faces {
            ascent: ink_extent(&regular, PROBE_ASC, px, true),
            descent: ink_extent(&regular, PROBE_DESC, px, false),
            regular,
            bold,
        })
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

/// Horizontal layout of one chunk's words: stable slots so the box width never
/// changes between states of the same chunk.
struct Layout {
    /// Left edge of each word slot, relative to the text origin.
    slots: Vec<f32>,
    /// Total text width.
    width: f32,
}

fn word_advance(font: &Font, text: &str, px: f32) -> f32 {
    text.chars().map(|c| font.metrics(c, px).advance_width).sum()
}

fn space_gap(faces: &Faces, px: f32) -> f32 {
    faces.regular.metrics(' ', px).advance_width * 1.15
}

fn layout(faces: &Faces, words: &[String], px: f32) -> Layout {
    let gap = space_gap(faces, px);
    let mut slots = Vec::with_capacity(words.len());
    let mut x = 0f32;
    for (i, w) in words.iter().enumerate() {
        slots.push(x);
        // Slot width = the wider of regular/bold so switching weight on the
        // highlighted word never shifts the rest of the line.
        let a = word_advance(&faces.regular, w, px);
        let b = word_advance(&faces.bold, w, px);
        x += a.max(b);
        if i + 1 < words.len() {
            x += gap;
        }
    }
    Layout { slots, width: x }
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
pub fn render_state_rgba(
    faces: &Faces,
    style: &Style,
    words: &[String],
    active: usize,
    font_px: f32,
    frame_w: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let pad_x = 0.50 * font_px;
    let pad_y = 0.24 * font_px;

    // --- layout, shrinking the font if the chunk would overflow the frame ---
    let mut px = font_px;
    let mut lay = layout(faces, words, px);
    let max_text_w = 0.92 * frame_w as f32 - 2.0 * pad_x;
    let mut ascent = faces.ascent;
    let mut descent = faces.descent;
    if lay.width > max_text_w && max_text_w > 0.0 {
        px = (px * (max_text_w / lay.width)).max(font_px * 0.4);
        lay = layout(faces, words, px);
        // Box metrics follow the shrunk size, or the text would float in an
        // oversized box.
        ascent = ink_extent(&faces.regular, PROBE_ASC, px, true);
        descent = ink_extent(&faces.regular, PROBE_DESC, px, false);
    }

    let box_w = lay.width + 2.0 * pad_x;
    let box_h = ascent + descent + 2.0 * pad_y;

    // --- canvas with room for the shadow ---
    let margin = if style.shadow { 0.30 * px } else { 0.0 };
    let canvas_w = (box_w + 2.0 * margin).ceil().max(1.0) as usize;
    let canvas_h = (box_h + 2.0 * margin).ceil().max(1.0) as usize;
    let box_x = margin;
    let box_y = margin;

    let mut pixmap =
        tiny_skia::Pixmap::new(canvas_w as u32, canvas_h as u32).context("pixmap alloc")?;

    // --- shadow: stacked expanding rounded rects, faking a blur ---
    if style.shadow {
        const LAYERS: f32 = 5.0;
        for i in 0..LAYERS as usize {
            let t = (i + 1) as f32 / LAYERS;
            let grow = 0.14 * px * t;
            let drop = 0.10 * px * t;
            let alpha = (18.0 * (1.0 - t)) as u8 + 4;
            let radius = if style.rounded {
                style.corner_radius_frac * box_h + grow
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
                paint.set_color_rgba8(0, 0, 0, alpha);
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

    // --- box ---
    let radius = if style.rounded {
        style.corner_radius_frac * box_h
    } else {
        0.0
    };
    let box_alpha = ((style.box_color.a as f32 * style.box_opacity).round() as u8).min(255);
    if let Some(path) = rounded_rect_path(box_x, box_y, box_w, box_h, radius) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(
            style.box_color.r,
            style.box_color.g,
            style.box_color.b,
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
    }

    // --- glyphs ---
    let baseline = box_y + pad_y + ascent;
    let data = pixmap.data_mut();
    for (i, w) in words.iter().enumerate() {
        let is_active = i == active;
        let font = if is_active { &faces.bold } else { &faces.regular };
        let color = if is_active { style.highlight } else { style.dim };
        let mut pen = box_x + pad_x + lay.slots[i];
        for c in w.chars() {
            let advance = font.metrics(c, px).advance_width;
            let (m, coverage) = font.rasterize(c, px);
            if m.width > 0 && m.height > 0 {
                let ox = (pen + m.xmin as f32).round() as i32;
                let oy = (baseline - m.ymin as f32 - m.height as f32).round() as i32;
                blend_glyph(
                    data,
                    canvas_w,
                    canvas_h,
                    ox,
                    oy,
                    &coverage,
                    m.width,
                    m.height,
                    color,
                );
            }
            pen += advance;
        }
    }

    // --- unpremultiply + encode ---
    let mut straight = pixmap.take();
    for p in straight.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 {
            p[0] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    Ok((straight, canvas_w as u32, canvas_h as u32))
}

/// Render one state and save it as a PNG.
fn render_state(faces: &Faces, style: &Style, words: &[String], active: usize, font_px: f32, frame_w: u32, out: &Path) -> Result<()> {
    let (rgba, w, h) = render_state_rgba(faces, style, words, active, font_px, frame_w)?;
    let img = image::RgbaImage::from_raw(w, h, rgba).context("RGBA buffer size mismatch")?;
    img.save(out)
        .with_context(|| format!("failed to write {}", out.display()))?;
    Ok(())
}

/// Render every state of every chunk into `out_dir`.
///
/// `on_state` receives 0..=1 after each state (for the GUI's export progress).
/// Returns states in chronological order with their visibility windows.
pub fn render_states(
    style: &Style,
    chunks: &[Chunk],
    frame_w: u32,
    frame_h: u32,
    out_dir: &Path,
    mut on_state: impl FnMut(f32),
) -> Result<Vec<CaptionState>> {
    let mut style = style.clone();
    style.sanitize();

    let font_px = font_px(&style, frame_h);
    let faces = Faces::load(&style, font_px)?;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let total: usize = chunks.iter().map(|c| c.words.len()).sum();
    eprintln!(
        "      font={}  size={font_px:.0}px  states={total}",
        style.font_family
    );

    let mut states = Vec::with_capacity(total);
    let mut done = 0usize;
    for (ci, chunk) in chunks.iter().enumerate() {
        let words: Vec<String> = chunk.words.iter().map(|w| w.text.clone()).collect();
        let windows = chunk.state_windows();
        for (k, &(start, end)) in windows.iter().enumerate() {
            let path = out_dir.join(format!("c{ci:04}_s{k:02}.png"));
            render_state(&faces, &style, &words, k, font_px, frame_w, &path)
                .with_context(|| format!("failed to render state {k} of chunk {ci}"))?;
            states.push(CaptionState {
                png: path,
                start_secs: start,
                end_secs: end,
            });
            done += 1;
            if total > 0 {
                on_state(done as f32 / total as f32);
            }
        }
    }
    Ok(states)
}
