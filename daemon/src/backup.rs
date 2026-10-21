//! A whole configuration as one .zip (profiles, mic profiles, presets, icons, and optionally the
//! samples), to move to another computer or keep aside, and restoring from one.

use crate::files::FilePaths;
use anyhow::{Context, Result, bail};
use log::warn;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// A restore never writes more than this, or more files than this.
const MAX_RESTORE_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
const MAX_RESTORE_FILES: usize = 20_000;

/// The folders in the archive, and where they are on disk.
fn sections(paths: &FilePaths, samples: bool) -> Vec<(&'static str, PathBuf)> {
    let mut sections = vec![
        ("profiles", paths.profiles.clone()),
        ("mic-profiles", paths.mic_profiles.clone()),
        ("presets", paths.presets.clone()),
        ("icons", paths.icons.clone()),
    ];
    if samples {
        sections.push(("samples", paths.samples.clone()));
    }
    sections
}

/// Writes the archive. Audio is already compressed, so samples are stored as they are.
pub fn write_backup<W: Write + Seek>(paths: &FilePaths, samples: bool, sink: W) -> Result<()> {
    let mut archive = ZipWriter::new(sink);
    for (name, directory) in sections(paths, samples) {
        let method = if name == "samples" {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .large_file(name == "samples");
        add_folder(&mut archive, &directory, &directory, name, options)?;
    }
    archive.finish()?;
    Ok(())
}

fn add_folder<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    base: &Path,
    directory: &Path,
    prefix: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        // Hidden files are temporary ones (imports, renames), links aren't followed.
        if entry.file_name().to_string_lossy().starts_with('.') || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            add_folder(archive, base, &path, prefix, options)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(base)?;
            let name = format!("{prefix}/{}", relative.to_string_lossy().replace('\\', "/"));
            archive.start_file(name, options)?;
            let mut file = File::open(&path)?;
            std::io::copy(&mut file, archive)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct RestoreSummary {
    pub added: usize,
    pub replaced: usize,
    pub unchanged: usize,
}

/// Restores an archive made by `write_backup`. A file that would be replaced is first copied
/// to `backups/restore-<time>/`, so a restore can be undone by hand. Entries that don't belong
/// to a known folder, or point outside of it, are refused.
pub fn restore_backup<R: Read + Seek>(paths: &FilePaths, source: R) -> Result<RestoreSummary> {
    let mut archive = ZipArchive::new(source).context("The file isn't a .zip archive")?;
    if archive.len() > MAX_RESTORE_FILES {
        bail!("The archive has too many files");
    }
    let known = sections(paths, true);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let set_aside = paths.backups.join(format!("restore-{stamp}"));

    // Check everything first: an archive that isn't one of ours changes nothing.
    let mut plan = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            bail!(
                "The archive contains an invalid path: {}",
                entry.name().unwrap_or_default()
            );
        };
        let mut parts = name.components();
        let Some(Component::Normal(section)) = parts.next() else {
            bail!(
                "Unexpected file in the archive: {}",
                entry.name().unwrap_or_default()
            );
        };
        let Some((section, directory)) = known
            .iter()
            .find(|(known, _)| *known == section.to_string_lossy())
        else {
            bail!(
                "Unexpected file in the archive: {}",
                entry.name().unwrap_or_default()
            );
        };
        let relative: PathBuf = parts.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        total += entry.size();
        if total > MAX_RESTORE_TOTAL {
            bail!("The archive is too large to restore");
        }
        plan.push((index, *section, directory.join(&relative), relative));
    }

    let mut summary = RestoreSummary::default();
    for (index, section, target, relative) in plan {
        let mut entry = archive.by_index(index)?;
        let mut contents = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut contents)?;

        if target.is_file() {
            if fs::read(&target).is_ok_and(|current| current == contents) {
                summary.unchanged += 1;
                continue;
            }
            let aside = set_aside.join(section).join(&relative);
            if let Some(parent) = aside.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&target, &aside)
                .with_context(|| format!("Unable to keep a copy of {}", target.display()))?;
            summary.replaced += 1;
        } else {
            summary.added += 1;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        goxlr_profile_loader::atomic::atomic_write(&target, |mut file| {
            file.write_all(&contents)?;
            Ok(())
        })
        .with_context(|| format!("Unable to write {}", target.display()))?;
    }
    if summary.replaced == 0
        && set_aside.exists()
        && let Err(e) = fs::remove_dir_all(&set_aside)
    {
        warn!("Unable to remove {}: {}", set_aside.display(), e);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn paths(root: &Path) -> FilePaths {
        let paths = FilePaths {
            profiles: root.join("profiles"),
            mic_profiles: root.join("mic"),
            presets: root.join("presets"),
            icons: root.join("icons"),
            samples: root.join("samples"),
            backups: root.join("backups"),
        };
        for dir in [
            &paths.profiles,
            &paths.mic_profiles,
            &paths.presets,
            &paths.icons,
            &paths.samples,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        paths
    }

    #[test]
    fn backup_and_restore() {
        let source = tempfile::tempdir().unwrap();
        let from = paths(source.path());
        fs::write(from.profiles.join("Stream.goxlr"), b"stream").unwrap();
        fs::write(from.mic_profiles.join("Desk.goxlrMicProfile"), b"desk").unwrap();
        fs::create_dir_all(from.samples.join("Jingles")).unwrap();
        fs::write(from.samples.join("Jingles/intro.wav"), b"intro").unwrap();
        fs::write(from.samples.join(".intro.wav.1-1.import"), b"temp").unwrap();

        let mut without_samples = Cursor::new(Vec::new());
        write_backup(&from, false, &mut without_samples).unwrap();
        let mut zip = Cursor::new(Vec::new());
        write_backup(&from, true, &mut zip).unwrap();
        assert!(zip.get_ref().len() > without_samples.get_ref().len());

        // Into another configuration, where one profile already exists with other content
        let target = tempfile::tempdir().unwrap();
        let to = paths(target.path());
        fs::write(to.profiles.join("Stream.goxlr"), b"older").unwrap();
        zip.set_position(0);
        let summary = restore_backup(&to, &mut zip).unwrap();
        assert_eq!(
            summary,
            RestoreSummary {
                added: 2,
                replaced: 1,
                unchanged: 0
            }
        );
        assert_eq!(
            fs::read(to.profiles.join("Stream.goxlr")).unwrap(),
            b"stream"
        );
        assert_eq!(
            fs::read(to.samples.join("Jingles/intro.wav")).unwrap(),
            b"intro"
        );
        assert!(!to.samples.join(".intro.wav.1-1.import").exists());
        // The replaced file was kept aside
        let aside = fs::read_dir(&to.backups)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(aside.join("profiles/Stream.goxlr")).unwrap(),
            b"older"
        );

        // Restoring again changes nothing
        zip.set_position(0);
        let again = restore_backup(&to, &mut zip).unwrap();
        assert_eq!(
            again,
            RestoreSummary {
                added: 0,
                replaced: 0,
                unchanged: 3
            }
        );
    }

    #[test]
    fn refuses_foreign_archives() {
        let root = tempfile::tempdir().unwrap();
        let to = paths(root.path());
        let mut zip = Cursor::new(Vec::new());
        {
            let mut archive = ZipWriter::new(&mut zip);
            archive
                .start_file("settings.json", SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"{}").unwrap();
            archive.finish().unwrap();
        }
        zip.set_position(0);
        assert!(restore_backup(&to, &mut zip).is_err());
        assert!(restore_backup(&to, Cursor::new(b"not a zip".to_vec())).is_err());
    }
}
