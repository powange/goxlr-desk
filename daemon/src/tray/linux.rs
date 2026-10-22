use crate::ICON;
use crate::events::EventTriggers;
use crate::shutdown::Shutdown;
use crate::tray::{tray_label, tray_state};
use anyhow::Result;
use goxlr_ipc::GoXLRCommand;
use goxlr_ipc::PathTypes::{Icons, Logs, MicProfiles, Presets, Profiles, Samples};
use goxlr_types::MuteState;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Category, MenuItem, Status, ToolTip, Tray, TrayMethods};
use log::{debug, warn};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

pub async fn handle_tray(mut stop: Shutdown, tx: mpsc::Sender<EventTriggers>) -> Result<()> {
    // Before we spawn the tray, we're going to extract our icon to a temporary location
    // so that it can be immediately used. Depending on pixmaps seems to cause issues under
    // gnome, where occasionally the icon will not correctly spawn.

    // We'll dump the icon here :)
    // This user's runtime folder (private to them), not a folder of /tmp shared by everyone
    let tmp_file_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .unwrap_or_else(|| std::env::temp_dir().join(format!("goxlr-utility-{}", nix_uid())))
        .join("goxlr-utility");

    // Extract the icon to a temporary directory, and pass its path..
    let tmp_file_path = tmp_file_dir.join("goxlr-utility-icon.png");
    if !tmp_file_dir.exists() {
        fs::create_dir_all(&tmp_file_dir)?;
    }

    // Rather than random shenanigans, we'll simply try to remove any existing files and
    // recycle whatever is there if we can't. These should evaluate in order, so if the
    // file is absent, or the file was successfully removed, we can write to it.
    if !tmp_file_path.exists() || fs::remove_file(&tmp_file_path).is_ok() {
        fs::write(&tmp_file_path, ICON)?;
    } else {
        warn!("Unable to remove existing icon, using whatever is already there..");
    }

    // Attempt to immediately update the environment..
    let icon = GoXLRTray::new(tx, &tmp_file_path);
    let handle = icon
        .disable_dbus_name(true)
        .assume_sni_available(true)
        .spawn()
        .await;

    let handle = match handle {
        Ok(handle) => handle,
        Err(e) => {
            // There's no harm in running without a tray icon, in some cases this may actually
            // be preferable (for example, when running under the CLI), so we just warn, tidy
            // up, and consider our work here done.
            fs::remove_file(&tmp_file_path)?;
            warn!("Unable to Spawn the Tray Handler: {}", e);
            return Ok(());
        }
    };

    // The menu shows profiles and the mic state: rebuild it when they change.
    let mut shown = tray_state();
    loop {
        tokio::select! {
            _ = stop.recv() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                let current = tray_state();
                if current != shown {
                    shown = current;
                    handle.update(|_| {}).await;
                }
            }
        }
    }

    debug!("Shutting Down Tray Handler..");
    let _ = handle.shutdown().await;
    fs::remove_file(&tmp_file_path)?;
    Ok(())
}

struct GoXLRTray {
    tx: mpsc::Sender<EventTriggers>,
    icon: PathBuf,
}

impl GoXLRTray {
    fn new(tx: mpsc::Sender<EventTriggers>, icon: &Path) -> Self {
        let icon = icon.to_path_buf();
        Self { tx, icon }
    }
}

impl Tray for GoXLRTray {
    fn id(&self) -> String {
        "goxlr-utility".to_string()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(EventTriggers::Activate);
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn title(&self) -> String {
        String::from("GoXLR Utility")
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_theme_path(&self) -> String {
        if let Some(parent) = self.icon.parent() {
            return parent.to_string_lossy().to_string();
        }

        String::from("")
    }

    fn icon_name(&self) -> String {
        if let Some(file) = self.icon.file_stem() {
            return file.to_string_lossy().to_string();
        }
        "goxlr-utility".to_string()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: String::from("GoXLR Utility"),
            description: String::from("A Tool for Configuring a GoXLR"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let state = tray_state();
        let lang = state.language.as_str();
        let path_item = |label: &str, path: goxlr_ipc::PathTypes| -> MenuItem<Self> {
            StandardItem {
                label: label.to_string(),
                activate: Box::new(move |this: &mut GoXLRTray| {
                    let _ = this.tx.try_send(EventTriggers::Open(path.clone()));
                }),
                ..Default::default()
            }
            .into()
        };
        let profile_items = |names: &[String], active: &Option<String>, mic: bool| {
            names
                .iter()
                .map(|name| {
                    let profile = name.clone();
                    CheckmarkItem {
                        label: name.replace('_', "__"),
                        checked: active.as_ref() == Some(name),
                        activate: Box::new(move |this: &mut GoXLRTray| {
                            let command = if mic {
                                GoXLRCommand::LoadMicProfile(profile.clone(), true)
                            } else {
                                GoXLRCommand::LoadProfile(profile.clone(), true)
                            };
                            let _ = this.tx.try_send(EventTriggers::TrayCommand(command));
                        }),
                        ..Default::default()
                    }
                    .into()
                })
                .collect::<Vec<MenuItem<Self>>>()
        };

        let mut menu: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: tray_label(lang, "configure").to_string(),
                activate: Box::new(|this: &mut GoXLRTray| {
                    let _ = this.tx.try_send(EventTriggers::Activate);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        if state.profile.is_some() {
            menu.push(
                SubMenu {
                    label: tray_label(lang, "profiles").to_string(),
                    submenu: profile_items(&state.profiles, &state.profile, false),
                    ..Default::default()
                }
                .into(),
            );
            menu.push(
                SubMenu {
                    label: tray_label(lang, "mic_profiles").to_string(),
                    submenu: profile_items(&state.mic_profiles, &state.mic_profile, true),
                    ..Default::default()
                }
                .into(),
            );
        }
        if let Some(muted) = state.mic_muted {
            menu.push(
                StandardItem {
                    label: tray_label(lang, if muted { "unmute" } else { "mute" }).to_string(),
                    activate: Box::new(move |this: &mut GoXLRTray| {
                        let state = if muted {
                            MuteState::Unmuted
                        } else {
                            MuteState::MutedToX
                        };
                        let command = GoXLRCommand::SetCoughMuteState(state);
                        let _ = this.tx.try_send(EventTriggers::TrayCommand(command));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        if state.profile.is_some() || state.mic_muted.is_some() {
            menu.push(MenuItem::Separator);
        }
        menu.push(
            SubMenu {
                label: tray_label(lang, "open_path").to_string(),
                submenu: vec![
                    path_item(tray_label(lang, "profiles"), Profiles),
                    path_item(tray_label(lang, "mic_profiles"), MicProfiles),
                    MenuItem::Separator,
                    path_item(tray_label(lang, "presets"), Presets),
                    path_item(tray_label(lang, "samples"), Samples),
                    path_item(tray_label(lang, "icons"), Icons),
                    MenuItem::Separator,
                    path_item(tray_label(lang, "logs"), Logs),
                ],
                ..Default::default()
            }
            .into(),
        );
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: tray_label(lang, "quit").to_string(),
                activate: Box::new(|this: &mut GoXLRTray| {
                    let _ = this.tx.try_send(EventTriggers::Stop(false));
                }),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

fn nix_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}
