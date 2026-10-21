//! A trash for profiles and mic profiles: deleting one moves its file here instead of removing
//! it, so it can be restored. Each deleted file is kept in its own entry folder, next to a small
//! JSON file recording what it was.

use anyhow::{Context, Result, bail};
use goxlr_ipc::ProfileTrashItem;
use log::warn;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::files::named_file_path;
use crate::sample_trash::{TRASH_RETENTION, move_path};

const ENTRY_FILE: &str = "trash-entry.json";
static ENTRY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct TrashEntry {
    name: String,
    mic: bool,
    deleted_at: u64,
}

pub fn profile_trash_directory(backups: &Path) -> PathBuf {
    backups.join("profile-trash")
}

fn extension(mic: bool) -> &'static str {
    if mic { "goxlrMicProfile" } else { "goxlr" }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or_default()
}

/// Moves the profile `name` (from `directory`) to the trash.
pub fn trash_profile(directory: &Path, backups: &Path, name: &str, mic: bool) -> Result<()> {
    let source = named_file_path(directory, name, extension(mic))?;
    if !source.is_file() {
        bail!("The profile {} doesn't exist", name);
    }

    let deleted_at = now();
    let id = format!(
        "{}-{}-{}",
        deleted_at,
        std::process::id(),
        ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let entry = profile_trash_directory(backups).join(&id);
    fs::create_dir_all(&entry).context("Unable to create the trash entry")?;
    let record = TrashEntry {
        name: name.to_string(),
        mic,
        deleted_at,
    };
    fs::write(entry.join(ENTRY_FILE), serde_json::to_vec_pretty(&record)?)?;

    let Some(file_name) = source.file_name() else {
        bail!("Invalid profile name: {}", name);
    };
    if let Err(e) = move_path(&source, &entry.join(file_name)) {
        let _ = fs::remove_dir_all(&entry);
        return Err(e).context(format!("Unable to move the profile {} to the trash", name));
    }
    Ok(())
}

fn read_entry(entry: &Path) -> Option<(TrashEntry, PathBuf)> {
    let record: TrashEntry =
        serde_json::from_slice(&fs::read(entry.join(ENTRY_FILE)).ok()?).ok()?;
    let file = entry.join(format!("{}.{}", record.name, extension(record.mic)));
    file.is_file().then_some((record, file))
}

/// The deleted profiles, most recently deleted first.
pub fn list_profile_trash(backups: &Path) -> Vec<ProfileTrashItem> {
    let Ok(entries) = fs::read_dir(profile_trash_directory(backups)) else {
        return vec![];
    };
    let mut items: Vec<ProfileTrashItem> = entries
        .flatten()
        .filter_map(|entry| {
            let (record, _) = read_entry(&entry.path())?;
            Some(ProfileTrashItem {
                id: entry.file_name().to_string_lossy().to_string(),
                name: record.name,
                mic: record.mic,
                deleted_at: record.deleted_at,
            })
        })
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.deleted_at));
    items
}

fn entry_path(backups: &Path, id: &str) -> Result<PathBuf> {
    // The id is a folder name we created, never a path.
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." {
        bail!("Invalid trash item: {}", id);
    }
    let path = profile_trash_directory(backups).join(id);
    if !path.is_dir() {
        bail!("This profile is no longer in the trash");
    }
    Ok(path)
}

/// Puts a deleted profile back in its folder (`profiles` or `mic_profiles` depending on its
/// kind), never over an existing one. Returns its name and whether it's a mic profile.
pub fn restore_profile(
    profiles: &Path,
    mic_profiles: &Path,
    backups: &Path,
    id: &str,
) -> Result<(String, bool)> {
    let entry = entry_path(backups, id)?;
    let Some((record, file)) = read_entry(&entry) else {
        bail!("This trash item is damaged");
    };
    let directory = if record.mic { mic_profiles } else { profiles };
    let target = named_file_path(directory, &record.name, extension(record.mic))?;
    if target.exists() {
        bail!(
            "A profile named {} already exists, rename it first",
            record.name
        );
    }
    fs::create_dir_all(directory)?;
    move_path(&file, &target).context(format!("Unable to restore {}", record.name))?;
    let _ = fs::remove_dir_all(&entry);
    Ok((record.name, record.mic))
}

/// Deletes a profile from the trash for good, or everything when `id` is None.
pub fn empty_profile_trash(backups: &Path, id: Option<&str>) -> Result<()> {
    match id {
        Some(id) => fs::remove_dir_all(entry_path(backups, id)?)?,
        None => {
            let trash = profile_trash_directory(backups);
            if trash.exists() {
                fs::remove_dir_all(&trash)?;
            }
        }
    }
    Ok(())
}

/// Removes profiles that have been in the trash for longer than TRASH_RETENTION.
pub fn purge_profile_trash(backups: &Path) {
    let limit = now().saturating_sub(TRASH_RETENTION.as_secs());
    for item in list_profile_trash(backups) {
        if item.deleted_at < limit
            && let Err(e) = empty_profile_trash(backups, Some(&item.id))
        {
            warn!(
                "Unable to purge {} from the profile trash: {}",
                item.name, e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_restore_profiles() {
        let root =
            std::env::temp_dir().join(format!("goxlr-test-profile-trash-{}", std::process::id()));
        let profiles = root.join("profiles");
        let mics = root.join("mic-profiles");
        let backups = root.join("backups");
        fs::create_dir_all(&profiles).unwrap();
        fs::create_dir_all(&mics).unwrap();
        fs::write(profiles.join("Stream.goxlr"), b"profile").unwrap();
        fs::write(mics.join("SM7B.goxlrMicProfile"), b"mic").unwrap();

        trash_profile(&profiles, &backups, "Stream", false).unwrap();
        trash_profile(&mics, &backups, "SM7B", true).unwrap();
        assert!(!profiles.join("Stream.goxlr").exists());
        assert!(trash_profile(&profiles, &backups, "Missing", false).is_err());
        assert!(trash_profile(&profiles, &backups, "../x", false).is_err());

        let items = list_profile_trash(&backups);
        assert_eq!(items.len(), 2);
        let profile = items.iter().find(|item| !item.mic).unwrap();
        let mic = items.iter().find(|item| item.mic).unwrap();

        // Never over an existing profile
        fs::write(profiles.join("Stream.goxlr"), b"new").unwrap();
        assert!(restore_profile(&profiles, &mics, &backups, &profile.id).is_err());
        fs::remove_file(profiles.join("Stream.goxlr")).unwrap();
        assert_eq!(
            restore_profile(&profiles, &mics, &backups, &profile.id).unwrap(),
            ("Stream".to_string(), false)
        );
        assert_eq!(fs::read(profiles.join("Stream.goxlr")).unwrap(), b"profile");

        assert!(restore_profile(&profiles, &mics, &backups, "..").is_err());
        empty_profile_trash(&backups, Some(&mic.id)).unwrap();
        assert!(list_profile_trash(&backups).is_empty());

        fs::remove_dir_all(&root).unwrap();
    }
}
