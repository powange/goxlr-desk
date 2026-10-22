//! The first start of GoXLR Desk on a computer where GoXLR Utility, the project it comes from, is
//! set up: its settings, profiles, mic profiles, samples, presets, icons and backups are copied
//! over, once. Copied, never moved: GoXLR Utility keeps working as it was.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde_json::Value;

use crate::settings::project_dirs;

/// Where GoXLR Utility keeps its settings and files.
fn utility_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", "GoXLR-on-Linux", "GoXLR-Utility")
}

/// The settings holding a folder, which may point inside GoXLR Utility's data folder.
const DIRECTORY_KEYS: &[&str] = &[
    "profile_directory",
    "mic_profile_directory",
    "samples_directory",
    "presets_directory",
    "icons_directory",
    "logs_directory",
    "backup_directory",
];

/// Copies GoXLR Utility's settings and files when GoXLR Desk starts for the first time with its
/// default settings file. Returns what was done, for the log (which isn't set up yet).
pub fn import_from_goxlr_utility(config_file: &Path) -> Result<Option<String>> {
    let (Some(desk), Some(utility)) = (project_dirs(), utility_project_dirs()) else {
        return Ok(None);
    };
    if config_file != desk.config_dir().join("settings.json") {
        return Ok(None);
    }
    import(
        &utility.config_dir().join("settings.json"),
        utility.data_dir(),
        config_file,
        desk.data_dir(),
    )
}

fn import(
    utility_settings: &Path,
    utility_data: &Path,
    desk_settings: &Path,
    desk_data: &Path,
) -> Result<Option<String>> {
    // Once: GoXLR Desk's data folder is made by the first start, imported or not
    if desk_data.exists() || !utility_settings.is_file() {
        return Ok(None);
    }

    // The files first, in a folder renamed once complete, so a copy cut short starts again
    let partial = desk_data.with_extension("importing");
    if partial.exists() {
        fs::remove_dir_all(&partial).context("Unable to clear an unfinished import")?;
    }
    if utility_data.is_dir() {
        copy_tree(utility_data, &partial, &["logs"])
            .context("Unable to copy GoXLR Utility's files")?;
    } else {
        fs::create_dir_all(&partial)?;
    }

    // The settings: folders inside GoXLR Utility's data folder now point inside GoXLR Desk's,
    // and the desktop app registered is GoXLR Desk's own (its installer registers it before the
    // first start), never GoXLR Utility's.
    let mut settings: Value = serde_json::from_slice(&fs::read(utility_settings)?)
        .context("GoXLR Utility's settings can't be read")?;
    let current = fs::read(desk_settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if let Some(object) = settings.as_object_mut() {
        let activate = current.and_then(|c| c.get("activate").cloned());
        object.insert("activate".into(), activate.unwrap_or(Value::Null));
        for key in DIRECTORY_KEYS {
            let Some(Value::String(folder)) = object.get(*key) else {
                continue;
            };
            if let Ok(inside) = Path::new(folder).strip_prefix(utility_data) {
                let moved = desk_data.join(inside).to_string_lossy().into_owned();
                object.insert((*key).into(), Value::String(moved));
            }
        }
    }
    if let Some(parent) = desk_settings.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(desk_settings, serde_json::to_vec_pretty(&settings)?)?;
    fs::rename(&partial, desk_data).context("Unable to finish the import")?;

    Ok(Some(format!(
        "Settings and files of GoXLR Utility copied from {} (it keeps its own)",
        utility_data.display()
    )))
}

/// Copies a folder's content; `skip` names entries of its top level left out (the logs).
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if skip.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, &[])?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_once_and_points_folders_at_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let utility = dir.path().join("utility");
        let (utility_config, utility_data) = (utility.join("config"), utility.join("data"));
        fs::create_dir_all(utility_data.join("profiles")).unwrap();
        fs::create_dir_all(utility_data.join("logs")).unwrap();
        fs::write(utility_data.join("profiles/Stream.goxlr"), b"profile").unwrap();
        fs::write(utility_data.join("logs/goxlr-daemon.log"), b"log").unwrap();
        fs::create_dir_all(&utility_config).unwrap();
        let elsewhere = dir.path().join("my-samples");
        let settings = serde_json::json!({
            "selected_locale": "fr_FR",
            "profile_directory": utility_data.join("profiles"),
            "samples_directory": elsewhere,
            "activate": "C:\\Program Files\\GoXLR Utility\\goxlr-utility-ui.exe",
        });
        fs::write(utility_config.join("settings.json"), settings.to_string()).unwrap();

        // The installer registered GoXLR Desk's desktop app before the first start
        let desk = dir.path().join("desk");
        let (desk_settings, desk_data) = (desk.join("config/settings.json"), desk.join("data"));
        fs::create_dir_all(desk_settings.parent().unwrap()).unwrap();
        fs::write(&desk_settings, r#"{"activate": "desk-ui"}"#).unwrap();

        let done = import(
            &utility_config.join("settings.json"),
            &utility_data,
            &desk_settings,
            &desk_data,
        )
        .unwrap();
        assert!(done.is_some());
        assert_eq!(
            fs::read(desk_data.join("profiles/Stream.goxlr")).unwrap(),
            b"profile"
        );
        assert!(!desk_data.join("logs").exists());
        assert!(utility_data.join("profiles/Stream.goxlr").exists());

        let copied: Value = serde_json::from_slice(&fs::read(&desk_settings).unwrap()).unwrap();
        assert_eq!(copied["selected_locale"], "fr_FR");
        assert_eq!(copied["activate"], "desk-ui");
        assert_eq!(
            Path::new(copied["profile_directory"].as_str().unwrap()),
            desk_data.join("profiles")
        );
        assert_eq!(
            Path::new(copied["samples_directory"].as_str().unwrap()),
            elsewhere
        );

        // Never twice
        fs::write(&desk_settings, r#"{"selected_locale": "en_GB"}"#).unwrap();
        assert!(
            import(
                &utility_config.join("settings.json"),
                &utility_data,
                &desk_settings,
                &desk_data
            )
            .unwrap()
            .is_none()
        );
        assert!(
            fs::read_to_string(&desk_settings)
                .unwrap()
                .contains("en_GB")
        );
    }
}
