//! Unit tests for the renderer: layout, motion, pixel effects and the
//! default-style golden hash.

use super::layout::break_lines;
use super::*;
use crate::config::{Color, Easing, Fit, MotionStyle, PopupEffect, Style};

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
        ..MotionStyle::default()
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
fn popup_exit_motion_progresses_monotonically() {
    let pop = MotionStyle {
        popup: PopupEffect::None,
        exit: PopupEffect::Pop,
        duration_ms: 0,
        exit_duration_ms: 200,
        easing: Easing::EaseOut,
        intensity: 0.25,
    };
    let scales: Vec<f32> = (0..=10)
        .map(|i| popup_exit_transform(&pop, i as f32 / 10.0, 50.0).scale)
        .collect();
    assert!(scales.windows(2).all(|w| w[1] <= w[0] + 1e-6));
    assert!((scales[10] - 0.75).abs() < 1e-4);
    let alphas: Vec<f32> = (0..=10)
        .map(|i| popup_exit_transform(&pop, i as f32 / 10.0, 50.0).alpha)
        .collect();
    assert!(alphas.windows(2).all(|w| w[1] <= w[0] + 1e-6));
    assert!(alphas[10].abs() < 1e-4);

    let slide = MotionStyle {
        popup: PopupEffect::None,
        exit: PopupEffect::SlideUp,
        duration_ms: 0,
        exit_duration_ms: 200,
        easing: Easing::EaseOut,
        intensity: 0.6,
    };
    let offsets: Vec<f32> = (0..=10)
        .map(|i| popup_exit_transform(&slide, i as f32 / 10.0, 50.0).dy)
        .collect();
    assert!(offsets.windows(2).all(|w| w[1] <= w[0] + 1e-6));
    assert!(offsets[0].abs() < 1e-4);
    assert!((offsets[10] + 30.0).abs() < 1e-4);

    assert!(!MotionPhase::Settled.is_hidden());
    assert!(MotionPhase::Hidden.is_hidden());
    assert!(MotionPhase::Settled.transform(&pop, 50.0).is_identity());
}

#[test]
fn popup_motion_progresses_monotonically() {
    let pop = MotionStyle {
        popup: PopupEffect::Pop,
        duration_ms: 300,
        easing: Easing::EaseOut,
        intensity: 0.25,
        ..MotionStyle::default()
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
        ..MotionStyle::default()
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
        ..MotionStyle::default()
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
