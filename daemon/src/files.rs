/*
This is simply a struct that manages and returns a list of files in various directories.

This will trigger mpsc events whenever a

This has been created as a separate mod primarily because profile.rs is big enough, and
secondly because it's managing different types of files
 */

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::fs::{File, create_dir_all};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
// use futures::channel::mpsc::{channel, Receiver};
// use futures::{SinkExt, StreamExt};
use log::{debug, info, warn};

use crate::sample_trash::{list_trash, restore_trash_item, trash_replaced_sample};
use goxlr_audio::editor::{DecodedAudio, MAX_EDIT_SECONDS, probe_duration};
use goxlr_ipc::PathTypes;
use goxlr_ipc::SampleEdit;
use goxlr_profile_loader::atomic::atomic_write;
use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::io::BufWriter;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::{SettingsHandle, Shutdown};

// This should probably be handled with an EnumSet..
#[derive(Debug, Clone)]
pub struct FilePaths {
    pub profiles: PathBuf,
    pub mic_profiles: PathBuf,
    pub presets: PathBuf,
    pub icons: PathBuf,
    pub samples: PathBuf,
    pub backups: PathBuf,
}

#[derive(Debug)]
pub struct FileManager {
    paths: FilePaths,
}

impl FileManager {
    pub async fn new(settings: &SettingsHandle) -> Self {
        let paths = FileManager::get_file_paths_from_settings(settings).await;
        FileManager::create_paths(&paths);

        Self { paths }
    }

    pub async fn get_file_paths_from_settings(settings: &SettingsHandle) -> FilePaths {
        FilePaths {
            profiles: settings.get_profile_directory().await,
            mic_profiles: settings.get_mic_profile_directory().await,
            presets: settings.get_presets_directory().await,
            icons: settings.get_icons_directory().await,
            samples: settings.get_samples_directory().await,
            backups: settings.get_backup_directory().await,
        }
    }

    pub fn create_paths(paths: &FilePaths) {
        if !paths.profiles.exists() {
            if let Err(e) = create_path(&paths.profiles) {
                warn!("Unable to Create Path: {:?}, {}", paths.profiles, e);
            } else if let Err(e) = extract_defaults(PathTypes::Profiles, &paths.profiles) {
                warn!("Unable to Extract Default Profiles: {}", e);
            }
        }

        // Microphone Path..
        if !&paths.mic_profiles.exists() {
            if let Err(e) = create_path(&paths.mic_profiles) {
                warn!("Unable to Create Path: {:?}, {}", paths.mic_profiles, e);
            } else if let Err(e) = extract_defaults(PathTypes::MicProfiles, &paths.mic_profiles) {
                warn!("Unable to Extract Default Mic Profiles {}", e);
            }
        }

        // Presets Path..
        if !&paths.presets.exists() {
            if let Err(e) = create_path(&paths.presets) {
                warn!("Unable to Create Path: {:?}, {}", paths.presets, e);
            } else if let Err(e) = extract_defaults(PathTypes::Presets, &paths.presets) {
                warn!("Unable to Extract Default Presets: {}", e);
            }
        }

        // Icons..
        if !&paths.icons.exists() {
            if let Err(e) = create_path(&paths.icons) {
                warn!("Unable to Create Path: {:?}, {}", paths.icons, e);
            } else if let Err(e) = extract_defaults(PathTypes::Icons, &paths.icons) {
                warn!("Unable to Extract Default Icons: {}", e);
            }
        }

        // This will create the Samples and Samples/Recorded directories
        let recorded_path = &paths.samples.join("Recorded");
        if !recorded_path.exists()
            && let Err(e) = create_path(recorded_path)
        {
            warn!("Unable to Create Path: {:?}, {}", recorded_path, e);
        }

        if !&paths.backups.exists()
            && let Err(e) = create_path(&paths.backups)
        {
            warn!("Unable to Create Path: {:?}, {}", paths.backups, e);
        }
    }

    pub fn get_profiles(&mut self) -> Vec<String> {
        let path = self.paths.profiles.clone();
        let extension = ["goxlr"].to_vec();
        self.get_files_from_path(path, extension, false)
    }

    pub fn get_mic_profiles(&mut self) -> Vec<String> {
        let path = self.paths.mic_profiles.clone();
        let extension = ["goxlrMicProfile"].to_vec();

        self.get_files_from_path(path, extension, false)
    }

    pub fn get_presets(&mut self) -> Vec<String> {
        let path = self.paths.presets.clone();
        let extension = ["preset"].to_vec();

        self.get_files_from_path(path, extension, false)
    }

    pub fn get_samples(&mut self) -> BTreeMap<String, String> {
        let base_path = self.paths.samples.clone();
        let extensions = SAMPLE_EXTENSIONS.to_vec();

        self.get_recursive_file_list(base_path, extensions)
    }

    /// Every folder below the samples directory (relative paths), empty ones included.
    pub fn get_sample_folders(&mut self) -> Vec<String> {
        let base = self.paths.samples.clone();
        let mut folders = Vec::new();
        collect_folders(&base, &base, &mut folders);
        folders.sort();
        folders
    }

    pub fn get_icons(&mut self) -> Vec<String> {
        let path = self.paths.icons.clone();
        let extension = ["gif", "jpg", "png"].to_vec();

        self.get_files_from_path(path, extension, true)
    }

    fn get_recursive_file_list(
        &self,
        path: PathBuf,
        extensions: Vec<&str>,
    ) -> BTreeMap<String, String> {
        recursive_file_list(path, extensions)
    }

    fn get_files_from_path(
        &self,
        path: PathBuf,
        extensions: Vec<&str>,
        with_extension: bool,
    ) -> Vec<String> {
        let mut result = self.get_files_from_drive(path, extensions.clone(), with_extension);
        result.sort_by_key(|a| a.to_lowercase());
        result.dedup();
        result
    }

    fn get_files_from_drive(
        &self,
        path: PathBuf,
        extensions: Vec<&str>,
        with_extension: bool,
    ) -> Vec<String> {
        if let Ok(list) = path.read_dir() {
            return list
                .filter_map(|entry| {
                    entry
                        .ok()
                        // Make sure this has an extension..
                        .filter(|e| e.path().extension().is_some())
                        // Is it the extension we're looking for?
                        .filter(|e| {
                            let path = e.path();
                            let os_ext = path.extension().unwrap();
                            for extension in extensions.clone() {
                                if extension == os_ext {
                                    return true;
                                }
                            }
                            false
                        })
                        // Get the File Name..
                        .and_then(|e| {
                            if with_extension {
                                e.path()
                                    .file_name()
                                    .and_then(|n| n.to_str().map(String::from))
                            } else {
                                e.path().file_stem().and_then(
                                    // Convert it to a String..
                                    |n| n.to_str().map(String::from),
                                )
                            }
                        })
                    // Collect the result.
                })
                .collect::<Vec<String>>();
        }

        Vec::new()
    }
    pub fn paths(&self) -> &FilePaths {
        &self.paths
    }
}

// How long the file watcher waits for things to settle before reporting a change.
const WATCHER_RETRY: std::time::Duration = std::time::Duration::from_secs(60);
const WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

pub async fn spawn_file_notification_service(
    paths: FilePaths,
    sender: Sender<PathTypes>,
    mut shutdown_signal: Shutdown,
) -> Result<()> {
    // Without a watcher the lists aren't refreshed by themselves, but the GoXLR keeps working:
    // the daemon carries on and the watcher is tried again (the system may be out of inotify
    // instances for a while).
    let (mut watcher, mut rx) = loop {
        match create_watcher() {
            Ok(created) => break created,
            Err(error) => {
                warn!(
                    "Unable to create the file watcher, retrying in a minute: {:?}",
                    error
                );
                tokio::select! {
                    () = tokio::time::sleep(WATCHER_RETRY) => {}
                    () = shutdown_signal.recv() => return Ok(()),
                }
            }
        }
    };

    // Add the Paths to the Watcher..
    if let Err(error) = watcher.watch(&paths.profiles, RecursiveMode::NonRecursive) {
        warn!("Unable to Monitor Profiles Path: {:?}", error);
    }
    if let Err(error) = watcher.watch(&paths.mic_profiles, RecursiveMode::NonRecursive) {
        warn!("Unable to Monitor the Microphone Profile Path {:?}", error);
    }
    if let Err(error) = watcher.watch(&paths.presets, RecursiveMode::NonRecursive) {
        warn!("Unable to Monitor the Presets Path: {:?}", error)
    }
    if let Err(error) = watcher.watch(&paths.icons, RecursiveMode::NonRecursive) {
        warn!("Unable to monitor the Icons Path: {:?}", error);
    }
    if let Err(error) = watcher.watch(&paths.samples, RecursiveMode::Recursive) {
        warn!("Unable to Monitor the Samples Path: {:?}", error);
    }

    // Changes come in bursts (an import, a folder moved, a file written then renamed), so they're
    // gathered and sent once nothing changed for a short while, rather than once per event.
    let mut pending: Vec<PathTypes> = Vec::new();
    let mut deadline = tokio::time::Instant::now();

    // Wait for any changes..
    loop {
        tokio::select! {
            () = shutdown_signal.recv() => {
                debug!("Shutdown Signal Received.");
                break;
            },
            () = tokio::time::sleep_until(deadline), if !pending.is_empty() => {
                for path_type in pending.drain(..) {
                    let _ = sender.send(path_type).await;
                }
            },
            result = rx.recv() => {
                if let Some(result) = result {
                    match result {
                        Ok(event) => {
                            match event.kind {
                                // Triggered on the Creation or Removal of a file / folder,
                                // whatever the backend calls it (some report 'Other')..
                                EventKind::Create(_) |
                                EventKind::Remove(_) |

                                // Triggered on Rename / Move of a file (FSEvents reports these
                                // as RenameMode::Any)
                                EventKind::Modify(ModifyKind::Name(_)) |

                                // A file written in place: only samples, whose size and length
                                // are listed (a recording is created empty, then written until
                                // it stops; the list is read again once the writes settle)
                                EventKind::Modify(ModifyKind::Data(_)) |
                                EventKind::Modify(ModifyKind::Any) |
                                EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                                    let written = !matches!(
                                        event.kind,
                                        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
                                    );

                                    for path in &event.paths {
                                        if written && !path.starts_with(&paths.samples) {
                                            continue;
                                        }
                                        // Hidden temporary files (.name.tmp, .name.import) come
                                        // and go during writes, the final rename is what counts.
                                        let hidden = path
                                            .file_name()
                                            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                                        if hidden {
                                            continue;
                                        }
                                        let path_type = if path.starts_with(&paths.profiles) {
                                            PathTypes::Profiles
                                        } else if path.starts_with(&paths.mic_profiles) {
                                            PathTypes::MicProfiles
                                        } else if path.starts_with(&paths.icons) {
                                            PathTypes::Icons
                                        } else if path.starts_with(&paths.presets) {
                                            PathTypes::Presets
                                        } else if path.starts_with(&paths.samples) {
                                            PathTypes::Samples
                                        } else {
                                            continue;
                                        };
                                        if !pending.contains(&path_type) {
                                            pending.push(path_type);
                                        }
                                        deadline = tokio::time::Instant::now() + WATCH_DEBOUNCE;
                                    }
                                },

                                _ => {
                                    // Do nothing, not our kind of event!
                                }
                            }
                        },
                        Err(error) => {
                            warn!("Error Reading File Event: {:?}", error);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn create_watcher() -> notify::Result<(RecommendedWatcher, Receiver<notify::Result<Event>>)> {
    let (tx, rx) = mpsc::channel(1);

    let watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.blocking_send(res);
        },
        Config::default(),
    )?;

    Ok((watcher, rx))
}

/// Every file below a directory, without following links (a link to a parent folder would loop,
/// and one leading elsewhere would take files from outside it).
fn walk_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_files(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

/// Lists files with the given extensions (any case) below a path, keyed by their path relative
/// to it.
pub(crate) fn recursive_file_list(
    path: PathBuf,
    extensions: Vec<&str>,
) -> BTreeMap<String, String> {
    let mut paths = Vec::new();
    walk_files(&path, &mut paths);

    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for file_path in paths {
        let wanted = file_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.iter().any(|e| e.eq_ignore_ascii_case(extension)));
        if !wanted {
            continue;
        }
        if let (Ok(relative), Some(name)) = (file_path.strip_prefix(&path), file_path.file_name()) {
            map.insert(
                relative.to_string_lossy().to_string(),
                name.to_string_lossy().to_string(),
            );
        }
    }
    map
}

pub const SAMPLE_EXTENSIONS: [&str; 4] = ["wav", "mp3", "ogg", "flac"];

fn collect_folders(base: &Path, directory: &Path, folders: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        // Links aren't followed: one pointing to a parent folder would loop.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if let Ok(relative) = path.strip_prefix(base) {
            folders.push(relative.to_string_lossy().to_string());
        }
        collect_folders(base, &path, folders);
    }
}

const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Names Windows can't store: reserved device names (with any extension), names ending with a
/// dot or a space (silently dropped), and characters it refuses.
fn invalid_on_windows(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    WINDOWS_RESERVED
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
        || name.ends_with(['.', ' '])
        || name.contains(['<', '>', ':', '"', '|', '?', '*'])
}

/// Checks one name (a file or a folder, no separators) given by a client. Names that existing
/// files can have are accepted, Windows' rules only apply on Windows.
fn check_name(name: &str, new: bool) -> Result<()> {
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        // Samples and profiles move between systems, so new names follow Windows' rules too.
        || ((new || cfg!(windows)) && invalid_on_windows(name));
    if invalid {
        bail!("Invalid name: {:?}", name);
    }
    Ok(())
}

/// Checks a name for a new file or folder (profile, preset, sample, folder). It's refused when
/// it can't be stored on every system the utility runs on.
pub fn validate_file_name(name: &str) -> Result<&str> {
    check_name(name, true)?;
    Ok(name)
}

/// Turns a path relative to the samples directory (from a client, '/' or '\\' separated) into
/// a full path, refusing anything that could point outside of the samples directory.
pub(crate) fn sample_path(base: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = base.to_path_buf();
    let mut parts = 0;
    for part in relative.split(['/', '\\']).filter(|part| !part.is_empty()) {
        check_name(part, false).map_err(|_| anyhow!("Invalid path: {:?}", relative))?;
        path.push(part);
        parts += 1;
    }
    if parts == 0 {
        bail!("Invalid path: {:?}", relative);
    }
    if !resolves_inside(base, &path) {
        bail!("{:?} leads outside of the samples folder", relative);
    }
    Ok(path)
}

/// Whether a path, once links are resolved, is still inside `base`. The part that doesn't exist
/// yet (a file about to be created) is checked through its closest existing parent.
fn resolves_inside(base: &Path, path: &Path) -> bool {
    let Ok(base) = base.canonicalize() else {
        // No samples folder yet: nothing below it can be a link
        return true;
    };
    let mut existing = path;
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return resolved.starts_with(&base);
        }
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return false,
        }
    }
}

/// Like `sample_path`, for a path that's about to be created: its last part must be a valid
/// new name.
fn new_sample_path(base: &Path, relative: &str) -> Result<PathBuf> {
    let path = sample_path(base, relative)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    validate_file_name(&name).map_err(|_| anyhow!("Invalid name: {:?}", name))?;
    Ok(path)
}

/// Whether a file or folder with exactly this name exists. `Path::exists` also matches other
/// cases of the name on Windows and macOS, where `Intro.wav` and `intro.wav` are the same file.
fn exists_exactly(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    fs::read_dir(parent).is_ok_and(|entries| entries.flatten().any(|e| e.file_name() == name))
}

/// Whether `target` only differs from `source` by the case of its name, and is the same file
/// on this system (a case insensitive one).
fn is_case_change(source: &Path, target: &Path) -> bool {
    let (Some(a), Some(b)) = (source.file_name(), target.file_name()) else {
        return false;
    };
    a != b
        && source.parent() == target.parent()
        && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        && target.exists()
        && !exists_exactly(target)
}

/// Whether another sample (anywhere in the samples folder) already uses this name. Names are
/// compared without case: on Windows and macOS two such files are the same one, and profiles
/// moved there from Linux would play either. `except` is the file being renamed.
fn sample_name_used(base: &Path, name: &str, except: Option<&Path>) -> bool {
    let name = name.to_lowercase();
    recursive_file_list(base.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
        .iter()
        .any(|(relative, used)| {
            used.to_lowercase() == name && except != Some(base.join(relative).as_path())
        })
}

fn is_sample_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SAMPLE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Creates a folder in the samples directory, its parent folder must already exist.
pub fn create_sample_folder(base: &Path, relative: &str) -> Result<()> {
    let path = new_sample_path(base, relative)?;
    if path.exists() {
        bail!("{} already exists", relative);
    }
    if !path.parent().is_some_and(|parent| parent.is_dir()) {
        bail!("The parent folder of {} doesn't exist", relative);
    }
    fs::create_dir(&path).context(format!("Unable to create {}", relative))?;
    Ok(())
}

/// A sample file that changed name, profiles referencing it need updating.
pub struct RenamedSample {
    pub old_name: String,
    pub new_name: String,
}

/// Moves or renames a sample file or folder inside the samples directory. Profiles only store
/// a sample's file name (it's then searched for in every folder), so a file keeps an audio
/// extension, and a new file name must not already be used by another sample.
pub fn move_sample_path(base: &Path, from: &str, to: &str) -> Result<Option<RenamedSample>> {
    if crate::sample_trash::is_recorded_folder(from) {
        bail!("The Recorded folder is where recordings go, it can't be moved or renamed");
    }
    let source = sample_path(base, from)?;
    let target = new_sample_path(base, to)?;

    if !source.exists() {
        bail!("{} doesn't exist", from);
    }
    let case_change = is_case_change(&source, &target);
    if target.exists() && !case_change {
        bail!("{} already exists", to);
    }
    if !target.parent().is_some_and(|parent| parent.is_dir()) {
        bail!("The destination folder of {} doesn't exist", to);
    }

    let mut renamed = None;
    if source.is_dir() {
        if target.starts_with(&source) {
            bail!("A folder can't be moved inside itself");
        }
    } else {
        if !is_sample_file(&target) {
            bail!(
                "{} must keep an audio extension ({})",
                to,
                SAMPLE_EXTENSIONS.join(", ")
            );
        }
        let old_name = source.file_name().map(|n| n.to_string_lossy().to_string());
        let new_name = target.file_name().map(|n| n.to_string_lossy().to_string());
        if let (Some(old_name), Some(new_name)) = (old_name, new_name)
            && old_name != new_name
        {
            if sample_name_used(base, &new_name, Some(&source)) {
                bail!("A sample named {} already exists", new_name);
            }
            // Profiles only know the name: with two files of that name (copied by hand), the
            // profiles playing the other one would be renamed too.
            if sample_name_used(base, &old_name, Some(&source)) {
                bail!(
                    "Another sample is also named {}, rename or move it first so the profiles stay right",
                    old_name
                );
            }
            renamed = Some(RenamedSample { old_name, new_name });
        }
    }

    let moved = if case_change {
        rename_case(&source, &target)
    } else {
        fs::rename(&source, &target).map_err(Into::into)
    };
    moved.context(format!("Unable to move {} to {}", from, to))?;
    Ok(renamed)
}

/// Changes only the case of a name. Some systems ignore such a rename, so it goes through a
/// temporary name.
fn rename_case(source: &Path, target: &Path) -> Result<()> {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    let temp = source.with_file_name(format!(".{}.{}.rename", name, std::process::id()));
    fs::rename(source, &temp)?;
    if let Err(e) = fs::rename(&temp, target) {
        let _ = fs::rename(&temp, source);
        return Err(e.into());
    }
    Ok(())
}

/// The names of the sample files at a path: the file itself, or every sample inside a folder.
/// Removes the hidden '.<name>.<id>.import' files an interrupted import leaves behind. Links
/// aren't followed.
pub fn remove_stale_imports(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            remove_stale_imports(&path);
        } else if kind.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                && name.ends_with(".import")
                && let Err(e) = fs::remove_file(&path)
            {
                warn!(
                    "Unable to remove the leftover import {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}

pub fn sample_names_at(base: &Path, relative: &str) -> Result<Vec<String>> {
    let path = sample_path(base, relative)?;
    if path.is_dir() {
        return Ok(recursive_file_list(path, SAMPLE_EXTENSIONS.to_vec())
            .into_values()
            .collect());
    }
    if path.is_file()
        && let Some(name) = path.file_name()
    {
        return Ok(vec![name.to_string_lossy().to_string()]);
    }
    bail!("{} doesn't exist", relative)
}

/// Edits a sample (removes parts, normalizes) and saves the result as WAV, either over the
/// original file or as a new file named `save_as` next to it. When replacing, the original is
/// moved to the trash in `backups` first, so it can be restored. Replacing a file that isn't a
/// WAV gives it a .wav name, which is returned so profiles can follow it.
pub fn edit_sample_file(
    base: &Path,
    backups: &Path,
    relative: &str,
    edit: &SampleEdit,
    save_as: Option<&str>,
) -> Result<Option<RenamedSample>> {
    let source = sample_path(base, relative)?;
    if !source.is_file() || !is_sample_file(&source) {
        bail!("{} isn't a sample file", relative);
    }
    if edit.remove.is_empty() && !edit.normalize {
        bail!("There is nothing to change in {}", relative);
    }

    let target = match save_as {
        Some(name) => {
            let name = validate_file_name(name.trim())?;
            let file_name = if name.to_ascii_lowercase().ends_with(".wav") {
                name.to_string()
            } else {
                format!("{name}.wav")
            };
            source.with_file_name(file_name)
        }
        None => source.with_extension("wav"),
    };

    let name_of = |path: &Path| path.file_name().map(|n| n.to_string_lossy().to_string());
    let (Some(old_name), Some(new_name)) = (name_of(&source), name_of(&target)) else {
        bail!("Invalid file name");
    };
    // Replacing 'X.WAV' writes 'X.wav', the same file on Windows and macOS.
    let same_file = save_as.is_none() && is_case_change(&source, &target);
    if target != source && !same_file {
        if target.exists() {
            bail!("{} already exists", new_name);
        }
        let except = save_as.is_none().then_some(source.as_path());
        if sample_name_used(base, &new_name, except) {
            bail!("A sample named {} already exists", new_name);
        }
    }

    // Refuse long files before decoding them all into memory.
    if probe_duration(&source).is_some_and(|seconds| seconds > MAX_EDIT_SECONDS as f64) {
        bail!(
            "{} is longer than {} minutes, which is too long to edit",
            relative,
            MAX_EDIT_SECONDS / 60
        );
    }
    let mut audio =
        DecodedAudio::decode(&source).context(format!("Unable to read {}", relative))?;
    audio.remove_ranges(&edit.remove)?;
    if edit.normalize {
        audio.normalize(-1.0);
    }

    // Keep the original in the trash when replacing it, the edit can then be undone.
    let replacing = save_as.is_none();
    if replacing {
        // Noting where the edit goes, so restoring the original undoes it
        let mut edited: Vec<&str> = relative
            .split(['/', '\\'])
            .filter(|p| !p.is_empty())
            .collect();
        edited.pop();
        edited.push(&new_name);
        trash_replaced_sample(base, backups, relative, &edited.join("/"))?;
    }

    let written = atomic_write(&target, |file| audio.write_wav(BufWriter::new(file)));
    if let Err(e) = written {
        if replacing {
            restore_latest_trash_of(base, backups, relative);
        }
        return Err(e).context(format!("Unable to save {}", new_name));
    }

    // Replacing an MP3 / OGG / FLAC wrote a new .wav file, so the sample has a new name.
    if replacing && target != source {
        return Ok(Some(RenamedSample { old_name, new_name }));
    }
    Ok(None)
}

/// Puts back the most recently trashed item that came from `relative` (used when an edit
/// couldn't be saved after its original was moved to the trash).
fn restore_latest_trash_of(base: &Path, backups: &Path, relative: &str) {
    let relative = relative.replace('\\', "/");
    let latest = list_trash(backups)
        .into_iter()
        .find(|item| item.path == relative.trim_matches('/'));
    if let Some(item) = latest
        && let Err(e) = restore_trash_item(base, backups, &item.id)
    {
        warn!("Unable to restore {} from the trash: {}", relative, e);
    }
}

/// A file name that isn't used by any sample yet: the name itself, or 'name (2).ext',
/// 'name (3).ext'... Profiles reference samples by name, so names must be unique.
pub fn free_sample_name(base: &Path, name: &str) -> String {
    let used: std::collections::HashSet<String> =
        recursive_file_list(base.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
            .into_values()
            .map(|name| name.to_lowercase())
            .collect();
    if !used.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|count| format!("{stem} ({count}){extension}"))
        .find(|candidate| !used.contains(&candidate.to_lowercase()))
        .expect("an unused name")
}

/// Copies a sample next to itself under a free name, returns the new file's relative path.
pub fn duplicate_sample(base: &Path, relative: &str) -> Result<String> {
    let source = sample_path(base, relative)?;
    if !source.is_file() || !is_sample_file(&source) {
        bail!("{} isn't a sample file", relative);
    }
    let Some(name) = source.file_name().map(|n| n.to_string_lossy().to_string()) else {
        bail!("Invalid file name");
    };
    let new_name = free_sample_name(base, &name);
    fs::copy(&source, source.with_file_name(&new_name))
        .context(format!("Unable to copy {}", relative))?;

    let mut parts: Vec<&str> = relative
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect();
    parts.pop();
    parts.push(&new_name);
    Ok(parts.join("/"))
}

/// Where an imported sample file goes: `folder` (relative to the samples directory, empty for
/// the top), under a free name. Only audio files are accepted.
pub fn import_sample_target(base: &Path, folder: &str, file_name: &str) -> Result<PathBuf> {
    // Only keep the file name the client sent, never a path. Browsers may send a Windows path
    // ('C:\\fakepath\\new.mp3'), whatever system the daemon runs on.
    let name = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    if validate_file_name(name).is_err() || !is_sample_file(Path::new(name)) {
        bail!(
            "{} isn't a supported audio file ({})",
            file_name,
            SAMPLE_EXTENSIONS.join(", ")
        );
    }

    let directory = if folder.trim_matches(['/', '\\']).is_empty() {
        base.to_path_buf()
    } else {
        sample_path(base, folder)?
    };
    if !directory.is_dir() {
        bail!("The folder {} doesn't exist", folder);
    }
    Ok(directory.join(free_sample_name(base, name)))
}

pub fn find_file_in_path(path: PathBuf, file: PathBuf) -> Option<PathBuf> {
    // The file name comes from profiles or clients, it must stay inside the directory.
    let escapes = file
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir));
    if escapes {
        warn!(
            "Refusing to look up {:?}, it points outside {:?}",
            file, path
        );
        return None;
    }

    // The first file (in a stable order) whose path ends with the requested one.
    let mut files = Vec::new();
    walk_files(&path, &mut files);
    files.sort();
    files.into_iter().find(|candidate| {
        candidate
            .strip_prefix(&path)
            .is_ok_and(|relative| relative.ends_with(&file))
    })
}

pub fn create_path(path: &Path) -> Result<()> {
    if !path.exists() {
        // Attempt to create the profile directory..
        if let Err(e) = create_dir_all(path) {
            Err(e).context(format!("Could not create path {}", path.to_string_lossy()))?;
        } else {
            info!("Created Path: {}", path.to_string_lossy());
        }
    }
    Ok(())
}

/// Builds the path of a named file (profile, preset..) in a directory. Names come from clients,
/// so refuse any that could point outside of that directory.
pub fn named_file_path(directory: &Path, name: &str, extension: &str) -> Result<PathBuf> {
    check_name(name, false)?;
    Ok(directory.join(format!("{name}.{extension}")))
}

/// Like `named_file_path`, for a file about to be created under a new name.
pub fn new_named_file_path(directory: &Path, name: &str, extension: &str) -> Result<PathBuf> {
    validate_file_name(name)?;
    named_file_path(directory, name, extension)
}

/// Renames a named file (profile, mic profile) in its directory. Changing only the case works on
/// case insensitive systems too.
pub fn rename_named_file(directory: &Path, from: &str, to: &str, extension: &str) -> Result<()> {
    let source = named_file_path(directory, from, extension)?;
    let target = new_named_file_path(directory, to, extension)?;
    if !source.is_file() {
        bail!("{} doesn't exist", from);
    }
    if is_case_change(&source, &target) {
        return rename_case(&source, &target);
    }
    if target.exists() {
        bail!("{} already exists", to);
    }
    fs::rename(&source, &target).context(format!("Unable to rename {} to {}", from, to))?;
    Ok(())
}

/// Copies a named file (profile, mic profile) under a new name in the same directory.
pub fn duplicate_named_file(directory: &Path, from: &str, to: &str, extension: &str) -> Result<()> {
    let source = named_file_path(directory, from, extension)?;
    let target = new_named_file_path(directory, to, extension)?;
    if !source.is_file() {
        bail!("{} doesn't exist", from);
    }
    if target.exists() {
        bail!("{} already exists", to);
    }
    fs::copy(&source, &target).context(format!("Unable to copy {} to {}", from, to))?;
    Ok(())
}

pub fn can_create_new_file(path: PathBuf) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        bail!("Parent Directory doesn't exist");
    }

    if path.exists() {
        return Err(anyhow!("File already exists."));
    }

    // Attempt to create a file in the path, throw an error if fails..
    File::create(&path)?;

    // Remove the file again.
    fs::remove_file(&path)?;

    Ok(())
}

const DEFAULTS_BINARY: &str = "goxlr-defaults";
pub fn extract_defaults(file_type: PathTypes, path: &Path) -> Result<()> {
    let binary_name = if cfg!(target_os = "windows") {
        format!("{DEFAULTS_BINARY}.exe")
    } else {
        String::from(DEFAULTS_BINARY)
    };

    let mut binary_path = None;

    // Next to the daemon, then on $PATH (not the current folder: this can be asked for remotely,
    // and would run whatever program of that name sits there)
    if let Some(parent) = std::env::current_exe()?.parent() {
        let bin = parent.join(binary_name.clone());
        if bin.exists() {
            binary_path.replace(bin);
        }
    }

    let final_bin = if let Some(path) = binary_path {
        path.into_os_string()
    } else {
        OsString::from(binary_name)
    };

    let file_type = match file_type {
        PathTypes::Profiles => "profiles",
        PathTypes::MicProfiles => "mic-profiles",
        PathTypes::Presets => "presets",
        PathTypes::Icons => "icons",
        _ => bail!("Invalid File Type Specified"),
    };

    let command = Command::new(final_bin)
        .arg(file_type)
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match command {
        Ok(output) => {
            if !output.status.success()
                && let Some(code) = output.status.code()
            {
                bail!("Unable to extract defaults, Error Code: {}", code);
            }
        }
        Err(error) => {
            bail!("Unable to run Default extractor: {}", error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_file_paths() {
        let dir = Path::new("/profiles");
        assert_eq!(
            named_file_path(dir, "My Profile", "goxlr").unwrap(),
            dir.join("My Profile.goxlr")
        );
        assert!(named_file_path(dir, "v1.2 (stream)", "preset").is_ok());

        assert!(named_file_path(dir, "", "goxlr").is_err());
        assert!(named_file_path(dir, "..", "goxlr").is_err());
        assert!(named_file_path(dir, "../../x", "goxlr").is_err());
        assert!(named_file_path(dir, "..\\..\\x", "goxlr").is_err());
        assert!(named_file_path(dir, "/etc/passwd", "goxlr").is_err());
        assert!(named_file_path(dir, "bad\nname", "goxlr").is_err());
    }
}

#[cfg(test)]
mod sample_management_tests {
    use super::*;

    #[test]
    fn create_rename_and_move() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        fs::write(base.join("intro.wav"), b"").unwrap();
        fs::write(base.join("outro.mp3"), b"").unwrap();

        // Folders, nested and empty
        create_sample_folder(&base, "Jingles").unwrap();
        create_sample_folder(&base, "Jingles/Old").unwrap();
        assert!(create_sample_folder(&base, "Jingles").is_err());
        assert!(create_sample_folder(&base, "Missing/Child").is_err());
        assert!(create_sample_folder(&base, "../escape").is_err());
        assert!(create_sample_folder(&base, "").is_err());

        // Moving a file keeps its name, nothing to update in profiles
        let moved = move_sample_path(&base, "intro.wav", "Jingles/intro.wav").unwrap();
        assert!(moved.is_none());
        assert!(base.join("Jingles/intro.wav").is_file());

        // Renaming a file reports the old and new names
        let renamed = move_sample_path(&base, "Jingles/intro.wav", "Jingles/start.wav")
            .unwrap()
            .expect("file name changed");
        assert_eq!(
            (renamed.old_name.as_str(), renamed.new_name.as_str()),
            ("intro.wav", "start.wav")
        );

        // Refused: name already used elsewhere, non audio extension, escaping, missing source
        assert!(move_sample_path(&base, "Jingles/start.wav", "Jingles/outro.mp3").is_err());
        assert!(move_sample_path(&base, "Jingles/start.wav", "Jingles/start.exe").is_err());
        assert!(move_sample_path(&base, "Jingles/start.wav", "../start.wav").is_err());
        assert!(move_sample_path(&base, "nothing.wav", "other.wav").is_err());

        // Folders can be renamed, but not moved inside themselves
        assert!(move_sample_path(&base, "Jingles", "Jingles/Old/Jingles").is_err());
        assert!(
            move_sample_path(&base, "Jingles", "Stingers")
                .unwrap()
                .is_none()
        );
        assert!(base.join("Stingers/start.wav").is_file());

        let mut folders = Vec::new();
        collect_folders(&base, &base, &mut folders);
        folders.sort();
        let separator = std::path::MAIN_SEPARATOR;
        assert_eq!(
            folders,
            vec!["Stingers".to_string(), format!("Stingers{separator}Old")]
        );
    }
}

#[cfg(test)]
mod sample_edit_tests {
    use super::*;
    use crate::sample_trash::list_trash;

    // One second of stereo audio at 8kHz
    fn write_wav(path: &Path) {
        let audio = DecodedAudio {
            sample_rate: 8000,
            channels: 2,
            samples: (0..16000).map(|i| (i % 50) as f32 / 100.0).collect(),
            bits_per_sample: Some(16),
        };
        atomic_write(path, |file| audio.write_wav(BufWriter::new(file))).unwrap();
    }

    fn cut(remove: &[(f64, f64)]) -> SampleEdit {
        SampleEdit {
            remove: remove.to_vec(),
            normalize: false,
        }
    }

    #[test]
    fn edit_duplicate_and_import() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let base = root.join("samples");
        let backups = root.join("backups");
        fs::create_dir_all(base.join("Jingles/Old")).unwrap();
        write_wav(&base.join("intro.wav"));
        write_wav(&base.join("Jingles/a.wav"));
        write_wav(&base.join("Jingles/Old/b.wav"));

        // Usage lookups know what a folder contains
        let mut names = sample_names_at(&base, "Jingles").unwrap();
        names.sort();
        assert_eq!(names, vec!["a.wav".to_string(), "b.wav".to_string()]);

        // Edit into a copy: keeps the original, refuses a name already in use
        let edit = cut(&[(0.5, 1.0)]);
        assert!(edit_sample_file(&base, &backups, "intro.wav", &edit, Some("a")).is_err());
        let copy =
            edit_sample_file(&base, &backups, "intro.wav", &edit, Some("intro short")).unwrap();
        assert!(copy.is_none());
        let short = DecodedAudio::decode(&base.join("intro short.wav")).unwrap();
        assert!((short.duration() - 0.5).abs() < 0.01);
        assert!(list_trash(&backups).is_empty());

        // Edit in place: the original goes to the trash
        let edit = cut(&[(0.0, 0.25)]);
        assert!(
            edit_sample_file(&base, &backups, "intro.wav", &edit, None)
                .unwrap()
                .is_none()
        );
        let edited = DecodedAudio::decode(&base.join("intro.wav")).unwrap();
        assert!((edited.duration() - 0.75).abs() < 0.01);
        let trash = list_trash(&backups);
        assert_eq!((trash.len(), trash[0].path.as_str()), (1, "intro.wav"));
        assert!(edit_sample_file(&base, &backups, "intro.wav", &cut(&[]), None).is_err());

        // Normalizing alone is a change
        let normalize = SampleEdit {
            remove: vec![],
            normalize: true,
        };
        assert!(
            edit_sample_file(&base, &backups, "Jingles/a.wav", &normalize, Some("a loud")).is_ok()
        );

        // Duplicates and imports get free names
        assert_eq!(
            duplicate_sample(&base, "Jingles/a.wav").unwrap(),
            "Jingles/a (2).wav"
        );
        assert_eq!(
            duplicate_sample(&base, "Jingles/a.wav").unwrap(),
            "Jingles/a (3).wav"
        );
        assert!(base.join("Jingles/a (3).wav").is_file());
        let target = import_sample_target(&base, "Jingles/Old", "b.wav").unwrap();
        assert_eq!(target, base.join("Jingles/Old/b (2).wav"));
        let target = import_sample_target(&base, "", "C:\\fakepath\\new.mp3").unwrap();
        assert_eq!(target, base.join("new.mp3"));
        assert!(import_sample_target(&base, "", "virus.exe").is_err());
        assert!(import_sample_target(&base, "Missing", "new.wav").is_err());
        assert!(import_sample_target(&base, "../..", "new.wav").is_err());
    }
}

#[cfg(test)]
mod edit_undo_tests {
    use super::*;
    use crate::sample_trash::list_trash;

    #[test]
    fn restoring_the_original_undoes_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let (base, backups) = (dir.path().join("samples"), dir.path().join("backups"));
        fs::create_dir_all(&base).unwrap();
        let audio = DecodedAudio {
            sample_rate: 8000,
            channels: 1,
            samples: (0..8000).map(|i| (i % 40) as f32 / 100.0).collect(),
            bits_per_sample: Some(16),
        };
        atomic_write(base.join("intro.wav"), |file| {
            audio.write_wav(BufWriter::new(file))
        })
        .unwrap();
        let original = fs::read(base.join("intro.wav")).unwrap();

        let edit = SampleEdit {
            remove: vec![(0.0, 0.5)],
            normalize: false,
        };
        edit_sample_file(&base, &backups, "intro.wav", &edit, None).unwrap();
        assert_ne!(fs::read(base.join("intro.wav")).unwrap(), original);

        // The original is in the trash; restoring it puts it back and trashes the edit
        let id = list_trash(&backups)[0].id.clone();
        let restored = restore_trash_item(&base, &backups, &id).unwrap();
        assert_eq!(restored.path, "intro.wav");
        assert!(restored.renamed.is_none());
        assert_eq!(fs::read(base.join("intro.wav")).unwrap(), original);
        assert_eq!(list_trash(&backups).len(), 1);
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn finds_files_with_pattern_characters() {
        let dir = tempfile::Builder::new()
            .prefix("goxlr-test-[samples]")
            .tempdir()
            .unwrap();
        let base = dir.path().to_path_buf();
        let sub = base.join("Recorded [Live]");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Song [Live].wav"), b"").unwrap();
        fs::write(base.join("other.wav"), b"").unwrap();

        let found = find_file_in_path(base.clone(), PathBuf::from("Song [Live].wav"));
        assert_eq!(found, Some(sub.join("Song [Live].wav")));
        assert!(find_file_in_path(base.clone(), PathBuf::from("Song ?Live?.wav")).is_none());
        assert!(find_file_in_path(base.clone(), PathBuf::from("../other.wav")).is_none());

        let manager_list = recursive_file_list(base.clone(), vec!["wav"]);
        assert_eq!(
            manager_list.get(&format!(
                "Recorded [Live]{}Song [Live].wav",
                std::path::MAIN_SEPARATOR
            )),
            Some(&String::from("Song [Live].wav"))
        );
        assert_eq!(manager_list.len(), 2);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;
    use crate::sample_trash::{list_trash, restore_trash_item, trash_sample_path};

    #[test]
    fn names_follow_every_systems_rules() {
        for good in [
            "intro.wav",
            "My Profile",
            "v1.2 (stream)",
            "Ça va.mp3",
            "CONSOLE.wav",
        ] {
            assert!(validate_file_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "bad\nname",
            "CON",
            "nul.wav",
            "Com1.mp3",
            "Jingles.",
            "trailing ",
            "what?.wav",
            "a:b.wav",
            "<x>",
        ] {
            assert!(validate_file_name(bad).is_err(), "{bad:?}");
        }

        // Existing names are still found on Linux, new ones must be portable
        let dir = Path::new("/samples");
        let lookup = sample_path(dir, "Live: 2024/a.wav");
        assert_eq!(lookup.is_ok(), !cfg!(windows));
        assert!(new_sample_path(dir, "Folder/CON").is_err());
        assert!(named_file_path(dir, "Stream: Main", "goxlr").is_ok() != cfg!(windows));
        assert!(new_named_file_path(dir, "Stream: Main", "goxlr").is_err());
    }

    #[test]
    fn uppercase_extensions_are_samples() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        fs::create_dir_all(base.join("Jingles")).unwrap();
        fs::write(base.join("Jingles/Intro.WAV"), b"").unwrap();
        fs::write(base.join("outro.Mp3"), b"").unwrap();

        let mut names: Vec<String> =
            recursive_file_list(base.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
                .into_values()
                .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Intro.WAV".to_string(), "outro.Mp3".to_string()]
        );

        // Names are unique without case, so they can't collide on Windows and macOS
        assert_eq!(free_sample_name(base, "intro.wav"), "intro (2).wav");
        assert!(
            import_sample_target(base, "", "INTRO.wav")
                .unwrap()
                .ends_with("INTRO (2).wav")
        );
        assert!(move_sample_path(base, "outro.Mp3", "Jingles/intro.wav").is_err());
    }

    #[test]
    fn case_only_renames() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        fs::write(base.join("intro.wav"), b"intro").unwrap();

        let renamed = move_sample_path(base, "intro.wav", "Intro.wav")
            .unwrap()
            .expect("name changed");
        assert_eq!(renamed.new_name, "Intro.wav");
        assert!(exists_exactly(&base.join("Intro.wav")));
        assert!(!exists_exactly(&base.join("intro.wav")));
        assert_eq!(fs::read(base.join("Intro.wav")).unwrap(), b"intro");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        let base = dir.path().join("samples");
        fs::create_dir_all(base.join("Jingles")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(base.join("Jingles/intro.wav"), b"").unwrap();
        fs::write(outside.join("secret.wav"), b"").unwrap();
        // A loop back to the samples folder, and a way out of it
        std::os::unix::fs::symlink(&base, base.join("Jingles/loop")).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("away")).unwrap();

        let names: Vec<String> = recursive_file_list(base.clone(), SAMPLE_EXTENSIONS.to_vec())
            .into_values()
            .collect();
        assert_eq!(names, vec!["intro.wav".to_string()]);
        let mut folders = Vec::new();
        collect_folders(&base, &base, &mut folders);
        assert_eq!(folders, vec!["Jingles".to_string()]);

        assert!(sample_path(&base, "away/secret.wav").is_err());
        assert!(sample_path(&base, "away/new.wav").is_err());
        assert!(sample_path(&base, "Jingles/new.wav").is_ok());
        assert!(find_file_in_path(base.clone(), PathBuf::from("secret.wav")).is_none());
    }

    #[test]
    fn rename_and_duplicate_named_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        fs::write(base.join("Stream.goxlr"), b"stream").unwrap();
        fs::write(base.join("Music.goxlr"), b"music").unwrap();

        duplicate_named_file(base, "Stream", "Stream copy", "goxlr").unwrap();
        assert_eq!(fs::read(base.join("Stream copy.goxlr")).unwrap(), b"stream");
        assert!(duplicate_named_file(base, "Stream", "Music", "goxlr").is_err());
        assert!(duplicate_named_file(base, "Missing", "Other", "goxlr").is_err());

        rename_named_file(base, "Stream copy", "Late night", "goxlr").unwrap();
        assert!(!base.join("Stream copy.goxlr").exists());
        assert!(rename_named_file(base, "Late night", "Music", "goxlr").is_err());
        assert!(rename_named_file(base, "Late night", "CON", "goxlr").is_err());
        rename_named_file(base, "Late night", "LATE night", "goxlr").unwrap();
        assert!(exists_exactly(&base.join("LATE night.goxlr")));
    }

    #[test]
    fn restore_keeps_names_unique() {
        let dir = tempfile::tempdir().unwrap();
        let (base, backups) = (dir.path().join("samples"), dir.path().join("backups"));
        fs::create_dir_all(base.join("Jingles")).unwrap();
        fs::write(base.join("intro.wav"), b"old").unwrap();
        trash_sample_path(&base, &backups, "intro.wav", vec![]).unwrap();

        // Another intro.wav arrived in a different folder meanwhile
        fs::write(base.join("Jingles/Intro.wav"), b"new").unwrap();
        let id = list_trash(&backups)[0].id.clone();
        assert!(restore_trash_item(&base, &backups, &id).is_err());
        fs::remove_file(base.join("Jingles/Intro.wav")).unwrap();
        assert!(restore_trash_item(&base, &backups, &id).is_ok());
    }

    #[test]
    fn failed_edit_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (base, backups) = (dir.path().join("samples"), dir.path().join("backups"));
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("intro.wav"), b"original").unwrap();
        trash_sample_path(&base, &backups, "intro.wav", vec![]).unwrap();
        assert!(!base.join("intro.wav").exists());

        restore_latest_trash_of(&base, &backups, "intro.wav");
        assert_eq!(fs::read(base.join("intro.wav")).unwrap(), b"original");
        assert!(list_trash(&backups).is_empty());
    }
}

#[cfg(test)]
mod watcher_tests {
    use super::*;
    use std::io::Write;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    /// A recording is created empty, then written: the samples are listed again after the
    /// writes, so its size and length show up.
    #[tokio::test]
    async fn written_samples_are_listed_again() {
        let root = tempfile::tempdir().unwrap();
        let paths = FilePaths {
            profiles: root.path().join("profiles"),
            mic_profiles: root.path().join("mic"),
            presets: root.path().join("presets"),
            icons: root.path().join("icons"),
            samples: root.path().join("samples"),
            backups: root.path().join("backups"),
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
        let (tx, mut rx) = mpsc::channel(8);
        let shutdown = crate::shutdown::Shutdown::new();
        tokio::spawn(spawn_file_notification_service(
            paths.clone(),
            tx,
            shutdown.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;

        let path = paths.samples.join("Recording.wav");
        let mut file = File::create(&path).unwrap();
        let created = timeout(Duration::from_secs(3), rx.recv()).await.unwrap();
        assert_eq!(created, Some(PathTypes::Samples));

        file.write_all(&[0; 4096]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let written = timeout(Duration::from_secs(3), rx.recv()).await;
        assert_eq!(written.ok().flatten(), Some(PathTypes::Samples));
        shutdown.trigger();
    }
}
