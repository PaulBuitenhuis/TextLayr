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
use crate::render::{self, blend_straight, Faces};
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
    let img = image::open(&png).context("failed to decode extracted frame")?.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(Frame {
        width,
        height,
        rgba: img.into_raw(),
    })
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
    pub fn ensure(&mut self, ctx: &egui::Context, style: &Style, words: &[String], active: usize) {
        if self.composite_built {
            return;
        }
        let Some(base) = self.base.clone() else { return };
        let (src_w, src_h) = self.src_size;
        let mut data = base.rgba.clone();

        if !words.is_empty() {
            match self.render_caption(style, words, active, src_w, src_h) {
                Ok((caption, cw, ch)) => {
                    // Scale the caption by the same factor as the frame, then
                    // place it with the exporter's math (centered horizontally,
                    // box center at position_pct of the height).
                    let (scaled, sw, sh) = if cw == base.width {
                        (caption, cw, ch)
                    } else {
                        let img = image::RgbaImage::from_raw(cw, ch, caption)
                            .expect("caption buffer size mismatch");
                        let th = ((ch as u64 * base.width as u64) / cw as u64).max(1) as u32;
                        let small =
                            image::imageops::resize(&img, base.width, th, FilterType::Lanczos3);
                        (small.into_raw(), base.width, th)
                    };
                    let x = ((base.width as f32 - sw as f32) / 2.0).round() as i32;
                    let y = (base.height as f32 * style.position_pct / 100.0 - sh as f32 / 2.0)
                        .round() as i32;
                    blend_straight(&mut data, base.width, base.height, &scaled, sw, sh, x, y);
                }
                Err(e) => self.caption_error = Some(format!("{e:#}")),
            }
        }

        let image = egui::ColorImage::from_rgba_unmultiplied(
            [base.width as usize, base.height as usize],
            &data,
        );
        self.texture = Some(ctx.load_texture(
            "preview_frame",
            image,
            egui::TextureOptions::LINEAR,
        ));
        self.composite_built = true;
    }

    /// Render one caption state at full source resolution (export parity).
    fn render_caption(
        &mut self,
        style: &Style,
        words: &[String],
        active: usize,
        src_w: u32,
        src_h: u32,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let font_px = render::font_px(style, src_h);
        let needs_reload = match &self.faces {
            Some(c) => {
                c.family != style.font_family || (c.font_px - font_px).abs() >= 1e-3
            }
            None => true,
        };
        if needs_reload {
            self.faces = Some(FaceCache {
                family: style.font_family.clone(),
                font_px,
                faces: Faces::load(style, font_px)?,
            });
        }
        let faces = &self.faces.as_ref().expect("faces loaded").faces;
        render::render_state_rgba(faces, style, words, active, font_px, src_w)
            .context("caption render failed")
    }
}
