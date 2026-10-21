use crate::FIRMWARE_PATHS;
use crate::firmware::device_hash::device_hash;
use crate::firmware::firmware_file::check_firmware;
use anyhow::{Result, bail};
use futures_util::StreamExt;
use goxlr_ipc::{FirmwareInfo, FirmwareSource, UpdateState};
use goxlr_types::{DeviceType, VersionNumber};
use log::{error, info, warn};
use reqwest::ClientBuilder;
use std::cmp::min;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::time::sleep;
use xmltree::Element;

type Sender = tokio::sync::mpsc::Sender<FirmwareRequest>;
type OneShot<T> = oneshot::Sender<T>;

const FAIL_BACK_FULL_FIRMWARE: &str = "GoXLR_firmware.bin";
const FAIL_BACK_MINI_FIRMWARE: &str = "GoXLR_MINI_firmware.bin";

#[derive(Clone)]
pub struct FirmwareUpdateSettings {
    pub sender: Sender,
    pub device: FirmwareUpdateDevice,
    pub file: Option<PathBuf>,
    pub force: bool,
}

#[derive(Clone)]
pub struct FirmwareUpdateDevice {
    pub serial: String,
    pub device_type: DeviceType,
    pub current_firmware: VersionNumber,
    pub source: FirmwareSource,
}

pub async fn start_firmware_update(settings: FirmwareUpdateSettings) {
    info!("Beginning Firmware Update...");
    let sender = settings.sender.clone();
    let device = settings.device.clone();

    if let Err(e) = set_update_state(&device.serial, sender.clone(), UpdateState::Starting).await {
        error!("Something's gone horribly wrong: {}", e);
        return;
    }

    // First thing we need to do, is grab and validate the file..
    let file_info = match &settings.file {
        None => download_firmware(&device, sender.clone()).await,
        Some(path) => check_firmware(path),
    };

    let file_info = match file_info {
        Ok(file) => file,
        Err(e) => {
            send_error(&device.serial, sender, e.to_string()).await;
            return;
        }
    };

    if !file_info.path.exists() {
        let error = String::from("File does not exist");
        send_error(&device.serial, sender, error).await;
        return;
    }

    if file_info.device_type != device.device_type {
        let error = String::from("Firmware is not compatible with the device");
        send_error(&device.serial, sender, error).await;
        return;
    }

    // So we go either one of two ways here, if there's a problem, we set the update as 'Paused' with
    // the file_info, and the UI can then send a 'Continue' if the user is happy with the info, otherwise
    // we just go with the update. A file provided by the user (rather than downloaded from the
    // official source) always needs that confirmation, so a request can't silently flash one.
    let user_file = settings.file.is_some();
    if !settings.force && (user_file || file_info.version <= device.current_firmware) {
        warn!(
            "Pausing, File: {}, Current: {}",
            file_info.version, device.current_firmware
        );
        let _ = set_update_state(&device.serial, sender, UpdateState::Pause(file_info)).await;
    } else {
        info!("Downloaded firmware is newer than current, proceeding without prompt..");
        do_firmware_update(settings, file_info).await;
    }
}

pub async fn do_firmware_update(settings: FirmwareUpdateSettings, file_info: FirmwareInfo) {
    let sender = settings.sender.clone();
    let device = settings.device;

    // Ok, when we get here we should be good to go, grab the firmware bytes from disk..
    let firmware = match fs::read(&file_info.path) {
        Ok(bytes) => bytes,
        Err(e) => {
            let error = format!("Unable to load firmware from disk: {e}");
            send_error(&device.serial, sender, error).await;
            return;
        }
    };
    // Flashed from memory from here: a downloaded file isn't needed any more
    remove_downloaded_firmware(&file_info.path);

    info!("Entering DFU Mode");
    if let Err(e) = enter_dfu(&device.serial, sender.clone()).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Clearing NVR");
    if let Err(e) = clear_nvr(&device.serial, sender.clone()).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Uploading Firmware..");
    if let Err(e) = upload_firmware(&device.serial, sender.clone(), &firmware).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Validating Upload");
    if let Err(e) = validate_upload(&device.serial, sender.clone(), &firmware).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Hardware Validation");
    if let Err(e) = hardware_verify(&device.serial, sender.clone()).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Hardware Writing");
    if let Err(e) = hardware_write(&device.serial, sender.clone()).await {
        send_error(&device.serial, sender.clone(), e.to_string()).await;
        reboot_goxlr(&device.serial, sender).await;
        return;
    }

    info!("Rebooting GoXLR");
    let _ = set_update_state(&device.serial, sender.clone(), UpdateState::Complete).await;
    reboot_goxlr(&device.serial, sender.clone()).await;
}

/// What the manifest says to download: the file, its version, and its SHA-256 when the
/// manifest gives one.
struct ManifestEntry {
    file: String,
    version: VersionNumber,
    sha256: Option<String>,
}

async fn get_firmware_file(device: &FirmwareUpdateDevice, sender: Sender) -> Result<ManifestEntry> {
    set_update_state(&device.serial, sender.clone(), UpdateState::Manifest).await?;

    // Firstly, grab some variables depending on device..
    let file_key = match device.device_type {
        DeviceType::Unknown => bail!("Unknown Device Type"),
        DeviceType::Full => "fwFullFileName",
        DeviceType::Mini => "fwMiniFileName",
    };
    let version_key = match device.device_type {
        DeviceType::Unknown => bail!("Unknown Device Type"),
        DeviceType::Full => "version",
        DeviceType::Mini => "miniVersion",
    };
    let sha_key = match device.device_type {
        DeviceType::Unknown => bail!("Unknown Device Type"),
        DeviceType::Full => "fwFullSha256",
        DeviceType::Mini => "fwMiniSha256",
    };
    let fail_back_path = match device.device_type {
        DeviceType::Unknown => bail!("Unknown Device Type"),
        DeviceType::Full => FAIL_BACK_FULL_FIRMWARE,
        DeviceType::Mini => FAIL_BACK_MINI_FIRMWARE,
    };

    let manifest_url = format!(
        "{}{}",
        FIRMWARE_PATHS[device.source], "UpdateManifest_v3.xml"
    );

    // We need to find out if the manifest has a path to the firmware file, otherwise we'll fall
    // back to 'Legacy' behaviour. Note that we're not going to track the percentage on this
    // download, as the manifest file is generally tiny.
    info!("Downloading Firmware Metadata from {}", manifest_url);
    let client = ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;

    if let Ok(response) = client.get(manifest_url).send().await
        && let Ok(text) = response.text().await
        && let Ok(root) = Element::parse(text.as_bytes())
    {
        let version = if root.attributes.contains_key(version_key) {
            VersionNumber::from(root.attributes[version_key].clone())
        } else {
            bail!("Unable to obtain Firmware Version");
        };

        let file = root
            .attributes
            .get(file_key)
            .cloned()
            .unwrap_or_else(|| String::from(fail_back_path));
        let sha256 = root
            .attributes
            .get(sha_key)
            .map(|sha| sha.trim().to_lowercase());
        return Ok(ManifestEntry {
            file,
            version,
            sha256,
        });
    }
    bail!("Error Downloading Manifest from TC-Helicon Servers");
}

async fn download_firmware(device: &FirmwareUpdateDevice, sender: Sender) -> Result<FirmwareInfo> {
    // First thing we're going to do, is determine which file to download
    let entry = get_firmware_file(device, sender.clone()).await?;

    // Now we'll grab and process that file
    set_update_state(&device.serial, sender.clone(), UpdateState::Download).await?;
    // The name comes from the server's manifest: only its last part is used on disk, and a
    // path leaving the firmware folder on the server is refused.
    let remote = entry.file.replace('\\', "/");
    if remote.starts_with('/')
        || remote
            .split('/')
            .any(|part| part == ".." || part.contains(':'))
    {
        bail!(
            "Unexpected firmware file name in the manifest: {}",
            entry.file
        );
    }
    let Some(local_name) = remote.rsplit('/').next().filter(|name| !name.is_empty()) else {
        bail!(
            "Unexpected firmware file name in the manifest: {}",
            entry.file
        );
    };
    let url = format!("{}{}", FIRMWARE_PATHS[device.source], remote);
    let output_path = private_download_dir()?.join(local_name);

    info!(
        "Downloading Firmware, URL: {}, Expected Version: {}",
        url, entry.version
    );

    if output_path.exists() && fs::remove_file(&output_path).is_err() {
        bail!("Error Cleaning old firmware");
    }

    let mut file = File::create(&output_path)?;
    let mut last_percent = 0;

    // A server that stops sending must not leave the update in Download for ever.
    let client = ClientBuilder::new()
        .connect_timeout(Duration::from_secs(2))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let res = client.get(url).send().await?;
    if let Some(size) = res.content_length() {
        let mut downloaded: u64 = 0;
        let mut stream = res.bytes_stream();

        while let Some(bytes) = stream.next().await {
            let bytes = bytes?;
            file.write_all(&bytes)?;

            let new = min(downloaded + (bytes.len() as u64), size);
            downloaded = new;

            let percent = ((downloaded as f32 / size as f32) * 100.) as u8;
            if percent != last_percent {
                set_update_stage_percent(&device.serial, sender.clone(), percent).await?;
                last_percent = percent;
            }
        }

        // A download cut short (the server closed early) is never used
        file.flush()?;
        if downloaded != size {
            bail!("Firmware download incomplete ({downloaded} of {size} bytes)");
        }
        if let Some(expected) = &entry.sha256 {
            let actual = sha256_of(&output_path)?;
            if &actual != expected {
                bail!("Downloaded Firmware doesn't match the manifest (SHA-256 {actual})");
            }
            info!("Firmware SHA-256 matches the manifest");
        }

        let firmware_info = check_firmware(&output_path)?;
        if firmware_info.version != entry.version {
            bail!(
                "Downloaded Firmware version does not match expected firmware (Received: {}, Expected: {})",
                firmware_info.version,
                entry.version
            );
        }

        if let Ok(data) = firmware_info.path.metadata() {
            info!(
                "Download complete, file: {}, size: {}",
                firmware_info.path.to_string_lossy(),
                data.len()
            );
        } else {
            info!(
                "Download complete, file: {}, size: unknown",
                firmware_info.path.to_string_lossy()
            );
        }
        return Ok(firmware_info);
    }
    bail!("Error Downloading content from TC-Helicon Servers");
}

async fn enter_dfu(serial: &str, sender: Sender) -> Result<()> {
    // Put the device into DFU mode..
    let (oneshot, receiver) = oneshot::channel();
    let message = FirmwareMessages::EnterDFUMode(oneshot);
    let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

    sender.send(message).await?;
    receiver.await?
}

async fn clear_nvr(serial: &str, sender: Sender) -> Result<()> {
    set_update_state(serial, sender.clone(), UpdateState::ClearNVR).await?;

    let (oneshot, receiver) = oneshot::channel();
    let message = FirmwareMessages::BeginEraseNVR(oneshot);
    let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

    sender.send(message).await?;
    receiver.await??;

    // Now we simply sit, wait, and update until we're done.
    let mut last_percent = 0_u8;
    let mut progress = 0;
    let deadline = Instant::now() + STEP_DEADLINE;
    while progress != 255 {
        sleep(Duration::from_millis(100)).await;
        check_deadline(deadline, "clearing the firmware memory")?;

        let (oneshot, receiver) = oneshot::channel();
        let message = FirmwareMessages::PollEraseNVR(oneshot);
        let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

        sender.send(message).await?;
        progress = receiver.await??.progress;
        let percent = ((progress as f32 / 255.) * 100.) as u8;
        if percent != last_percent {
            last_percent = percent;
            set_update_stage_percent(serial, sender.clone(), percent).await?;
        }
    }

    set_update_stage_percent(serial, sender.clone(), 100).await
}

async fn upload_firmware(serial: &str, sender: Sender, bytes: &[u8]) -> Result<()> {
    set_update_state(serial, sender.clone(), UpdateState::UploadFirmware).await?;

    // Monitor the current percentage
    let mut last_percent = 0_u8;

    // Chunk size is set to 1012, as it's 1024 - header (12 bytes)
    let chunk_size = 1012;

    // Monitoring how much data has been sent
    let mut sent: u64 = 0;

    for chunk in bytes.chunks(chunk_size) {
        let (oneshot, receiver) = oneshot::channel();

        let message = FirmwareMessages::UploadFirmwareChunk(
            UploadFirmwareChunkRequest {
                total_byes_sent: sent,
                chunk: Vec::from(chunk),
            },
            oneshot,
        );
        let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

        sender.send(message).await?;
        receiver.await??;

        sent += chunk.len() as u64;
        let percent = ((sent as f32 / bytes.len() as f32) * 100.) as u8;
        if percent != last_percent {
            last_percent = percent;
            set_update_stage_percent(serial, sender.clone(), percent).await?;
        }
    }

    Ok(())
}

async fn validate_upload(serial: &str, sender: Sender, firmware: &[u8]) -> Result<()> {
    set_update_state(serial, sender.clone(), UpdateState::ValidateUpload).await?;
    let firmware_size = firmware.len() as u32;

    let mut chunks = 0;

    let mut last_percent = 0;

    let mut processed_bytes = 0;
    let mut remaining_bytes = firmware_size;
    let mut hash_in = 0_u32;

    while remaining_bytes > 0 {
        let (oneshot, receiver) = oneshot::channel();

        let message = FirmwareMessages::ValidateUploadChunk(
            ValidateUploadChunkRequest {
                processed_bytes,
                hash_in,
                remaining_bytes,
            },
            oneshot,
        );
        let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

        sender.send(message).await?;
        let response = receiver.await??;

        // Nothing validated would loop for ever
        if response.count == 0 {
            bail!("Error Validating Firmware, the device validated nothing");
        }
        if processed_bytes + response.count > firmware_size {
            bail!("Error Validating Firmware, Length Mismatch");
        }
        let chunk =
            &firmware[processed_bytes as usize..(processed_bytes + response.count) as usize];
        // The device's hash of what it received, against the one of the file: different bytes
        // stop the update here, before anything is written.
        let expected = device_hash(hash_in, chunk);
        if response.hash != expected {
            bail!(
                "Error Validating Firmware, the device received different data at byte {} (hash {:08x}, expected {:08x})",
                processed_bytes,
                response.hash,
                expected
            );
        }
        chunks += 1;

        processed_bytes += response.count;

        hash_in = response.hash;
        remaining_bytes -= response.count;

        let percent = ((processed_bytes as f32 / firmware_size as f32) * 100.) as u8;
        if percent != last_percent {
            last_percent = percent;
            set_update_stage_percent(serial, sender.clone(), percent).await?;
        }
    }

    info!(
        "Firmware upload validated: {} chunks, hash {:08x} as computed here",
        chunks, hash_in
    );

    Ok(())
}

async fn hardware_verify(serial: &str, sender: Sender) -> Result<()> {
    set_update_state(serial, sender.clone(), UpdateState::HardwareValidate).await?;
    let mut last_percent = 0_u8;

    let (oneshot, receiver) = oneshot::channel();
    let message = FirmwareMessages::BeginHardwareVerify(oneshot);
    let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

    sender.send(message).await?;
    receiver.await??;

    let mut complete = false;
    let deadline = Instant::now() + STEP_DEADLINE;
    while !complete {
        // Leave the device's USB link to the other requests between polls
        sleep(Duration::from_millis(100)).await;
        check_deadline(deadline, "verifying the firmware")?;
        let (oneshot, receiver) = oneshot::channel();
        let message = FirmwareMessages::PollHardwareVerify(oneshot);
        let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

        sender.send(message).await?;
        let response = receiver.await??;

        complete = response.completed;

        let processed = response.processed_bytes;
        let total = response.total_bytes;

        let percent = percent_of(processed as u64, total as u64);
        if percent != last_percent {
            last_percent = percent;
            set_update_stage_percent(serial, sender.clone(), percent).await?;
        }
    }
    Ok(())
}

async fn hardware_write(serial: &str, sender: Sender) -> Result<()> {
    set_update_state(serial, sender.clone(), UpdateState::HardwareWrite).await?;
    let mut last_percent = 0_u8;

    let (oneshot, receiver) = oneshot::channel();
    let message = FirmwareMessages::BeginHardwareWrite(oneshot);
    let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

    sender.send(message).await?;
    receiver.await??;

    let mut complete = false;
    let deadline = Instant::now() + STEP_DEADLINE;
    while !complete {
        // Leave the device's USB link to the other requests between polls
        sleep(Duration::from_millis(100)).await;
        check_deadline(deadline, "writing the firmware")?;
        let (oneshot, receiver) = oneshot::channel();
        let message = FirmwareMessages::PollHardwareWrite(oneshot);
        let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

        sender.send(message).await?;
        let response = receiver.await??;

        complete = response.completed;

        let processed = response.processed_bytes;
        let total = response.total_bytes;

        let percent = percent_of(processed as u64, total as u64);
        if percent != last_percent {
            last_percent = percent;
            set_update_stage_percent(serial, sender.clone(), percent).await?;
        }
    }
    Ok(())
}

/// Removes a firmware this daemon downloaded, with its private folder (see private_download_dir).
/// A file picked or uploaded by the user isn't one.
pub fn remove_downloaded_firmware(path: &std::path::Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let downloaded = dir.parent() == Some(std::env::temp_dir().as_path())
        && dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("goxlr-firmware-"));
    if downloaded && let Err(e) = fs::remove_dir_all(dir) {
        warn!(
            "Unable to remove the downloaded firmware {}: {}",
            dir.display(),
            e
        );
    }
}

/// A new folder only this user can use, for the downloaded firmware: the shared temporary
/// folder would let another user swap the file between its check and the flash.
fn private_download_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..100 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = base.join(format!(
            "goxlr-firmware-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("Unable to create a folder for the firmware download");
}

// No firmware step should take this long; past it, the update fails rather than wait for ever.
const STEP_DEADLINE: Duration = Duration::from_secs(10 * 60);

fn check_deadline(deadline: Instant, step: &str) -> Result<()> {
    if Instant::now() > deadline {
        bail!("The device took too long {}", step);
    }
    Ok(())
}

fn percent_of(processed: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((processed.min(total) as f32 / total as f32) * 100.) as u8
}

async fn reboot_goxlr(serial: &str, sender: Sender) {
    let (oneshot, receiver) = oneshot::channel();
    let message = FirmwareMessages::RebootGoXLR(oneshot);
    let message = FirmwareRequest::FirmwareMessage(serial.to_owned(), message);

    // We're basically up shits creek if this breaks, there's nothing we can do to recover.
    if let Err(e) = sender.send(message).await {
        error!("Unable to Reboot GoXLR: {}", e);
    };

    match receiver.await {
        Ok(resp) => {
            if let Err(e) = resp {
                error!("Unable to Reboot the Device: {}", e);
            }
        }
        Err(e) => error!("Unable to Read Receiver: {}", e),
    }
}

async fn send_error(serial: &str, sender: Sender, error: String) {
    let message = FirmwareRequest::SetUpdateState(serial.to_owned(), UpdateState::Failed);
    error!("Error Received: {}", error);

    if let Err(e) = sender.send(message).await {
        error!("Error Updating State: {}", e);
    }
    let message = FirmwareRequest::SetError(serial.to_owned(), error);
    if let Err(e) = sender.send(message).await {
        error!("Error Setting Error: {}", e);
    }
}

async fn set_update_state(serial: &str, sender: Sender, state: UpdateState) -> Result<()> {
    let message = FirmwareRequest::SetUpdateState(serial.to_owned(), state);
    sender.send(message).await.map_err(anyhow::Error::msg)
}

async fn set_update_stage_percent(serial: &str, sender: Sender, percent: u8) -> Result<()> {
    let message = FirmwareRequest::SetStateProgress(serial.to_owned(), percent);
    sender.send(message).await.map_err(anyhow::Error::msg)
}

// No responses needed for:
// begin_erase_nvr
// send_firmware_packet
// verify_firmware_status - TODO: being_hardware_verify
// finalise_firmware_upload - TODO: begin_hardware_write

// Used for 'poll_erase_nvr'
pub struct ProgressResponse {
    pub progress: u8,
}

// Used for 'send_firmware_packet'
pub struct UploadFirmwareChunkRequest {
    pub total_byes_sent: u64,
    pub chunk: Vec<u8>,
}

// Used for 'validate_firmware_packet' -> validate_upload_chunk
pub struct ValidateUploadChunkRequest {
    pub processed_bytes: u32,
    pub hash_in: u32,
    pub remaining_bytes: u32,
}

pub struct ValidateUploadChunkResponse {
    pub hash: u32,
    pub count: u32,
}

// poll_verify_firmware_status, TODO: -> poll_hardware_verify
// poll_finalise_firmware_upload, TODO: -> poll_hardware_write
pub struct HardwareProgressResponse {
    pub completed: bool,
    pub total_bytes: u32,
    pub processed_bytes: u32,
}

pub enum FirmwareRequest {
    SetUpdateState(String, UpdateState),
    SetStateProgress(String, u8),
    SetError(String, String),
    FirmwareMessage(String, FirmwareMessages),
}

pub enum FirmwareMessages {
    EnterDFUMode(OneShot<Result<()>>),

    BeginEraseNVR(OneShot<Result<()>>),
    PollEraseNVR(OneShot<Result<ProgressResponse>>),

    UploadFirmwareChunk(UploadFirmwareChunkRequest, OneShot<Result<()>>),
    ValidateUploadChunk(
        ValidateUploadChunkRequest,
        OneShot<Result<ValidateUploadChunkResponse>>,
    ),

    BeginHardwareVerify(OneShot<Result<()>>),
    PollHardwareVerify(OneShot<Result<HardwareProgressResponse>>),

    BeginHardwareWrite(OneShot<Result<()>>),
    PollHardwareWrite(OneShot<Result<HardwareProgressResponse>>),

    RebootGoXLR(OneShot<Result<()>>),
}

/// The SHA-256 of a file, in lower case hexadecimal.
fn sha256_of(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
