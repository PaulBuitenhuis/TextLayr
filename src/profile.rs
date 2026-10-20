//! Named style profiles: saved looks plus caption grouping.
//!
//! Built-in profiles are embedded from `assets/profiles/*.json` (so there is no
//! packaging step and they are always present), while user profiles live as one
//! JSON file each under `<config>/textlayr/profiles/`.
//!
//! Profiles have a stable `id` that is independent of the display name:
//! `builtin:<slug>` for read-only built-ins and `user:<slug>` for saved copies.
//! Renaming a profile never changes its id, so the "active profile" pointer and
//! any external references keep working.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::{self, Grouping, Style};

/// Bump when a profile file's shape changes incompatibly.
pub const PROFILE_SCHEMA: u32 = 1;
/// File extension for user profiles.
const EXT: &str = "json";

/// A named, reusable style snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Shape version of this file.
    pub schema: u32,
    /// Stable id: `builtin:<slug>` or `user:<slug>`.
    pub id: String,
    /// Display name (editable without changing `id`).
    pub name: String,
    /// Short description kept for profiles created by earlier versions.
    pub description: String,
    /// The look itself.
    pub style: Style,
    /// Caption grouping, when the profile wants to control it.
    pub grouping: Option<Grouping>,
    /// App version that created the profile (diagnostics/forward compat).
    pub created_with: String,
    /// Unix seconds of the last write.
    pub updated_at: u64,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            schema: PROFILE_SCHEMA,
            id: String::new(),
            name: String::new(),
            description: String::new(),
            style: Style::default(),
            // `None` means "apply the look only" for hand-written profiles
            // that predate grouping.
            grouping: None,
            created_with: String::new(),
            updated_at: 0,
        }
    }
}

impl Profile {
    /// Clamp hand-edited or newer-version values into the supported ranges.
    pub fn sanitize(&mut self) {
        if self.schema == 0 {
            self.schema = PROFILE_SCHEMA;
        }
        self.style.sanitize();
        if let Some(g) = &mut self.grouping {
            g.sanitize();
        }
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            self.name = "Untitled profile".to_string();
        }
        self.description = self.description.trim().to_string();
    }

    pub fn is_builtin(&self) -> bool {
        self.id.starts_with("builtin:")
    }
}

/// Where user profiles live: `<config>/textlayr/profiles`.
pub fn profiles_dir() -> Option<PathBuf> {
    Some(config::config_dir()?.join("profiles"))
}

/// Built-in profiles embedded at compile time, in menu order.
const BUILTIN_FILES: &[&str] = &[
    include_str!("../assets/profiles/classic.json"),
    include_str!("../assets/profiles/bold-pop.json"),
    include_str!("../assets/profiles/clean-subtitle.json"),
    include_str!("../assets/profiles/minimal.json"),
    include_str!("../assets/profiles/highlight-pill.json"),
    include_str!("../assets/profiles/editorial.json"),
    include_str!("../assets/profiles/word-by-word.json"),
    include_str!("../assets/profiles/lower-third.json"),
];

/// Parsed built-ins, cached for the process. Panics only on a malformed
/// embedded asset, which the unit tests also catch at build time.
pub fn builtins() -> &'static [Profile] {
    static BUILTINS: OnceLock<Vec<Profile>> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        BUILTIN_FILES
            .iter()
            .enumerate()
            .map(|(i, json)| {
                let mut p: Profile = serde_json::from_str(json)
                    .unwrap_or_else(|e| panic!("builtin profile #{i} is invalid: {e}"));
                p.sanitize();
                if !p.is_builtin() {
                    panic!(
                        "builtin profile #{i} must have a builtin: id, got {:?}",
                        p.id
                    );
                }
                p
            })
            .collect()
    })
}

/// Look up a built-in by id.
pub fn builtin(id: &str) -> Option<&'static Profile> {
    builtins().iter().find(|p| p.id == id)
}

/// Is this id one of the embedded, read-only profiles?
pub fn is_builtin_id(id: &str) -> bool {
    id.starts_with("builtin:")
}

/// A repository of built-in plus user profiles.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    /// User profile directory. `None` when the platform has no config dir.
    dir: Option<PathBuf>,
}

impl Default for ProfileStore {
    fn default() -> Self {
        ProfileStore::new()
    }
}

impl ProfileStore {
    pub fn new() -> Self {
        ProfileStore {
            dir: profiles_dir(),
        }
    }

    /// A store backed by an explicit directory (tests, or a custom location).
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        ProfileStore {
            dir: Some(dir.into()),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// All profiles: user profiles (alphabetical) followed by built-ins (menu
    /// order). Invalid user files are skipped with a warning rather than
    /// preventing the app from starting.
    pub fn list(&self) -> Vec<Profile> {
        let mut out: Vec<Profile> = Vec::new();
        if let Some(dir) = &self.dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some(EXT) {
                        continue;
                    }
                    match read_profile(&path) {
                        Ok(mut p) => {
                            // The file name is the stable id for user profiles;
                            // this keeps update/delete correct even if a file
                            // was copied or hand-edited.
                            p.id = user_id_from_path(&path);
                            p.sanitize();
                            out.push(p);
                        }
                        Err(e) => eprintln!("(ignoring profile {}: {e:#})", path.display()),
                    }
                }
            }
        }
        // Most recently saved/updated first, so a new profile shows up at the
        // top of "My profiles"; ties break alphabetically.
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        out.extend(builtins().iter().cloned());
        out
    }

    /// Find a profile by exact id, display name (case-insensitive), or a file
    /// path. Used by the CLI and the profile picker.
    pub fn find(&self, id_or_name: &str) -> Option<Profile> {
        let needle = id_or_name.trim();
        if needle.is_empty() {
            return None;
        }
        // A path is unambiguous and wins over a same-named profile.
        let as_path = Path::new(needle);
        if as_path.is_file() && as_path.extension().and_then(|e| e.to_str()) == Some(EXT) {
            if let Ok(mut p) = read_profile(as_path) {
                p.sanitize();
                return Some(p);
            }
        }
        if let Some(p) = builtin(needle) {
            return Some(p.clone());
        }
        let profiles = self.list();
        if let Some(p) = profiles.iter().find(|p| p.id == needle) {
            return Some(p.clone());
        }
        profiles
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(needle))
    }

    /// Create a new user profile. Sets `id`, `schema`, `created_with` and
    /// `updated_at`, and makes the display name unique. Returns the final name.
    pub fn save_new(&self, profile: &mut Profile) -> Result<()> {
        let dir = self.dir.as_ref().context("no platform profile directory")?;
        profile.name = self.unique_name(&profile.name, None);
        profile.schema = PROFILE_SCHEMA;
        profile.created_with = if profile.created_with.trim().is_empty() {
            env!("CARGO_PKG_VERSION").to_string()
        } else {
            profile.created_with.clone()
        };
        profile.updated_at = now_secs();
        profile.grouping.get_or_insert_with(Grouping::default);
        profile.id = self.unique_id(&profile.name, None);
        profile.sanitize();
        write_profile(
            &dir.join(format!("{}.{EXT}", file_stem_for_id(&profile.id))),
            profile,
        )?;
        Ok(())
    }

    /// Overwrite an existing user profile. Built-ins are read-only.
    pub fn update(&self, profile: &mut Profile) -> Result<()> {
        if profile.is_builtin() {
            bail!("built-in profiles cannot be modified");
        }
        let dir = self.dir.as_ref().context("no platform profile directory")?;
        profile.name = self.unique_name(&profile.name, Some(&profile.id));
        profile.sanitize();
        profile.updated_at = now_secs();
        if profile.created_with.trim().is_empty() {
            profile.created_with = env!("CARGO_PKG_VERSION").to_string();
        }
        write_profile(
            &dir.join(format!("{}.{EXT}", file_stem_for_id(&profile.id))),
            profile,
        )?;
        Ok(())
    }

    /// Remove a user profile. Built-ins are read-only.
    pub fn delete(&self, id: &str) -> Result<()> {
        if is_builtin_id(id) {
            bail!("built-in profiles cannot be deleted");
        }
        let dir = self.dir.as_ref().context("no platform profile directory")?;
        let path = dir.join(format!("{}.{EXT}", file_stem_for_id(id)));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // Already gone is the outcome the caller wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }

    /// Copy a profile (built-in or user) into a new user profile.
    pub fn duplicate(&self, id: &str, name: Option<String>) -> Result<Profile> {
        let mut p = self
            .find(id)
            .with_context(|| format!("profile not found: {id}"))?;
        p.id.clear();
        p.name = name.unwrap_or_else(|| format!("{} copy", p.name));
        self.save_new(&mut p)?;
        Ok(p)
    }

    /// Rename a profile without changing its id or file name.
    pub fn rename(&self, id: &str, new_name: &str) -> Result<Profile> {
        let mut p = self
            .find(id)
            .with_context(|| format!("profile not found: {id}"))?;
        p.name = new_name.to_string();
        self.update(&mut p)?;
        Ok(p)
    }

    /// A display name that doesn't collide with any other profile.
    pub fn unique_name(&self, wanted: &str, except_id: Option<&str>) -> String {
        let wanted = wanted.trim();
        let base = if wanted.is_empty() {
            "My profile"
        } else {
            wanted
        };
        let existing: Vec<String> = self
            .list()
            .into_iter()
            .filter(|p| except_id != Some(p.id.as_str()))
            .map(|p| p.name.to_lowercase())
            .collect();
        if !existing.iter().any(|n| n == &base.to_lowercase()) {
            return base.to_string();
        }
        for n in 2..1000 {
            let candidate = format!("{base} {n}");
            if !existing.iter().any(|e| e == &candidate.to_lowercase()) {
                return candidate;
            }
        }
        format!("{base} (copy)")
    }

    fn unique_id(&self, name: &str, except_id: Option<&str>) -> String {
        let base = slug(name);
        let existing: Vec<String> = self
            .list()
            .into_iter()
            .filter(|p| except_id != Some(p.id.as_str()))
            .map(|p| p.id)
            .collect();
        let candidate = format!("user:{base}");
        if !existing.iter().any(|id| id == &candidate) {
            return candidate;
        }
        for n in 2..1000 {
            let candidate = format!("user:{base}-{n}");
            if !existing.iter().any(|id| id == &candidate) {
                return candidate;
            }
        }
        format!("user:{base}-{}", now_secs())
    }
}

fn read_profile(path: &Path) -> Result<Profile> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let p: Profile =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(p)
}

fn write_profile(path: &Path, profile: &Profile) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let json = serde_json::to_vec_pretty(profile).context("serialize profile")?;
    let tmp = path.with_extension(format!("{EXT}.tmp"));
    std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn user_id_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "profile".to_string());
    format!("user:{stem}")
}

/// File stem for a stable id: `user:my-look` → `my-look`. Built-ins are never
/// written, but stripping their prefix keeps this total.
fn file_stem_for_id(id: &str) -> String {
    let short = id
        .strip_prefix("user:")
        .or_else(|| id.strip_prefix("builtin:"))
        .unwrap_or(id);
    slug(short)
}

/// Turn a display name into a filesystem-safe slug.
fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("profile");
    }
    out.truncate(64);
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Human-readable summary for `--list-profiles`.
pub fn list_lines() -> Vec<String> {
    let store = ProfileStore::new();
    let mut lines = Vec::new();
    for p in store.list() {
        let grouping = p
            .grouping
            .as_ref()
            .map(|g| {
                if g.words_per_popup == 1 {
                    " · 1 word/popup".to_string()
                } else {
                    format!(" · {} words/popup", g.words_per_popup)
                }
            })
            .unwrap_or_default();
        lines.push(format!(
            "{:<24} {:<10} {}{}",
            p.id,
            if p.is_builtin() { "built-in" } else { "user" },
            p.name,
            grouping
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Color;

    fn temp_store() -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::with_dir(dir.path().join("profiles"));
        (dir, store)
    }

    fn sample(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            style: Style {
                font: config::FontStyle {
                    family: "Inter".into(),
                    size_pct: 4.0,
                    ..Default::default()
                },
                text: config::TextStyle {
                    highlight: Color::new(1, 2, 3, 255),
                    ..Default::default()
                },
                ..Default::default()
            },
            grouping: Some(Grouping { words_per_popup: 4 }),
            ..Default::default()
        }
    }

    #[test]
    fn builtins_parse_and_are_unique() {
        let all = builtins();
        assert!(all.len() >= 6);
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for p in all {
            assert!(p.id.starts_with("builtin:"), "{}", p.id);
            assert!(!p.name.is_empty());
            assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
            assert!(
                names.insert(p.name.to_lowercase()),
                "duplicate name {}",
                p.name
            );
            assert!(config::FONT_FAMILIES.contains(&p.style.font.family.as_str()));
        }
    }

    #[test]
    fn save_list_find_and_delete_round_trip() {
        let (_dir, store) = temp_store();
        let mut p = sample("My Look");
        store.save_new(&mut p).unwrap();
        assert_eq!(p.id, "user:my-look");
        assert!(p.id.starts_with("user:"));

        let listed = store.list();
        let user: Vec<_> = listed.iter().filter(|p| !p.is_builtin()).collect();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].name, "My Look");

        assert_eq!(store.find("user:my-look").unwrap().name, "My Look");
        assert_eq!(store.find("my look").unwrap().id, "user:my-look");

        store.delete(&p.id).unwrap();
        assert!(store.find("user:my-look").is_none());
        // Deleting twice is fine.
        store.delete(&p.id).unwrap();
    }

    #[test]
    fn names_and_ids_are_uniqued() {
        let (_dir, store) = temp_store();
        let mut a = sample("Same");
        store.save_new(&mut a).unwrap();
        let mut b = sample("Same");
        store.save_new(&mut b).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(b.name, "Same 2");
    }

    #[test]
    fn update_keeps_the_id_and_can_rename() {
        let (_dir, store) = temp_store();
        let mut p = sample("First");
        store.save_new(&mut p).unwrap();
        let id = p.id.clone();
        p.name = "Second".into();
        p.style.font.size_pct = 7.0;
        store.update(&mut p).unwrap();
        assert_eq!(p.id, id);
        let found = store.find(&id).unwrap();
        assert_eq!(found.name, "Second");
        assert_eq!(found.style.font.size_pct, 7.0);
    }

    #[test]
    fn builtins_are_read_only_and_can_be_duplicated() {
        let (_dir, store) = temp_store();
        let classic = builtin("builtin:classic").unwrap().clone();
        let mut p = classic.clone();
        assert!(store.update(&mut p).is_err());
        assert!(store.delete(&p.id).is_err());

        let copy = store
            .duplicate("builtin:classic", Some("Mine".into()))
            .unwrap();
        assert!(copy.id.starts_with("user:"));
        assert_eq!(copy.name, "Mine");
        assert_eq!(copy.style, classic.style);
    }

    #[test]
    fn a_renamed_profile_keeps_its_file_id() {
        let (_dir, store) = temp_store();
        let mut p = sample("Old Name");
        store.save_new(&mut p).unwrap();
        let id = p.id.clone();
        let renamed = store.rename(&id, "New Name").unwrap();
        assert_eq!(renamed.id, id);
        assert_eq!(renamed.name, "New Name");
        assert!(store.find(&id).is_some());
    }

    #[test]
    fn unknown_fields_in_a_profile_are_ignored() {
        let p: Profile = serde_json::from_str(
            r#"{"schema":1,"id":"user:x","name":"X","style":{"font":{"family":"Inter","size_pct":2.0}},"future_field":true}"#,
        )
        .unwrap();
        assert_eq!(p.name, "X");
        assert_eq!(p.style.font.size_pct, 2.0);
        assert_eq!(p.grouping, None);
    }

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slug("Bold Pop!"), "bold-pop");
        assert_eq!(slug("  "), "profile");
        assert_eq!(slug("Café Crème"), "caf-cr-me");
    }
}
