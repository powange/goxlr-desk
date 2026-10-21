use crate::device::Device;
use crate::device_actor::{DeviceActor, DeviceUpdate, Reply};
use crate::events::EventTriggers;
use crate::files::{
    RenamedSample, create_sample_folder, duplicate_sample, edit_sample_file, extract_defaults,
    move_sample_path, sample_names_at,
};
use crate::firmware::firmware_update::{
    FirmwareMessages, FirmwareRequest, FirmwareUpdateDevice, FirmwareUpdateSettings,
    do_firmware_update, start_firmware_update,
};
use crate::platform::{get_ui_app_path, has_autostart, set_autostart};
use crate::profile::{
    rename_sample_in_saved_profiles, reset_sample_in_saved_profiles, saved_profiles_using_samples,
};
use crate::profile_trash::{
    empty_profile_trash, list_profile_trash, purge_profile_trash, restore_profile,
};
use crate::sample_trash::{
    TrashedTrack, empty_trash, list_trash, purge_trash, restore_trash_item, trash_sample_path,
};
use crate::{
    FIRMWARE_PATHS, FileManager, PatchEvent, SYSTEM_LOCALE, SettingsHandle, Shutdown, VERSION,
};
use anyhow::{Result, anyhow};
use enum_map::EnumMap;
use goxlr_audio::editor::probe_duration;
use goxlr_ipc::{
    Activation, ColourWay, DaemonCommand, DaemonConfig, DaemonStatus, DriverDetails, Files,
    FirmwareSource, FirmwareStatus, GoXLRCommand, HardwareStatus, HttpSettings, Locale,
    MixerStatus, NetworkAccess, PathTypes, Paths, ProfileTrashItem, SampleFile, SampleTrashItem,
    SampleUsage, UpdateState, UsbProductInformation,
};
use goxlr_types::{DeviceType, FirmwareDetails, VersionNumber};
use goxlr_usb::device::base::{FullGoXLRDevice, GoXLRDevice};
use goxlr_usb::device::{find_devices, from_device, get_version};
use goxlr_usb::{PID_GOXLR_FULL, PID_GOXLR_MINI};
use json_patch::diff;
use log::{debug, error, info, warn};
use reqwest::{ClientBuilder, StatusCode};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use xmltree::Element;

const IGNORE_DEVICE_DURATION: Duration = Duration::from_secs(10);

// Devices are identified by bus number, address and (on Windows) identifier.
type DeviceKey = (u8, u8, Option<String>);

// A device opened and identified on a blocking thread, or the reason it couldn't be.
type AttachResult = (
    DeviceKey,
    Instant,
    Result<(Box<dyn FullGoXLRDevice>, HardwareStatus)>,
);

/// How long to wait before trying a device again after it failed to load. A device often isn't
/// ready right after a session starts (driver still loading), so retry quickly at first.
fn retry_delay(failures: u32) -> Duration {
    match failures {
        0 | 1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        3 => Duration::from_secs(5),
        _ => IGNORE_DEVICE_DURATION,
    }
}
const APP_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const TRASH_PURGE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

// Adding a third entry has tripped enum_variant_names, I'll probably need to rename
// RunDeviceCommand, but that'll need to be in a separate commit, for now, suppress.
#[allow(clippy::enum_variant_names)]
pub enum DeviceCommand {
    SendDaemonStatus(oneshot::Sender<DaemonStatus>),
    RunDaemonCommand(DaemonCommand, oneshot::Sender<Result<()>>),
    RunDeviceCommand(String, GoXLRCommand, oneshot::Sender<Result<()>>),
    GetDeviceMicLevel(String, oneshot::Sender<Result<f64>>),
    GetSampleUsage(String, oneshot::Sender<Result<SampleUsage>>),
    GetNetworkAccess(oneshot::Sender<NetworkAccess>),
    RunFirmwareUpdate(String, Option<PathBuf>, bool, oneshot::Sender<Result<()>>),
    ContinueFirmwareUpdate(String, oneshot::Sender<Result<()>>),
    ClearFirmwareState(String, oneshot::Sender<Result<()>>),
}

#[allow(dead_code)]
pub enum DeviceStateChange {
    Shutdown(bool),
    Sleep(oneshot::Sender<()>),
    Wake(oneshot::Sender<()>),
    // A command for the first device (from the tray menu)
    Command(GoXLRCommand),
}

#[derive(Clone)]
struct FirmwareUpdateState {
    settings: FirmwareUpdateSettings,
    status: FirmwareStatus,
}

pub type DeviceSender = Sender<DeviceCommand>;
pub type DeviceReceiver = Receiver<DeviceCommand>;

// Fix this later..
#[allow(clippy::too_many_arguments)]
pub async fn spawn_usb_handler(
    mut command_rx: DeviceReceiver,
    mut file_rx: Receiver<PathTypes>,
    mut device_state_rx: Receiver<DeviceStateChange>,
    broadcast_tx: BroadcastSender<PatchEvent>,
    global_tx: Sender<EventTriggers>,
    ready_tx: oneshot::Sender<()>,
    mut shutdown: Shutdown,
    settings: SettingsHandle,
    http_settings: HttpSettings,
    mut file_manager: FileManager,
) {
    let mut firmware_version = None;

    // We can probably either merge these, or struct them..
    let (disconnect_sender, mut disconnect_receiver) = mpsc::channel(16);
    let (event_sender, mut event_receiver) = mpsc::channel(16);
    let (firmware_sender, mut firmware_receiver) = mpsc::channel(1);

    // The channel size depends on how many GoXLRs are simultaneously connected and performing
    // firmware updates.. Let's say.. 6?
    let (firmware_update_sender, mut firmware_update_receiver) = mpsc::channel(6);

    // Spawn a task in the background to check for the latest firmware versions.
    tokio::spawn(check_firmware_versions(
        firmware_sender.clone(),
        settings.get_firmware_source().await,
    ));

    // Create the device detection Sleep Timer..
    let detection_duration = Duration::from_millis(1000);
    let detection_sleep = sleep(Duration::from_millis(0));
    tokio::pin!(detection_sleep);

    // Create the State update Sleep Timer..
    let update_duration = Duration::from_millis(50);
    let update_sleep = sleep(update_duration);
    tokio::pin!(update_sleep);

    // Timer for checking whether the UI App has appeared
    let mut app_check: Option<String> = None;
    get_app_path(&mut app_check);

    let app_duration = APP_CHECK_INTERVAL;
    let app_sleep = sleep(app_duration);
    tokio::pin!(app_sleep);

    // The trashes are purged at startup (when listed) and then once a day
    let purge_sleep = sleep(TRASH_PURGE_INTERVAL);
    tokio::pin!(purge_sleep);

    // Get the Driver Type and Details..
    let (interface, version) = get_version();
    let mut driver_interface = DriverDetails { interface, version };

    // Create the Primary Device List, and 'Ignore' list..
    // Each device runs in its own task (see device_actor), and sends its status back here
    let mut devices: HashMap<String, DeviceActor> = HashMap::new();
    let mut mixers: HashMap<String, MixerStatus> = HashMap::new();
    // Unbounded: a device task must never wait for this loop, which may itself be waiting for
    // that device (run()), or both would wait for ever. A status per job, consumed promptly.
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<DeviceUpdate>();
    let mut devices_firmware: HashMap<String, FirmwareUpdateState> = HashMap::new();
    let mut ignore_list: HashMap<DeviceKey, (Instant, u32)> = HashMap::new();

    // Devices are opened on a blocking thread (it takes a few seconds), one at a time, so this
    // loop keeps answering requests meanwhile.
    let (attach_tx, mut attach_rx) = mpsc::channel::<AttachResult>(1);
    // Then their profiles are applied in a task of their own, the device comes back here
    let (loaded_tx, mut loaded_rx) = mpsc::channel::<(DeviceKey, Result<Device>)>(1);

    // Slow sample operations run in the background and report back here.
    let (sample_job_tx, mut sample_job_rx) = mpsc::channel::<SampleJobDone>(16);
    // Sample jobs still running. While one renames or replaces a file, the profiles loaded on
    // the devices aren't checked against the files (validate_sampler would drop the track whose
    // file is about to get its new name): the check waits for the jobs to end.
    let mut sample_jobs_running = 0usize;
    let mut validate_pending = false;
    // Samples being edited, so the same file isn't edited twice at once.
    let mut busy_samples: HashSet<String> = HashSet::new();
    let mut attaching = false;

    let mut files = get_files(&mut file_manager, &settings).await;
    let mut daemon_status = get_daemon_status(
        &mixers,
        &settings,
        &http_settings,
        &driver_interface,
        &firmware_version,
        &devices_firmware,
        files.clone(),
        &app_check,
    )
    .await;

    // The status as last sent, kept as JSON so a change is diffed without serialising it again
    let mut status_json = serde_json::to_value(&daemon_status).unwrap_or_default();

    let mut shutdown_triggered = false;
    let _ = ready_tx.send(());

    loop {
        // Something changed anywhere (the whole status is rebuilt), or only one device's
        // inputs moved (only its part is: that's most changes, while faders move)
        let mut change_found = false;
        let mut device_changed: Option<String> = None;
        let mut pending_reply: Option<Reply> = None;
        tokio::select! {
            Some(version) = firmware_receiver.recv() => {
                firmware_version = Some(version);
                change_found = true;
            },
            Some(received) = firmware_update_receiver.recv() => {
                match received {
                    FirmwareRequest::SetUpdateState(serial,status) => {
                        if let Some(state) = devices_firmware.get_mut(&serial) {
                            // A file uploaded for this update isn't needed once it's flashed
                            if status == UpdateState::Complete {
                                remove_uploaded_firmware(&state.settings.file);
                            }
                            state.status.state = status;
                            state.status.progress = 0;
                            change_found = true;
                        } else {
                            // The update state was cleared while the update was still running,
                            // there's nothing left to attach this state to.
                            warn!("Ignoring firmware state {:?} for {}, update state was cleared", status, serial);
                        }
                    }
                    FirmwareRequest::SetStateProgress(serial, progress) => {
                        if let Some(state) = devices_firmware.get_mut(&serial) {
                            state.status.progress = progress;
                            change_found = true;
                        } else {
                            error!("Update State does not exist! Ignoring.");
                        }
                    }
                    FirmwareRequest::SetError(serial, error) => {
                        debug!("Setting Error: {}", error);
                        if let Some(state) = devices_firmware.get_mut(&serial) {
                            state.status.error = Some(error);
                            change_found = true;
                        } else {
                            error!("Update State does not exist! Ignoring..");
                        }
                    }
                    FirmwareRequest::FirmwareMessage(serial,message) => {
                        if let Some(device) = devices.get(&serial) {
                            // Pass this to the device to manage
                            device.send(move |d| Box::pin(async move {
                                d.handle_firmware_message(message).await;
                                true
                            }));
                        } else {
                            // We need to locate the sender, and inform it that the device is gone, these
                            // will pop back a SetError.
                            match message {
                                FirmwareMessages::EnterDFUMode(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::BeginEraseNVR(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::PollEraseNVR(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::UploadFirmwareChunk(_,sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::ValidateUploadChunk(_,sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::BeginHardwareVerify(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::PollHardwareVerify(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::BeginHardwareWrite(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::PollHardwareWrite(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                                FirmwareMessages::RebootGoXLR(sender) => {
                                    let _ = sender.send(Err(anyhow!("Device Not Found!")));
                                }
                            }
                        }
                    }
                }
            }
            () = &mut detection_sleep => {
                if !attaching && let Some(device) = find_new_device(&daemon_status, &ignore_list) {
                    let key = (device.bus_number(), device.address(), device.identifier().clone());
                    let existing_serials: Vec<String> = get_all_serials(&devices);
                    let disconnect_sender = disconnect_sender.clone();
                    let event_sender = event_sender.clone();
                    let attach_tx = attach_tx.clone();

                    info!("Found GoXLR on bus {} address {}, attaching..", key.0, key.1);
                    attaching = true;
                    let started = Instant::now();
                    tokio::task::spawn_blocking(move || {
                        // A panic in the USB or driver code must still answer, otherwise
                        // 'attaching' stays set and no device is ever detected again.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            attach_device(device, existing_serials, disconnect_sender, event_sender)
                        }))
                        .unwrap_or_else(|_| Err(anyhow!("The device code panicked while attaching")));
                        let _ = attach_tx.blocking_send((key, started, result));
                    });
                }
                detection_sleep.as_mut().reset(tokio::time::Instant::now() + detection_duration);
            },
            Some((key, started, attached)) = attach_rx.recv() => {
                match attached {
                    Ok((handled_device, hardware)) => {
                        // The profiles are applied in their own task (a few seconds of USB
                        // requests), this loop keeps answering meanwhile
                        info!("Attached GoXLR {} in {:?}, loading profiles..", hardware.serial_number, started.elapsed());
                        let (settings, global_tx, loaded_tx) = (settings.clone(), global_tx.clone(), loaded_tx.clone());
                        tokio::spawn(async move {
                            let profile_start = Instant::now();
                            let loaded = load_device(handled_device, hardware, global_tx, &settings).await;
                            if let Ok(device) = &loaded {
                                info!(
                                    "GoXLR {} ready, profiles applied in {:?} ({:?} after it was found)",
                                    device.serial(), profile_start.elapsed(), started.elapsed()
                                );
                            }
                            let _ = loaded_tx.send((key, loaded)).await;
                        });
                    }
                    Err(error) => {
                        let _ = loaded_tx.send((key, Err(error))).await;
                    }
                }
            },
            Some((key, loaded)) = loaded_rx.recv() => {
                attaching = false;
                match loaded {
                    Ok(device) => {
                        let serial = String::from(device.serial());

                        mixers.insert(serial.clone(), device.status().await);
                        devices.insert(serial.clone(), DeviceActor::spawn(device, update_tx.clone()));
                        ignore_list.remove(&key);
                        change_found = true;

                        // Get the Driver Type and Details again as Theysecon does not show the driver
                        // version when no device is connected..
                        if driver_interface.version.is_none() {
                            let (_, version) = get_version();
                            if let Some(version) = version {
                                debug!("Driver Version found, updating.. {}", version);
                                driver_interface.version = Some(version);
                            }
                        }
                    }
                    Err(e) => {
                        let failures = ignore_list.get(&key).map_or(0, |(_, count)| *count) + 1;
                        let delay = retry_delay(failures);
                        error!(
                            "Couldn't load potential GoXLR on bus {} address {} (attempt {}), retrying in {:?}: {}",
                            key.0, key.1, failures, delay, e
                        );
                        ignore_list.insert(key, (Instant::now() + delay, failures));
                    }
                }
            },
            () = &mut update_sleep => {
                for device in devices.values() {
                    device.tick();
                }
                update_sleep.as_mut().reset(tokio::time::Instant::now() + update_duration);
            },
            () = &mut purge_sleep => {
                let backups = settings.get_backup_directory().await;
                tokio::task::spawn_blocking(move || {
                    purge_trash(&backups);
                    purge_profile_trash(&backups);
                });
                purge_sleep.as_mut().reset(tokio::time::Instant::now() + TRASH_PURGE_INTERVAL);
            },
            () = &mut app_sleep => {
                if get_app_path(&mut app_check) {
                    change_found = true;
                }
                app_sleep.as_mut().reset(tokio::time::Instant::now() + APP_CHECK_INTERVAL);
            },
            Some(serial) = disconnect_receiver.recv() => {
                info!("[{}] Device Disconnected", serial);
                devices.remove(&serial);
                mixers.remove(&serial);

                // If this device was actively doing a firmware update that's not complete, we should scream
                // INCREDIBLY loudly (in the logs).. We will keep this device around though, the error will
                // be handled by the firmware updater, and presented accordingly.
                if devices_firmware.contains_key(&serial) {
                    let state = devices_firmware.get(&serial).unwrap();
                    match state.status.state {
                        UpdateState::Failed | UpdateState::Pause(_) | UpdateState::Complete => info!("Restarting device after firmware update"),
                        _ => warn!("DEVICE REMOVED BEFORE UPDATE COMPLETE")
                    }
                }
                change_found = true;
            },
            Some(serial) = event_receiver.recv() => {
                if let Some(device) = devices.get(&serial) {
                    device.input_event();
                } else {
                    warn!("Cannot find registered device with serial: {}", serial);
                }
            }
            Some(update) = update_rx.recv() => {
                // A device that was removed meanwhile is gone from the status
                if devices.contains_key(&update.serial) {
                    mixers.insert(update.serial.clone(), update.status);
                    device_changed = Some(update.serial);
                }
                // Answered once the status below is up to date
                pending_reply = update.reply;
            }
            Some(event) = device_state_rx.recv() => {
                match event {
                    DeviceStateChange::Shutdown(avoid_write) => {
                        // Make sure this only happens once..
                        if shutdown_triggered {
                            continue;
                        }
                        shutdown_triggered = true;

                        // Flip through all the devices, send a shutdown signal..
                        for device in devices.values() {
                            let _ = device.run(move |d| Box::pin(d.shutdown(avoid_write))).await;
                        }

                        // Send a notification that we're done here..
                        let _ = global_tx.send(EventTriggers::DevicesStopped).await;
                    },
                    DeviceStateChange::Sleep(sender) => {
                        debug!("Received Sleep Notification");
                        for device in devices.values() {
                            let _ = device.run(|d| Box::pin(d.sleep())).await;
                        }
                        let _ = sender.send(());
                    },
                    DeviceStateChange::Command(command) => {
                        let mut serials: Vec<String> = devices.keys().cloned().collect();
                        serials.sort();
                        if let Some(device) = serials.first().and_then(|s| devices.get(s)) {
                            device.send(move |d| Box::pin(async move {
                                if let Err(e) = d.perform_command(command).await {
                                    warn!("Unable to run the tray's command: {}", e);
                                }
                                true
                            }));
                        }
                    }
                    DeviceStateChange::Wake(sender) => {
                        debug!("Received Wake Notification");
                        for device in devices.values() {
                            let _ = device.run(|d| Box::pin(d.wake())).await;
                        }
                        let _ = sender.send(());

                        // We'll set this regardless and refresh the Status object, this
                        // allows the UI to update when waking up.
                        change_found = true;
                    }
                }


            }
            () = shutdown.recv() => {
                info!("Shutting down device worker");
                return;
            },
            Some(command) = command_rx.recv() => {
                match command {
                    DeviceCommand::SendDaemonStatus(sender) => {
                        let _ = sender.send(daemon_status.clone());
                    }

                    DeviceCommand::RunDaemonCommand(command, sender) => {
                        match command {
                            DaemonCommand::RestartDaemon => {
                                crate::RESTART_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
                                let _ = global_tx.send(EventTriggers::Stop(false)).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::StopDaemon => {
                                // These should probably be moved upstream somewhere, they're not
                                // device specific!
                                let _ = global_tx.send(EventTriggers::Stop(false)).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::OpenUi => {
                                let _ = global_tx.send(EventTriggers::OpenUi).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::Activate => {
                                let _ = global_tx.send(EventTriggers::Activate).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::RecoverDefaults(path_type) => {
                                let path = match path_type {
                                    PathTypes::Profiles => Some(settings.get_profile_directory().await),
                                    PathTypes::Presets => Some(settings.get_presets_directory().await),
                                    PathTypes::Icons => Some(settings.get_icons_directory().await),
                                    PathTypes::MicProfiles => Some(settings.get_mic_profile_directory().await),
                                    _ => None,
                                };

                                // An unsupported type is an error for this request only, the
                                // worker must keep running for every other request.
                                let result = match path {
                                    Some(path) => extract_defaults(path_type, &path),
                                    None => Err(anyhow!("Invalid Path type Sent")),
                                };
                                let _ = sender.send(result);
                            }
                            DaemonCommand::SetAutoStartEnabled(enabled) => {
                                let _ = sender.send(set_autostart(enabled));
                                forget_autostart();
                                change_found = true;
                            }
                            DaemonCommand::SetLogLevel(level) => {
                                settings.set_log_level(level).await;
                                settings.save().await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetFirmwareSource(source) => {
                                settings.set_firmware_source(source).await;
                                settings.save().await;

                                // Respawn the firmware checker with the new data
                                tokio::spawn(check_firmware_versions(
                                    firmware_sender.clone(),
                                    source,
                                ));

                                let _ = sender.send(Ok(()));
                                change_found = true;
                            }
                            DaemonCommand::SetLocale(language) => {
                                settings.set_selected_locale(language).await;
                                settings.save().await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetUiLaunchOnLoad(value) => {
                                settings.set_open_ui_on_launch(value).await;
                                settings.save().await;
                                change_found = true;

                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetShowTrayIcon(enabled) => {
                                settings.set_show_tray_icon(enabled).await;
                                settings.save().await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetTTSEnabled(enabled) => {
                                settings.set_tts_enabled(enabled).await;
                                settings.save().await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::RegenerateNetworkToken => {
                                settings.regenerate_network_token().await;
                                settings.save().await;
                                info!("A new network access token was made, older links stop working");
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetAllowNetworkAccess(enabled) => {
                                settings.set_allow_network_access(enabled).await;
                                settings.save().await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::OpenPath(path_type) => {
                                // There's nothing we can really do if this errors..
                                let _ = global_tx.send(EventTriggers::Open(path_type)).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetSampleGainPct(sample, gain) => {
                                settings.set_sample_gain_percent(sample, gain).await;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::CreateSampleFolder(path) => {
                                let base = settings.get_samples_directory().await;
                                let result = create_sample_folder(&base, &path);
                                if result.is_ok() {
                                    files = update_files(files, PathTypes::Samples, &mut file_manager, &settings).await;
                                    change_found = true;
                                }
                                let _ = sender.send(result);
                            }
                            DaemonCommand::MoveSamplePath(from, to) => {
                                let base = settings.get_samples_directory().await;
                                let profiles = settings.get_profile_directory().await;
                                let backups = settings.get_backup_directory().await;
                                // Renaming rewrites the saved profiles, which is slow: done in the
                                // background, the result comes back through sample_job_rx.
                                sample_jobs_running += 1;
                                spawn_sample_job(sample_job_tx.clone(), None, sender, move || {
                                    let renamed = move_sample_path(&base, &from, &to)?;
                                    if let Some(renamed) = &renamed {
                                        rename_in_saved_profiles(&profiles, &backups, renamed);
                                    }
                                    Ok(SampleJobOutput { renamed, ..Default::default() })
                                });
                            }
                            DaemonCommand::DeleteSamplePath(path) => {
                                let base = settings.get_samples_directory().await;
                                let backups = settings.get_backup_directory().await;
                                // The loaded profiles drop the tracks of a missing file, so note
                                // them: they're put back if the file is restored.
                                let names: HashSet<String> =
                                    sample_names_at(&base, &path).unwrap_or_default().into_iter().collect();
                                let mut tracks = Vec::new();
                                for device in devices.values() {
                                    let names = names.clone();
                                    if let Ok(found) = device.run(move |d| Box::pin(async move { d.trashed_tracks(&names) })).await {
                                        tracks.extend(found);
                                    }
                                }
                                // Kept in the trash (with its gain setting), so it can be restored.
                                // A move to another drive copies files, so it's done in the background.
                                sample_jobs_running += 1;
                                spawn_sample_job(sample_job_tx.clone(), None, sender, move || {
                                    trash_sample_path(&base, &backups, &path, tracks)?;
                                    info!("Moved sample path {} to the trash", path);
                                    Ok(SampleJobOutput::default())
                                });
                            }
                            DaemonCommand::RestoreSampleTrash(id) => {
                                let base = settings.get_samples_directory().await;
                                let backups = settings.get_backup_directory().await;
                                let profiles = settings.get_profile_directory().await;
                                sample_jobs_running += 1;
                                spawn_sample_job(sample_job_tx.clone(), None, sender, move || {
                                    let restored = restore_trash_item(&base, &backups, &id)?;
                                    info!("Restored sample path {} from the trash", restored.path);
                                    // Undoing an edit that renamed the sample: profiles go back to it
                                    let renamed = restored.renamed.map(|(old_name, new_name)| RenamedSample { old_name, new_name });
                                    if let Some(renamed) = &renamed {
                                        rename_in_saved_profiles(&profiles, &backups, renamed);
                                    }
                                    Ok(SampleJobOutput { renamed, tracks: restored.tracks, ..Default::default() })
                                });
                            }
                            DaemonCommand::RestoreProfileTrash(id) => {
                                let profiles = settings.get_profile_directory().await;
                                let mics = settings.get_mic_profile_directory().await;
                                let backups = settings.get_backup_directory().await;
                                let result = restore_profile(&profiles, &mics, &backups, &id);
                                if let Ok((name, mic)) = &result {
                                    info!("Restored the profile {} from the trash", name);
                                    let kind = if *mic { PathTypes::MicProfiles } else { PathTypes::Profiles };
                                    files = update_files(files, kind, &mut file_manager, &settings).await;
                                    change_found = true;
                                }
                                let _ = sender.send(result.map(|_| ()));
                            }
                            DaemonCommand::EmptyProfileTrash(id) => {
                                let backups = settings.get_backup_directory().await;
                                let result = empty_profile_trash(&backups, id.as_deref());
                                if result.is_ok() {
                                    files.profile_trash = list_profile_trash(&backups);
                                    change_found = true;
                                }
                                let _ = sender.send(result);
                            }
                            DaemonCommand::EmptySampleTrash(id) => {
                                let backups = settings.get_backup_directory().await;
                                let result = empty_trash(&backups, id.as_deref());
                                if result.is_ok() {
                                    files = update_files(files, PathTypes::Samples, &mut file_manager, &settings).await;
                                    change_found = true;
                                }
                                let _ = sender.send(result);
                            }
                            DaemonCommand::DuplicateSample(path) => {
                                let base = settings.get_samples_directory().await;
                                let result = match duplicate_sample(&base, &path) {
                                    Ok(copy) => {
                                        // The copy starts with the same gain as the original.
                                        let name_of = |path: &str| path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                                        let gain = settings.get_sample_gain_percent(name_of(&path)).await;
                                        settings.set_sample_gain_percent(name_of(&copy), gain).await;
                                        settings.save().await;
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                };
                                if result.is_ok() {
                                    files = update_files(files, PathTypes::Samples, &mut file_manager, &settings).await;
                                    change_found = true;
                                }
                                let _ = sender.send(result);
                            }
                            DaemonCommand::EditSampleFile(path, edit, save_as) => {
                                if !busy_samples.insert(path.clone()) {
                                    let _ = sender.send(Err(anyhow!("{} is already being edited", path)));
                                } else {
                                    let base = settings.get_samples_directory().await;
                                    let backups = settings.get_backup_directory().await;
                                    let profiles = settings.get_profile_directory().await;
                                    // Decoding and writing audio takes a while: done in the background.
                                    sample_jobs_running += 1;
                                    spawn_sample_job(sample_job_tx.clone(), Some(path.clone()), sender, move || {
                                        let renamed = edit_sample_file(&base, &backups, &path, &edit, save_as.as_deref())?;
                                        if let Some(renamed) = &renamed {
                                            rename_in_saved_profiles(&profiles, &backups, renamed);
                                        }
                                        // Replaced: the tracks playing it measured the old audio,
                                        // they play the whole new file at its new gain
                                        let edited = save_as.is_none().then(|| {
                                            let name = match &renamed {
                                                Some(renamed) => renamed.new_name.clone(),
                                                None => path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string(),
                                            };
                                            let file = match &renamed {
                                                Some(renamed) => base.join(Path::new(&path).with_file_name(&renamed.new_name)),
                                                None => base.join(&path),
                                            };
                                            let gain = measure_gain(&file).unwrap_or(1.0);
                                            reset_sample_in_saved_profiles(&profiles, &backups, &name, gain);
                                            (name, gain)
                                        });
                                        info!("Edited sample {} ({} part(s) removed, normalized: {})", path, edit.remove.len(), edit.normalize);
                                        // A copy saved under a new name keeps the original's gain
                                        let copied_gain = save_as.as_deref().map(|name| {
                                            let name = name.trim();
                                            let new_name = if name.to_ascii_lowercase().ends_with(".wav") { name.to_string() } else { format!("{name}.wav") };
                                            (path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string(), new_name)
                                        });
                                        Ok(SampleJobOutput { renamed, copied_gain, edited, ..Default::default() })
                                    });
                                }
                            }
                            DaemonCommand::ApplySampleChange => {
                                // Change is committed, save it..
                                settings.save().await;

                                // Resend the value.
                                files = update_files(files, PathTypes::Samples, &mut file_manager, &settings).await;
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::SetActivatorPath(path) => {
                                if let Some(path) = path {
                                    settings.set_activate(Some(path.to_string_lossy().to_string())).await;
                                    settings.save().await;
                                } else {
                                    settings.set_activate(None).await;
                                    settings.save().await;
                                }
                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                            DaemonCommand::HandleMacOSAggregates(value) => {
                                settings.set_macos_handle_aggregates(value).await;
                                settings.save().await;

                                change_found = true;
                                let _ = sender.send(Ok(()));
                            }
                        }
                    },

                    DeviceCommand::RunDeviceCommand(serial, command, sender) => {
                        if let Some(device) = devices.get(&serial) {
                            // Run by the device's task, the answer comes once the status is
                            // up to date
                            device.send_reply(true, sender, move |d| Box::pin(async move {
                                let result = d.perform_command(command.clone()).await;
                                if let Err(error) = &result {
                                    warn!("Error Executing: {:?}, {}", command, error);
                                }
                                result
                            }));
                        } else {
                            let _ = sender.send(Err(anyhow!("Device {} is not connected", serial)));
                        }
                    },

                    DeviceCommand::GetNetworkAccess(sender) => {
                        let _ = sender.send(network_access(&http_settings, settings.get_network_token().await));
                    }
                    DeviceCommand::GetSampleUsage(path, sender) => {
                        let base = settings.get_samples_directory().await;
                        let profile_directory = settings.get_profile_directory().await;
                        match sample_names_at(&base, &path) {
                            Err(e) => {
                                let _ = sender.send(Err(e));
                            }
                            Ok(names) => {
                                let names: HashSet<String> = names.into_iter().collect();
                                // The loaded profiles may have changes that aren't saved yet.
                                let mut loaded: BTreeSet<String> = BTreeSet::new();
                                for device in devices.values() {
                                    let check = names.clone();
                                    let used = device.run(move |d| Box::pin(async move {
                                        d.profile().uses_sample_file(&check).then(|| d.profile().name().to_string())
                                    })).await;
                                    if let Ok(Some(name)) = used {
                                        loaded.insert(name);
                                    }
                                }
                                // Reading every saved profile is slow, so it's done in the background.
                                tokio::spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        let mut profiles = loaded;
                                        profiles.extend(saved_profiles_using_samples(&profile_directory, &names));
                                        SampleUsage {
                                            files: names.len(),
                                            profiles: profiles.into_iter().collect(),
                                        }
                                    })
                                    .await
                                    .map_err(|e| anyhow!("Unable to check the sample usage: {}", e));
                                    let _ = sender.send(result);
                                });
                            }
                        }
                    }
                    DeviceCommand::GetDeviceMicLevel(serial, sender) => {
                        if let Some(device) = devices.get(&serial) {
                            device.send_reply(false, sender, |d| Box::pin(d.get_mic_level()));
                        } else {
                            let _ = sender.send(Err(anyhow!("Device {} is not connected", serial)));
                        }
                    },

                    DeviceCommand::RunFirmwareUpdate(serial, file, force, sender) => {
                        let mut start_update = true;
                        if let Some(device) = devices.get(&serial) {
                            if let Some(state) = devices_firmware.get(&serial) {
                                match state.status.state {
                                    UpdateState::Failed | UpdateState::Complete | UpdateState::Pause(_) => {
                                        warn!("Update attempted before previous update was cleared, continuing because safe.");
                                    }
                                    _ => {
                                        start_update = false;
                                    }
                                }
                            }

                            if start_update {
                                let Ok((device_type, current_firmware)) = device
                                    .run(|d| Box::pin(async move { (d.get_hardware_type(), d.get_firmware_version()) }))
                                    .await
                                else {
                                    let _ = sender.send(Err(anyhow!("Device {} is not connected", serial)));
                                    continue;
                                };

                                // Create and run a firmware updater for this device
                                let update_settings = FirmwareUpdateSettings {
                                    sender: firmware_update_sender.clone(),
                                    device: FirmwareUpdateDevice {
                                        source: settings.get_firmware_source().await,
                                        serial: serial.clone(),
                                        device_type,
                                        current_firmware,
                                    },
                                    file,
                                    force,
                                };
                                let update_state = FirmwareUpdateState {
                                    settings: update_settings.clone(),
                                    status: FirmwareStatus {
                                        state: UpdateState::Starting,
                                        progress: 0,
                                        error: None
                                    },
                                };

                                devices_firmware.insert(serial, update_state);
                                tokio::spawn(start_firmware_update(update_settings));

                                let _ = sender.send(Ok(()));
                                change_found = true;
                            } else {
                                let _ = sender.send(Err(anyhow!("Update Already in progress!")));
                            }
                        } else {
                            let _ = sender.send(Err(anyhow!("Device {} is not connected", serial)));
                        }
                    },
                    DeviceCommand::ContinueFirmwareUpdate(serial, sender) => {
                        if let Some(state) = devices_firmware.get_mut(&serial) {
                            match state.status.state.clone() {
                                UpdateState::Pause(file_info) => {
                                    info!("Continuing Firmware Update..");
                                    // Leave Pause now: a second Continue (double click, a retry)
                                    // must not start another flash of the same device.
                                    state.status.state = UpdateState::Starting;
                                    change_found = true;
                                    tokio::spawn(do_firmware_update(state.settings.clone(), file_info));
                                    let _ = sender.send(Ok(()));
                                }
                                _ => {
                                    let _ = sender.send(Err(anyhow!("Update not in Paused State!")));
                                }
                            }
                        } else {
                            let _ = sender.send(Err(anyhow!("Device not performing firmware update.")));
                        }
                    },

                    DeviceCommand::ClearFirmwareState(serial, sender) => {
                        if let Some(device) = devices_firmware.get(&serial) {
                            match device.status.state {
                                UpdateState::Complete | UpdateState::Failed | UpdateState::Pause(_) => {
                                    // We're at a point where this update can be stopped, nuke it from the firmware list..
                                    if let Some(state) = devices_firmware.remove(&serial) {
                                        remove_uploaded_firmware(&state.settings.file);
                                    }
                                    change_found = true;

                                    let _ = sender.send(Ok(()));
                                },
                                _ => {
                                    // Can't stop it just yet.. :D
                                    let _ = sender.send(Err(anyhow!("Cannot Clear, update in progress..")));
                                }
                            };
                        } else {
                            let _ = sender.send(Err(anyhow!("Device not performing firmware update.")));
                        }
                    }
                }
            },
            Some(done) = sample_job_rx.recv() => {
                sample_jobs_running = sample_jobs_running.saturating_sub(1);
                if let Some(key) = &done.key {
                    busy_samples.remove(key);
                }
                let result = match done.result {
                    Ok(output) => {
                        if let Some(renamed) = &output.renamed {
                            for device in devices.values() {
                                let (old, new) = (renamed.old_name.clone(), renamed.new_name.clone());
                                let _ = device.run(move |d| Box::pin(async move {
                                    d.rename_sample_file(&old, &new);
                                })).await;
                            }
                            settings.rename_sample_gain(&renamed.old_name, &renamed.new_name).await;
                            settings.save().await;
                        }
                        if let Some((original, copy)) = &output.copied_gain {
                            let gain = settings.get_sample_gain_percent(original.clone()).await;
                            settings.set_sample_gain_percent(copy.clone(), gain).await;
                            settings.save().await;
                        }
                        if let Some((name, gain)) = &output.edited {
                            for device in devices.values() {
                                let name = name.clone();
                                let gain = *gain;
                                device.send(move |d| Box::pin(async move { d.sample_file_edited(&name, gain) }));
                            }
                        }
                        for device in devices.values() {
                            let tracks = output.tracks.clone();
                            device.send(move |d| Box::pin(async move {
                                if let Err(e) = d.restore_trashed_tracks(&tracks).await {
                                    warn!("Unable to put the restored sample back on the sampler: {}", e);
                                }
                                true
                            }));
                        }
                        files = update_files(files, PathTypes::Samples, &mut file_manager, &settings).await;
                        change_found = true;
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                let _ = done.sender.send(result);

                // The renames are applied above, the files can be checked now
                if sample_jobs_running == 0 && validate_pending {
                    validate_pending = false;
                    validate_samplers(&devices);
                }
            }
            Some(path) = file_rx.recv() => {
                // Notify devices if Samples have changed..
                if path == PathTypes::Samples {
                    if sample_jobs_running > 0 {
                        validate_pending = true;
                    } else {
                        validate_samplers(&devices);
                    }
                }

                files = update_files(files, path, &mut file_manager, &settings).await;
                change_found = true;
            }
        }

        if change_found {
            let new_status = get_daemon_status(
                &mixers,
                &settings,
                &http_settings,
                &driver_interface,
                &firmware_version,
                &devices_firmware,
                files.clone(),
                &app_check,
            )
            .await;
            publish_status(
                new_status,
                &mut daemon_status,
                &mut status_json,
                &broadcast_tx,
            );
        } else if let Some(serial) = device_changed
            && let Some(status) = mixers.get(&serial)
        {
            let mut new_status = daemon_status.clone();
            new_status.mixers.insert(serial, status.clone());
            crate::tray::update_tray_state(&new_status);
            publish_status(
                new_status,
                &mut daemon_status,
                &mut status_json,
                &broadcast_tx,
            );
        }
        if let Some(reply) = pending_reply {
            reply();
        }
    }
}

/// Sends what changed between the status as last sent and the new one (nothing when they're the
/// same), then keeps the new one.
fn publish_status(
    new_status: DaemonStatus,
    daemon_status: &mut DaemonStatus,
    status_json: &mut serde_json::Value,
    broadcast_tx: &tokio::sync::broadcast::Sender<PatchEvent>,
) {
    let Ok(json_new) = serde_json::to_value(&new_status) else {
        warn!("Unable to serialise the status");
        return;
    };
    let patch = diff(status_json, &json_new);
    if !patch.0.is_empty() {
        let _ = broadcast_tx.send(PatchEvent { data: patch });
    }
    *status_json = json_new;
    *daemon_status = new_status;
}

// Whether autostart is set looks at the disk (shortcut, task, desktop file): it's checked at
// most every few seconds, not on each status rebuild, and again right after it's changed.
static AUTOSTART: std::sync::Mutex<Option<(Instant, bool)>> = std::sync::Mutex::new(None);

fn autostart_enabled() -> bool {
    let Ok(mut cached) = AUTOSTART.lock() else {
        return has_autostart();
    };
    match *cached {
        Some((checked, value)) if checked.elapsed() < Duration::from_secs(5) => value,
        _ => {
            let value = has_autostart();
            *cached = Some((Instant::now(), value));
            value
        }
    }
}

fn forget_autostart() {
    if let Ok(mut cached) = AUTOSTART.lock() {
        *cached = None;
    }
}

// Dis getting big..
#[allow(clippy::too_many_arguments)]
async fn get_daemon_status(
    mixers: &HashMap<String, MixerStatus>,
    settings: &SettingsHandle,
    http_settings: &HttpSettings,
    driver_details: &DriverDetails,
    firmware_versions: &Option<EnumMap<DeviceType, Option<FirmwareDetails>>>,
    firmware_state: &HashMap<String, FirmwareUpdateState>,
    files: Files,
    app_check: &Option<String>,
) -> DaemonStatus {
    let mut status = DaemonStatus {
        config: DaemonConfig {
            http_settings: http_settings.clone(),
            daemon_version: String::from(VERSION),
            driver_interface: driver_details.clone(),
            latest_firmware: firmware_versions.clone(),
            locale: Locale {
                user_locale: settings.get_selected_locale().await,
                system_locale: SYSTEM_LOCALE.clone(),
            },
            autostart_enabled: autostart_enabled(),
            show_tray_icon: settings.get_show_tray_icon().await,
            tts_enabled: settings.get_tts_enabled().await,
            allow_network_access: settings.get_allow_network_access().await,
            log_level: settings.get_log_level().await,
            firmware_source: settings.get_firmware_source().await,
            open_ui_on_launch: settings.get_open_ui_on_launch().await,
            activation: Activation {
                active_path: settings.get_activate().await,
                app_path: app_check.clone(),
            },
            platform: env::consts::OS.to_string(),
            handle_macos_aggregates: settings.get_macos_handle_aggregates().await,
        },
        paths: Paths {
            profile_directory: settings.get_profile_directory().await,
            mic_profile_directory: settings.get_mic_profile_directory().await,
            samples_directory: settings.get_samples_directory().await,
            presets_directory: settings.get_presets_directory().await,
            icons_directory: settings.get_icons_directory().await,
            logs_directory: settings.get_log_directory().await,
        },
        files,
        ..Default::default()
    };

    for (serial, state) in firmware_state {
        status
            .firmware
            .insert(serial.to_owned(), state.status.clone());
    }

    for (serial, mixer) in mixers {
        status.mixers.insert(serial.to_owned(), mixer.clone());
    }

    crate::tray::update_tray_state(&status);
    status
}

#[allow(const_item_mutation)]
fn get_app_path(app_check: &mut Option<String>) -> bool {
    if let Some(path) = get_ui_app_path() {
        let mut changed = false;

        // We need to escape the value..
        let wrap = if cfg!(windows) { "\"" } else { "'" };
        let path = format!("{}{}{}", wrap, path.to_string_lossy(), wrap);

        if let Some(old_path) = app_check {
            if *old_path != path {
                app_check.replace(path);
                changed = true;
            }
        } else {
            app_check.replace(path);
            changed = true
        }

        changed
    } else {
        let mut changed = false;
        if app_check.is_some() {
            changed = true;
        }
        app_check.take();
        changed
    }
}

/// A sample file was renamed. Profiles reference samples by file name, so update them all: the
/// ones loaded on devices and the ones saved on disk, and keep the sample's gain.
/// What a background sample job changed, applied by the worker loop once it's done.
#[derive(Default)]
struct SampleJobOutput {
    // A sample file renamed: loaded profiles and the gain setting follow it
    renamed: Option<RenamedSample>,
    // Sampler tracks to put back (a sample restored from the trash)
    tracks: Vec<TrashedTrack>,
    // A copy saved by the editor: (original name, copy name), the copy gets the same gain
    copied_gain: Option<(String, String)>,
    // A sample edited in place: (its name, its new gain), for the tracks loaded on the devices
    edited: Option<(String, f64)>,
}

struct SampleJobDone {
    // The sample being edited, if the job holds one
    key: Option<String>,
    sender: oneshot::Sender<Result<()>>,
    result: Result<SampleJobOutput>,
}

/// Runs slow sample work (file I/O, audio decoding, saved profiles) on a blocking thread, so the
/// worker loop keeps driving the devices; the result is applied when it comes back.
fn spawn_sample_job<F>(
    tx: mpsc::Sender<SampleJobDone>,
    key: Option<String>,
    sender: oneshot::Sender<Result<()>>,
    job: F,
) where
    F: FnOnce() -> Result<SampleJobOutput> + Send + 'static,
{
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(job)
            .await
            .unwrap_or_else(|e| Err(anyhow!("The sample operation failed: {}", e)));
        let _ = tx
            .send(SampleJobDone {
                key,
                sender,
                result,
            })
            .await;
    });
}

/// The gain that brings a sample to the sampler's loudness, as when it's added to a button.
fn measure_gain(file: &Path) -> Option<f64> {
    let mut player =
        goxlr_audio::player::Player::new(&file.to_path_buf(), None, None, None, None, None).ok()?;
    let state = player.get_state();
    player.calculate_gain();
    if state.error.lock().ok()?.is_some() {
        return None;
    }
    Some(
        state
            .calculated_gain
            .load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Drops the sampler tracks whose file is gone, on every device.
fn validate_samplers(devices: &HashMap<String, DeviceActor>) {
    for device in devices.values() {
        device.send(|d| {
            Box::pin(async move {
                let _ = d.validate_sampler().await;
                true
            })
        });
    }
}

/// Points the saved profiles at a renamed sample file (slow: every profile is read).
fn rename_in_saved_profiles(profiles: &Path, backups: &Path, renamed: &RenamedSample) {
    let changed =
        rename_sample_in_saved_profiles(profiles, backups, &renamed.old_name, &renamed.new_name);
    info!(
        "Renamed sample {} to {}, updated profiles: {:?}",
        renamed.old_name, renamed.new_name, changed
    );
}

// Sample durations, kept while a file keeps the same size and modification time, so a rescan
// doesn't read every file again.
type DurationCache = HashMap<PathBuf, (u64, Option<std::time::SystemTime>, Option<f32>)>;
static SAMPLE_DURATIONS: std::sync::Mutex<Option<DurationCache>> = std::sync::Mutex::new(None);

fn sample_size_and_duration(path: &Path) -> (u64, Option<f32>) {
    let Ok(metadata) = fs::metadata(path) else {
        return (0, None);
    };
    let (size, modified) = (metadata.len(), metadata.modified().ok());
    if let Ok(cache) = SAMPLE_DURATIONS.lock()
        && let Some(cache) = cache.as_ref()
        && let Some((cached_size, cached_modified, duration)) = cache.get(path)
        && *cached_size == size
        && *cached_modified == modified
    {
        return (size, *duration);
    }
    let duration = probe_duration(path).map(|d| d as f32);
    if let Ok(mut cache) = SAMPLE_DURATIONS.lock() {
        cache
            .get_or_insert_with(HashMap::new)
            .insert(path.to_path_buf(), (size, modified, duration));
    }
    (size, duration)
}

async fn get_sample_files(
    file_manager: &mut FileManager,
    settings: &SettingsHandle,
) -> BTreeMap<String, SampleFile> {
    let file_samples = file_manager.get_samples();
    let config_samples = settings.get_sample_gain_list().await;
    let base = settings.get_samples_directory().await;

    // We need to pair the two together, starting with the file samples..
    let mut samples: BTreeMap<String, SampleFile> = Default::default();
    for (key, value) in file_samples {
        let mut gain = 100;

        if let Some(config_gain) = config_samples.get(&*value) {
            gain = *config_gain;
        }

        let path = base.join(&key);
        let (size, duration) = sample_size_and_duration(&path);
        samples.insert(
            key,
            SampleFile {
                name: value,
                gain_pct: gain,
                size,
                duration,
            },
        );
    }
    samples
}

async fn get_files(file_manager: &mut FileManager, settings: &SettingsHandle) -> Files {
    Files {
        profiles: file_manager.get_profiles(),
        mic_profiles: file_manager.get_mic_profiles(),
        presets: file_manager.get_presets(),
        samples: get_sample_files(file_manager, settings).await,
        sample_folders: file_manager.get_sample_folders(),
        sample_trash: get_sample_trash(settings).await,
        profile_trash: get_profile_trash(settings).await,
        icons: file_manager.get_icons(),
    }
}

/// The links other machines use: the interface on each address of this machine, with the token.
fn network_access(http: &HttpSettings, token: Option<String>) -> NetworkAccess {
    let enabled = http.enabled
        && http
            .bind_address
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| !ip.is_loopback());
    let mut urls = vec![];
    if enabled && let Some(token) = token {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let mut addresses: BTreeSet<String> = BTreeSet::new();
        for (_, network) in &networks {
            for ip in network.ip_networks() {
                if ip.addr.is_ipv4() && !ip.addr.is_loopback() && !ip.addr.is_unspecified() {
                    addresses.insert(ip.addr.to_string());
                }
            }
        }
        for address in addresses {
            urls.push(format!("http://{}:{}/?token={}", address, http.port, token));
        }
    }
    NetworkAccess { enabled, urls }
}

async fn get_profile_trash(settings: &SettingsHandle) -> Vec<ProfileTrashItem> {
    let backups = settings.get_backup_directory().await;
    purge_profile_trash(&backups);
    list_profile_trash(&backups)
}

async fn get_sample_trash(settings: &SettingsHandle) -> Vec<SampleTrashItem> {
    let backups = settings.get_backup_directory().await;
    purge_trash(&backups);
    list_trash(&backups)
}

async fn update_files(
    files: Files,
    file_type: PathTypes,
    file_manager: &mut FileManager,
    settings: &SettingsHandle,
) -> Files {
    // Only re-poll for the changed type.
    Files {
        profiles: if file_type != PathTypes::Profiles {
            files.profiles
        } else {
            file_manager.get_profiles()
        },

        mic_profiles: if file_type != PathTypes::MicProfiles {
            files.mic_profiles
        } else {
            file_manager.get_mic_profiles()
        },

        presets: if file_type != PathTypes::Presets {
            files.presets
        } else {
            file_manager.get_presets()
        },

        samples: if file_type != PathTypes::Samples {
            files.samples
        } else {
            get_sample_files(file_manager, settings).await
        },

        sample_folders: if file_type != PathTypes::Samples {
            files.sample_folders
        } else {
            file_manager.get_sample_folders()
        },

        sample_trash: if file_type != PathTypes::Samples {
            files.sample_trash
        } else {
            get_sample_trash(settings).await
        },

        profile_trash: if file_type != PathTypes::Profiles && file_type != PathTypes::MicProfiles {
            files.profile_trash
        } else {
            get_profile_trash(settings).await
        },

        icons: if file_type != PathTypes::Icons {
            files.icons
        } else {
            file_manager.get_icons()
        },
    }
}

fn find_new_device(
    current_status: &DaemonStatus,
    devices_to_ignore: &HashMap<DeviceKey, (Instant, u32)>,
) -> Option<GoXLRDevice> {
    let now = Instant::now();

    let goxlr_devices = find_devices();
    goxlr_devices.into_iter().find(|device| {
        // Check the Mixers on the existing DaemonStatus..
        !current_status.mixers.values().any(|d| {
            if let Some(identifier) = device.identifier()
                && let Some(device_identifier) = &d.hardware.usb_device.identifier
            {
                return identifier.clone() == device_identifier.clone();
            }

            d.hardware.usb_device.bus_number == device.bus_number()
                && d.hardware.usb_device.address == device.address()
        }) && !devices_to_ignore
            .iter()
            .any(|((bus_number, address, identifier), (expires, _))| {
                if let Some(identifier) = identifier
                    && let Some(device_identifier) = device.identifier()
                {
                    return identifier == device_identifier && expires > &now;
                }

                *bus_number == device.bus_number() && *address == device.address() && expires > &now
            })
    })
}

fn get_all_serials<T>(existing_devices: &HashMap<String, T>) -> Vec<String> {
    let mut serials: Vec<String> = vec![];

    for device in existing_devices {
        serials.push(device.0.clone());
    }

    serials
}

/// Opens a device and reads what identifies it. This blocks for a few seconds (the device is
/// given time to settle, then several USB requests are made), so it runs on a blocking thread.
fn attach_device(
    device: GoXLRDevice,
    existing_serials: Vec<String>,
    disconnect_sender: Sender<String>,
    event_sender: Sender<String>,
) -> Result<(Box<dyn FullGoXLRDevice>, HardwareStatus)> {
    let device_copy = device.clone();

    let mut handled_device = from_device(device, disconnect_sender, event_sender, false)?;
    let descriptor = handled_device.get_descriptor()?;

    let device_type = match descriptor.product_id() {
        PID_GOXLR_FULL => DeviceType::Full,
        PID_GOXLR_MINI => DeviceType::Mini,
        _ => DeviceType::Unknown,
    };
    let device_version = descriptor.device_version();
    let version = (device_version.0, device_version.1, device_version.2);
    let usb_device = UsbProductInformation {
        manufacturer_name: descriptor.device_manufacturer(),
        product_name: descriptor.product_name(),
        bus_number: device_copy.bus_number(),
        address: device_copy.address(),
        identifier: device_copy.identifier().clone(),
        version,
    };
    let (mut serial_number, manufactured_date) = handled_device.get_serial_number()?;
    if serial_number.is_empty() {
        let mut serial = String::from("");
        for i in 0..=24 {
            serial = format!("UNKNOWN-SN-{i}");
            if !existing_serials.contains(&serial) {
                break;
            }
        }

        warn!(
            "This GoXLR isn't reporting a serial number, this may cause issues if you're running with multiple devices."
        );
        serial_number = serial;
        warn!("Generated Internal Serial Number: {}", serial_number);
    }
    handled_device.set_unique_identifier(serial_number.clone());

    let colour_way = if serial_number.ends_with("AAI") || serial_number.ends_with("3AA") {
        ColourWay::White
    } else {
        ColourWay::Black
    };

    let hardware = HardwareStatus {
        versions: handled_device.get_firmware_version()?,
        serial_number: serial_number.clone(),
        manufactured_date,
        device_type,
        colour_way,
        usb_device,
    };
    Ok((handled_device, hardware))
}

/// Sets up an attached device: loads its profiles and applies them to the hardware.
async fn load_device(
    handled_device: Box<dyn FullGoXLRDevice>,
    hardware: HardwareStatus,
    global_events: Sender<EventTriggers>,
    settings: &SettingsHandle,
) -> Result<Device> {
    let serial_number = hardware.serial_number.clone();
    let device = Device::new(handled_device, hardware, settings, global_events).await?;
    settings
        .set_device_profile_name(&serial_number, device.profile().name())
        .await;
    settings
        .set_device_mic_profile_name(&serial_number, device.mic_profile().name())
        .await;
    settings.save().await;
    Ok(device)
}

type FwSender = Sender<EnumMap<DeviceType, Option<FirmwareDetails>>>;
async fn check_firmware_versions(x: FwSender, source: FirmwareSource) {
    let full_key = "version";
    let mini_key = "miniVersion";

    let full_changelog = "GoXLRFirmwareUpdateInfo";
    let mini_changelog = "MiniFirmwareUpdateInfo";

    let mut map: EnumMap<DeviceType, Option<FirmwareDetails>> = EnumMap::default();

    let client = if let Ok(client) = ClientBuilder::new()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(1)
        .build()
    {
        client
    } else {
        // This shouldn't happen, but just in case
        warn!("Unable to Build HTTP Client");
        return;
    };

    debug!("Performing Firmware Version Check..");
    let url = format!("{}{}", FIRMWARE_PATHS[source], "UpdateManifest_v3.xml");
    if let Ok(response) = client.get(url).send().await {
        if response.status() != StatusCode::OK {
            warn!(
                "Firmware Version Check Failed, Invalid Response Code: {}",
                response.status()
            );
            let _ = x.send(map).await;
            return;
        }

        if let Ok(text) = response.text().await {
            // Parse this into an XML tree...
            if let Ok(root) = Element::parse(text.as_bytes()) {
                if root.attributes.contains_key(mini_key) {
                    let change_log = if root.attributes.contains_key(mini_changelog) {
                        Some(root.attributes[mini_changelog].clone())
                    } else {
                        None
                    };
                    map[DeviceType::Mini] = Some(FirmwareDetails {
                        version: VersionNumber::from(root.attributes[mini_key].clone()),
                        change_log,
                    });
                } else {
                    warn!("{mini_key} node not found in result");
                }

                // We can probably abstract this slightly, the mini and full behaviours are the same.
                if root.attributes.contains_key(full_key) {
                    let change_log = if root.attributes.contains_key(full_changelog) {
                        Some(root.attributes[full_changelog].clone())
                    } else {
                        None
                    };
                    map[DeviceType::Full] = Some(FirmwareDetails {
                        version: VersionNumber::from(root.attributes[full_key].clone()),
                        change_log,
                    });
                } else {
                    warn!("{full_key} node not found in result");
                }
            } else {
                warn!("Unable to Parse the XML Response from the TC-Helicon Update Server");
            }
        } else {
            warn!("Failed to Fetch a Response from the TC-Helicon Update Server");
        }
    } else {
        warn!("Unable to connect to the TC-Helicon Update Server");
    }

    debug!("Firmware Update Process Finished");
    let _ = x.send(map).await;
}

/// Removes a firmware file the interface uploaded (to the temporary folder, named by the HTTP
/// server), once its update is done or cleared. A file picked on disk is never touched.
fn remove_uploaded_firmware(file: &Option<PathBuf>) {
    let Some(file) = file else {
        return;
    };
    let uploaded = file.parent() == Some(std::env::temp_dir().as_path())
        && file
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("goxlr-firmware-"));
    if uploaded && let Err(e) = fs::remove_file(file) {
        warn!(
            "Unable to remove the uploaded firmware {}: {}",
            file.display(),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_quickly_then_backs_off() {
        let delays: Vec<u64> = (1..=6).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, vec![1, 2, 5, 10, 10, 10]);
    }
}
