//! Profile dropdown and inline management.
//!
//! Profiles are a low-friction picker, not a gallery: the dropdown lists names
//! and the working style updates as soon as one is chosen. Saving, renaming and
//! deleting are inline controls in the same panel, never floating windows.
//! Built-in profiles are read-only; saving one creates a user profile.

use std::time::{Duration, Instant};

use egui::RichText;

use crate::profile::{Profile, ProfileStore, PROFILE_SCHEMA};

use super::review::{Action, ReviewState};
use super::theme;

const STATUS_SECS: u64 = 4;

pub struct ProfileUi {
    /// Stable id of the profile the working style is based on.
    pub active_id: Option<String>,
    /// The profile as saved, used for the dirty comparison and the dropdown.
    snapshot: Option<Profile>,
    /// Working style differs from the saved profile (or its grouping does).
    pub dirty: bool,
    status: Option<(String, Instant)>,
    /// Cached user+built-in list for the dropdown.
    profiles: Vec<Profile>,
    profiles_loaded: bool,
    save_as: Option<SaveForm>,
    manage_open: bool,
    rename: Option<RenameForm>,
    delete: Option<Profile>,
    /// A profile picked while edited transcript text would be re-grouped.
    pending: Option<Profile>,
}

struct SaveForm {
    name: String,
    error: Option<String>,
}

struct RenameForm {
    id: String,
    name: String,
    error: Option<String>,
}

impl ProfileUi {
    pub fn new(active_id: Option<String>, store: &ProfileStore) -> Self {
        let mut ui = ProfileUi {
            active_id: None,
            snapshot: None,
            dirty: false,
            status: None,
            profiles: Vec::new(),
            profiles_loaded: false,
            save_as: None,
            manage_open: false,
            rename: None,
            delete: None,
            pending: None,
        };
        if let Some(id) = active_id {
            if let Some(p) = store.find(&id) {
                ui.active_id = Some(p.id.clone());
                ui.snapshot = Some(p);
            } else {
                // The profile was deleted or renamed outside the app: keep the
                // working style, but don't claim it is still based on it.
                eprintln!("(profile {id} is no longer available; using the working style)");
            }
        }
        ui
    }

    fn ensure_profiles(&mut self, store: &ProfileStore) {
        if !self.profiles_loaded {
            self.profiles = store.list();
            self.profiles_loaded = true;
        }
    }

    fn invalidate_profiles(&mut self) {
        self.profiles_loaded = false;
    }

    /// Recompute the dirty state and drop a stale active pointer.
    fn sync(&mut self, r: &ReviewState, store: &ProfileStore) {
        if let Some(id) = self.active_id.clone() {
            let stale = self.snapshot.as_ref().map(|p| p.id != id).unwrap_or(true);
            if stale {
                match store.find(&id) {
                    Some(p) => self.snapshot = Some(p),
                    None => {
                        self.active_id = None;
                        self.snapshot = None;
                    }
                }
            }
        } else {
            self.snapshot = None;
        }
        self.dirty = self
            .snapshot
            .as_ref()
            .map(|p| {
                p.style != r.style
                    || p.grouping
                        .as_ref()
                        .map(|g| *g != r.grouping)
                        .unwrap_or(false)
            })
            .unwrap_or(false);
    }

    /// The Look section: dropdown, status and inline save/revert management.
    pub fn section(
        &mut self,
        ui: &mut egui::Ui,
        r: &mut ReviewState,
        store: &ProfileStore,
    ) -> Action {
        self.sync(r, store);
        self.ensure_profiles(store);
        let mut action = Action::None;

        theme::section(ui, "Look");
        ui.add_space(4.0);

        let active_id = self.active_id.clone();
        let selected = self
            .snapshot
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Custom style".to_string());
        let mut picked: Option<Profile> = None;
        egui::ComboBox::from_id_salt("look_profile")
            .selected_text(selected)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                let user: Vec<&Profile> =
                    self.profiles.iter().filter(|p| !p.is_builtin()).collect();
                let built: Vec<&Profile> =
                    self.profiles.iter().filter(|p| p.is_builtin()).collect();

                for p in user {
                    let active = active_id.as_deref() == Some(p.id.as_str());
                    if ui.selectable_label(active, &p.name).clicked() {
                        picked = Some(p.clone());
                    }
                }
                if !built.is_empty() {
                    if !self.profiles.iter().all(|p| p.is_builtin()) {
                        ui.separator();
                    }
                    for p in built {
                        let active = active_id.as_deref() == Some(p.id.as_str());
                        if ui.selectable_label(active, &p.name).clicked() {
                            picked = Some(p.clone());
                        }
                    }
                }
            });
        if let Some(p) = picked {
            action = join(action, self.request_apply(p, r));
        }

        if let Some(p) = &self.snapshot {
            if self.dirty {
                ui.label(RichText::new("Modified").small().color(theme::ACCENT));
            } else if p.is_builtin() {
                ui.label(RichText::new("Built-in · read-only").small().weak());
            }
        }

        let user_profile = self
            .snapshot
            .as_ref()
            .map(|p| !p.is_builtin())
            .unwrap_or(false);
        let can_manage = !self.profiles.iter().all(|p| p.is_builtin());
        if !can_manage {
            self.manage_open = false;
        }
        let mut save = false;
        let mut save_as = false;
        let mut revert = false;
        let mut manage = false;

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if user_profile && self.dirty && theme::button(ui, "Save").clicked() {
                save = true;
            }
            if theme::button(ui, "Save as\u{2026}").clicked() {
                save_as = true;
            }
            if self.snapshot.is_some() && self.dirty && theme::button(ui, "Revert").clicked() {
                revert = true;
            }
            if can_manage
                && theme::button(
                    ui,
                    if self.manage_open {
                        "Hide manage"
                    } else {
                        "Manage"
                    },
                )
                .clicked()
            {
                manage = true;
            }
        });

        if save {
            if let Some(p) = self.snapshot.clone() {
                self.update_active(&p.id, r, store);
            }
        }
        if save_as {
            let base = self
                .snapshot
                .as_ref()
                .map(|p| format!("{} copy", p.name))
                .unwrap_or_else(|| "My style".to_string());
            self.save_as = Some(SaveForm {
                name: base,
                error: None,
            });
        }
        if revert {
            if let Some(p) = self.snapshot.clone() {
                action = join(action, self.request_apply(p, r));
            }
        }
        if manage {
            self.manage_open = !self.manage_open;
            if !self.manage_open {
                self.rename = None;
                self.delete = None;
            }
        }

        action = join(action, self.pending_ui(ui, r));
        self.save_as_ui(ui, r, store);
        self.manage_ui(ui, store);

        if let Some((msg, at)) = &self.status {
            if at.elapsed() < Duration::from_secs(STATUS_SECS) {
                ui.label(RichText::new(msg).small().color(theme::ACCENT));
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            } else {
                self.status = None;
            }
        }

        ui.add_space(6.0);
        action
    }

    /// Inline confirmation when a profile would re-group an edited transcript.
    fn pending_ui(&mut self, ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
        let Some(p) = self.pending.clone() else {
            return Action::None;
        };
        let n = p.grouping.as_ref().map(|g| g.words_per_popup).unwrap_or(3);

        let mut action = Action::None;
        let mut decided = false;
        ui.add_space(4.0);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(6, 6))
            .corner_radius(theme::ROUND)
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "\u{201c}{}\u{201d} uses {n} words per caption.",
                        p.name
                    ))
                    .small(),
                );
                ui.label(
                    RichText::new(
                        "Changing the caption mode re-groups the transcript and discards text edits.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                let mut apply_both = false;
                let mut apply_look = false;
                let mut cancel = false;
                ui.horizontal_wrapped(|ui| {
                    if theme::button(ui, "Apply both").clicked() {
                        apply_both = true;
                    }
                    if theme::button(ui, "Look only").clicked() {
                        apply_look = true;
                    }
                    if theme::button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
                if apply_both {
                    action = self.apply(p.clone(), r, true);
                    decided = true;
                } else if apply_look {
                    action = self.apply(p.clone(), r, false);
                    decided = true;
                } else if cancel {
                    decided = true;
                }
            });
        if decided {
            self.pending = None;
        }
        action
    }

    fn save_as_ui(&mut self, ui: &mut egui::Ui, r: &ReviewState, store: &ProfileStore) {
        if self.save_as.is_none() {
            return;
        }
        let mut request: Option<String> = None;
        let mut cancel = false;
        if let Some(form) = &mut self.save_as {
            ui.add_space(4.0);
            ui.label(RichText::new("Save as profile").small().weak());
            let resp = ui.add(
                egui::TextEdit::singleline(&mut form.name)
                    .desired_width(f32::INFINITY)
                    .hint_text("Profile name"),
            );
            if let Some(e) = &form.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            if !form.name.trim().is_empty()
                && resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                request = Some(form.name.clone());
            }
            ui.horizontal(|ui| {
                if theme::button_enabled(ui, !form.name.trim().is_empty(), "Save").clicked() {
                    request = Some(form.name.clone());
                }
                if theme::button(ui, "Cancel").clicked() {
                    cancel = true;
                }
            });
        }
        if cancel {
            self.save_as = None;
        }
        if let Some(name) = request {
            self.save_as_profile(name, r, store);
        }
    }

    fn manage_ui(&mut self, ui: &mut egui::Ui, store: &ProfileStore) {
        if !self.manage_open {
            return;
        }

        ui.add_space(6.0);
        let users: Vec<Profile> = self
            .profiles
            .iter()
            .filter(|p| !p.is_builtin())
            .cloned()
            .collect();
        if users.is_empty() {
            ui.label(
                RichText::new("No custom profiles yet. Use Save as\u{2026} to create one.")
                    .small()
                    .weak(),
            );
        } else {
            for p in &users {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&p.name).small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::button(ui, "Delete").clicked() {
                            self.delete = Some(p.clone());
                            self.rename = None;
                        }
                        if theme::button(ui, "Rename").clicked() {
                            self.rename = Some(RenameForm {
                                id: p.id.clone(),
                                name: p.name.clone(),
                                error: None,
                            });
                            self.delete = None;
                        }
                    });
                });
            }
        }

        if let Some(p) = self.delete.clone() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("Remove \u{201c}{}\u{201d}?", p.name))
                    .small()
                    .strong(),
            );
            ui.label(
                RichText::new("The current caption style stays on screen.")
                    .small()
                    .weak(),
            );
            let mut remove = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                if theme::button(ui, "Remove").clicked() {
                    remove = true;
                }
                if theme::button(ui, "Cancel").clicked() {
                    cancel = true;
                }
            });
            if remove {
                self.delete_profile(&p.id, store);
            } else if cancel {
                self.delete = None;
            }
        }

        if self.rename.is_some() {
            let mut request: Option<(String, String)> = None;
            let mut cancel = false;
            if let Some(form) = &mut self.rename {
                ui.add_space(4.0);
                ui.label(RichText::new("Rename profile").small().weak());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut form.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("Profile name"),
                );
                if let Some(e) = &form.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if !form.name.trim().is_empty()
                    && resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    request = Some((form.id.clone(), form.name.clone()));
                }
                ui.horizontal(|ui| {
                    if theme::button_enabled(ui, !form.name.trim().is_empty(), "Rename").clicked() {
                        request = Some((form.id.clone(), form.name.clone()));
                    }
                    if theme::button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
            if cancel {
                self.rename = None;
            }
            if let Some((id, name)) = request {
                self.rename_profile(&id, name, store);
            }
        }

        ui.add_space(4.0);
        if theme::button(ui, "Reveal folder").clicked() {
            if let Some(dir) = store.dir() {
                let _ = std::fs::create_dir_all(dir);
                reveal_path(dir);
            }
        }
    }

    fn request_apply(&mut self, p: Profile, r: &mut ReviewState) -> Action {
        let needs_confirm = match &p.grouping {
            Some(g) => r.edited_any && *g != r.grouping,
            None => false,
        };
        if needs_confirm {
            self.pending = Some(p);
            Action::None
        } else {
            self.apply(p, r, true)
        }
    }

    fn apply(&mut self, p: Profile, r: &mut ReviewState, include_grouping: bool) -> Action {
        r.style = p.style.clone();
        r.style.sanitize();
        let mut action = Action::None;
        if include_grouping {
            if let Some(g) = &p.grouping {
                let mut g = g.clone();
                g.sanitize();
                if g != r.grouping {
                    let discarded = r.rechunk(g.words_per_popup);
                    r.grouping = g;
                    action = Action::Rechunked {
                        discarded_edits: discarded,
                    };
                }
            }
        }
        r.preview.invalidate();
        self.active_id = Some(p.id.clone());
        self.snapshot = Some(p);
        self.dirty = self
            .snapshot
            .as_ref()
            .map(|p| {
                p.style != r.style
                    || p.grouping
                        .as_ref()
                        .map(|g| *g != r.grouping)
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        action
    }

    fn capture(&self, r: &ReviewState, id: String, name: String, description: String) -> Profile {
        Profile {
            schema: PROFILE_SCHEMA,
            id,
            name,
            description,
            style: r.style.clone(),
            grouping: Some(r.grouping.clone()),
            created_with: env!("CARGO_PKG_VERSION").to_string(),
            updated_at: 0,
        }
    }

    fn save_as_profile(&mut self, name: String, r: &ReviewState, store: &ProfileStore) {
        let mut p = self.capture(r, String::new(), name, String::new());
        match store.save_new(&mut p) {
            Ok(()) => {
                self.active_id = Some(p.id.clone());
                self.snapshot = Some(p.clone());
                self.dirty = false;
                self.status = Some((format!("Saved \u{201c}{}\u{201d}", p.name), Instant::now()));
                self.save_as = None;
                self.invalidate_profiles();
            }
            Err(e) => {
                if let Some(form) = &mut self.save_as {
                    form.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    fn update_active(&mut self, id: &str, r: &ReviewState, store: &ProfileStore) {
        let Some(old) = self.snapshot.clone() else {
            return;
        };
        let mut p = self.capture(r, id.to_string(), old.name.clone(), old.description.clone());
        p.created_with = old.created_with.clone();
        match store.update(&mut p) {
            Ok(()) => {
                self.active_id = Some(p.id.clone());
                self.snapshot = Some(p.clone());
                self.dirty = false;
                self.status = Some((
                    format!("Updated \u{201c}{}\u{201d}", p.name),
                    Instant::now(),
                ));
                self.invalidate_profiles();
            }
            Err(e) => {
                self.status = Some((format!("Could not update: {e:#}"), Instant::now()));
            }
        }
    }

    fn rename_profile(&mut self, id: &str, name: String, store: &ProfileStore) {
        match store.rename(id, &name) {
            Ok(p) => {
                if self.active_id.as_deref() == Some(id) {
                    self.snapshot = Some(p.clone());
                }
                self.status = Some((
                    format!("Renamed to \u{201c}{}\u{201d}", p.name),
                    Instant::now(),
                ));
                self.rename = None;
                self.invalidate_profiles();
            }
            Err(e) => {
                if let Some(form) = &mut self.rename {
                    form.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    fn delete_profile(&mut self, id: &str, store: &ProfileStore) {
        match store.delete(id) {
            Ok(()) => {
                if self.active_id.as_deref() == Some(id) {
                    // Keep the working style on screen; just stop claiming it
                    // is based on a profile that no longer exists.
                    self.active_id = None;
                    self.snapshot = None;
                    self.dirty = false;
                }
                if self.rename.as_ref().map(|f| f.id == id).unwrap_or(false) {
                    self.rename = None;
                }
                self.delete = None;
                self.status = Some(("Profile removed".to_string(), Instant::now()));
                self.invalidate_profiles();
            }
            Err(e) => {
                self.status = Some((format!("Could not remove: {e:#}"), Instant::now()));
            }
        }
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

fn reveal_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd = "true";

    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile saved from the current state captures both the look and the
    /// grouping, so applying it can reproduce the caption mode.
    #[test]
    fn capture_takes_style_and_grouping() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::with_dir(dir.path().join("profiles"));
        let mut p = Profile {
            name: "Mine".into(),
            style: crate::config::Style::default(),
            grouping: Some(crate::config::Grouping { words_per_popup: 5 }),
            ..Default::default()
        };
        store.save_new(&mut p).unwrap();
        let found = store.find(&p.id).unwrap();
        assert_eq!(found.grouping.unwrap().words_per_popup, 5);
    }
}
