//! Live preview: a real video frame with the current caption state composited
//! on top, using the same renderer and placement math as the export — what you
//! see here is what gets burned in.
//!
//! Frame extraction (ffmpeg) runs on a worker thread; the caption overlay is
//! re-rendered in-memory whenever style, text, or the highlighted word changes
//! (fontdue makes that a few milliseconds).

use anyhow::{Context, Result};
use image::imageops::FilterType;

use crate::config::Style;
use crate::render::{self, blend_straight, blend_transformed, Faces, MotionPhase};
use crate::{export, probe};

/// Preview texture width in pixels; the height follows the video's aspect.
const PREVIEW_WIDTH: u32 = 480;

/// A decoded full-resolution video frame (straight RGBA, fully opaque).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Extract a frame at `at_secs` and decode it. Meant for a worker thread.
pub fn extract(tools: &probe::Fftools, input: &std::path::Path, at_secs: f64) -> Result<Frame> {
    let tmp = tempfile::tempdir().context("frame temp dir")?;
    let png = tmp.path().join("frame.png");
    export::extract_frame(tools, input, at_secs, &png)?;
    let img = image::open(&png)
        .context("failed to decode extracted frame")?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Ok(Frame {
        width,
        height,
        rgba: img.into_raw(),
    })
}

/// Scale a caption crop from full source resolution to the preview scale by
/// `factor` (the same factor the frame was downscaled by). Preserves aspect;
/// returns the buffer unchanged when no scaling is needed.
fn scale_caption(caption: Vec<u8>, cw: u32, ch: u32, factor: f32) -> (Vec<u8>, u32, u32) {
    if cw == 0 || ch == 0 {
        return (caption, cw, ch);
    }
    let sw = (cw as f32 * factor).round().max(1.0) as u32;
    let sh = (ch as f32 * factor).round().max(1.0) as u32;
    if sw == cw && sh == ch {
        return (caption, cw, ch);
    }
    let img = image::RgbaImage::from_raw(cw, ch, caption).expect("caption buffer size mismatch");
    let small = image::imageops::resize(&img, sw, sh, FilterType::Lanczos3);
    (small.into_raw(), sw, sh)
}

/// A scaled RGBA surface (frame base and composite results).
#[derive(Clone)]
struct Surface {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Loaded fonts for caption rendering, keyed by family + pixel size.
struct FaceCache {
    family: String,
    font_px: f32,
    faces: Faces,
}

/// Preview state kept across UI frames.
pub struct Preview {
    /// The extracted frame, downscaled to `PREVIEW_WIDTH`.
    base: Option<Surface>,
    /// The source frame's full resolution (caption renders at export scale,
    /// including the 92%-of-width clamp, then scales down with the frame).
    src_size: (u32, u32),
    /// Uploaded texture of the composite (base + caption).
    texture: Option<egui::TextureHandle>,
    /// Whether `texture` reflects the current style/text/word.
    composite_built: bool,
    /// Last caption render failure, surfaced in the preview pane.
    pub caption_error: Option<String>,
    faces: Option<FaceCache>,
    /// Motion phase the current composite was built for.
    last_motion: MotionPhase,
}

impl Default for Preview {
    fn default() -> Self {
        Preview {
            base: None,
            src_size: (0, 0),
            texture: None,
            composite_built: false,
            caption_error: None,
            faces: None,
            last_motion: MotionPhase::Settled,
        }
    }
}

impl Preview {
    /// Swap in a freshly extracted frame (invalidates the composite).
    pub fn set_frame(&mut self, frame: Frame) {
        let src_size = (frame.width, frame.height);
        let target = PREVIEW_WIDTH.min(frame.width).max(1);
        let base = if frame.width <= PREVIEW_WIDTH {
            Surface {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
            }
        } else {
            let h = ((frame.height as u64 * target as u64) / frame.width as u64).max(1) as u32;
            let img = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
                .expect("frame buffer size mismatch");
            let small = image::imageops::resize(&img, target, h, FilterType::Lanczos3);
            Surface {
                width: target,
                height: h,
                rgba: small.into_raw(),
            }
        };
        self.base = Some(base);
        self.src_size = src_size;
        self.composite_built = false;
        self.caption_error = None;
    }

    /// Style/text/highlight changed: the composite needs a rebuild (the frame
    /// itself stays cached).
    pub fn invalidate(&mut self) {
        self.composite_built = false;
        self.caption_error = None;
    }

    /// Display size of the preview texture, if a frame has been extracted.
    pub fn image_size(&self) -> Option<(u32, u32)> {
        self.base.as_ref().map(|b| (b.width, b.height))
    }

    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        self.texture.as_ref()
    }

    /// Rebuild the composite if it is stale. `words` is the selected chunk's
    /// caption text; `active` is the highlighted word index.
    pub fn ensure(
        &mut self,
        ctx: &egui::Context,
        style: &Style,
        words: &[String],
        active: usize,
        motion: MotionPhase,
    ) {
        if self.composite_built && self.last_motion == motion {
            return;
        }
        let Some(base) = self.base.clone() else {
            return;
        };
        let data = self.build_composite(style, words, active, motion);
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [base.width as usize, base.height as usize],
            &data,
        );
        self.texture = Some(ctx.load_texture("preview_frame", image, egui::TextureOptions::LINEAR));
        self.composite_built = true;
        self.last_motion = motion;
    }

    /// Composite the caption onto the (already downscaled) preview frame and
    /// return the straight-alpha RGBA buffer. Split out from [`Self::ensure`]
    /// so the exact pixels the preview shows are testable without a GPU.
    fn build_composite(
        &mut self,
        style: &Style,
        words: &[String],
        active: usize,
        motion: MotionPhase,
    ) -> Vec<u8> {
        let Some(base) = self.base.clone() else {
            return Vec::new();
        };
        let (src_w, src_h) = self.src_size;
        let mut data = base.rgba.clone();

        if !words.is_empty() && !motion.is_hidden() {
            match self.render_caption(style, words, active, src_w, src_h) {
                Ok(cap) => {
                    // Scale the caption by the *same* factor the frame was
                    // scaled by (base.width / src_w) — not up to base.width,
                    // which would stretch every crop to the full frame width
                    // and make preview text far larger than the export's.
                    let factor = base.width as f32 / src_w.max(1) as f32;
                    let box_top = cap.box_top * factor;
                    let box_h = cap.box_h * factor;
                    let (scaled, sw, sh) = scale_caption(cap.rgba, cap.width, cap.height, factor);
                    let x = ((base.width as f32 - sw as f32) / 2.0).round() as i32;
                    let y = crate::export::caption_y(
                        base.height,
                        style.layout.position_pct,
                        style.layout.anchor,
                        box_top,
                        box_h,
                    )
                    .round() as i32;

                    let transform =
                        motion.transform(&style.motion, render::font_px(style, src_h) * factor);
                    if transform.is_identity() {
                        blend_straight(&mut data, base.width, base.height, &scaled, sw, sh, x, y);
                    } else {
                        let pivot_x = sw as f32 / 2.0;
                        let pivot_y = match style.layout.anchor {
                            crate::config::Anchor::Center => box_top + box_h / 2.0,
                            crate::config::Anchor::Top => box_top,
                            crate::config::Anchor::Bottom => box_top + box_h,
                        };
                        blend_transformed(
                            &mut data,
                            base.width,
                            base.height,
                            &scaled,
                            sw,
                            sh,
                            x,
                            y,
                            pivot_x,
                            pivot_y,
                            transform,
                        );
                    }
                }
                Err(e) => self.caption_error = Some(format!("{e:#}")),
            }
        }

        data
    }

    /// Render one caption state at full source resolution (export parity).
    fn render_caption(
        &mut self,
        style: &Style,
        words: &[String],
        active: usize,
        src_w: u32,
        src_h: u32,
    ) -> Result<render::RenderedCaption> {
        let font_px = render::font_px(style, src_h);
        let needs_reload = match &self.faces {
            Some(c) => c.family != style.font.family || (c.font_px - font_px).abs() >= 1e-3,
            None => true,
        };
        if needs_reload {
            self.faces = Some(FaceCache {
                family: style.font.family.clone(),
                font_px,
                faces: Faces::load(style, font_px)?,
            });
        }
        let faces = &self.faces.as_ref().expect("faces loaded").faces;
        render::render_state_rgba(faces, style, words, active, font_px, src_w, src_h)
            .context("caption render failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Color, Easing, MotionStyle, PopupEffect};
    use crate::render::{font_px, render_state_rgba};

    /// The reporter's saved style: a very small 1.1% font, 4 words per popup.
    fn reporter_style() -> Style {
        Style {
            font: crate::config::FontStyle {
                size_pct: 1.1,
                ..Default::default()
            },
            layout: crate::config::LayoutStyle {
                position_pct: 83.0,
                ..Default::default()
            },
            text: crate::config::TextStyle {
                highlight: Color::new(94, 232, 86, 255),
                dim: Color::new(212, 175, 175, 255),
                ..Default::default()
            },
            box_style: crate::config::BoxStyle {
                opacity: 0.68,
                radius_frac: 0.39,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// A flat gray frame (uniform base makes the caption box easy to isolate).
    fn flat_frame(w: u32, h: u32) -> Frame {
        let mut rgba = vec![60u8; (w * h * 4) as usize];
        for p in rgba.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        Frame {
            width: w,
            height: h,
            rgba,
        }
    }

    /// Bounding box of the (dark) caption box within a composite, as
    /// `(w, h)` fractions of the frame.
    fn caption_frac(data: &[u8], w: u32, h: u32) -> (f32, f32) {
        let mut x0 = u32::MAX;
        let mut y0 = u32::MAX;
        let mut x1 = 0u32;
        let mut y1 = 0u32;
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let lum = data[i] as u32 + data[i + 1] as u32 + data[i + 2] as u32;
                if lum < 3 * 45 {
                    any = true;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        assert!(any, "no caption box found in the composite");
        (
            (x1 - x0 + 1) as f32 / w as f32,
            (y1 - y0 + 1) as f32 / h as f32,
        )
    }

    #[test]
    fn preview_applies_popup_motion() {
        let mut style = reporter_style();
        style.motion = MotionStyle {
            popup: PopupEffect::Pop,
            duration_ms: 180,
            easing: Easing::EaseOut,
            intensity: 0.25,
            ..MotionStyle::default()
        };
        let words: Vec<String> = ["Alright,", "I", "wanted", "to"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut preview = Preview::default();
        preview.set_frame(flat_frame(1080, 1920));
        let start = preview.build_composite(&style, &words, 0, MotionPhase::Entrance(0.0));
        let settled = preview.build_composite(&style, &words, 0, MotionPhase::Settled);
        assert_ne!(start, settled);
    }

    /// End-to-end guard for the reported bug: the preview must show the caption
    /// at the *same fraction of the frame* as the export. Before the fix the
    /// crop was stretched to the preview width, so at 1.1% the preview read as
    /// ~90% wide while the video was ~23%.
    #[test]
    fn preview_caption_matches_export_fraction() {
        let style = reporter_style();
        let (src_w, src_h) = (1080u32, 1920u32);
        let words: Vec<String> = ["Alright,", "I", "wanted", "to"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Export reference: caption blended onto a full-res frame.
        let fpx = font_px(&style, src_h);
        let faces = Faces::load(&style, fpx).unwrap();
        let cap = render_state_rgba(&faces, &style, &words, 0, fpx, src_w, src_h).unwrap();
        let (caption, cw, ch) = (cap.rgba, cap.width, cap.height);
        let mut full = flat_frame(src_w, src_h).rgba;
        let fx = ((src_w as f32 - cw as f32) / 2.0).round() as i32;
        let fy = crate::export::caption_y(
            src_h,
            style.layout.position_pct,
            style.layout.anchor,
            cap.box_top,
            cap.box_h,
        )
        .round() as i32;
        blend_straight(&mut full, src_w, src_h, &caption, cw, ch, fx, fy);
        let (export_w, export_h) = caption_frac(&full, src_w, src_h);

        // What the GUI preview actually composites.
        let mut preview = Preview::default();
        preview.set_frame(flat_frame(src_w, src_h));
        let (pw, ph) = preview.image_size().unwrap();
        let data = preview.build_composite(&style, &words, 0, MotionPhase::Settled);
        let (preview_w, preview_h) = caption_frac(&data, pw, ph);

        eprintln!("export : frac_w={export_w:.3} frac_h={export_h:.3} (frame {src_w}x{src_h})");
        eprintln!("preview: frac_w={preview_w:.3} frac_h={preview_h:.3} (frame {pw}x{ph})");

        assert!(
            (preview_w - export_w).abs() < 0.01,
            "preview width {preview_w:.3} != export {export_w:.3}"
        );
        assert!(
            (preview_h - export_h).abs() < 0.01,
            "preview height {preview_h:.3} != export {export_h:.3}"
        );
        // The regression: the preview must not stretch the crop to full width.
        assert!(
            preview_w < 0.5,
            "caption stretched to full width: {preview_w:.3}"
        );

        let dir = std::path::Path::new("/tmp/textlayr_diag");
        std::fs::create_dir_all(dir).unwrap();
        image::RgbaImage::from_raw(src_w, src_h, full)
            .unwrap()
            .save(dir.join("export_1080.png"))
            .unwrap();
        image::RgbaImage::from_raw(pw, ph, data)
            .unwrap()
            .save(dir.join("preview_actual.png"))
            .unwrap();
    }

    #[test]
    fn caption_scales_with_the_frame_not_to_full_width() {
        // A 700x150 crop from a 1080-wide source, previewed at 480 wide, must
        // scale to ~311x67 (factor 480/1080) — not stretch to 480 wide, which
        // would make the preview text ~1.5x larger than the export's.
        let factor = 480.0 / 1080.0;
        let (rgba, sw, sh) = scale_caption(vec![0u8; 700 * 150 * 4], 700, 150, factor);
        assert_eq!(sw, (700.0 * factor).round() as u32);
        assert_eq!(sh, (150.0 * factor).round() as u32);
        assert_eq!(rgba.len(), (sw * sh * 4) as usize);
        // A one-to-one factor is a no-op (same buffer, no resample).
        let (_, sw, sh) = scale_caption(vec![0u8; 40], 10, 1, 1.0);
        assert_eq!((sw, sh), (10, 1));
    }
}
