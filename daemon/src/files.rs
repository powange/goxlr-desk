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

use glob::{Pattern, glob};
use goxlr_audio::editor::DecodedAudio;
use goxlr_ipc::PathTypes;
use goxlr_profile_loader::atomic::atomic_write;
use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
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

pub async fn spawn_file_notification_service(
    paths: FilePaths,
    sender: Sender<PathTypes>,
    mut shutdown_signal: Shutdown,
) -> Result<()> {
    let watcher = create_watcher();
    if let Err(error) = watcher {
        warn!("Error Creating the File Watcher, aborting: {:?}", error);
        bail!("Error Creating the File Watcher: {:?}", error);
    }

    // Create the worker..
    let (mut watcher, mut rx) = watcher.unwrap();

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

    // Wait for any changes..
    loop {
        tokio::select! {
            () = shutdown_signal.recv() => {
                debug!("Shutdown Signal Received.");
                break;
            },
            result = rx.recv() => {
                if let Some(result) = result {
                    match result {
                        Ok(event) => {
                            match event.kind {
                                // Triggered on the Creation of a file / folder..
                                EventKind::Create(CreateKind::File) |
                                EventKind::Create(CreateKind::Folder) |
                                EventKind::Create(CreateKind::Any) |

                                // Triggered on the Removal of a File / Folder
                                EventKind::Remove(RemoveKind::File) |
                                EventKind::Remove(RemoveKind::Folder) |
                                EventKind::Remove(RemoveKind::Any) |

                                // Triggered on Rename / Move of a file
                                EventKind::Modify(ModifyKind::Name(RenameMode::From)) |
                                EventKind::Modify(ModifyKind::Name(RenameMode::To)) |
                                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {

                                    let path = &event.paths[0];
                                    if path.starts_with(&paths.profiles) {
                                        let _ = sender.send(PathTypes::Profiles).await;
                                        continue;
                                    }

                                    if path.starts_with(&paths.mic_profiles) {
                                        let _ = sender.send(PathTypes::MicProfiles).await;
                                        continue;
                                    }

                                    if path.starts_with(&paths.icons) {
                                        let _ = sender.send(PathTypes::Icons).await;
                                        continue;
                                    }

                                    if path.starts_with(&paths.presets) {
                                        let _ = sender.send(PathTypes::Presets).await;
                                        continue;
                                    }

                                    if path.starts_with(&paths.samples) {
                                        let _ = sender.send(PathTypes::Samples).await;
                                        continue;
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

/// Lists files with the given extensions below a path, keyed by their path relative to it.
fn recursive_file_list(path: PathBuf, extensions: Vec<&str>) -> BTreeMap<String, String> {
    let mut paths: Vec<PathBuf> = Vec::new();

    // Escape the directory, so names like '[Bob]' aren't treated as patterns.
    let base = Pattern::escape(&path.to_string_lossy());
    for extension in extensions {
        let format = format!("{base}/**/*.{extension}");
        if let Ok(files) = glob(format.as_str()) {
            // Skip entries that can't be read (eg. a folder without permission).
            paths.extend(files.filter_map(|file| file.ok()));
        }
    }

    let mut map: BTreeMap<String, String> = BTreeMap::new();
    // Ok, we need to split stuff up..
    for file_path in paths {
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
        let path = entry.path();
        if path.is_dir() {
            if let Ok(relative) = path.strip_prefix(base) {
                folders.push(relative.to_string_lossy().to_string());
            }
            collect_folders(base, &path, folders);
        }
    }
}

/// Turns a path relative to the samples directory (from a client, '/' or '\\' separated) into
/// a full path, refusing anything that could point outside of the samples directory.
fn sample_path(base: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = base.to_path_buf();
    let mut parts = 0;
    for part in relative.split(['/', '\\']).filter(|part| !part.is_empty()) {
        let invalid = part == "."
            || part == ".."
            || part
                .chars()
                .any(|c| c.is_control() || (cfg!(windows) && c == ':'));
        if invalid {
            bail!("Invalid path: {:?}", relative);
        }
        path.push(part);
        parts += 1;
    }
    if parts == 0 {
        bail!("Invalid path: {:?}", relative);
    }
    Ok(path)
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
    let path = sample_path(base, relative)?;
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
    let source = sample_path(base, from)?;
    let target = sample_path(base, to)?;

    if !source.exists() {
        bail!("{} doesn't exist", from);
    }
    if target.exists() {
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
            let in_use = recursive_file_list(base.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
                .values()
                .any(|name| name == &new_name);
            if in_use {
                bail!("A sample named {} already exists", new_name);
            }
            renamed = Some(RenamedSample { old_name, new_name });
        }
    }

    fs::rename(&source, &target).context(format!("Unable to move {} to {}", from, to))?;
    Ok(renamed)
}

/// The names of the sample files at a path: the file itself, or every sample inside a folder.
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

/// Deletes a sample file, or a folder with everything in it. Returns the names of the sample
/// files that were deleted.
pub fn delete_sample_path(base: &Path, relative: &str) -> Result<Vec<String>> {
    let names = sample_names_at(base, relative)?;
    let path = sample_path(base, relative)?;
    if path.is_dir() {
        fs::remove_dir_all(&path).context(format!("Unable to delete {}", relative))?;
    } else {
        fs::remove_file(&path).context(format!("Unable to delete {}", relative))?;
    }
    Ok(names)
}

/// Removes parts of a sample (start, end in seconds) and saves the result as WAV, either over
/// the original file or as a new file named `save_as` next to it. Replacing a file that isn't a
/// WAV gives it a .wav name, which is returned so profiles can follow it.
pub fn edit_sample_file(
    base: &Path,
    relative: &str,
    remove: &[(f64, f64)],
    save_as: Option<&str>,
) -> Result<Option<RenamedSample>> {
    let source = sample_path(base, relative)?;
    if !source.is_file() || !is_sample_file(&source) {
        bail!("{} isn't a sample file", relative);
    }
    if remove.is_empty() {
        bail!("There is nothing to change in {}", relative);
    }

    let target = match save_as {
        Some(name) => {
            let name = name.trim();
            let invalid = name.is_empty()
                || name == "."
                || name == ".."
                || name.chars().any(|c| {
                    c.is_control() || c == '/' || c == '\\' || (cfg!(windows) && c == ':')
                });
            if invalid {
                bail!("Invalid name: {:?}", name);
            }
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
    if target != source {
        if target.exists() {
            bail!("{} already exists", new_name);
        }
        let in_use = recursive_file_list(base.to_path_buf(), SAMPLE_EXTENSIONS.to_vec())
            .values()
            .any(|name| name == &new_name);
        if in_use {
            bail!("A sample named {} already exists", new_name);
        }
    }

    let mut audio =
        DecodedAudio::decode(&source).context(format!("Unable to read {}", relative))?;
    audio.remove_ranges(remove)?;
    atomic_write(&target, |file| audio.write_wav(BufWriter::new(file)))
        .context(format!("Unable to save {}", new_name))?;

    // Replacing an MP3 / OGG / FLAC wrote a new .wav file, remove the original.
    if save_as.is_none() && target != source {
        fs::remove_file(&source).context(format!("Unable to remove {}", relative))?;
        return Ok(Some(RenamedSample { old_name, new_name }));
    }
    Ok(None)
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

    // Both parts are escaped, so names like 'Song [Live].wav' are matched literally.
    let format = format!(
        "{}/**/{}",
        Pattern::escape(&path.to_string_lossy()),
        Pattern::escape(&file.to_string_lossy())
    );
    glob(format.as_str()).ok()?.find_map(|file| file.ok())
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
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\' || (cfg!(windows) && c == ':'));
    if invalid {
        bail!("Invalid name: {:?}", name);
    }
    Ok(directory.join(format!("{name}.{extension}")))
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

    // There are three possible places to check for this, the CWD, the binary WD, and $PATH
    let cwd = std::env::current_dir()?.join(binary_name.clone());
    if cwd.exists() {
        binary_path.replace(cwd);
    }

    if binary_path.is_none()
        && let Some(parent) = std::env::current_exe()?.parent()
    {
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
        let base = std::env::temp_dir().join(format!("goxlr-test-manage-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
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

        fs::remove_dir_all(&base).unwrap();
    }
}

#[cfg(test)]
mod sample_edit_tests {
    use super::*;

    // One second of silence-ish stereo audio at 8kHz
    fn write_wav(path: &Path) {
        let audio = DecodedAudio {
            sample_rate: 8000,
            channels: 2,
            samples: (0..16000).map(|i| (i % 50) as f32 / 100.0).collect(),
            bits_per_sample: Some(16),
        };
        atomic_write(path, |file| audio.write_wav(BufWriter::new(file))).unwrap();
    }

    #[test]
    fn delete_and_edit() {
        let base = std::env::temp_dir().join(format!("goxlr-test-edit-{}", std::process::id()));
        fs::create_dir_all(base.join("Jingles/Old")).unwrap();
        write_wav(&base.join("intro.wav"));
        write_wav(&base.join("Jingles/a.wav"));
        write_wav(&base.join("Jingles/Old/b.wav"));

        // Usage lookups know what a folder contains
        let mut names = sample_names_at(&base, "Jingles").unwrap();
        names.sort();
        assert_eq!(names, vec!["a.wav".to_string(), "b.wav".to_string()]);

        // Edit into a copy: keeps the original, refuses a name already in use
        assert!(edit_sample_file(&base, "intro.wav", &[(0.5, 1.0)], Some("a")).is_err());
        let copy =
            edit_sample_file(&base, "intro.wav", &[(0.5, 1.0)], Some("intro short")).unwrap();
        assert!(copy.is_none());
        let short = DecodedAudio::decode(&base.join("intro short.wav")).unwrap();
        assert!((short.duration() - 0.5).abs() < 0.01);
        assert!(
            (DecodedAudio::decode(&base.join("intro.wav"))
                .unwrap()
                .duration()
                - 1.0)
                .abs()
                < 0.01
        );

        // Edit in place
        assert!(
            edit_sample_file(&base, "intro.wav", &[(0.0, 0.25)], None)
                .unwrap()
                .is_none()
        );
        assert!(
            (DecodedAudio::decode(&base.join("intro.wav"))
                .unwrap()
                .duration()
                - 0.75)
                .abs()
                < 0.01
        );
        assert!(edit_sample_file(&base, "intro.wav", &[], None).is_err());

        // Delete a file, then a folder with its content
        assert_eq!(
            delete_sample_path(&base, "intro.wav").unwrap(),
            vec!["intro.wav".to_string()]
        );
        assert!(!base.join("intro.wav").exists());
        assert_eq!(delete_sample_path(&base, "Jingles").unwrap().len(), 2);
        assert!(!base.join("Jingles").exists());
        assert!(delete_sample_path(&base, "Jingles").is_err());
        assert!(delete_sample_path(&base, "..").is_err());
        assert!(base.join("intro short.wav").exists());

        fs::remove_dir_all(&base).unwrap();
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn finds_files_with_pattern_characters() {
        let base =
            std::env::temp_dir().join(format!("goxlr-test-[samples]-{}", std::process::id()));
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

        fs::remove_dir_all(&base).unwrap();
    }
}
