//! Dated copies of a profile, taken each time it's saved over, so an older version can be found
//! again in `backups/history/` (the backup copy next to it only holds the last loaded version).

use anyhow::Result;
use log::warn;
use std::fs;
use std::path::{Path, PathBuf};

// Copies kept for each profile, the oldest go first
const HISTORY_SIZE: usize = 20;

pub fn history_directory(backups: &Path) -> PathBuf {
    backups.join("history")
}

/// Keeps a dated copy of `file` (about to be overwritten) in the history. Nothing is kept when
/// the file doesn't exist yet, or is the same as the newest copy.
pub fn keep_history(file: &Path, backups: &Path) {
    if let Err(e) = try_keep_history(file, backups) {
        warn!("Unable to keep a dated copy of {}: {}", file.display(), e);
    }
}

fn try_keep_history(file: &Path, backups: &Path) -> Result<()> {
    let (Some(stem), Some(extension)) = (file.file_stem(), file.extension()) else {
        return Ok(());
    };
    let Ok(contents) = fs::read(file) else {
        return Ok(());
    };

    // One folder per file (the extension tells profiles and mic profiles of the same name apart)
    let directory = history_directory(backups).join(format!(
        "{}.{}",
        stem.to_string_lossy(),
        extension.to_string_lossy()
    ));
    fs::create_dir_all(&directory)?;

    let mut copies = copies(&directory);
    if let Some(newest) = copies.last()
        && fs::read(newest).is_ok_and(|newest| newest == contents)
    {
        return Ok(());
    }

    // Dated, with a counter for saves in the same second, so the names sort by age
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let extension = extension.to_string_lossy();
    // (after the newest one, the older ones of that second may have been pruned already)
    let stamp = stamp.to_string();
    let mut index = copies
        .last()
        .and_then(|newest| newest.file_stem())
        .and_then(|name| {
            name.to_string_lossy()
                .strip_prefix(&format!("{stamp}-"))?
                .parse()
                .ok()
        })
        .map_or(0, |newest: u32| newest + 1);
    let mut target = directory.join(format!("{stamp}-{index:03}.{extension}"));
    while target.exists() {
        index += 1;
        target = directory.join(format!("{stamp}-{index:03}.{extension}"));
    }
    fs::write(&target, contents)?;
    copies.push(target);

    while copies.len() > HISTORY_SIZE {
        let oldest = copies.remove(0);
        fs::remove_file(&oldest)?;
    }
    Ok(())
}

/// The copies in a history folder, oldest first (the names start with the date).
fn copies(directory: &Path) -> Vec<PathBuf> {
    let mut copies: Vec<PathBuf> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    copies.sort();
    copies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_dated_copies_of_changed_profiles() {
        let root = tempfile::tempdir().unwrap();
        let backups = root.path().join("backups");
        let profile = root.path().join("Stream.goxlr");
        let history = history_directory(&backups).join("Stream.goxlr");

        // Nothing to keep before the first save
        keep_history(&profile, &backups);
        assert!(!history.exists());

        fs::write(&profile, b"first").unwrap();
        keep_history(&profile, &backups);
        // Saved again without a change: no new copy
        keep_history(&profile, &backups);
        assert_eq!(copies(&history).len(), 1);

        for version in 0..HISTORY_SIZE + 5 {
            fs::write(&profile, format!("version {version}")).unwrap();
            keep_history(&profile, &backups);
        }
        let kept = copies(&history);
        assert_eq!(kept.len(), HISTORY_SIZE);
        assert_eq!(
            fs::read(kept.last().unwrap()).unwrap(),
            format!("version {}", HISTORY_SIZE + 4).as_bytes()
        );
    }
}
