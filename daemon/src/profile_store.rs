//! Loading a profile or mic profile with a fallback on its backup copy, written once for both
//! (it used to be copied four times, and one copy went wrong).

use crate::mic_profile::MicProfileAdapter;
use crate::profile::ProfileAdapter;
use anyhow::{Result, anyhow};
use log::{debug, info, warn};
use std::path::Path;

pub(crate) trait StoredProfile: Sized {
    const KIND: &'static str;
    fn load(name: String, directory: &Path) -> Result<Self>;
    fn store(&mut self, directory: &Path) -> Result<()>;
}

impl StoredProfile for ProfileAdapter {
    const KIND: &'static str = "profile";
    fn load(name: String, directory: &Path) -> Result<Self> {
        ProfileAdapter::from_named(name, directory)
    }
    fn store(&mut self, directory: &Path) -> Result<()> {
        self.save(directory, true)
    }
}

impl StoredProfile for MicProfileAdapter {
    const KIND: &'static str = "mic profile";
    fn load(name: String, directory: &Path) -> Result<Self> {
        MicProfileAdapter::from_named(name, directory)
    }
    fn store(&mut self, directory: &Path) -> Result<()> {
        self.save(directory, true)
    }
}

/// Loads a profile. With `use_backup`, a good load refreshes the backup copy, and a failed one
/// falls back on that copy (then puts it back in place of the damaged or missing file). Without
/// it (a temporary load, at shutdown for example) the backups aren't touched.
pub(crate) fn load_with_backup<T: StoredProfile>(
    name: &str,
    directory: &Path,
    backups: &Path,
    use_backup: bool,
) -> Result<T> {
    match T::load(name.to_string(), directory) {
        Ok(mut profile) => {
            if use_backup {
                debug!("The {} {} loaded, refreshing its backup", T::KIND, name);
                if let Err(e) = profile.store(backups) {
                    warn!("Unable to back up the {} {}: {}", T::KIND, name, e);
                }
            }
            Ok(profile)
        }
        Err(e) if !use_backup => Err(e),
        Err(e) => {
            warn!(
                "Failed to load the {} {}: {}, trying its backup",
                T::KIND,
                name,
                e
            );
            let mut profile = T::load(name.to_string(), backups).map_err(|e| {
                anyhow!(
                    "Failed to load the backup of the {} {}: {}",
                    T::KIND,
                    name,
                    e
                )
            })?;
            info!(
                "Loaded the backup of the {} {}, putting it back",
                T::KIND,
                name
            );
            if let Err(e) = profile.store(directory) {
                warn!("Unable to replace the {} {}: {}", T::KIND, name, e);
            }
            Ok(profile)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_on_the_backup_and_puts_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let (profiles, backups) = (dir.path().join("profiles"), dir.path().join("backups"));
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::create_dir_all(&backups).unwrap();
        ProfileAdapter::default()
            .save_as("Stream".into(), &profiles, false)
            .unwrap();

        // A good load refreshes the backup
        let _: ProfileAdapter = load_with_backup("Stream", &profiles, &backups, true).unwrap();
        assert!(backups.join("Stream.goxlr").is_file());

        // The file goes missing: the backup is used and put back
        std::fs::remove_file(profiles.join("Stream.goxlr")).unwrap();
        let _: ProfileAdapter = load_with_backup("Stream", &profiles, &backups, true).unwrap();
        assert!(profiles.join("Stream.goxlr").is_file());

        // A temporary load doesn't look at the backups
        std::fs::remove_file(profiles.join("Stream.goxlr")).unwrap();
        assert!(load_with_backup::<ProfileAdapter>("Stream", &profiles, &backups, false).is_err());
        assert!(!profiles.join("Stream.goxlr").exists());
    }
}
