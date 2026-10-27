//! Render caption "states": one PNG per (chunk, highlighted word).
//!
//! A chunk of 3 words produces 3 PNGs — identical text, different word bright.
//! ffmpeg shows each PNG only during its word's time window.
//!
//! PNGs are **tight crops** of the caption box (not full-frame): the export
//! step positions them with percentage-based overlay expressions. Full-frame
//! PNGs at 1080x1920 would cost ~8 MB each (~4.5 GB RSS for 550 states); a
//! tight crop costs ~300 KB and composites identically.
//!
//! # Layout
//!
//! `layout` owns fonts and line breaking, `metrics` turns a broken block into
//! box geometry, `pixels` does the buffer maths, and `motion` is the popup
//! transform maths. The dependency order is one-way:
//!
//! ```text
//! layout  -> (nothing in this module)
//! motion  -> (nothing in this module)
//! pixels  -> layout, motion
//! metrics -> layout
//! mod.rs  -> all four
//! ```
//!
//! This file only sequences those pieces into a state PNG or RGBA buffer.

mod layout;
mod metrics;
mod motion;
mod pixels;

#[cfg(test)]
mod tests;

pub use layout::{font_px, Faces};
pub use motion::{popup_exit_transform, popup_transform, MotionPhase, PopupTransform};
pub use pixels::{blend_straight, blend_transformed};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};

use crate::chunk::Chunk;
use crate::config::{Anchor, Color, Fit, Style};

use layout::{display_text, face_for, ink_extent, word_advance, PROBE_ASC, PROBE_DESC};
use metrics::{build_metrics, wrap_fits};
use pixels::{
    blend_glyph, box_blur, canvas_effect_margin_em, distance_transform, fill_shape,
    rounded_rect_path, with_opacity, word_mask, WordMask,
};

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
    /// When the containing popup disappears (for exit motion).
    pub popup_end_secs: f64,
    /// Font size in pixels at render resolution (for slide distances).
    pub font_px: f32,
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
                popup_end_secs: chunk.end_secs,
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
