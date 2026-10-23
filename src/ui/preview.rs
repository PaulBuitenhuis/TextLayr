//! Live preview: a real video frame with the current caption state composited
//! on top, using the same renderer and placement math as the export — what you
//! see here is what gets burned in.
//!
//! Frame extraction (ffmpeg) runs on a worker thread. The frame is decoded
//! straight to a 480px RGBA buffer (no full-resolution PNG), and frames are
//! cached per popup. The caption overlay is rendered at source resolution once
//! per style/word change, then reused when only the background frame changes.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use image::imageops::FilterType;

use crate::config::Style;
use crate::probe;
use crate::render::{self, blend_straight, blend_transformed, Faces, MotionPhase};

/// Preview texture width in pixels; the height follows the video's aspect.
const PREVIEW_WIDTH: u32 = 480;

/// How long a single preview frame extraction may take. Preview frames are
/// small, so a decode that takes longer than this is wedged; the next click
/// retries instead of leaving the worker stuck for the full probe timeout.
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(3);

/// A decoded video frame, already scaled to preview size.
#[derive(Clone)]
pub struct Frame {
    /// Preview dimensions (width <= [`PREVIEW_WIDTH`]).
    pub width: u32,
    pub height: u32,
    /// The source's display dimensions, for caption layout parity.
    pub src_width: u32,
    pub src_height: u32,
    /// Straight RGBA at the preview dimensions.
    pub rgba: Vec<u8>,
}

/// Cache key for a popup's time span. Re-chunking changes the span, so cached
/// frames cannot leak across a different chunking.
pub type FrameKey = (u64, u64);

/// Extract one frame at `at_secs`, scaled to the preview width, as raw RGBA.
/// Meant for a worker thread.
pub fn extract(
    tools: &probe::Fftools,
    input: &Path,
    at_secs: f64,
    src_width: u32,
    src_height: u32,
) -> Result<Frame> {
    let width = PREVIEW_WIDTH.min(src_width.max(1)).max(1);
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{at_secs:.3}"))
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf"])
        .arg(format!("scale={width}:-2"))
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    let out = crate::process::output_with_timeout(&mut cmd, PREVIEW_TIMEOUT)
        .context("failed to extract a preview frame")?;
    if !out.status.success() {
        bail!(
            "preview frame extraction failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stride = width as usize * 4;
    if stride == 0 || out.stdout.len() % stride != 0 {
        bail!(
            "the decoder returned {} bytes, not whole {width}px RGBA frames",
            out.stdout.len()
        );
    }
    let height = (out.stdout.len() / stride) as u32;
    if height == 0 {
        bail!("the decoder returned an empty preview frame");
    }
    Ok(Frame {
        width,
        height,
        src_width: src_width.max(1),
        src_height: src_height.max(1),
        rgba: out.stdout,
    })
}

/// A small LRU cache of extracted frames, keyed by popup time span.
pub struct FrameCache {
    map: HashMap<FrameKey, Frame>,
    order: VecDeque<FrameKey>,
    cap: usize,
}

impl FrameCache {
    pub fn new(cap: usize) -> Self {
        FrameCache {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub fn contains(&self, key: FrameKey) -> bool {
        self.map.contains_key(&key)
    }

    pub fn get(&mut self, key: FrameKey) -> Option<Frame> {
        if !self.map.contains_key(&key) {
            return None;
        }
        self.touch(key);
        self.map.get(&key).cloned()
    }

    pub fn insert(&mut self, key: FrameKey, frame: Frame) {
        if self.map.insert(key, frame).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(key);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: FrameKey) {
        if let Some(pos) = self.order.iter().position(|k| *k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }
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

/// A caption rendered at source resolution, scaled to preview size.
///
/// Kept so a background-frame swap (the common case while moving between
/// popups) only blends the cached crop instead of re-rasterising text.
struct CachedCaption {
    style: Style,
    words: Vec<String>,
    active: usize,
    src_size: (u32, u32),
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    box_top: f32,
    box_h: f32,
    /// Font size at source resolution, for motion distances.
    font_px: f32,
}

/// Preview state kept across UI frames.
pub struct Preview {
    /// The extracted frame, already at preview size.
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
    /// Whether the preview is waiting for a new frame and should keep the old
    /// composite on screen.
    hold: bool,
    /// The most recently rendered caption crop.
    caption_cache: Option<CachedCaption>,
    /// Motion phase the current composite was built for.
    last_motion: MotionPhase,
    /// Test-only counter proving the caption cache is reused.
    #[cfg(test)]
    caption_renders: usize,
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
            caption_cache: None,
            last_motion: MotionPhase::Settled,
            hold: false,
            #[cfg(test)]
            caption_renders: 0,
        }
    }
}

impl Preview {
    /// Swap in a freshly extracted frame (invalidates the composite).
    pub fn set_frame(&mut self, frame: Frame) {
        let src_size = (frame.src_width.max(1), frame.src_height.max(1));
        let target = PREVIEW_WIDTH.min(frame.width).max(1);
        let base = if frame.width <= PREVIEW_WIDTH {
            Surface {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
            }
        } else {
            // Defensive: extract() already scales. Keeps tests and any future
            // full-resolution caller correct.
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
        if self.src_size != src_size {
            self.caption_cache = None;
        }
        self.base = Some(base);
        self.src_size = src_size;
        self.composite_built = false;
        self.caption_error = None;
        self.hold = false;
    }

    /// Keep the current composite on screen while a new frame is extracted.
    /// Prevents the new popup's caption being drawn over the previous frame.
    pub fn hold(&mut self) {
        self.hold = true;
    }

    /// Stop holding (the old frame is still the right one, e.g. the user went
    /// back to the popup that is already displayed).
    pub fn release_hold(&mut self) {
        self.hold = false;
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
        if self.hold {
            return;
        }
        if self.composite_built && self.last_motion == motion {
            return;
        }
        let Some(base) = self.base.as_ref() else {
            return;
        };
        let (width, height) = (base.width, base.height);
        let data = self.build_composite(style, words, active, motion);
        let image =
            egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &data);
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("preview_frame", image, egui::TextureOptions::LINEAR));
        }
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
            self.ensure_caption(style, words, active, src_w, src_h, base.width);
            if let Some(cap) = self.caption_cache.as_ref() {
                let factor = base.width as f32 / src_w.max(1) as f32;
                let x = ((base.width as f32 - cap.width as f32) / 2.0).round() as i32;
                let y = crate::export::caption_y(
                    base.height,
                    style.layout.position_pct,
                    style.layout.anchor,
                    cap.box_top,
                    cap.box_h,
                )
                .round() as i32;

                let transform = motion.transform(&style.motion, cap.font_px * factor);
                if transform.is_identity() {
                    blend_straight(
                        &mut data,
                        base.width,
                        base.height,
                        &cap.rgba,
                        cap.width,
                        cap.height,
                        x,
                        y,
                    );
                } else {
                    let pivot_x = cap.width as f32 / 2.0;
                    let pivot_y = match style.layout.anchor {
                        crate::config::Anchor::Center => cap.box_top + cap.box_h / 2.0,
                        crate::config::Anchor::Top => cap.box_top,
                        crate::config::Anchor::Bottom => cap.box_top + cap.box_h,
                    };
                    blend_transformed(
                        &mut data,
                        base.width,
                        base.height,
                        &cap.rgba,
                        cap.width,
                        cap.height,
                        x,
                        y,
                        pivot_x,
                        pivot_y,
                        transform,
                    );
                }
            }
        }

        data
    }

    /// Render the caption if the cache does not already match this state.
    fn ensure_caption(
        &mut self,
        style: &Style,
        words: &[String],
        active: usize,
        src_w: u32,
        src_h: u32,
        base_w: u32,
    ) {
        let matches = self.caption_cache.as_ref().is_some_and(|c| {
            c.style == *style
                && c.words == words
                && c.active == active
                && c.src_size == (src_w, src_h)
        });
        if matches {
            return;
        }
        let factor = base_w as f32 / src_w.max(1) as f32;
        match self.render_caption(style, words, active, src_w, src_h) {
            Ok(cap) => {
                let (rgba, width, height) = scale_caption(cap.rgba, cap.width, cap.height, factor);
                self.caption_cache = Some(CachedCaption {
                    style: style.clone(),
                    words: words.to_vec(),
                    active,
                    src_size: (src_w, src_h),
                    width,
                    height,
                    rgba,
                    box_top: cap.box_top * factor,
                    box_h: cap.box_h * factor,
                    font_px: render::font_px(style, src_h),
                });
                self.caption_error = None;
                #[cfg(test)]
                {
                    self.caption_renders += 1;
                }
            }
            Err(e) => {
                self.caption_cache = None;
                self.caption_error = Some(format!("{e:#}"));
            }
        }
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
            src_width: w,
            src_height: h,
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

    #[test]
    fn frame_cache_is_an_lru() {
        let mut cache = FrameCache::new(2);
        cache.insert((1, 1), flat_frame(4, 4));
        cache.insert((2, 2), flat_frame(4, 4));
        assert!(cache.get((1, 1)).is_some(), "1 is now most recently used");
        cache.insert((3, 3), flat_frame(4, 4));
        assert!(cache.contains((1, 1)));
        assert!(!cache.contains((2, 2)), "the least recent frame is evicted");
        assert!(cache.contains((3, 3)));
    }

    #[test]
    fn the_caption_is_cached_when_only_the_frame_changes() {
        let style = reporter_style();
        let words: Vec<String> = ["Alright,", "I", "wanted", "to"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut preview = Preview::default();
        preview.set_frame(flat_frame(1080, 1920));
        let _ = preview.build_composite(&style, &words, 0, MotionPhase::Settled);
        let renders = preview.caption_renders;
        assert!(renders > 0);

        // A new background for the same popup: the caption crop is reused.
        preview.set_frame(flat_frame(1080, 1920));
        let _ = preview.build_composite(&style, &words, 0, MotionPhase::Settled);
        assert_eq!(
            preview.caption_renders, renders,
            "the caption was re-rasterised for a frame swap"
        );
    }

    #[test]
    fn extraction_is_scaled_to_the_preview_width() {
        use std::process::Command;

        let tools = match crate::probe::find_ffmpeg() {
            Ok(tools) => tools,
            Err(e) => {
                eprintln!("skipping: no real ffmpeg available ({e:#})");
                return;
            }
        };
        let work = tempfile::tempdir().unwrap();
        let clip = work.path().join("clip.mp4");
        let status = Command::new(&tools.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x203040:s=640x360:r=30:d=1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .expect("spawn the synthesizing ffmpeg");
        assert!(status.success());

        let frame = extract(&tools, &clip, 0.2, 640, 360).unwrap();
        assert_eq!(frame.width, PREVIEW_WIDTH);
        assert_eq!(frame.height, 270);
        assert_eq!((frame.src_width, frame.src_height), (640, 360));
        assert_eq!(frame.rgba.len(), (frame.width * frame.height * 4) as usize);
    }
}
