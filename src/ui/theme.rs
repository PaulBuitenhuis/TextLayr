//! macOS-flavoured egui theme + the shared widget builders.
//!
//! Every visual decision (spacing, corner radii, the accent colour, control
//! heights) lives here so the drop / setup / ingest / review / export / done
//! screens stay consistent instead of each picking its own sizes.

use egui::{Color32, CornerRadius, Margin, Response, RichText, Stroke, TextStyle, Vec2};

/// macOS system accent (blue).
pub const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
const ACCENT_HOVER: Color32 = Color32::from_rgb(48, 156, 255);
const ACCENT_ACTIVE: Color32 = Color32::from_rgb(0, 106, 224);

/// Corner radius for buttons, inputs and small cards (≈ Big Sur controls).
pub const ROUND: u8 = 6;
/// Corner radius for larger surfaces (drop card, setup cards).
pub const ROUND_LG: u8 = 12;
/// Shared height of a control (button / single-line input), so rows align.
pub const CONTROL_H: f32 = 26.0;

// ── palette ───────────────────────────────────────────────────────────────
const SURFACE: Color32 = Color32::from_rgb(30, 30, 33);
const SURFACE_PANEL: Color32 = Color32::from_rgb(26, 26, 29);
const SUNKEN: Color32 = Color32::from_rgb(18, 18, 20);
const CONTROL: Color32 = Color32::from_rgb(58, 58, 63);
const CONTROL_HOVER: Color32 = Color32::from_rgb(72, 72, 78);
const CONTROL_ACTIVE: Color32 = Color32::from_rgb(48, 48, 52);
const BORDER: Color32 = Color32::from_rgb(72, 72, 78);
const TEXT: Color32 = Color32::from_rgb(236, 236, 241);
const TEXT_WEAK: Color32 = Color32::from_rgb(152, 152, 160);
const ROW_SELECTED: Color32 = Color32::from_rgb(44, 44, 50);

/// Install the app theme on the context (call once at startup).
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        // ── type scale ────────────────────────────────────────────────────
        style
            .text_styles
            .insert(TextStyle::Heading, egui::FontId::proportional(19.0));
        style
            .text_styles
            .insert(TextStyle::Body, egui::FontId::proportional(13.5));
        style
            .text_styles
            .insert(TextStyle::Button, egui::FontId::proportional(13.5));
        style
            .text_styles
            .insert(TextStyle::Small, egui::FontId::proportional(11.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, egui::FontId::monospace(12.5));

        // ── spacing ───────────────────────────────────────────────────────
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 8.0);
        s.button_padding = Vec2::new(12.0, 5.0);
        s.interact_size = Vec2::new(40.0, CONTROL_H);
        s.slider_width = 128.0;
        s.slider_rail_height = 6.0;
        s.combo_width = 150.0;
        s.window_margin = Margin::same(12);
        s.menu_margin = Margin::same(6);

        // ── palette ───────────────────────────────────────────────────────
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = SURFACE_PANEL;
        v.window_fill = SURFACE;
        v.extreme_bg_color = SUNKEN;
        v.text_edit_bg_color = Some(SUNKEN);
        v.faint_bg_color = ROW_SELECTED;
        v.hyperlink_color = ACCENT;
        v.weak_text_color = Some(TEXT_WEAK);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.slider_trailing_fill = true;
        // `selection.stroke.color` is the *text* colour drawn on top of the
        // selection fill (e.g. the selected row of a dropdown), so it must
        // contrast with the accent, not match it.
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.warn_fg_color = Color32::from_rgb(255, 179, 64);
        v.error_fg_color = Color32::from_rgb(255, 105, 97);

        let corner = CornerRadius::same(ROUND);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CONTROL;
        w.noninteractive.weak_bg_fill = SURFACE_PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_WEAK);
        w.noninteractive.corner_radius = corner;

        w.inactive.bg_fill = CONTROL;
        w.inactive.weak_bg_fill = CONTROL;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = corner;

        w.hovered.bg_fill = CONTROL_HOVER;
        w.hovered.weak_bg_fill = CONTROL_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, BORDER);
        w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        w.hovered.corner_radius = corner;

        w.active.bg_fill = CONTROL_ACTIVE;
        w.active.weak_bg_fill = CONTROL_ACTIVE;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        w.active.corner_radius = corner;

        w.open = w.active;
    });
}

// ── widget builders ───────────────────────────────────────────────────────

/// Run `add` with the accent "default button" style installed. Disabled
/// widgets fall through to the (grey) non-interactive style, so a disabled
/// primary button still reads as disabled.
fn accent_scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut style = (**ui.style()).clone();
    let w = &mut style.visuals.widgets;
    for v in [&mut w.inactive, &mut w.hovered, &mut w.open] {
        v.bg_fill = ACCENT;
        v.weak_bg_fill = ACCENT;
        v.bg_stroke = Stroke::NONE;
        v.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.corner_radius = CornerRadius::same(ROUND);
    }
    for (v, fill) in [
        (&mut w.hovered, ACCENT_HOVER),
        (&mut w.active, ACCENT_ACTIVE),
    ] {
        v.bg_fill = fill;
        v.weak_bg_fill = fill;
        v.bg_stroke = Stroke::NONE;
        v.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.corner_radius = CornerRadius::same(ROUND);
    }
    ui.scope(|ui| {
        ui.set_style(style);
        add(ui)
    })
    .inner
}

/// A button occupying an explicit size, with its label **centred**. Uses
/// `add_sized`, whose justified layout is what centres the content; `min_size`
/// alone would leave the label stranded at the left edge.
fn sized_button(ui: &mut egui::Ui, enabled: bool, size: Vec2, text: RichText) -> Response {
    ui.add_enabled_ui(enabled, |ui| ui.add_sized(size, egui::Button::new(text)))
        .inner
}

/// The macOS "default" action: accent-filled, full width of its container.
pub fn primary_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> Response {
    let size = Vec2::new(ui.available_width(), CONTROL_H);
    accent_scope(ui, |ui| sized_button(ui, enabled, size, RichText::new(label).strong()))
}

/// A secondary (grey) button at the shared control height, sized to its label.
pub fn button(ui: &mut egui::Ui, label: &str) -> Response {
    button_enabled(ui, true, label)
}

/// As [`button`], but disable-able (a disabled button renders grey).
pub fn button_enabled(ui: &mut egui::Ui, enabled: bool, label: &str) -> Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(label).min_size(Vec2::new(0.0, CONTROL_H)),
    )
}

/// A secondary button that fills the available width (label centred).
pub fn wide_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> Response {
    let size = Vec2::new(ui.available_width(), CONTROL_H);
    sized_button(ui, enabled, size, RichText::new(label))
}

/// A secondary button of a fixed width (label centred), e.g. Retry / Close.
pub fn fixed_button(ui: &mut egui::Ui, enabled: bool, width: f32, label: &str) -> Response {
    sized_button(ui, enabled, Vec2::new(width, CONTROL_H), RichText::new(label))
}

/// A small-caps section label used to group panel controls.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .small()
            .color(TEXT_WEAK)
            .strong(),
    );
}

/// A subtle card surface (setup notes, centered progress/status blocks).
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(ROUND_LG))
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| add(ui));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applying the theme sets the shared control height on both palettes.
    #[test]
    fn theme_sets_the_control_height() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let style = ctx.style_of(egui::Theme::Dark);
        assert_eq!(style.spacing.interact_size.y, CONTROL_H);
        assert_eq!(style.visuals.selection.bg_fill, ACCENT);
    }

    /// Headless smoke test: build one frame containing the widget vocabulary the
    /// screens use (grid, sliders, swatch, buttons, card, pinned bottom panel).
    /// Guards against panics and egui id clashes in the shared builders.
    #[test]
    fn theme_widgets_build_without_panicking() {
        egui::__run_test_ui(|ui| {
            apply(ui.ctx());

            egui::Panel::top("t").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("TextLayr").size(17.0).strong());
                    ui.separator();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let _ = button(ui, "Change video");
                    });
                });
            });

            egui::CentralPanel::default().show(ui, |ui| {
                // Pinned footer, as the style panel does.
                egui::Panel::bottom("footer").show(ui, |ui| {
                    let _ = primary_button(ui, true, "Export video");
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("or press \u{2318}E").weak().small());
                    });
                });

                ui.heading("Style");
                ui.add_space(8.0);

                let mut pct = 3.4f32;
                let mut rounded = true;
                egui::Grid::new("grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Size");
                        ui.add(egui::Slider::new(&mut pct, 1.0..=20.0).suffix("%"));
                        ui.end_row();
                        ui.label("Highlight");
                        let mut c = Color32::WHITE;
                        let _ = ui.color_edit_button_srgba(&mut c);
                        ui.end_row();
                    });

                ui.checkbox(&mut rounded, "Rounded corners");
                let _ = button_enabled(ui, false, "Previous");
                let _ = button(ui, "Dismiss");
                let _ = wide_button(ui, true, "Reset to defaults");
                card(ui, |ui| {
                    ui.label(egui::RichText::new("what").strong());
                    ui.label(egui::RichText::new("how").weak());
                });
            });
        });
    }
}
