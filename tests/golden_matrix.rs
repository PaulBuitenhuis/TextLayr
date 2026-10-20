//! Pixel-regression matrix for the renderer and the motion compositor.
//!
//! Hashes are intentionally platform-independent: fonts are bundled and both
//! fontdue and tiny-skia are pure Rust. Run with `TEXTLAYR_PRINT_GOLDEN=1` to
//! print fresh tuples after an intentional visual change.

use textlayr::config::{Color, Easing, Fit, MotionStyle, PopupEffect, Style};
use textlayr::export::caption_y;
use textlayr::profile;
use textlayr::render::{self, blend_transformed, popup_transform, Faces};

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn builtin(id: &str) -> Style {
    profile::builtins()
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("missing builtin {id}"))
        .style
        .clone()
}

fn torture_style() -> Style {
    let mut s = Style::default();
    s.font.family = "Inter".into();
    s.font.size_pct = 5.0;
    s.font.uppercase = true;
    s.font.dim_bold = true;
    s.font.letter_spacing = 0.03;
    s.text.outline.enabled = true;
    s.text.outline.color = Color::new(0, 0, 0, 255);
    s.text.outline.width = 0.06;
    s.text.shadow.enabled = true;
    s.text.shadow.color = Color::new(0, 0, 0, 210);
    s.text.shadow.offset_x = 0.04;
    s.text.shadow.offset_y = 0.06;
    s.text.shadow.blur = 0.04;
    s.text.glow.enabled = true;
    s.text.glow.color = Color::new(255, 80, 120, 120);
    s.text.glow.radius = 0.12;
    s.box_style.color = Color::new(18, 24, 40, 255);
    s.box_style.opacity = 0.75;
    s.box_style.padding_x = 0.7;
    s.box_style.padding_y = 0.35;
    s.box_style.border_width = 0.05;
    s.box_style.border_color = Color::new(255, 255, 255, 255);
    s.box_style.shadow = true;
    s.box_style.shadow_color = Color::new(0, 0, 0, 255);
    s.box_style.shadow_opacity = 0.8;
    s.box_style.shadow_offset_y = 0.12;
    s.box_style.shadow_blur = 0.18;
    s.highlight.pill.enabled = true;
    s.highlight.pill.color = Color::new(255, 205, 76, 255);
    s.highlight.pill.padding_x = 0.14;
    s.highlight.pill.padding_y = 0.05;
    s.highlight.pill.radius_frac = 0.35;
    s.highlight.underline.enabled = true;
    s.highlight.underline.color = Color::new(255, 205, 76, 255);
    s.highlight.underline.width = 0.06;
    s.highlight.underline.offset = 0.03;
    s.highlight.scale = 1.08;
    s
}

fn wrap_style() -> Style {
    let mut s = Style::default();
    s.font.family = "Inter".into();
    s.font.size_pct = 3.0;
    s.box_style.opacity = 0.6;
    s.box_style.shadow = false;
    s.layout.fit = Fit::Wrap;
    s.layout.max_lines = 2;
    s.layout.reserve_lines = 2;
    s.layout.box_width_pct = Some(34.0);
    s.layout.line_height = 1.15;
    s
}

struct Case {
    name: &'static str,
    style: Style,
    words: &'static [&'static str],
    active: usize,
    frame: (u32, u32),
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "default-portrait",
            style: Style::default(),
            words: &["Make", "every", "word"],
            active: 1,
            frame: (1080, 1920),
        },
        Case {
            name: "default-landscape",
            style: Style::default(),
            words: &["Make", "every", "word"],
            active: 1,
            frame: (1920, 1080),
        },
        Case {
            name: "bold-pop-portrait",
            style: builtin("builtin:bold-pop"),
            words: &["putting", "words", "on"],
            active: 1,
            frame: (1080, 1920),
        },
        Case {
            name: "highlight-pill-portrait",
            style: builtin("builtin:highlight-pill"),
            words: &["putting", "words", "on"],
            active: 1,
            frame: (1080, 1920),
        },
        Case {
            name: "clean-subtitle-portrait",
            style: builtin("builtin:clean-subtitle"),
            words: &["putting", "words", "on", "how", "you", "feel"],
            active: 2,
            frame: (1080, 1920),
        },
        Case {
            name: "lower-third-landscape",
            style: builtin("builtin:lower-third"),
            words: &["putting", "words", "on", "how", "you", "feel"],
            active: 2,
            frame: (1920, 1080),
        },
        Case {
            name: "word-by-word-portrait",
            style: builtin("builtin:word-by-word"),
            words: &["word"],
            active: 0,
            frame: (1080, 1920),
        },
        Case {
            name: "torture-portrait",
            style: torture_style(),
            words: &["Make", "every", "word"],
            active: 1,
            frame: (1080, 1920),
        },
        Case {
            name: "torture-landscape",
            style: torture_style(),
            words: &["Make", "every", "word"],
            active: 1,
            frame: (1920, 1080),
        },
        Case {
            name: "wrap-portrait",
            style: wrap_style(),
            words: &["another", "long", "caption", "line"],
            active: 1,
            frame: (1080, 1920),
        },
    ]
}

/// `(hash, width, height, box_top bits, box_h bits)`.
fn render_case(c: &Case) -> (u64, u32, u32, u32, u32) {
    let px = render::font_px(&c.style, c.frame.1);
    let faces = Faces::load(&c.style, px).unwrap();
    let words: Vec<String> = c.words.iter().map(|w| w.to_string()).collect();
    let cap =
        render::render_state_rgba(&faces, &c.style, &words, c.active, px, c.frame.0, c.frame.1)
            .unwrap();
    (
        fnv1a(&cap.rgba),
        cap.width,
        cap.height,
        cap.box_top.to_bits(),
        cap.box_h.to_bits(),
    )
}

/// Static state hashes. Regenerate with `TEXTLAYR_PRINT_GOLDEN=1`.
const GOLDEN: &[(&str, u64, u32, u32, u32, u32)] = &[
    (
        "default-portrait",
        0x87f2_60ff_c735_2900,
        713,
        143,
        0x419c_ac0a,
        0x42ce_ab37,
    ),
    (
        "default-landscape",
        0x1215_bdc6_fb77_9b7f,
        401,
        81,
        0x4130_418a,
        0x426a_809e,
    ),
    (
        "bold-pop-portrait",
        0xc849_bfbb_5446_9777,
        697,
        295,
        0x415f_a43f,
        0x4385_5d98,
    ),
    (
        "highlight-pill-portrait",
        0xf160_bb39_4406_1069,
        778,
        144,
        0x415d_2f1a,
        0x42e7_ba5e,
    ),
    (
        "clean-subtitle-portrait",
        0x10b7_dc17_cfb3_c717,
        908,
        156,
        0x0000_0000,
        0x431b_7efa,
    ),
    (
        "lower-third-landscape",
        0x847e_a4de_bfb9_621d,
        1728,
        87,
        0x0000_0000,
        0x42ac_cccd,
    ),
    (
        "word-by-word-portrait",
        0xd444_3b22_14a4_5d0c,
        523,
        262,
        0x4213_74bd,
        0x433b_fb7f,
    ),
    (
        "torture-portrait",
        0x2626_db64_4ce8_c1f2,
        1023,
        192,
        0x41e6_6e2c,
        0x4306_0f44,
    ),
    (
        "torture-landscape",
        0x6b37_31d1_a6c4_937b,
        729,
        137,
        0x41a4_28f6,
        0x42bf_999a,
    ),
    (
        "wrap-portrait",
        0x8d9e_a032_80f7_ab0e,
        368,
        126,
        0x0000_0000,
        0x42fb_f6b8,
    ),
];

#[test]
fn built_in_styles_and_layouts_render_consistently() {
    let print = std::env::var_os("TEXTLAYR_PRINT_GOLDEN").is_some();
    let cases = cases();
    for c in &cases {
        let got = render_case(c);
        if print {
            println!(
                "        (\"{}\", 0x{:016x}, {}, {}, 0x{:08x}, 0x{:08x}),",
                c.name, got.0, got.1, got.2, got.3, got.4
            );
            continue;
        }
        let expected = GOLDEN
            .iter()
            .find(|g| g.0 == c.name)
            .unwrap_or_else(|| panic!("no golden entry for {}", c.name));
        assert_eq!(
            (got.0, got.1, got.2, got.3, got.4),
            (expected.1, expected.2, expected.3, expected.4, expected.5),
            "golden mismatch for {}",
            c.name
        );
    }
    if !print {
        assert_eq!(cases.len(), GOLDEN.len(), "golden table is stale");
    }
}

fn gradient(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let t = y as f32 / h.max(1) as f32;
        let r = (34.0 * (1.0 - t) + 120.0 * t) as u8;
        let g = (58.0 * (1.0 - t) + 90.0 * t) as u8;
        let b = (92.0 * (1.0 - t) + 70.0 * t) as u8;
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = 255;
        }
    }
    out
}

fn motion_hashes() -> Vec<(f32, u64)> {
    let (w, h) = (540u32, 960u32);
    let mut style = builtin("builtin:bold-pop");
    style.motion = MotionStyle {
        popup: PopupEffect::Pop,
        duration_ms: 300,
        easing: Easing::EaseOut,
        intensity: 0.25,
    };
    let px = render::font_px(&style, h);
    let faces = Faces::load(&style, px).unwrap();
    let words: Vec<String> = ["putting", "words", "on"]
        .iter()
        .map(|w| w.to_string())
        .collect();
    let cap = render::render_state_rgba(&faces, &style, &words, 1, px, w, h).unwrap();
    let x = ((w as f32 - cap.width as f32) / 2.0).round() as i32;
    let y = caption_y(
        h,
        style.layout.position_pct,
        style.layout.anchor,
        cap.box_top,
        cap.box_h,
    )
    .round() as i32;
    let pivot_x = cap.width as f32 / 2.0;
    let pivot_y = cap.box_top + cap.box_h / 2.0;

    [0.0f32, 0.5, 1.0]
        .iter()
        .map(|&phase| {
            let mut frame = gradient(w, h);
            let transform = popup_transform(&style.motion, phase, px);
            blend_transformed(
                &mut frame, w, h, &cap.rgba, cap.width, cap.height, x, y, pivot_x, pivot_y,
                transform,
            );
            (phase, fnv1a(&frame))
        })
        .collect()
}

/// `(phase, composite hash)`. Regenerate with `TEXTLAYR_PRINT_GOLDEN=1`.
const MOTION_GOLDEN: &[(f32, u64)] = &[
    (0.0f32, 0xa0bc_119b_a34f_cfc3),
    (0.5f32, 0x3636_4bc9_5acc_2535),
    (1.0f32, 0x6fdd_44c8_13d0_cdb1),
];

#[test]
fn popup_motion_composites_consistently() {
    let print = std::env::var_os("TEXTLAYR_PRINT_GOLDEN").is_some();
    let got = motion_hashes();
    for (phase, hash) in &got {
        if print {
            println!("        ({:.1}f32, 0x{:016x}),", phase, hash);
            continue;
        }
        let expected = MOTION_GOLDEN
            .iter()
            .find(|g| g.0.to_bits() == phase.to_bits())
            .unwrap_or_else(|| panic!("no motion golden for phase {phase}"));
        assert_eq!(*hash, expected.1, "motion golden mismatch at {phase}");
    }
    if !print {
        assert_eq!(
            got.len(),
            MOTION_GOLDEN.len(),
            "motion golden table is stale"
        );
    }
}
