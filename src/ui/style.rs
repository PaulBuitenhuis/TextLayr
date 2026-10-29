//! The single style inspector.
//!
//! The panel exposes the small set of decisions that make a caption look the
//! way it does. The remaining [`Style`] fields are derived from those choices
//! (outline width from an edge strength, plate geometry from a plate mode, and
//! so on) so the user never has to keep a dozen coupled parameters coherent.
//!
//! Everything lives in the side panel. There is deliberately no editor window:
//! the preview must stay visible while a style is being tuned.

use egui::{Color32, RichText};

use crate::config::{Align, Anchor, Color, Easing, Fit, PopupEffect, Style, FONT_FAMILIES};
use crate::profile::ProfileStore;

use super::profile::ProfileUi;
use super::review::{Action, ReviewState};
use super::theme;

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

/// Draw the whole style panel. Export stays pinned at the bottom; everything
/// else scrolls in one surface.
pub fn panel(
    ui: &mut egui::Ui,
    r: &mut ReviewState,
    profiles: &ProfileStore,
    profile_ui: &mut ProfileUi,
) -> Action {
    let mut action = Action::None;
    let before = r.style.clone();

    egui::Panel::bottom("style_export").show(ui, |ui| {
        ui.add_space(6.0);
        let exportable = r.chunks.iter().any(|c| !c.text.trim().is_empty());
        if theme::primary_button(ui, exportable, "Export video").clicked() {
            action = Action::Export;
        }
        ui.add_space(4.0);
        let hint = if !exportable {
            "nothing to export"
        } else if cfg!(target_os = "macos") {
            "or press \u{2318}E"
        } else {
            "or press Ctrl+E"
        };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(hint).weak().small());
        });
    });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("Style");
            ui.add_space(6.0);
            action = join(action, profile_ui.section(ui, r, profiles));

            text_section(ui, r);
            plate_section(ui, r);
            highlight_section(ui, r);
            placement_section(ui, r);
            motion_section(ui, r);
            action = join(action, caption_section(ui, r));

            ui.add_space(14.0);
            if theme::wide_button(ui, r.style != Style::default(), "Reset style").clicked() {
                r.style = Style::default();
                r.preview.invalidate();
                action = join(action, Action::Seek);
            }
            ui.add_space(8.0);
        });

    r.style.sanitize();
    r.grouping.sanitize();
    if r.style != before {
        r.preview.invalidate();
    }
    action
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn text_section(ui: &mut egui::Ui, r: &mut ReviewState) {
    section(ui, "Text");
    let s = &mut r.style;

    let mut font = font_choice(s);
    let mut index = font.index();
    if option(ui, "Font", &mut index, &["Serif", "Sans", "Rounded"]) {
        font = FontChoice::from_index(index).unwrap_or(font);
        set_font(s, font);
    }

    if slider(ui, "Size", &mut s.font.size_pct, 1.0..=20.0, 1, "%")
        .on_hover_text("Caption size as a percentage of the frame height.")
        .changed()
        && plate_choice(s) == PlateChoice::Bar
    {
        s.layout.box_height_pct = None;
    }

    toggle(ui, "Uppercase", &mut s.font.uppercase);
    toggle(ui, "Bold all words", &mut s.font.dim_bold);

    if swatch(ui, "Text colour", &mut s.text.dim) && edge_choice(s) == EdgeChoice::Outline {
        s.text.outline.color = contrast_color(s.text.dim);
    }

    let mut edge = edge_choice(s);
    let mut index = edge.index();
    if option(
        ui,
        "Edge",
        &mut index,
        &["None", "Outline", "Shadow", "Glow"],
    ) {
        edge = EdgeChoice::from_index(index).unwrap_or(edge);
        set_edge(s, edge);
    }
    if edge_choice(s) != EdgeChoice::None {
        let mut strength = edge_strength(s);
        if slider(ui, "Strength", &mut strength, 0.0..=1.0, 2, "")
            .on_hover_text("Outline weight, shadow softness or glow radius.")
            .changed()
        {
            set_edge_strength(s, strength);
        }
    }
}

fn plate_section(ui: &mut egui::Ui, r: &mut ReviewState) {
    section(ui, "Background");
    let s = &mut r.style;

    let mut plate = plate_choice(s);
    let mut index = plate.index();
    if option(ui, "Background", &mut index, &["None", "Box", "Bar"]) {
        plate = PlateChoice::from_index(index).unwrap_or(plate);
        set_plate(s, plate);
    }

    if plate_choice(s) == PlateChoice::None {
        return;
    }

    swatch(ui, "Colour", &mut s.box_style.color);
    slider(ui, "Opacity", &mut s.box_style.opacity, 0.05..=1.0, 2, "");

    let mut shape = plate_shape(s);
    let mut index = shape.index();
    if option(ui, "Shape", &mut index, &["Square", "Soft", "Round"]) {
        shape = PlateShape::from_index(index).unwrap_or(shape);
        set_plate_shape(s, shape);
    }

    let mut padding = plate_padding(s);
    if slider(ui, "Padding", &mut padding, 0.10..=0.80, 2, "em").changed() {
        set_plate_padding(s, padding);
    }

    toggle(ui, "Shadow", &mut s.box_style.shadow);
}

fn highlight_section(ui: &mut egui::Ui, r: &mut ReviewState) {
    section(ui, "Highlight");
    let s = &mut r.style;

    let mut style = highlight_choice(s);
    let mut index = style.index();
    if option(
        ui,
        "Active word",
        &mut index,
        &["Colour", "Pill", "Underline", "Scale"],
    ) {
        style = HighlightChoice::from_index(index).unwrap_or(style);
        set_highlight(s, style);
    }

    let mut accent = accent_color(s);
    if swatch(ui, "Accent", &mut accent) {
        set_accent(s, accent);
    }
}

fn placement_section(ui: &mut egui::Ui, r: &mut ReviewState) {
    section(ui, "Placement");
    let s = &mut r.style;

    slider(
        ui,
        "Position",
        &mut s.layout.position_pct,
        5.0..=95.0,
        0,
        "%",
    )
    .on_hover_text("Vertical position in the frame; the background mode chooses the anchor.");

    let mut width = plate_width(s);
    if slider(ui, "Width", &mut width, 40.0..=100.0, 0, "%")
        .on_hover_text("Text wrap width; a bar uses it as the plate width.")
        .changed()
    {
        set_plate_width(s, width);
    }

    let mut align = align_index(s.layout.align);
    if option(ui, "Align", &mut align, &["Left", "Center", "Right"]) {
        s.layout.align = match align {
            0 => Align::Left,
            1 => Align::Center,
            _ => Align::Right,
        };
    }

    let mut lines = lines_choice(s);
    if option(ui, "Lines", &mut lines, &["1", "2", "3"]) {
        set_lines(s, lines + 1);
    }
}

fn motion_section(ui: &mut egui::Ui, r: &mut ReviewState) {
    section(ui, "Motion");
    {
        let s = &mut r.style;
        let mut motion = motion_choice(s);
        let mut index = motion.index();
        if option(ui, "Motion", &mut index, &["None", "Pop", "Slide", "Fade"]) {
            motion = MotionChoice::from_index(index).unwrap_or(motion);
            set_motion(s, motion);
        }

        if motion_choice(s) != MotionChoice::None {
            let mut speed = speed_choice(s);
            let mut index = speed.index();
            if option(ui, "Speed", &mut index, &["Slow", "Normal", "Fast"]) {
                speed = SpeedChoice::from_index(index).unwrap_or(speed);
                set_speed(s, speed);
            }
        }
    }

    if motion_choice(&r.style) == MotionChoice::None {
        r.motion_playing = false;
        return;
    }

    let can_play = (r.style.motion.popup != PopupEffect::None && r.style.motion.duration_ms > 0)
        || (r.style.motion.exit != PopupEffect::None && r.style.motion.exit_duration_ms > 0);
    ui.add_enabled(
        can_play,
        egui::Checkbox::new(&mut r.motion_playing, "Play preview"),
    );
}

fn caption_section(ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
    section(ui, "Caption");
    let mut action = Action::None;
    let mut n = r.grouping.words_per_popup;

    ui.horizontal(|ui| {
        if theme::button_enabled(ui, n > 1, "\u{2212}").clicked() {
            n -= 1;
        }
        ui.add_sized(
            [72.0, theme::CONTROL_H],
            egui::Label::new(format!("{n} words")),
        );
        if theme::button_enabled(ui, n < 8, "+").clicked() {
            n += 1;
        }
    });

    if n != r.grouping.words_per_popup {
        r.grouping.words_per_popup = n;
        let discarded = r.rechunk(n);
        action = Action::Rechunked {
            discarded_edits: discarded,
        };
    }

    ui.add_space(2.0);
    ui.label(
        RichText::new("Changing this re-groups the transcript and can discard edits.")
            .small()
            .weak(),
    );
    action
}

// ---------------------------------------------------------------------------
// Small layout helpers
// ---------------------------------------------------------------------------

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    theme::section(ui, title);
    ui.add_space(4.0);
}

/// A full-width segmented choice, with its label above. Four options do not
/// fit comfortably beside a label in a 320px panel.
fn option(ui: &mut egui::Ui, label: &str, selected: &mut usize, labels: &[&str]) -> bool {
    ui.add_space(2.0);
    ui.label(RichText::new(label).small().weak());
    ui.add_space(2.0);
    let changed = theme::segmented(ui, selected, labels);
    ui.add_space(4.0);
    changed
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    decimals: usize,
    suffix: &str,
) -> egui::Response {
    let mut response = None;
    ui.horizontal(|ui| {
        ui.add_sized(
            [86.0, theme::CONTROL_H],
            egui::Label::new(RichText::new(label).small()).truncate(),
        );
        ui.spacing_mut().slider_width = (ui.available_width() - 46.0).clamp(90.0, 180.0);
        response = Some(
            ui.add(
                egui::Slider::new(value, range)
                    .fixed_decimals(decimals)
                    .suffix(suffix),
            ),
        );
    });
    response.expect("slider row always draws a slider")
}

fn swatch(ui: &mut egui::Ui, label: &str, value: &mut Color) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [86.0, theme::CONTROL_H],
            egui::Label::new(RichText::new(label).small()).truncate(),
        );
        changed = color_swatch(ui, value);
    });
    changed
}

fn toggle(ui: &mut egui::Ui, label: &str, value: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [86.0, theme::CONTROL_H],
            egui::Label::new(RichText::new(label).small()).truncate(),
        );
        changed = ui.checkbox(value, "").changed();
    });
    changed
}

fn color_swatch(ui: &mut egui::Ui, value: &mut Color) -> bool {
    let mut c = Color32::from_rgba_unmultiplied(value.r, value.g, value.b, value.a);
    if ui.color_edit_button_srgba(&mut c).changed() {
        *value = Color::new(c.r(), c.g(), c.b(), c.a());
        true
    } else {
        false
    }
}

fn join(a: Action, b: Action) -> Action {
    use Action::*;
    match (a, b) {
        (Export, _) | (_, Export) => Export,
        (Rechunked { discarded_edits }, _) | (_, Rechunked { discarded_edits }) => {
            Rechunked { discarded_edits }
        }
        (Seek, _) | (_, Seek) => Seek,
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Style decision -> field mappings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontChoice {
    Serif,
    Sans,
    Rounded,
}

impl FontChoice {
    fn index(self) -> usize {
        match self {
            FontChoice::Serif => 0,
            FontChoice::Sans => 1,
            FontChoice::Rounded => 2,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(FontChoice::Serif),
            1 => Some(FontChoice::Sans),
            2 => Some(FontChoice::Rounded),
            _ => None,
        }
    }
}

fn font_choice(s: &Style) -> FontChoice {
    match s.font.family.as_str() {
        "Inter" => FontChoice::Sans,
        "Nunito" => FontChoice::Rounded,
        _ => FontChoice::Serif,
    }
}

fn set_font(s: &mut Style, choice: FontChoice) {
    s.font.family = match choice {
        FontChoice::Serif => FONT_FAMILIES[0],
        FontChoice::Sans => FONT_FAMILIES[1],
        FontChoice::Rounded => FONT_FAMILIES[2],
    }
    .to_string();
    if plate_choice(s) == PlateChoice::Bar {
        s.layout.box_height_pct = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeChoice {
    None,
    Outline,
    Shadow,
    Glow,
}

impl EdgeChoice {
    fn index(self) -> usize {
        match self {
            EdgeChoice::None => 0,
            EdgeChoice::Outline => 1,
            EdgeChoice::Shadow => 2,
            EdgeChoice::Glow => 3,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(EdgeChoice::None),
            1 => Some(EdgeChoice::Outline),
            2 => Some(EdgeChoice::Shadow),
            3 => Some(EdgeChoice::Glow),
            _ => None,
        }
    }
}

fn edge_choice(s: &Style) -> EdgeChoice {
    if s.text.outline.enabled {
        EdgeChoice::Outline
    } else if s.text.shadow.enabled {
        EdgeChoice::Shadow
    } else if s.text.glow.enabled {
        EdgeChoice::Glow
    } else {
        EdgeChoice::None
    }
}

fn set_edge(s: &mut Style, choice: EdgeChoice) {
    s.text.outline.enabled = choice == EdgeChoice::Outline;
    s.text.shadow.enabled = choice == EdgeChoice::Shadow;
    s.text.glow.enabled = choice == EdgeChoice::Glow;

    match choice {
        EdgeChoice::None => {}
        EdgeChoice::Outline => {
            if s.text.outline.width <= 0.0 {
                s.text.outline.width = 0.08;
            }
            s.text.outline.color = contrast_color(s.text.dim);
        }
        EdgeChoice::Shadow => {
            if s.text.shadow.blur <= 0.0 {
                s.text.shadow.blur = 0.08;
            }
            if s.text.shadow.offset_y == 0.0 {
                s.text.shadow.offset_y = 0.05;
            }
            s.text.shadow.offset_x = 0.0;
            s.text.shadow.color = Color::new(0, 0, 0, 160);
        }
        EdgeChoice::Glow => {
            if s.text.glow.radius <= 0.0 {
                s.text.glow.radius = 0.12;
            }
            let accent = accent_color(s);
            s.text.glow.color = Color::new(accent.r, accent.g, accent.b, 180);
        }
    }
}

fn edge_strength(s: &Style) -> f32 {
    match edge_choice(s) {
        EdgeChoice::Outline => ((s.text.outline.width - 0.03) / 0.15).clamp(0.0, 1.0),
        EdgeChoice::Shadow => ((s.text.shadow.blur - 0.02) / 0.18).clamp(0.0, 1.0),
        EdgeChoice::Glow => ((s.text.glow.radius - 0.04) / 0.24).clamp(0.0, 1.0),
        EdgeChoice::None => 0.5,
    }
}

fn set_edge_strength(s: &mut Style, strength: f32) {
    let strength = strength.clamp(0.0, 1.0);
    match edge_choice(s) {
        EdgeChoice::Outline => {
            s.text.outline.width = 0.03 + strength * 0.15;
            s.text.outline.color = contrast_color(s.text.dim);
        }
        EdgeChoice::Shadow => {
            s.text.shadow.blur = 0.02 + strength * 0.18;
            s.text.shadow.offset_y = 0.03 + strength * 0.07;
            s.text.shadow.offset_x = 0.0;
            s.text.shadow.color = Color::new(0, 0, 0, 160);
        }
        EdgeChoice::Glow => {
            s.text.glow.radius = 0.04 + strength * 0.24;
            let accent = accent_color(s);
            s.text.glow.color = Color::new(accent.r, accent.g, accent.b, 180);
        }
        EdgeChoice::None => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlateChoice {
    None,
    Box,
    Bar,
}

impl PlateChoice {
    fn index(self) -> usize {
        match self {
            PlateChoice::None => 0,
            PlateChoice::Box => 1,
            PlateChoice::Bar => 2,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(PlateChoice::None),
            1 => Some(PlateChoice::Box),
            2 => Some(PlateChoice::Bar),
            _ => None,
        }
    }
}

fn plate_choice(s: &Style) -> PlateChoice {
    if s.box_style.opacity <= 0.01 || s.box_style.color.a == 0 {
        PlateChoice::None
    } else if s.layout.box_width_pct.is_some() {
        PlateChoice::Bar
    } else {
        PlateChoice::Box
    }
}

fn set_plate(s: &mut Style, choice: PlateChoice) {
    match choice {
        PlateChoice::None => {
            s.box_style.opacity = 0.0;
            s.layout.box_width_pct = None;
            s.layout.box_height_pct = None;
        }
        PlateChoice::Box => {
            if s.box_style.opacity <= 0.01 {
                s.box_style.opacity = 0.72;
            }
            if s.box_style.color.a == 0 {
                s.box_style.color.a = 255;
            }
            s.layout.box_width_pct = None;
            s.layout.box_height_pct = None;
            s.layout.anchor = Anchor::Center;
        }
        PlateChoice::Bar => {
            if s.box_style.opacity <= 0.01 {
                s.box_style.opacity = 0.65;
            }
            if s.box_style.color.a == 0 {
                s.box_style.color.a = 255;
            }
            let width = s.layout.box_width_pct.unwrap_or(s.layout.max_width_pct);
            s.layout.box_width_pct = Some(width.clamp(40.0, 100.0));
            s.layout.box_height_pct = None;
            s.layout.anchor = Anchor::Bottom;
        }
    }
}

fn plate_width(s: &Style) -> f32 {
    if plate_choice(s) == PlateChoice::Bar {
        s.layout
            .box_width_pct
            .unwrap_or(s.layout.max_width_pct)
            .clamp(40.0, 100.0)
    } else {
        s.layout.max_width_pct.clamp(40.0, 100.0)
    }
}

fn set_plate_width(s: &mut Style, width: f32) {
    let width = width.clamp(40.0, 100.0);
    s.layout.max_width_pct = width;
    if plate_choice(s) == PlateChoice::Bar {
        s.layout.box_width_pct = Some(width);
    } else {
        s.layout.box_width_pct = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlateShape {
    Square,
    Soft,
    Round,
}

impl PlateShape {
    fn index(self) -> usize {
        match self {
            PlateShape::Square => 0,
            PlateShape::Soft => 1,
            PlateShape::Round => 2,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(PlateShape::Square),
            1 => Some(PlateShape::Soft),
            2 => Some(PlateShape::Round),
            _ => None,
        }
    }
}

fn plate_shape(s: &Style) -> PlateShape {
    if !s.box_style.rounded || s.box_style.radius_frac <= 0.05 {
        PlateShape::Square
    } else if s.box_style.radius_frac >= 0.30 {
        PlateShape::Round
    } else {
        PlateShape::Soft
    }
}

fn set_plate_shape(s: &mut Style, shape: PlateShape) {
    match shape {
        PlateShape::Square => {
            s.box_style.rounded = false;
            s.box_style.radius_frac = 0.0;
        }
        PlateShape::Soft => {
            s.box_style.rounded = true;
            s.box_style.radius_frac = 0.18;
        }
        PlateShape::Round => {
            s.box_style.rounded = true;
            s.box_style.radius_frac = 0.32;
        }
    }
}

fn plate_padding(s: &Style) -> f32 {
    if s.box_style.padding_x > 0.0 {
        s.box_style.padding_x.clamp(0.10, 0.80)
    } else {
        (s.box_style.padding_y * 2.0).clamp(0.10, 0.80)
    }
}

fn set_plate_padding(s: &mut Style, padding: f32) {
    let padding = padding.clamp(0.10, 0.80);
    s.box_style.padding_x = padding;
    s.box_style.padding_y = padding * 0.5;
    if plate_choice(s) == PlateChoice::Bar {
        s.layout.box_height_pct = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightChoice {
    Colour,
    Pill,
    Underline,
    Scale,
}

impl HighlightChoice {
    fn index(self) -> usize {
        match self {
            HighlightChoice::Colour => 0,
            HighlightChoice::Pill => 1,
            HighlightChoice::Underline => 2,
            HighlightChoice::Scale => 3,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(HighlightChoice::Colour),
            1 => Some(HighlightChoice::Pill),
            2 => Some(HighlightChoice::Underline),
            3 => Some(HighlightChoice::Scale),
            _ => None,
        }
    }
}

fn highlight_choice(s: &Style) -> HighlightChoice {
    if s.highlight.pill.enabled {
        HighlightChoice::Pill
    } else if s.highlight.underline.enabled {
        HighlightChoice::Underline
    } else if s.highlight.scale > 1.001 {
        HighlightChoice::Scale
    } else {
        HighlightChoice::Colour
    }
}

fn accent_color(s: &Style) -> Color {
    if s.highlight.pill.enabled {
        s.highlight.pill.color
    } else if s.highlight.underline.enabled {
        s.highlight.underline.color
    } else {
        s.text.highlight
    }
}

fn set_accent(s: &mut Style, accent: Color) {
    if s.highlight.pill.enabled {
        s.highlight.pill.color = accent;
        s.text.highlight = contrast_color(accent);
    } else if s.highlight.underline.enabled {
        s.highlight.underline.color = accent;
        s.text.highlight = accent;
    } else {
        s.text.highlight = accent;
    }
    if s.text.glow.enabled {
        s.text.glow.color = Color::new(accent.r, accent.g, accent.b, 180);
    }
}

fn set_highlight(s: &mut Style, choice: HighlightChoice) {
    let accent = accent_color(s);
    match choice {
        HighlightChoice::Colour => {
            s.highlight.pill.enabled = false;
            s.highlight.underline.enabled = false;
            s.highlight.scale = 1.0;
            s.text.highlight = accent;
        }
        HighlightChoice::Pill => {
            s.highlight.pill.enabled = true;
            s.highlight.underline.enabled = false;
            s.highlight.scale = 1.04;
            s.highlight.pill.color = accent;
            s.text.highlight = contrast_color(accent);
            if s.highlight.pill.padding_x <= 0.0 {
                s.highlight.pill.padding_x = 0.14;
            }
            if s.highlight.pill.padding_y <= 0.0 {
                s.highlight.pill.padding_y = 0.05;
            }
            if s.highlight.pill.radius_frac <= 0.0 {
                s.highlight.pill.radius_frac = 0.35;
            }
        }
        HighlightChoice::Underline => {
            s.highlight.pill.enabled = false;
            s.highlight.underline.enabled = true;
            s.highlight.scale = 1.0;
            s.highlight.underline.color = accent;
            s.text.highlight = accent;
            if s.highlight.underline.width <= 0.0 {
                s.highlight.underline.width = 0.05;
            }
            if s.highlight.underline.offset <= 0.0 {
                s.highlight.underline.offset = 0.02;
            }
        }
        HighlightChoice::Scale => {
            s.highlight.pill.enabled = false;
            s.highlight.underline.enabled = false;
            s.highlight.scale = 1.06;
            s.text.highlight = accent;
        }
    }
}

fn align_index(align: Align) -> usize {
    match align {
        Align::Left => 0,
        Align::Center => 1,
        Align::Right => 2,
    }
}

fn lines_choice(s: &Style) -> usize {
    if s.layout.fit == Fit::Shrink || s.layout.max_lines <= 1 {
        0
    } else if s.layout.max_lines >= 3 {
        2
    } else {
        1
    }
}

fn set_lines(s: &mut Style, lines: usize) {
    let lines = lines.clamp(1, 3);
    if lines == 1 {
        s.layout.fit = Fit::Shrink;
        s.layout.max_lines = 1;
        s.layout.reserve_lines = 1;
    } else {
        s.layout.fit = Fit::Wrap;
        s.layout.max_lines = lines;
        s.layout.reserve_lines = lines;
    }
    s.layout.max_words_per_line = 0;
    s.layout.line_height = 1.2;
    s.layout.box_height_pct = None;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotionChoice {
    None,
    Pop,
    Slide,
    Fade,
}

impl MotionChoice {
    fn index(self) -> usize {
        match self {
            MotionChoice::None => 0,
            MotionChoice::Pop => 1,
            MotionChoice::Slide => 2,
            MotionChoice::Fade => 3,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(MotionChoice::None),
            1 => Some(MotionChoice::Pop),
            2 => Some(MotionChoice::Slide),
            3 => Some(MotionChoice::Fade),
            _ => None,
        }
    }
}

fn motion_choice(s: &Style) -> MotionChoice {
    let m = &s.motion;
    if m.popup == PopupEffect::None && m.exit == PopupEffect::None {
        MotionChoice::None
    } else if m.popup == PopupEffect::Pop || m.exit == PopupEffect::Pop {
        MotionChoice::Pop
    } else if matches!(
        m.popup,
        PopupEffect::SlideUp
            | PopupEffect::SlideDown
            | PopupEffect::SlideLeft
            | PopupEffect::SlideRight
    ) || matches!(
        m.exit,
        PopupEffect::SlideUp
            | PopupEffect::SlideDown
            | PopupEffect::SlideLeft
            | PopupEffect::SlideRight
    ) {
        MotionChoice::Slide
    } else if m.popup == PopupEffect::Fade || m.exit == PopupEffect::Fade {
        MotionChoice::Fade
    } else {
        MotionChoice::None
    }
}

fn set_motion(s: &mut Style, choice: MotionChoice) {
    match choice {
        MotionChoice::None => {
            s.motion.popup = PopupEffect::None;
            s.motion.exit = PopupEffect::None;
        }
        MotionChoice::Pop => {
            s.motion.popup = PopupEffect::Pop;
            s.motion.exit = PopupEffect::Pop;
            s.motion.easing = Easing::EaseOut;
            s.motion.intensity = 0.20;
        }
        MotionChoice::Slide => {
            s.motion.popup = PopupEffect::SlideUp;
            s.motion.exit = PopupEffect::SlideDown;
            s.motion.easing = Easing::EaseOut;
            s.motion.intensity = 0.80;
        }
        MotionChoice::Fade => {
            s.motion.popup = PopupEffect::Fade;
            s.motion.exit = PopupEffect::Fade;
            s.motion.easing = Easing::EaseOut;
            s.motion.intensity = 0.0;
        }
    }
    set_speed(s, speed_choice(s));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeedChoice {
    Slow,
    Normal,
    Fast,
}

impl SpeedChoice {
    fn index(self) -> usize {
        match self {
            SpeedChoice::Slow => 0,
            SpeedChoice::Normal => 1,
            SpeedChoice::Fast => 2,
        }
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(SpeedChoice::Slow),
            1 => Some(SpeedChoice::Normal),
            2 => Some(SpeedChoice::Fast),
            _ => None,
        }
    }
}

fn speed_choice(s: &Style) -> SpeedChoice {
    if s.motion.duration_ms <= 180 {
        SpeedChoice::Fast
    } else if s.motion.duration_ms >= 280 {
        SpeedChoice::Slow
    } else {
        SpeedChoice::Normal
    }
}

fn set_speed(s: &mut Style, speed: SpeedChoice) {
    let (enter, exit) = match speed {
        SpeedChoice::Slow => (320, 220),
        SpeedChoice::Normal => (240, 160),
        SpeedChoice::Fast => (160, 110),
    };
    s.motion.duration_ms = enter;
    s.motion.exit_duration_ms = exit;
}

fn contrast_color(ink: Color) -> Color {
    let luma = (0.2126 * ink.r as f32 + 0.7152 * ink.g as f32 + 0.0722 * ink.b as f32) / 255.0;
    if luma > 0.55 {
        Color::new(0, 0, 0, 235)
    } else {
        Color::new(255, 255, 255, 255)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin(id: &str) -> Style {
        crate::profile::builtin(id).unwrap().style.clone()
    }

    #[test]
    fn every_builtin_maps_to_a_control_state() {
        for profile in crate::profile::builtins() {
            let s = &profile.style;
            let _ = (
                font_choice(s).index(),
                edge_choice(s).index(),
                plate_choice(s).index(),
                plate_shape(s).index(),
                highlight_choice(s).index(),
                motion_choice(s).index(),
                speed_choice(s).index(),
                lines_choice(s),
                accent_color(s),
            );
        }
    }

    #[test]
    fn edge_is_exclusive_and_strength_maps_to_width() {
        let mut s = Style::default();
        set_edge(&mut s, EdgeChoice::Outline);
        assert!(s.text.outline.enabled);
        assert!(!s.text.shadow.enabled);
        assert!(!s.text.glow.enabled);

        set_edge_strength(&mut s, 1.0);
        assert!((s.text.outline.width - 0.18).abs() < 1e-5);

        set_edge(&mut s, EdgeChoice::Shadow);
        assert!(!s.text.outline.enabled);
        assert!(s.text.shadow.enabled);
    }

    #[test]
    fn bar_plate_uses_fixed_width_and_bottom_anchor() {
        let mut s = Style::default();
        set_plate(&mut s, PlateChoice::Bar);
        assert_eq!(plate_choice(&s), PlateChoice::Bar);
        assert!(s.layout.box_width_pct.is_some());
        assert_eq!(s.layout.anchor, Anchor::Bottom);

        set_plate_width(&mut s, 84.0);
        assert_eq!(s.layout.box_width_pct, Some(84.0));
        assert_eq!(s.layout.max_width_pct, 84.0);
    }

    #[test]
    fn two_lines_reserve_two_lines_and_wrap() {
        let mut s = Style::default();
        set_lines(&mut s, 2);
        assert_eq!(s.layout.fit, Fit::Wrap);
        assert_eq!(s.layout.max_lines, 2);
        assert_eq!(s.layout.reserve_lines, 2);
    }

    #[test]
    fn pill_highlight_uses_accent_and_contrasting_text() {
        let mut s = Style::default();
        let yellow = Color::new(255, 209, 102, 255);
        set_accent(&mut s, yellow);
        set_highlight(&mut s, HighlightChoice::Pill);
        assert!(s.highlight.pill.enabled);
        assert_eq!(s.highlight.pill.color, yellow);
        assert_eq!(s.text.highlight, Color::new(0, 0, 0, 235));
    }

    #[test]
    fn slide_motion_mirrors_entrance_and_exit() {
        let mut s = Style::default();
        set_motion(&mut s, MotionChoice::Slide);
        assert_eq!(s.motion.popup, PopupEffect::SlideUp);
        assert_eq!(s.motion.exit, PopupEffect::SlideDown);
    }

    #[test]
    fn builtin_profiles_still_map_to_expected_sections() {
        assert_eq!(font_choice(&builtin("builtin:classic")), FontChoice::Serif);
        assert_eq!(plate_choice(&builtin("builtin:classic")), PlateChoice::Box);
        assert_eq!(
            highlight_choice(&builtin("builtin:highlight-pill")),
            HighlightChoice::Pill
        );
        assert_eq!(
            plate_choice(&builtin("builtin:lower-third")),
            PlateChoice::Bar
        );
        assert_eq!(
            motion_choice(&builtin("builtin:word-by-word")),
            MotionChoice::Pop
        );
    }

    #[test]
    fn panel_builds_headlessly_for_every_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::with_dir(dir.path().join("profiles"));

        for profile in crate::profile::builtins() {
            let ing = crate::pipeline::Ingest {
                tools: crate::probe::Fftools {
                    ffmpeg: std::path::PathBuf::from("ffmpeg"),
                    ffprobe: std::path::PathBuf::from("ffprobe"),
                    source: crate::source::Source::System,
                },
                info: crate::probe::VideoInfo {
                    display_width: 1080,
                    display_height: 1920,
                    duration_secs: 10.0,
                    fps: 30.0,
                    frame_rate: "30/1".to_string(),
                    has_audio: true,
                    audio_codec: Some("aac".to_string()),
                    pix_fmt: "yuv420p".to_string(),
                    color_range: "tv".to_string(),
                    color_space: "bt709".to_string(),
                    color_primaries: "bt709".to_string(),
                    color_transfer: "bt709".to_string(),
                },
                words: Vec::new(),
                from_cache: true,
                model: crate::model::Model::BaseEn,
            };
            let mut r = ReviewState::new(
                std::path::PathBuf::from("sample.mp4"),
                ing,
                profile.style.clone(),
                crate::config::Grouping::default(),
            );
            let mut profile_ui = ProfileUi::new(None, &store);
            egui::__run_test_ui(|ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let _ = panel(ui, &mut r, &store, &mut profile_ui);
                });
            });
        }
    }
}
