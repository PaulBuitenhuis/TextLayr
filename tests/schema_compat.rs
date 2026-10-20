//! Compatibility fixtures: old configs and profiles must keep loading, and
//! values from newer versions must degrade rather than reject the file.

use std::path::PathBuf;

use textlayr::config::{load_prefs_from, Align, Anchor, Easing, Fit, PopupEffect, FONT_FAMILIES};
use textlayr::model::Model;
use textlayr::profile::ProfileStore;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load_profile(name: &str) -> textlayr::profile::Profile {
    let store = ProfileStore::new();
    store
        .find(&fixture(name).to_string_lossy())
        .unwrap_or_else(|| panic!("fixture {name} did not load"))
}

#[test]
fn v2_config_migrates_into_the_grouped_style() {
    let p = load_prefs_from(&fixture("config-v2.json"));
    assert_eq!(p.style.font.family, "Inter");
    assert_eq!(p.style.font.size_pct, 4.2);
    assert_eq!(p.style.text.highlight.r, 1);
    assert_eq!(p.style.text.dim.b, 6);
    assert_eq!(p.style.box_style.color.r, 7);
    assert_eq!(p.style.box_style.opacity, 0.4);
    assert!(!p.style.box_style.rounded);
    assert_eq!(p.style.box_style.radius_frac, 0.3);
    assert!(!p.style.box_style.shadow);
    assert_eq!(p.style.layout.position_pct, 44.0);
    assert_eq!(p.grouping.words_per_popup, 5);
    assert_eq!(p.active_profile, None);
    assert_eq!(p.model, Model::SmallEn);
    assert!(!p.auto_check_updates);
    assert_eq!(p.last_update_check, Some(99));

    // The new groups default to the old single-line look.
    assert_eq!(p.style.layout.fit, Fit::Shrink);
    assert_eq!(p.style.layout.anchor, Anchor::Center);
    assert_eq!(p.style.motion.popup, PopupEffect::None);
}

#[test]
fn v3_config_round_trips_all_groups() {
    let p = load_prefs_from(&fixture("config-v3.json"));
    assert_eq!(p.style.font.family, "Nunito");
    assert!(p.style.font.uppercase);
    assert!(p.style.text.outline.enabled);
    assert_eq!(p.style.text.outline.width, 0.1);
    assert_eq!(p.style.box_style.padding_x, 0.6);
    assert_eq!(p.style.layout.fit, Fit::Wrap);
    assert_eq!(p.style.layout.max_lines, 2);
    assert_eq!(p.style.layout.anchor, Anchor::Bottom);
    assert_eq!(p.style.highlight.scale, 1.1);
    assert!(p.style.highlight.pill.enabled);
    assert_eq!(p.style.motion.popup, PopupEffect::Pop);
    assert_eq!(p.style.motion.duration_ms, 250);
    assert_eq!(p.style.motion.easing, Easing::Spring);
    assert_eq!(p.style.motion.intensity, 0.2);
    assert_eq!(p.grouping.words_per_popup, 4);
    assert_eq!(p.active_profile.as_deref(), Some("builtin:bold-pop"));
    assert_eq!(p.model, Model::BaseEn);
    assert!(p.auto_check_updates);
    assert_eq!(p.last_update_check, Some(123));
}

#[test]
fn a_newer_config_version_still_loads_best_effort() {
    let p = load_prefs_from(&fixture("config-future.json"));
    assert_eq!(p.style.font.family, "Inter");
    assert_eq!(p.style.layout.fit, Fit::Wrap);
    assert_eq!(p.style.layout.max_lines, 2);
    assert_eq!(p.grouping.words_per_popup, 3);
}

#[test]
fn an_old_profile_gains_new_defaults_without_losing_its_look() {
    let p = load_profile("profile-old.json");
    assert_eq!(p.name, "Old Style");
    assert_eq!(p.schema, 1);
    assert_eq!(p.style.font.family, "Libre Baskerville");
    assert_eq!(p.style.font.size_pct, 3.4);
    assert_eq!(p.style.box_style.opacity, 0.72);
    assert_eq!(p.grouping.unwrap().words_per_popup, 3);

    // Everything added after this profile was written defaults to the old look.
    assert_eq!(p.style.layout.fit, Fit::Shrink);
    assert_eq!(p.style.layout.max_lines, 1);
    assert_eq!(p.style.layout.reserve_lines, 0);
    assert_eq!(p.style.layout.align, Align::Center);
    assert_eq!(p.style.layout.anchor, Anchor::Center);
    assert_eq!(p.style.layout.max_width_pct, 92.0);
    assert_eq!(p.style.layout.box_width_pct, None);
    assert_eq!(p.style.layout.box_height_pct, None);
    assert!(!p.style.text.outline.enabled);
    assert!(!p.style.text.shadow.enabled);
    assert!(!p.style.text.glow.enabled);
    assert_eq!(p.style.box_style.padding_x, 0.50);
    assert_eq!(p.style.box_style.padding_y, 0.24);
    assert_eq!(p.style.box_style.border_width, 0.0);
    assert_eq!(p.style.highlight.scale, 1.0);
    assert!(!p.style.highlight.pill.enabled);
    assert!(!p.style.highlight.underline.enabled);
    assert_eq!(p.style.motion.popup, PopupEffect::None);
    assert_eq!(p.style.motion.exit, PopupEffect::None);
    assert_eq!(p.style.motion.duration_ms, 260);
    assert_eq!(p.style.motion.exit_duration_ms, 160);
    assert_eq!(p.style.motion.easing, Easing::EaseOut);
}

#[test]
fn unknown_values_degrade_to_defaults_instead_of_rejecting_the_profile() {
    let p = load_profile("profile-unknown.json");
    assert_eq!(p.style.font.family, FONT_FAMILIES[0]);
    assert_eq!(p.style.font.size_pct, 20.0);
    assert_eq!(p.style.layout.anchor, Anchor::Center);
    assert_eq!(p.style.layout.align, Align::Center);
    assert_eq!(p.style.layout.fit, Fit::Shrink);
    assert_eq!(p.style.layout.max_lines, 4);
    assert_eq!(p.style.motion.popup, PopupEffect::None);
    assert_eq!(p.style.motion.exit, PopupEffect::None);
    assert_eq!(p.style.motion.easing, Easing::EaseOut);
    assert_eq!(p.style.motion.duration_ms, 1200);
    assert_eq!(p.style.motion.exit_duration_ms, 160);
    assert_eq!(p.grouping.unwrap().words_per_popup, 8);
    assert_eq!(p.schema, 1);
}

#[test]
fn a_newer_profile_schema_still_loads() {
    let p = load_profile("profile-future.json");
    assert_eq!(p.schema, 99);
    assert_eq!(p.name, "Future Profile");
    assert_eq!(p.style.font.family, "Inter");
    assert_eq!(p.style.layout.fit, Fit::Wrap);
    assert_eq!(p.style.layout.max_lines, 3);
    assert_eq!(p.style.layout.reserve_lines, 3);
    assert_eq!(p.grouping.unwrap().words_per_popup, 2);
    assert_eq!(p.created_with, "9.9.9");
}
