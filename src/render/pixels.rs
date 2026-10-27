//! Pixel work: blending, blur, distance fields, masks and glyph stamping.
//!
//! Everything here is plain RGBA buffer maths: it takes a face, a colour and a
//! buffer, and never touches caption text, layout or the export pipeline.

use crate::config::{Color, Style};
use fontdue::Font;

use super::layout::{ink_extent, word_advance, PROBE_ASC, PROBE_DESC};
use super::motion::PopupTransform;

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

/// Bilinear sample of a straight-alpha RGBA source into premultiplied RGBA.
pub(super) fn sample_premul(
    src: &[u8],
    sw: usize,
    sh: usize,
    x: i32,
    y: i32,
) -> (f32, f32, f32, f32) {
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

/// Multiply a colour's alpha by a 0..=1 opacity.
pub(super) fn with_opacity(color: Color, opacity: f32) -> Color {
    Color {
        a: (color.a as f32 * opacity.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, 255.0) as u8,
        ..color
    }
}

/// A separable box blur over an 8-bit coverage mask, with clamped edges.
pub(super) fn box_blur(src: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
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
pub(super) fn distance_transform(mask: &[u8], w: usize, h: usize) -> Vec<f32> {
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
pub(super) fn word_mask_pad_em(style: &Style) -> f32 {
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
pub(super) fn canvas_effect_margin_em(style: &Style) -> f32 {
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

pub(super) struct WordMask {
    pub(super) mask: Vec<u8>,
    pub(super) w: usize,
    pub(super) h: usize,
    pub(super) pad: f32,
    pub(super) ascent: f32,
}

/// Rasterize one word into a padded coverage mask, ready for effects.
pub(super) fn word_mask(face: &Font, style: &Style, text: &str, px: f32) -> WordMask {
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
pub(super) fn fill_shape(
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
pub(super) fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
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
pub(super) fn blend_glyph(
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
