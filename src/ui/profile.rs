//! Profile picker, gallery and management dialogs.
//!
//! The compact picker lives at the top of the style panel; the gallery is a
//! modal window with thumbnails rendered by the real caption renderer. Built-in
//! profiles are read-only; saving or duplicating one creates a user profile.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::profile::{Profile, ProfileStore, PROFILE_SCHEMA};
use crate::render::{self, blend_straight, Faces};

use super::review::{Action, ReviewState};
use super::theme;

const THUMB_W: u32 = 240;
const THUMB_H: u32 = 76;
const STATUS_SECS: u64 = 4;

pub struct ProfileUi {
    /// Stable id of the profile the working style is based on.
    pub active_id: Option<String>,
    /// The profile as saved, used for the dirty comparison and the picker.
    snapshot: Option<Profile>,
    /// Working style differs from the saved profile (or its grouping does).
    pub dirty: bool,
    gallery_open: bool,
    dialog: Option<Dialog>,
    status: Option<(String, Instant)>,
    thumbs: HashMap<String, egui::TextureHandle>,
    faces: HashMap<(String, u32), Faces>,
    /// Cached user+built-in list for the compact strip.
    profiles: Vec<Profile>,
    profiles_loaded: bool,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum Dialog {
    SaveAs {
        name: String,
        error: Option<String>,
    },
    Rename {
        id: String,
        name: String,
        error: Option<String>,
    },
    Delete {
        id: String,
        name: String,
    },
    Update {
        id: String,
        name: String,
    },
    ApplyGrouping {
        profile: Profile,
    },
}

enum Outcome {
    None,
    Close,
    SaveAs(String),
    Rename(String, String),
    Delete(String),
    Update(String),
    ApplyBoth(Profile),
    ApplyLook(Profile),
}

impl ProfileUi {
    pub fn new(active_id: Option<String>, store: &ProfileStore) -> Self {
        let mut ui = ProfileUi {
            active_id: None,
            snapshot: None,
            dirty: false,
            gallery_open: false,
            dialog: None,
            status: None,
            thumbs: HashMap::new(),
            faces: HashMap::new(),
            profiles: Vec::new(),
            profiles_loaded: false,
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

    /// The compact profile picker shown at the top of the style panel.
    pub fn section(
        &mut self,
        ui: &mut egui::Ui,
        r: &mut ReviewState,
        store: &ProfileStore,
    ) -> Action {
        self.sync(r, store);
        self.ensure_profiles(store);
        let mut action = Action::None;

        theme::section(ui, "Profile");
        ui.add_space(4.0);

        let active_id = self.active_id.clone();
        // Show the recent profiles, but never hide the active one.
        let mut strip: Vec<Profile> = Vec::new();
        if let Some(active) = &self.snapshot {
            strip.push(active.clone());
        }
        for p in self.profiles.iter() {
            if strip.iter().all(|a| a.id != p.id) {
                strip.push(p.clone());
            }
            if strip.len() >= 12 {
                break;
            }
        }
        let mut picked: Option<Profile> = None;
        egui::ScrollArea::horizontal()
            .id_salt("profile_strip")
            .max_height(62.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for p in &strip {
                        let selected = active_id.as_deref() == Some(p.id.as_str());
                        let card = ui.allocate_ui(egui::vec2(104.0, 54.0), |ui| {
                            ui.vertical_centered(|ui| {
                                if let Some(tex) = self.thumbnail(ui.ctx(), p) {
                                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                        tex.id(),
                                        egui::vec2(96.0, 30.0),
                                    )));
                                } else {
                                    ui.allocate_space(egui::vec2(96.0, 30.0));
                                }
                                ui.add(egui::Label::new(RichText::new(&p.name).small()).truncate());
                            });
                        });
                        let resp = card.response.interact(egui::Sense::click());
                        if selected {
                            ui.painter().rect_stroke(
                                card.response.rect.expand(2.0),
                                egui::CornerRadius::same(theme::ROUND),
                                egui::Stroke::new(1.5, theme::ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if resp.clicked() {
                            picked = Some(p.clone());
                        }
                    }
                });
            });
        if let Some(p) = picked {
            action = self.request_apply(p, r);
        }

        let (state, accent) = match &self.snapshot {
            Some(p) if self.dirty => (format!("{} · modified", p.name), true),
            Some(p) if p.is_builtin() => ("Built-in · read-only".to_string(), false),
            Some(p) => (format!("{} · saved", p.name), false),
            None => ("Not saved as a profile".to_string(), false),
        };
        let mut text = RichText::new(state).small();
        if accent {
            text = text.color(theme::ACCENT);
        } else {
            text = text.weak();
        }
        ui.label(text);

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if theme::button(ui, "All profiles…").clicked() {
                self.gallery_open = true;
            }
            if theme::button(ui, "Save as…").clicked() {
                let base = self
                    .snapshot
                    .as_ref()
                    .map(|p| format!("{} copy", p.name))
                    .unwrap_or_else(|| "My style".to_string());
                self.dialog = Some(Dialog::SaveAs {
                    name: base,
                    error: None,
                });
            }
            let user_profile = self
                .snapshot
                .as_ref()
                .map(|p| !p.is_builtin())
                .unwrap_or(false);
            if user_profile && theme::button_enabled(ui, self.dirty, "Update").clicked() {
                if let Some(p) = &self.snapshot {
                    self.dialog = Some(Dialog::Update {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            if self.snapshot.is_some() && theme::button_enabled(ui, self.dirty, "Revert").clicked()
            {
                if let Some(p) = self.snapshot.clone() {
                    action = self.request_apply(p, r);
                }
            }
        });

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

    /// The gallery window and any open management dialog. Call once per frame,
    /// after the panel content, so a modal is drawn above everything.
    pub fn windows(
        &mut self,
        ctx: &egui::Context,
        r: &mut ReviewState,
        store: &ProfileStore,
    ) -> Action {
        self.sync(r, store);
        let mut action = Action::None;

        if self.gallery_open {
            action = self.gallery(ctx, r, store);
        }
        let dialog_action = self.show_dialog(ctx, r, store);
        if dialog_action != Action::None {
            action = dialog_action;
        }
        action
    }

    fn gallery(
        &mut self,
        ctx: &egui::Context,
        r: &mut ReviewState,
        store: &ProfileStore,
    ) -> Action {
        let mut action = Action::None;
        let mut open = self.gallery_open;
        let mut save_as = false;
        let mut reveal = false;

        egui::Window::new("Style profiles")
            .open(&mut open)
            .default_width(760.0)
            .default_height(560.0)
            .min_width(520.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(
                            "Saved looks and caption modes. Built-ins are read-only; duplicate one to customise it.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::button(ui, "Reveal folder").clicked() {
                            reveal = true;
                        }
                        if theme::button(ui, "Save current…").clicked() {
                            save_as = true;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let profiles = store.list();
                        let user: Vec<Profile> = profiles
                            .iter()
                            .filter(|p| !p.is_builtin())
                            .cloned()
                            .collect();
                        let built: Vec<Profile> = profiles
                            .into_iter()
                            .filter(|p| p.is_builtin())
                            .collect();

                        ui.heading("My profiles");
                        ui.add_space(4.0);
                        if user.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No custom profiles yet. Tweak the style, then use “Save current…”.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.add_space(4.0);
                        } else {
                            for p in user {
                                action = self.profile_row(ui, &p, r, store, action);
                                ui.add_space(4.0);
                            }
                        }

                        ui.add_space(10.0);
                        ui.heading("Built-in");
                        ui.add_space(4.0);
                        for p in built {
                            action = self.profile_row(ui, &p, r, store, action);
                            ui.add_space(4.0);
                        }
                        ui.add_space(6.0);
                    });
            });
        // `profile_row` may close the gallery after applying a profile, while
        // the Window close button writes to `open`. Keep both paths.
        self.gallery_open = open && self.gallery_open;

        if save_as {
            let base = self
                .snapshot
                .as_ref()
                .map(|p| format!("{} copy", p.name))
                .unwrap_or_else(|| "My style".to_string());
            self.dialog = Some(Dialog::SaveAs {
                name: base,
                error: None,
            });
        }
        if reveal {
            if let Some(dir) = store.dir() {
                let _ = std::fs::create_dir_all(dir);
                reveal_path(dir);
            }
        }
        action
    }

    fn profile_row(
        &mut self,
        ui: &mut egui::Ui,
        p: &Profile,
        r: &mut ReviewState,
        store: &ProfileStore,
        mut action: Action,
    ) -> Action {
        let active = self.active_id.as_deref() == Some(p.id.as_str());
        let text = ui.available_width();
        ui.scope(|ui| {
            ui.set_width(text);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .corner_radius(theme::ROUND)
                .fill(if active {
                    ui.visuals().faint_bg_color
                } else {
                    Color32::TRANSPARENT
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let size = egui::vec2(168.0, 54.0);
                        if let Some(tex) = self.thumbnail(ui.ctx(), p) {
                            ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                tex.id(),
                                size,
                            )));
                        } else {
                            ui.allocate_ui(size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.label(RichText::new("no preview").small().weak());
                                });
                            });
                        }

                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&p.name).strong());
                                if active {
                                    ui.label(RichText::new("Active").small().color(theme::ACCENT));
                                }
                                if p.is_builtin() {
                                    ui.label(RichText::new("Built-in").small().weak());
                                }
                            });
                            if !p.description.is_empty() {
                                ui.add(
                                    egui::Label::new(RichText::new(&p.description).small().weak())
                                        .wrap(),
                                );
                            }
                            let grouping = p
                                .grouping
                                .as_ref()
                                .map(|g| {
                                    if g.words_per_popup == 1 {
                                        "1 word/popup".to_string()
                                    } else {
                                        format!("{} words/popup", g.words_per_popup)
                                    }
                                })
                                .unwrap_or_else(|| "look only".to_string());
                            ui.label(RichText::new(grouping).small().weak());
                        });
                    });

                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::button(ui, "Use").clicked() {
                                self.gallery_open = false;
                                action = self.request_apply(p.clone(), r);
                            }
                            if theme::button(ui, "Look only").clicked() {
                                self.gallery_open = false;
                                action = self.apply(p.clone(), r, false);
                            }
                            if p.is_builtin() {
                                if theme::button(ui, "Duplicate").clicked() {
                                    action = self.duplicate(p, r, store, action);
                                }
                            } else {
                                if self.dirty
                                    && self.active_id.as_deref() == Some(p.id.as_str())
                                    && theme::button(ui, "Update").clicked()
                                {
                                    self.dialog = Some(Dialog::Update {
                                        id: p.id.clone(),
                                        name: p.name.clone(),
                                    });
                                }
                                if theme::button(ui, "Rename").clicked() {
                                    self.dialog = Some(Dialog::Rename {
                                        id: p.id.clone(),
                                        name: p.name.clone(),
                                        error: None,
                                    });
                                }
                                if theme::button(ui, "Delete").clicked() {
                                    self.dialog = Some(Dialog::Delete {
                                        id: p.id.clone(),
                                        name: p.name.clone(),
                                    });
                                }
                                if theme::button(ui, "Duplicate").clicked() {
                                    action = self.duplicate(p, r, store, action);
                                }
                            }
                        });
                    });
                });
        });
        action
    }

    fn duplicate(
        &mut self,
        p: &Profile,
        _r: &mut ReviewState,
        store: &ProfileStore,
        action: Action,
    ) -> Action {
        match store.duplicate(&p.id, None) {
            Ok(copy) => {
                // Add the copy without hijacking the active profile; the user
                // can Apply it when they are ready.
                self.thumbs.clear();
                self.invalidate_profiles();
                self.status = Some((format!("Created “{}”", copy.name), Instant::now()));
                action
            }
            Err(e) => {
                self.status = Some((format!("Could not duplicate: {e:#}"), Instant::now()));
                action
            }
        }
    }

    fn request_apply(&mut self, p: Profile, r: &mut ReviewState) -> Action {
        let needs_confirm = match &p.grouping {
            Some(g) => r.edited_any && *g != r.grouping,
            None => false,
        };
        if needs_confirm {
            self.dialog = Some(Dialog::ApplyGrouping { profile: p });
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

    fn save_as(&mut self, name: String, r: &ReviewState, store: &ProfileStore) {
        let mut p = self.capture(r, String::new(), name, String::new());
        match store.save_new(&mut p) {
            Ok(()) => {
                self.active_id = Some(p.id.clone());
                self.snapshot = Some(p.clone());
                self.dirty = false;
                self.thumbs.clear();
                self.status = Some((format!("Saved “{}”", p.name), Instant::now()));
                self.dialog = None;
                self.invalidate_profiles();
            }
            Err(e) => {
                self.dialog = Some(Dialog::SaveAs {
                    name: p.name,
                    error: Some(format!("{e:#}")),
                });
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
                self.thumbs.clear();
                self.status = Some((format!("Updated “{}”", p.name), Instant::now()));
                self.dialog = None;
                self.invalidate_profiles();
            }
            Err(e) => {
                self.status = Some((format!("Could not update: {e:#}"), Instant::now()));
                self.dialog = None;
            }
        }
    }

    fn rename(&mut self, id: &str, name: String, store: &ProfileStore) {
        match store.rename(id, &name) {
            Ok(p) => {
                if self.active_id.as_deref() == Some(id) {
                    self.snapshot = Some(p.clone());
                }
                self.thumbs.clear();
                self.status = Some((format!("Renamed to “{}”", p.name), Instant::now()));
                self.dialog = None;
                self.invalidate_profiles();
            }
            Err(e) => {
                self.dialog = Some(Dialog::Rename {
                    id: id.to_string(),
                    name,
                    error: Some(format!("{e:#}")),
                });
            }
        }
    }

    fn delete(&mut self, id: &str, store: &ProfileStore) {
        match store.delete(id) {
            Ok(()) => {
                if self.active_id.as_deref() == Some(id) {
                    // Keep the working style on screen; just stop claiming it
                    // is based on a profile that no longer exists.
                    self.active_id = None;
                    self.snapshot = None;
                    self.dirty = false;
                }
                self.thumbs.retain(|k, _| !k.starts_with(&format!("{id}:")));
                self.status = Some(("Profile removed".to_string(), Instant::now()));
                self.invalidate_profiles();
            }
            Err(e) => {
                self.status = Some((format!("Could not remove: {e:#}"), Instant::now()));
            }
        }
        self.dialog = None;
    }

    fn show_dialog(
        &mut self,
        ctx: &egui::Context,
        r: &mut ReviewState,
        store: &ProfileStore,
    ) -> Action {
        let Some(mut dialog) = self.dialog.take() else {
            return Action::None;
        };
        let mut outcome = Outcome::None;
        let mut action = Action::None;

        let resp = egui::Modal::new(egui::Id::new("profile_dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escape {
                outcome = Outcome::Close;
            }
            match &mut dialog {
                Dialog::SaveAs { name, error } => {
                    ui.heading("Save style as profile");
                    ui.add_space(4.0);
                    ui.label(RichText::new("The current look and caption mode are saved together.").small().weak());
                    ui.add_space(8.0);
                    ui.label("Name");
                    let resp = ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(f32::INFINITY)
                            .hint_text("Profile name"),
                    );
                    if let Some(e) = error {
                        ui.add_space(4.0);
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    ui.add_space(10.0);
                    let valid = !name.trim().is_empty();
                    if theme::primary_button(ui, valid, "Save profile").clicked()
                        || (valid && enter && resp.lost_focus())
                    {
                        outcome = Outcome::SaveAs(name.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Cancel").clicked() {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Rename { id, name, error } => {
                    ui.heading("Rename profile");
                    ui.add_space(8.0);
                    ui.label("Name");
                    let resp = ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(f32::INFINITY)
                            .hint_text("Profile name"),
                    );
                    if let Some(e) = error {
                        ui.add_space(4.0);
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    ui.add_space(10.0);
                    let valid = !name.trim().is_empty();
                    if theme::primary_button(ui, valid, "Rename").clicked()
                        || (valid && enter && resp.lost_focus())
                    {
                        outcome = Outcome::Rename(id.clone(), name.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Cancel").clicked() {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Delete { id, name } => {
                    ui.heading("Remove profile?");
                    ui.add_space(6.0);
                    ui.label(format!("Remove “{name}”? This cannot be undone."));
                    ui.label(
                        RichText::new("The current caption style stays on screen.")
                            .small()
                            .weak(),
                    );
                    ui.add_space(12.0);
                    if theme::primary_button(ui, true, "Remove profile").clicked() {
                        outcome = Outcome::Delete(id.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Cancel").clicked() {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Update { id, name } => {
                    ui.heading("Update profile?");
                    ui.add_space(6.0);
                    ui.label(format!(
                        "Replace “{name}” with the current style and caption mode?"
                    ));
                    ui.add_space(12.0);
                    if theme::primary_button(ui, true, "Replace profile").clicked() {
                        outcome = Outcome::Update(id.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Cancel").clicked() {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::ApplyGrouping { profile } => {
                    let n = profile
                        .grouping
                        .as_ref()
                        .map(|g| g.words_per_popup)
                        .unwrap_or(3);
                    ui.heading("Apply this profile?");
                    ui.add_space(6.0);
                    ui.label(format!(
                        "“{}” changes caption grouping to {n} words per popup.",
                        profile.name
                    ));
                    ui.label(
                        RichText::new(
                            "Applying the caption mode re-chunks the transcript and discards your text edits.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(12.0);
                    if theme::primary_button(ui, true, "Apply look + caption mode").clicked() {
                        outcome = Outcome::ApplyBoth(profile.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Apply look only").clicked() {
                        outcome = Outcome::ApplyLook(profile.clone());
                    }
                    ui.add_space(4.0);
                    if theme::wide_button(ui, true, "Cancel").clicked() {
                        outcome = Outcome::Close;
                    }
                }
            }
        });

        // A click on the dimmed backdrop cancels (safe for every dialog here).
        if resp.backdrop_response.clicked() {
            outcome = Outcome::Close;
        }

        match outcome {
            Outcome::None => {
                self.dialog = Some(dialog);
            }
            Outcome::Close => {
                self.dialog = None;
            }
            Outcome::SaveAs(name) => self.save_as(name, r, store),
            Outcome::Rename(id, name) => self.rename(&id, name, store),
            Outcome::Delete(id) => self.delete(&id, store),
            Outcome::Update(id) => self.update_active(&id, r, store),
            Outcome::ApplyBoth(p) => {
                self.dialog = None;
                action = self.apply(p, r, true);
            }
            Outcome::ApplyLook(p) => {
                self.dialog = None;
                action = self.apply(p, r, false);
            }
        }
        action
    }

    /// Render (and cache) a small caption preview for a profile.
    fn thumbnail(&mut self, ctx: &egui::Context, p: &Profile) -> Option<egui::TextureHandle> {
        let key = format!("{}:{}", p.id, p.updated_at);
        if let Some(tex) = self.thumbs.get(&key) {
            return Some(tex.clone());
        }

        let (words, active, px): (Vec<String>, usize, f32) =
            match p.grouping.as_ref().map(|g| g.words_per_popup) {
                Some(1) => (vec!["word".to_string()], 0, 30.0),
                _ => (
                    vec![
                        "Make".to_string(),
                        "every".to_string(),
                        "word".to_string(),
                        "land".to_string(),
                    ],
                    2,
                    20.0,
                ),
            };
        // A thumbnail is a look preview, not a layout preview. A fixed box
        // height measured against the tiny thumbnail frame can clip the text,
        // so let the box hug/reserve naturally here; the real preview shows the
        // fixed dimensions.
        let mut thumb_style = p.style.clone();
        thumb_style.layout.box_height_pct = None;

        let face_key = (thumb_style.font.family.clone(), px.to_bits());
        if !self.faces.contains_key(&face_key) {
            let faces = Faces::load(&thumb_style, px).ok()?;
            self.faces.insert(face_key.clone(), faces);
        }
        let faces = self.faces.get(&face_key)?;
        let cap = render::render_state_rgba(
            faces,
            &thumb_style,
            &words,
            active,
            px,
            THUMB_W * 2,
            THUMB_H * 2,
        )
        .ok()?;
        let (cw, ch) = (cap.width, cap.height);

        let mut bg = vec![0u8; (THUMB_W * THUMB_H * 4) as usize];
        for y in 0..THUMB_H {
            let t = y as f32 / THUMB_H.max(1) as f32;
            let r = (58.0 * (1.0 - t) + 18.0 * t) as u8;
            let g = (62.0 * (1.0 - t) + 20.0 * t) as u8;
            let b = (70.0 * (1.0 - t) + 25.0 * t) as u8;
            for x in 0..THUMB_W {
                let i = ((y * THUMB_W + x) * 4) as usize;
                bg[i] = r;
                bg[i + 1] = g;
                bg[i + 2] = b;
                bg[i + 3] = 255;
            }
        }

        // Scale the tight caption crop down to fit the thumbnail, then centre it.
        let max_w = (THUMB_W as f32 - 12.0).max(1.0);
        let max_h = (THUMB_H as f32 - 10.0).max(1.0);
        let scale = (max_w / cw as f32).min(max_h / ch as f32).min(1.0);
        let nw = (cw as f32 * scale).round().max(1.0) as u32;
        let nh = (ch as f32 * scale).round().max(1.0) as u32;
        let img = image::RgbaImage::from_raw(cw, ch, cap.rgba)?;
        let small = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
        let ox = ((THUMB_W - nw) / 2) as i32;
        let oy = ((THUMB_H - nh) / 2) as i32;
        blend_straight(&mut bg, THUMB_W, THUMB_H, small.as_raw(), nw, nh, ox, oy);

        let color =
            egui::ColorImage::from_rgba_unmultiplied([THUMB_W as usize, THUMB_H as usize], &bg);
        let tex = ctx.load_texture(
            format!("profile-thumb-{}", p.id),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.thumbs.insert(key, tex.clone());
        Some(tex)
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
