//! A trash for samples: deleting a sample (or a folder) moves it here instead of removing it,
//! so it can be restored. Each deleted item is kept in its own entry folder, next to a small
//! JSON file recording where it came from.

use anyhow::{Context, Result, bail};
use goxlr_ipc::SampleTrashItem;
use log::warn;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::files::{SAMPLE_EXTENSIONS, recursive_file_list, sample_path};

// Items older than this are removed for good.
pub const TRASH_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const ENTRY_FILE: &str = "trash-entry.json";
static ENTRY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct TrashEntry {
    // Where the item was, relative to the samples directory ('/' separated)
    path: String,
    // When it was deleted (seconds since the epoch)
    deleted_at: u64,
}

pub fn trash_directory(backups: &Path) -> PathBuf {
    backups.join("sample-trash")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or_default()
}

/// Moves a file or folder, falling back to copy + delete when a rename isn't possible (the
/// trash and the samples may be on different drives).
fn move_path(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_path(from, to)?;
    if from.is_dir() {
        fs::remove_dir_all(from)?;
    } else {
        fs::remove_file(from)?;
    }
    Ok(())
}

fn copy_path(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

/// Moves a sample file or folder (relative to `samples`) to the trash.
pub fn trash_sample_path(samples: &Path, backups: &Path, relative: &str) -> Result<()> {
    let source = sample_path(samples, relative)?;
    if !source.exists() {
        bail!("{} doesn't exist", relative);
    }
    let Some(name) = source.file_name() else {
        bail!("Invalid path: {}", relative);
    };

    let deleted_at = now();
    let id = format!(
        "{}-{}-{}",
        deleted_at,
        std::process::id(),
        ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let entry = trash_directory(backups).join(&id);
    fs::create_dir_all(&entry).context("Unable to create the trash entry")?;

    let record = TrashEntry {
        path: relative.replace('\\', "/").trim_matches('/').to_string(),
        deleted_at,
    };
    fs::write(entry.join(ENTRY_FILE), serde_json::to_vec_pretty(&record)?)?;

    if let Err(e) = move_path(&source, &entry.join(name)) {
        let _ = fs::remove_dir_all(&entry);
        return Err(e).context(format!("Unable to move {} to the trash", relative));
    }
    Ok(())
}

fn read_entry(entry: &Path) -> Option<(TrashEntry, PathBuf)> {
    let record: TrashEntry =
        serde_json::from_slice(&fs::read(entry.join(ENTRY_FILE)).ok()?).ok()?;
    let name = record.path.rsplit('/').next()?.to_string();
    let item = entry.join(name);
    item.exists().then_some((record, item))
}

/// What's in the trash, most recently deleted first.
pub fn list_trash(backups: &Path) -> Vec<SampleTrashItem> {
    let Ok(entries) = fs::read_dir(trash_directory(backups)) else {
        return vec![];
    };
    let mut items: Vec<SampleTrashItem> = entries
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().to_string();
            let (record, item) = read_entry(&entry.path())?;
            let folder = item.is_dir();
            let files = if folder {
                recursive_file_list(item, SAMPLE_EXTENSIONS.to_vec()).len()
            } else {
                1
            };
            Some(SampleTrashItem {
                id,
                path: record.path,
                deleted_at: record.deleted_at,
                folder,
                files,
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
    let path = trash_directory(backups).join(id);
    if !path.is_dir() {
        bail!("This item is no longer in the trash");
    }
    Ok(path)
}

/// Puts a trashed item back where it was (its parent folders are recreated if needed).
pub fn restore_trash_item(samples: &Path, backups: &Path, id: &str) -> Result<String> {
    let entry = entry_path(backups, id)?;
    let Some((record, item)) = read_entry(&entry) else {
        bail!("This trash item is damaged");
    };

    let target = sample_path(samples, &record.path)?;
    if target.exists() {
        bail!("{} already exists, rename or move it first", record.path);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    move_path(&item, &target).context(format!("Unable to restore {}", record.path))?;
    let _ = fs::remove_dir_all(&entry);
    Ok(record.path)
}

/// Deletes an item from the trash for good, or everything when `id` is None.
pub fn empty_trash(backups: &Path, id: Option<&str>) -> Result<()> {
    match id {
        Some(id) => fs::remove_dir_all(entry_path(backups, id)?)?,
        None => {
            let trash = trash_directory(backups);
            if trash.exists() {
                fs::remove_dir_all(&trash)?;
            }
        }
    }
    Ok(())
}

/// Removes items that have been in the trash for longer than TRASH_RETENTION.
pub fn purge_trash(backups: &Path) {
    let limit = now().saturating_sub(TRASH_RETENTION.as_secs());
    for item in list_trash(backups) {
        if item.deleted_at < limit
            && let Err(e) = empty_trash(backups, Some(&item.id))
        {
            warn!("Unable to purge {} from the sample trash: {}", item.path, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_restore_and_empty() {
        let root = std::env::temp_dir().join(format!("goxlr-test-trash-{}", std::process::id()));
        let samples = root.join("samples");
        let backups = root.join("backups");
        fs::create_dir_all(samples.join("Jingles")).unwrap();
        fs::write(samples.join("intro.wav"), b"intro").unwrap();
        fs::write(samples.join("Jingles/a.wav"), b"a").unwrap();
        fs::write(samples.join("Jingles/b.mp3"), b"b").unwrap();

        trash_sample_path(&samples, &backups, "intro.wav").unwrap();
        trash_sample_path(&samples, &backups, "Jingles").unwrap();
        assert!(!samples.join("intro.wav").exists());
        assert!(!samples.join("Jingles").exists());
        assert!(trash_sample_path(&samples, &backups, "missing.wav").is_err());

        let items = list_trash(&backups);
        assert_eq!(items.len(), 2);
        let folder = items.iter().find(|item| item.folder).unwrap();
        assert_eq!((folder.path.as_str(), folder.files), ("Jingles", 2));

        // Restoring puts it back, but never over an existing file
        let file = items.iter().find(|item| !item.folder).unwrap();
        fs::write(samples.join("intro.wav"), b"new").unwrap();
        assert!(restore_trash_item(&samples, &backups, &file.id).is_err());
        fs::remove_file(samples.join("intro.wav")).unwrap();
        assert_eq!(
            restore_trash_item(&samples, &backups, &file.id).unwrap(),
            "intro.wav"
        );
        assert_eq!(fs::read(samples.join("intro.wav")).unwrap(), b"intro");

        // Invalid ids can't point elsewhere
        assert!(restore_trash_item(&samples, &backups, "..").is_err());
        assert!(empty_trash(&backups, Some("../samples")).is_err());

        empty_trash(&backups, Some(&folder.id)).unwrap();
        assert!(list_trash(&backups).is_empty());
        empty_trash(&backups, None).unwrap();

        fs::remove_dir_all(&root).unwrap();
    }
}
