//! A trash for samples: deleting a sample (or a folder) moves it here instead of removing it,
//! so it can be restored. Each deleted item is kept in its own entry folder, next to a small
//! JSON file recording where it came from.

use anyhow::{Context, Result, bail};
use goxlr_ipc::SampleTrashItem;
use goxlr_types::{SampleBank, SampleButtons};
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::files::{SAMPLE_EXTENSIONS, recursive_file_list, sample_path};

/// The folder button recordings are written to, which must stay in place.
pub const RECORDED_FOLDER: &str = "Recorded";

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
    // The sampler tracks that played it in the loaded profiles, put back on restore
    #[serde(default)]
    tracks: Vec<TrashedTrack>,
    // For an original replaced by an edit: where the edited version went (it may have a new
    // name, an MP3 becomes a WAV). Restoring the original undoes the edit.
    #[serde(default)]
    replaced_by: Option<String>,
}

/// What a restore put back.
#[derive(Debug)]
pub struct RestoredSample {
    pub path: String,
    pub tracks: Vec<TrashedTrack>,
    // Undoing an edit that renamed the sample: (edited name, original name), for the profiles
    pub renamed: Option<(String, String)>,
}

/// A sampler track removed from a loaded profile because its file went to the trash.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrashedTrack {
    pub serial: String,
    pub profile: String,
    pub bank: SampleBank,
    pub button: SampleButtons,
    pub index: usize,
    pub track: String,
    pub start: f32,
    pub end: f32,
    pub gain: f64,
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

fn crosses_devices(error: &std::io::Error) -> bool {
    // EXDEV on Unix (18), ERROR_NOT_SAME_DEVICE on Windows (17)
    error.kind() == std::io::ErrorKind::CrossesDevices
        || matches!(error.raw_os_error(), Some(18) if cfg!(unix))
        || matches!(error.raw_os_error(), Some(17) if cfg!(windows))
}

/// Moves a file or folder. A rename is used when possible; across drives (the trash and the
/// samples may be on different ones), folders are moved file by file and each source is only
/// removed once its own copy is done, so a failure part way never loses a file: whatever wasn't
/// moved yet stays where it was. Links are not followed.
pub(crate) fn move_path(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(e) if !crosses_devices(&e) => return Err(e.into()),
        Err(_) => {}
    }
    move_by_copy(from, to)
}

fn move_by_copy(from: &Path, to: &Path) -> Result<()> {
    let kind = fs::symlink_metadata(from)?.file_type();
    if kind.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            move_by_copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::remove_dir(from)?;
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

/// Whether this relative path is the recordings folder, which can't be moved or deleted.
pub fn is_recorded_folder(relative: &str) -> bool {
    relative
        .replace('\\', "/")
        .trim_matches('/')
        .eq_ignore_ascii_case(RECORDED_FOLDER)
}

/// Moves a sample file or folder (relative to `samples`) to the trash, with the sampler tracks
/// that played it (put back if it's restored).
pub fn trash_sample_path(
    samples: &Path,
    backups: &Path,
    relative: &str,
    tracks: Vec<TrashedTrack>,
) -> Result<()> {
    trash_entry(samples, backups, relative, tracks, None)
}

/// Moves an original that an edit is about to replace, noting where the edited version goes.
pub fn trash_replaced_sample(
    samples: &Path,
    backups: &Path,
    relative: &str,
    replaced_by: &str,
) -> Result<()> {
    let replaced_by = replaced_by.replace('\\', "/").trim_matches('/').to_string();
    trash_entry(samples, backups, relative, vec![], Some(replaced_by))
}

fn trash_entry(
    samples: &Path,
    backups: &Path,
    relative: &str,
    tracks: Vec<TrashedTrack>,
    replaced_by: Option<String>,
) -> Result<()> {
    if is_recorded_folder(relative) {
        bail!(
            "The {} folder is where recordings go, it can't be deleted",
            RECORDED_FOLDER
        );
    }
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
        tracks,
        replaced_by,
    };
    fs::write(entry.join(ENTRY_FILE), serde_json::to_vec_pretty(&record)?)?;

    let target = entry.join(name);
    if let Err(e) = move_path(&source, &target) {
        if target.exists() {
            // Part of it is in the trash already: keep that entry (it can be restored), the
            // rest is still in its folder.
            return Err(e).context(format!(
                "{} was only partly moved to the trash, the remaining files are still in place",
                relative
            ));
        }
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

/// Puts a trashed item back where it was (its parent folders are recreated if needed), and
/// returns its path with the sampler tracks to put back.
pub fn restore_trash_item(samples: &Path, backups: &Path, id: &str) -> Result<RestoredSample> {
    let entry = entry_path(backups, id)?;
    let Some((record, item)) = read_entry(&entry) else {
        bail!("This trash item is damaged");
    };

    // Restoring an original that an edit replaced undoes the edit: the edited version goes to
    // the trash (so it can be brought back too), and profiles follow if its name was different.
    // Everything is checked first: a restore that can't happen leaves the edited version alone.
    let edited = record
        .replaced_by
        .as_ref()
        .and_then(|edited| sample_path(samples, edited).ok().map(|path| (edited, path)))
        .filter(|(_, path)| path.is_file());

    let target = sample_path(samples, &record.path)?;
    let target_is_edited = edited.as_ref().is_some_and(|(_, path)| path == &target);
    if target.exists() && !target_is_edited {
        bail!("{} already exists, rename or move it first", record.path);
    }
    // Profiles find a sample by its file name alone, so a name used elsewhere would be ambiguous.
    let mut names = Vec::new();
    if item.is_dir() {
        names.extend(recursive_file_list(item.clone(), SAMPLE_EXTENSIONS.to_vec()).into_values());
    } else if let Some(name) = item.file_name() {
        names.push(name.to_string_lossy().to_string());
    }
    let used: HashSet<String> =
        recursive_file_list(samples.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
            .into_iter()
            // The edited version is about to leave (another file of that name still counts)
            .filter(|(relative, _)| {
                edited
                    .as_ref()
                    .is_none_or(|(_, path)| path != &samples.join(relative))
            })
            .map(|(_, name)| name.to_lowercase())
            .collect();
    let taken: Vec<String> = names
        .into_iter()
        .filter(|name| used.contains(&name.to_lowercase()))
        .collect();
    if !taken.is_empty() {
        bail!(
            "A sample named {} already exists, rename or move it first",
            taken.join(", ")
        );
    }

    let mut renamed = None;
    if let Some((edited, _)) = &edited {
        trash_sample_path(samples, backups, edited, vec![])
            .context("Unable to move the edited version to the trash")?;
        let name_of = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        let (edited_name, original_name) = (name_of(edited), name_of(&record.path));
        if edited_name != original_name {
            renamed = Some((edited_name, original_name));
        }
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    move_path(&item, &target).context(format!("Unable to restore {}", record.path))?;
    let _ = fs::remove_dir_all(&entry);
    Ok(RestoredSample {
        path: record.path,
        tracks: record.tracks,
        renamed,
    })
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
// Past this size, the oldest items go first (each in place edit keeps a full copy)
pub const MAX_TRASH_SIZE: u64 = 5 * 1024 * 1024 * 1024;

// An entry that can't be read (a crash while it was made) is removed after this long
const DAMAGED_ENTRY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn size_of(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| size_of(&entry.path())).sum())
        .unwrap_or(0)
}

/// Removes items kept longer than TRASH_RETENTION, damaged entries, and the oldest items while
/// the trash is larger than MAX_TRASH_SIZE.
pub fn purge_trash(backups: &Path) {
    purge_trash_with(backups, TRASH_RETENTION, MAX_TRASH_SIZE);
}

fn purge_trash_with(backups: &Path, retention: Duration, max_size: u64) {
    // Damaged entries never show in the list, so they'd never be purged otherwise
    if let Ok(entries) = fs::read_dir(trash_directory(backups)) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > DAMAGED_ENTRY_RETENTION);
            if old && read_entry(&entry.path()).is_none() {
                warn!(
                    "Removing a damaged sample trash entry: {}",
                    entry.path().display()
                );
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    let limit = now().saturating_sub(retention.as_secs());
    let mut kept = Vec::new();
    for item in list_trash(backups) {
        if item.deleted_at < limit {
            if let Err(e) = empty_trash(backups, Some(&item.id)) {
                warn!("Unable to purge {} from the sample trash: {}", item.path, e);
            }
        } else {
            let size = size_of(&trash_directory(backups).join(&item.id));
            kept.push((item, size));
        }
    }

    // Oldest first, until the trash fits
    kept.sort_by_key(|(item, _)| item.deleted_at);
    let mut total: u64 = kept.iter().map(|(_, size)| size).sum();
    for (item, size) in kept {
        if total <= max_size {
            break;
        }
        if let Err(e) = empty_trash(backups, Some(&item.id)) {
            warn!("Unable to purge {} from the sample trash: {}", item.path, e);
            continue;
        }
        total = total.saturating_sub(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_restore_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let samples = root.join("samples");
        let backups = root.join("backups");
        fs::create_dir_all(samples.join("Jingles")).unwrap();
        fs::write(samples.join("intro.wav"), b"intro").unwrap();
        fs::write(samples.join("Jingles/a.wav"), b"a").unwrap();
        fs::write(samples.join("Jingles/b.mp3"), b"b").unwrap();

        trash_sample_path(&samples, &backups, "intro.wav", vec![]).unwrap();
        trash_sample_path(&samples, &backups, "Jingles", vec![]).unwrap();
        assert!(!samples.join("intro.wav").exists());
        assert!(!samples.join("Jingles").exists());
        assert!(trash_sample_path(&samples, &backups, "missing.wav", vec![]).is_err());
        fs::create_dir_all(samples.join("Recorded")).unwrap();
        assert!(trash_sample_path(&samples, &backups, "Recorded", vec![]).is_err());
        assert!(trash_sample_path(&samples, &backups, "/recorded/", vec![]).is_err());

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
            restore_trash_item(&samples, &backups, &file.id)
                .unwrap()
                .path,
            "intro.wav"
        );
        assert_eq!(fs::read(samples.join("intro.wav")).unwrap(), b"intro");

        // Invalid ids can't point elsewhere
        assert!(restore_trash_item(&samples, &backups, "..").is_err());
        assert!(empty_trash(&backups, Some("../samples")).is_err());

        // The sampler tracks noted at deletion come back with the file
        let track = TrashedTrack {
            serial: "S1".into(),
            profile: "Stream".into(),
            bank: SampleBank::B,
            button: SampleButtons::TopLeft,
            index: 1,
            track: "intro.wav".into(),
            start: 5.0,
            end: 90.0,
            gain: 0.5,
        };
        trash_sample_path(&samples, &backups, "intro.wav", vec![track]).unwrap();
        let id = list_trash(&backups)
            .into_iter()
            .find(|item| !item.folder)
            .unwrap()
            .id;
        let tracks = restore_trash_item(&samples, &backups, &id).unwrap().tracks;
        assert_eq!(tracks.len(), 1);
        assert_eq!(
            (tracks[0].bank, tracks[0].index, tracks[0].start),
            (SampleBank::B, 1, 5.0)
        );

        empty_trash(&backups, Some(&folder.id)).unwrap();
        assert!(list_trash(&backups).is_empty());
        empty_trash(&backups, None).unwrap();
    }

    #[test]
    fn purge_keeps_the_trash_small() {
        let dir = tempfile::tempdir().unwrap();
        let (samples, backups) = (dir.path().join("samples"), dir.path().join("backups"));
        fs::create_dir_all(&samples).unwrap();
        for name in ["a.wav", "b.wav", "c.wav"] {
            fs::write(samples.join(name), vec![0u8; 1000]).unwrap();
            trash_sample_path(&samples, &backups, name, vec![]).unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
        // A damaged entry, old enough to go
        let damaged = trash_directory(&backups).join("damaged");
        fs::create_dir_all(&damaged).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        // A folder can't be opened as a file on Windows: there it just isn't aged
        if let Ok(folder) = fs::File::open(&damaged) {
            folder.set_modified(old).ok();
        }

        // At most about two items' worth: the oldest goes
        purge_trash_with(&backups, TRASH_RETENTION, 2500);
        let left: Vec<String> = list_trash(&backups)
            .into_iter()
            .map(|item| item.path)
            .collect();
        assert_eq!(left, vec!["c.wav".to_string(), "b.wav".to_string()]);
        if damaged.exists() {
            // set_modified isn't available on every file system; the entry is at least listed out
            assert!(read_entry(&damaged).is_none());
        }

        // Nothing kept past the retention
        purge_trash_with(&backups, Duration::ZERO, MAX_TRASH_SIZE);
        std::thread::sleep(Duration::from_millis(1100));
        purge_trash_with(&backups, Duration::ZERO, MAX_TRASH_SIZE);
        assert!(list_trash(&backups).is_empty());
    }

    #[test]
    fn copies_across_drives_without_losing_files() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        fs::create_dir_all(from.join("Sub")).unwrap();
        fs::write(from.join("a.wav"), b"a").unwrap();
        fs::write(from.join("Sub/b.wav"), b"b").unwrap();

        move_by_copy(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(to.join("a.wav")).unwrap(), b"a");
        assert_eq!(fs::read(to.join("Sub/b.wav")).unwrap(), b"b");

        // A failure part way leaves what wasn't moved where it was
        let (from, to) = (dir.path().join("from2"), dir.path().join("to2"));
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("a.wav"), b"a").unwrap();
        fs::create_dir_all(to.join("a.wav")).unwrap(); // the copy of a.wav can't be written
        assert!(move_by_copy(&from, &to).is_err());
        assert_eq!(fs::read(from.join("a.wav")).unwrap(), b"a");
    }
}
