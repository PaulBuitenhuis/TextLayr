//! In-app viewer for the third-party license texts.
//!
//! `THIRD_PARTY_LICENSES.md` is embedded at build time and split into sections
//! (bundled components, the dependency inventory, and one block per distinct
//! license text). A sidebar picks a section and only that section is laid out,
//! so the ~600 KB document stays responsive instead of being one giant label.

use egui::Context;

/// The generated notices, baked into the binary so they ship with the app.
const TEXT: &str = include_str!("../../THIRD_PARTY_LICENSES.md");

/// Split the document into `(title, body)` blocks at `##`/`###` headings,
/// ignoring anything inside fenced code blocks (license texts can contain
/// markdown-looking lines).
fn sections(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut title = "Overview".to_string();
    let mut body = String::new();
    let mut in_fence = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            body.push_str(line);
            body.push('\n');
            continue;
        }
        if !in_fence {
            if let Some(rest) = line
                .strip_prefix("### ")
                .or_else(|| line.strip_prefix("## "))
            {
                if !body.trim().is_empty() {
                    out.push((std::mem::take(&mut title), std::mem::take(&mut body)));
                } else {
                    body.clear();
                }
                title = rest.trim().to_string();
                continue;
            }
        }
        body.push_str(line);
        body.push('\n');
    }
    if !body.trim().is_empty() {
        out.push((title, body));
    }
    out
}

/// Draw the licenses window when `open` is set.
pub fn window(ctx: &Context, open: &mut bool, selected: &mut usize) {
    if !*open {
        return;
    }
    let sections = sections(TEXT);
    if sections.is_empty() {
        *open = false;
        return;
    }
    *selected = (*selected).min(sections.len() - 1);

    egui::Window::new("Open source licenses")
        .open(open)
        .resizable(true)
        .default_size([900.0, 640.0])
        .min_size([620.0, 420.0])
        .collapsible(false)
        .show(ctx, |ui| {
            // Section list down the left, resizable.
            egui::Panel::left("license_index")
                .default_size(300.0)
                .min_size(220.0)
                .max_size(420.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("license_index_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, (title, _)) in sections.iter().enumerate() {
                                let label = egui::RichText::new(title).small();
                                ui.selectable_value(selected, i, label);
                            }
                        });
                });
            // The selected section fills the rest.
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("license_body")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let body = &sections[*selected].1;
                        ui.add(
                            egui::Label::new(egui::RichText::new(body).monospace().size(11.0))
                                .wrap(),
                        );
                    });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_named_sections() {
        let secs = sections(TEXT);
        assert!(secs.len() > 3, "expected several sections, got {}", secs.len());
        assert_eq!(secs[0].0, "Overview");
        // The bundled fonts section is present and includes the OFL preamble.
        assert!(secs.iter().any(|(t, _)| t.contains("Caption fonts")));
        assert!(TEXT.contains("SIL OPEN FONT LICENSE"));
    }

    #[test]
    fn fenced_headings_do_not_split() {
        let input = "# Top\n\n## Real heading\n\n```text\n## not a heading\n```\n\nbody\n";
        let secs = sections(input);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[1].0, "Real heading");
        assert!(secs[1].1.contains("## not a heading"));
    }

    #[test]
    fn window_renders_headlessly() {
        let ctx = egui::Context::default();
        let mut open = true;
        let mut sel = 0;
        // Headless passes must consume their texture deltas or epaint's debug
        // assert fires on drop.
        let mut render = |sel: &mut usize| {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                window(ui.ctx(), &mut open, sel);
            });
            out.textures_delta.clear();
        };
        // First section (Overview) and a later, code-heavy one.
        render(&mut sel);
        sel = sections(TEXT).len() - 1;
        render(&mut sel);
        assert!(open);
    }
}
