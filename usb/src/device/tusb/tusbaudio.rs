use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::CStr;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use byteorder::{ByteOrder, LittleEndian};
use libloading::{Library, Symbol};
use log::{debug, error, info, warn};
use std::sync::LazyLock;
use tokio::sync::mpsc::Sender;
use widestring::U16CStr;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeA,
    CM_Get_Device_Interface_ListA, CR_SUCCESS,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateEventA, WaitForSingleObject};
use windows::core::GUID;
use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

use goxlr_types::VersionNumber;

use crate::device::base::GoXLRDevice;

// Define the Types of the various methods..
type EnumerateDevices = unsafe extern "C" fn() -> u32;
type GetDriverInfo = unsafe extern "C" fn(*mut DriverInfo) -> u32;
type GetAPIVersion = unsafe extern "C" fn() -> ApiVersion;
type CheckAPIVersion = unsafe extern "C" fn(u32, u32) -> bool;
type GetDeviceCount = unsafe extern "C" fn() -> u32;
type OpenDeviceByIndex = unsafe extern "C" fn(u32, *mut u32) -> u32;
type GetDeviceInstanceIdString = unsafe extern "C" fn(u32, *mut u16, u32) -> u32;
type GetDeviceProperties = unsafe extern "C" fn(u32, *mut Properties) -> u32;
type GetUsbConfigDescriptor = unsafe extern "C" fn(u32, *mut u8, u32, *mut u32) -> u32;
type VendorRequestOut =
    unsafe extern "C" fn(u32, u32, u32, u32, u32, u16, u16, *const u8, *mut u8, u32) -> u32;
type VendorRequestIn =
    unsafe extern "C" fn(u32, u32, u32, u32, u32, u16, u16, *mut u8, *mut u8, u32) -> u32;
type RegisterDeviceNotification = unsafe extern "C" fn(u32, u32, HANDLE, u32) -> u32;
type RegisterPnpNotification = unsafe extern "C" fn(HANDLE, HANDLE, u32, u32, u32) -> u32;
type ReadDeviceNotification = unsafe extern "C" fn(u32, *const u32, *mut u8, u32, *mut u32) -> u32;
type StatusCodeString = unsafe extern "C" fn(u32) -> *const i8;
type CloseDevice = unsafe extern "C" fn(u32) -> u32;

/// An error code from the TUSB driver, with its text. Errors are recognised by their code: the
/// text depends on the driver's version.
#[derive(Debug)]
pub struct TStatus {
    pub code: u32,
    text: String,
}

/// The device handle isn't valid any more (TSTATUS_INVALID_HANDLE)
pub const TSTATUS_INVALID_HANDLE: u32 = 0xEE00_0048;

impl TStatus {
    fn new(code: u32, text: String) -> Self {
        Self { code, text }
    }
}

impl std::fmt::Display for TStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:#x})", self.text, self.code)
    }
}

impl std::error::Error for TStatus {}

/// Whether an error is the driver saying the handle is no longer valid.
pub fn is_invalid_handle(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TStatus>()
        .is_some_and(|status| status.code == TSTATUS_INVALID_HANDLE)
}

static GOXLR_GUID: GUID = GUID::from_u128(0x024D0372_641F_4B7B_8140_F4DFE458C982);
// Initialise the Library..
static LIBRARY: LazyLock<Library> = LazyLock::new(|| unsafe {
    Library::new(locate_library().as_str()).expect("Unable to Load GoXLR API Driver")
});
pub static TUSB_INTERFACE: LazyLock<TUSBAudio<'static>> =
    LazyLock::new(|| TUSBAudio::new().expect("Unable to Parse GoXLR API Driver"));

fn locate_library() -> String {
    // HKLM only: HKEY_CLASSES_ROOT also reads the user's own classes (HKCU), which any program
    // run by the user can change to load another DLL here. And only a DLL under Program Files,
    // which needs an administrator to write.
    let regpath =
        "SOFTWARE\\Classes\\CLSID\\{024D0372-641F-4B7B-8140-F4DFE458C982}\\InprocServer32\\";
    let program_files: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    let trusted = |path: &PathBuf| {
        path.canonicalize()
            .is_ok_and(|real| program_files.iter().any(|base| real.starts_with(base)))
    };
    let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(folders) = machine.open_subkey(regpath)
        // Name is blank because we need the default key
        && let Ok(api) = folders.get_value::<String, &str>("")
    {
        let path = PathBuf::from(&api);
        if path.exists() && trusted(&path) {
            debug!("Located API From Registry at  {}", api);
            return api;
        }
        warn!(
            "Ignoring the GoXLR API registered at {}, it's not under Program Files",
            api
        );
    }
    // If we get here, we didn't find it, return a default and hope for the best!
    debug!("GoXLR API not found in registry, using default path.");
    String::from("C:/Program Files/TC-HELICON/GoXLR_Audio_Driver/W10_x64/goxlr_audioapi_x64.dll")
}

#[allow(dead_code)]
pub struct TUSBAudio<'lib> {
    // Need to enumerate..
    pnp_thread_running: Arc<Mutex<bool>>,
    discovered_devices: Arc<Mutex<Vec<String>>>,

    // API Related Commands
    get_api_version: Symbol<'lib, GetAPIVersion>,
    check_api_version: Symbol<'lib, CheckAPIVersion>,

    // Enumeration / Opening..
    enumerate_devices: Symbol<'lib, EnumerateDevices>,
    open_device_by_index: Symbol<'lib, OpenDeviceByIndex>,

    get_device_count: Symbol<'lib, GetDeviceCount>,
    get_device_id_string: Symbol<'lib, GetDeviceInstanceIdString>,
    get_device_properties: Symbol<'lib, GetDeviceProperties>,
    get_device_usb: Symbol<'lib, GetUsbConfigDescriptor>,

    // Sending and Receiving..
    vendor_request_out: Symbol<'lib, VendorRequestOut>,
    vendor_request_in: Symbol<'lib, VendorRequestIn>,

    register_pnp_notification: Symbol<'lib, RegisterPnpNotification>,
    register_device_notification: Symbol<'lib, RegisterDeviceNotification>,
    read_device_notification: Symbol<'lib, ReadDeviceNotification>,

    status_code_string: Symbol<'lib, StatusCodeString>,

    // Closing
    close_device: Symbol<'lib, CloseDevice>,
}

impl TUSBAudio<'_> {
    pub fn new() -> Result<Self> {
        let get_api_version: Symbol<_> = unsafe { LIBRARY.get(b"TUSBAUDIO_GetApiVersion")? };
        let check_api_version = unsafe { LIBRARY.get(b"TUSBAUDIO_CheckApiVersion")? };

        let enumerate_devices =
            unsafe { LIBRARY.get::<EnumerateDevices>(b"TUSBAUDIO_EnumerateDevices")? };
        let open_device_by_index = unsafe { LIBRARY.get(b"TUSBAUDIO_OpenDeviceByIndex")? };

        let get_device_count = unsafe { LIBRARY.get(b"TUSBAUDIO_GetDeviceCount")? };
        let get_device_id_string = unsafe { LIBRARY.get(b"TUSBAUDIO_GetDeviceInstanceIdString")? };
        let get_device_properties = unsafe { LIBRARY.get(b"TUSBAUDIO_GetDeviceProperties")? };
        let get_device_usb = unsafe { LIBRARY.get(b"TUSBAUDIO_GetUsbConfigDescriptor")? };

        let vendor_request_out = unsafe { LIBRARY.get(b"TUSBAUDIO_ClassVendorRequestOut")? };
        let vendor_request_in = unsafe { LIBRARY.get(b"TUSBAUDIO_ClassVendorRequestIn")? };

        let register_device_notification =
            unsafe { LIBRARY.get(b"TUSBAUDIO_RegisterDeviceNotification")? };
        let register_pnp_notification =
            unsafe { LIBRARY.get(b"TUSBAUDIO_RegisterPnpNotification")? };
        let read_device_notification = unsafe { LIBRARY.get(b"TUSBAUDIO_ReadDeviceNotification")? };

        let status_code_string = unsafe { LIBRARY.get(b"TUSBAUDIO_StatusCodeStringA")? };
        let close_device = unsafe { LIBRARY.get(b"TUSBAUDIO_CloseDevice")? };

        debug!("Performing initial Enumeration..");
        unsafe { (enumerate_devices)() };

        let tusb_audio = Self {
            pnp_thread_running: Arc::new(Mutex::new(false)),
            discovered_devices: Arc::new(Mutex::new(Vec::new())),

            get_api_version,
            check_api_version,
            enumerate_devices,
            open_device_by_index,
            get_device_count,
            get_device_id_string,
            get_device_properties,
            get_device_usb,

            vendor_request_out,
            vendor_request_in,

            register_pnp_notification,
            register_device_notification,
            read_device_notification,

            status_code_string,
            close_device,
        };

        let api_version = unsafe { (tusb_audio.get_api_version)() };

        // API Version Checking (7.5, 11.5, 12.5 are valid)
        if (api_version.major == 12 || api_version.major == 11 || api_version.major == 7)
            && api_version.minor == 5
        {
            info!(
                "Using GoXLR API Version {}.{}",
                api_version.major, api_version.minor
            );
        } else {
            warn!(
                "API VERSION DETECTED: {}.{}",
                api_version.major, api_version.minor
            );
            warn!(
                "API VERSION MISMATCH: This code was made with Versions 7.5 / 11.5 / 12.5 of the API"
            );
            warn!("Please install version 5.12, 5.57 or 5.68 of the GoXLR Drivers");
            warn!("We'll try to keep going, but you may experience instability");
        }

        Ok(tusb_audio)
    }

    pub fn get_driver_version(&self) -> Option<VersionNumber> {
        match unsafe { LIBRARY.get::<GetDriverInfo>(b"TUSBAUDIO_GetDriverInfo") } {
            Ok(get_driver_info) => {
                debug!("Fetching Versioning Information..");
                let mut driver_info = DriverInfo::default();
                let driver_info_ptr: *mut DriverInfo = &mut driver_info;
                let result = unsafe { (get_driver_info)(driver_info_ptr) };
                if result != 0 {
                    warn!("Unable to Get Driver Info: {}", self.get_error(result));
                    return None;
                }

                Some(VersionNumber(
                    driver_info.driver_major,
                    driver_info.driver_minor,
                    Some(driver_info.driver_patch),
                    None,
                ))
            }
            Err(e) => {
                warn!("Unable to Get Driver Info: {}", e);
                None
            }
        }
    }

    fn get_error(&self, error: u32) -> String {
        let res = unsafe { (self.status_code_string)(error) };
        let text = unsafe { CStr::from_ptr(res) };

        text.to_string_lossy().to_string()
    }

    // We need to mildly abuse inner mutability here, due to the nature of lazy_static..
    fn enumerate_devices(&self) {
        unsafe { (self.enumerate_devices)() };
    }

    fn detect_devices(&self) -> Result<()> {
        let mut result_vec = Vec::new();
        self.enumerate_devices();

        let device_count = self.get_device_count();
        for i in 0..device_count {
            match self.open_device_by_index(i) {
                Ok(handle) => {
                    match self.get_device_id_string(handle) {
                        Ok(handle) => result_vec.push(handle),
                        Err(e) => warn!("Unable to Open Device Handle: {}", e),
                    }
                    // Try to close it, just in case..
                    let _ = self.close_device(handle);
                }
                Err(e) => {
                    error!("Unable to Open Device: {}", e);
                }
            }
        }

        // All devices handled, replace the stored vec..
        let mut discovered = self.discovered_devices.lock().unwrap();
        *discovered = result_vec;

        Ok(())
    }

    pub fn get_devices(&self) -> Vec<String> {
        self.discovered_devices.lock().unwrap().clone()
    }

    fn get_device_count(&self) -> u32 {
        unsafe { (self.get_device_count)() }
    }

    pub fn get_properties_by_handle(&self, handle: u32) -> Result<Properties> {
        // Create the properties struct..
        let mut properties = Properties::default();

        // Grab the Pointer for it..
        let properties_ptr: *mut Properties = &mut properties;

        // Attempt to get the Properties for this device..
        let result = unsafe { (self.get_device_properties)(handle, properties_ptr) };

        if result != 0 {
            bail!("Unable to Get Properties: {}", result);
        }

        Ok(properties)
    }

    fn get_device_id_string(&self, handle: u32) -> Result<String> {
        // The driver writes the id into this buffer, so it must be initialised and writable.
        let mut buffer = vec![0_u16; 256];
        let buffer_len = buffer.len() as u32;
        let result =
            unsafe { (self.get_device_id_string)(handle, buffer.as_mut_ptr(), buffer_len) };

        if result != 0 {
            let error = self.get_error(result);
            bail!("Error Getting Device Id: {}", error);
        }

        let device_id = U16CStr::from_slice_truncate(&buffer)?;
        Ok(device_id.to_string_lossy())
    }

    pub fn send_request(
        &self,
        handle: u32,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        let data_length: u16 = data
            .len()
            .try_into()
            .map_err(|_| anyhow!("Request too large: {} bytes", data.len()))?;
        let data_pointer = data.as_ptr();

        // The Driver writes number of bytes written to a buffer, so create it here.
        let bytes_written_len: u32 = 64;
        let mut bytes_written = vec![0_u8; bytes_written_len as usize];
        let bytes_written_ptr = bytes_written.as_mut_ptr();

        let result = unsafe {
            (self.vendor_request_out)(
                handle,
                1_u32,
                0_u32,
                request.into(),
                value.into(),
                index,
                data_length,
                data_pointer,
                bytes_written_ptr,
                bytes_written_len,
            )
        };

        if result != 0 {
            // Known Errors:
            // 4009754628 - ?
            // 3992977412 - Invalid Request
            // 3992977480 - INVALID HANDLE!

            //3992977411
            return Err(TStatus::new(result, self.get_error(result)).into());
        }

        Ok(())
    }

    pub fn read_response(
        &self,
        handle: u32,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>> {
        let request_length: u16 = length
            .try_into()
            .map_err(|_| anyhow!("Response length too large: {}", length))?;

        // The driver writes into both buffers, so they must be initialised.
        let mut buffer = vec![0_u8; length];
        let buffer_ptr = buffer.as_mut_ptr();

        let buffer2_len = 64;
        let mut buffer2 = vec![0_u8; buffer2_len as usize];
        let buffer2_ptr = buffer2.as_mut_ptr();

        // This likely has 'Bytes Returned' somewhere in there, we need to check for that!
        let result = unsafe {
            (self.vendor_request_in)(
                handle,
                1_u32,
                0_u32,
                request as u32,
                value as u32,
                index,
                request_length,
                buffer_ptr,
                buffer2_ptr,
                buffer2_len,
            )
        };

        if result != 0 {
            return Err(TStatus::new(result, self.get_error(result)).into());
        }

        // Ok, Buffer2 contains a u32 containing the length of the returned response..
        let read_len = LittleEndian::read_u32(&buffer2[0..4]) as usize;
        if read_len > length {
            bail!(
                "Driver reported {} bytes read, but only {} were requested",
                read_len,
                length
            );
        }

        buffer.truncate(read_len);
        Ok(buffer)
    }

    /// Opens a device by its identifier. The driver opens by position, and the list can change
    /// in between (another GoXLR plugged, unplugged): the opened device's identifier is checked,
    /// and on a mismatch the list is read again once.
    pub fn open_device_by_identifier(&self, identifier: String) -> Result<u32> {
        for attempt in 0..2 {
            if attempt > 0 {
                self.detect_devices()?;
            }
            let device_index = self
                .discovered_devices
                .lock()
                .unwrap()
                .iter()
                .position(|id| identifier == *id)
                .ok_or_else(|| anyhow!("Cannot Find Device"))?;
            let handle = self.open_device_by_index(device_index.try_into()?)?;
            match self.get_device_id_string(handle) {
                Ok(opened) if opened == identifier => return Ok(handle),
                _ => {
                    warn!("Opened another device than {}, looking again", identifier);
                    let _ = self.close_device(handle);
                }
            }
        }
        bail!(
            "Unable to open {}, the device list keeps changing",
            identifier
        )
    }

    fn open_device_by_index(&self, device_index: u32) -> Result<u32> {
        let mut handle: u32 = 0;
        let ptr: *mut u32 = &mut handle;
        let result = unsafe { (self.open_device_by_index)(device_index, ptr) };
        if result == 0 {
            return Ok(handle);
        }

        bail!("Unable to Open Device: {}", result)
    }

    pub fn close_device(&self, handle: u32) -> Result<()> {
        let close = unsafe { (self.close_device)(handle) };
        if close != 0 {
            bail!("Unable to Close Handle: {}", close);
        }
        Ok(())
    }

    pub fn event_loop(
        &self,
        device_identifier: String,
        identifier: Arc<Mutex<Option<String>>>,
        callbacks: EventChannelSender,
        terminator: Arc<AtomicBool>,
    ) -> Result<()> {
        // Open a Handle to the Device..
        let mut handle = TUSB_INTERFACE.open_device_by_identifier(device_identifier.clone())?;

        // Register a new windows event, it's closed when this function returns..
        let event = match unsafe { CreateEventA(None, false, false, None) } {
            Ok(event) => EventGuard(event),
            Err(error) => {
                let _ = TUSB_INTERFACE.close_device(handle);
                bail!("Unable to create event: {}", error);
            }
        };
        let event = event.0;

        // Register this event with the notifier..
        let result = unsafe { (self.register_device_notification)(handle, u32::MAX, event, 0) };
        if result != 0 {
            warn!("Unable to Register Notifications");
            let _ = TUSB_INTERFACE.close_device(handle);
            bail!("Unable to register notifications");
        }

        // Assign useful variables for later :p
        // The driver writes into the Vec's heap data, so pass that pointer (not the Vec itself).
        let mut buffer = vec![0_u8; 1024];
        let buffer_len = buffer.len();
        let buffer_ptr = buffer.as_mut_ptr();

        let mut response_len: u32 = 0;
        let len_ptr: *mut u32 = &mut response_len;

        // Honestly don't know what this variable does, but 0 seems to work.
        let a = 0;

        if callbacks.ready_notifier.send(true).is_err() {
            warn!("Error Sending Ready Notification..");
            terminator.store(true, Ordering::Relaxed);
        }

        // Now we loop :D
        loop {
            // Wait for the event Trigger (I'd love for this to be async one day :p)..
            let wait_result = unsafe { WaitForSingleObject(event, 500) };
            if wait_result != WAIT_TIMEOUT {
                // Check the Queued Events :)
                loop {
                    let event_result = unsafe {
                        (self.read_device_notification)(
                            handle,
                            &a,
                            buffer_ptr,
                            buffer_len as u32,
                            len_ptr,
                        )
                    };
                    if event_result != 0 {
                        // We've either hit the end of the list, or something's gone wrong, break
                        // out and double check our handle.
                        if event_result != 3992977442 {
                            warn!("Error Reading Event! {}", event_result);
                        }
                        break;
                    }

                    // Never trust the driver's length beyond the size of our buffer.
                    let read_len = (response_len as usize).min(buffer_len);
                    let event_response =
                        unsafe { std::slice::from_raw_parts(buffer_ptr, read_len) };

                    // Generally caused by an 'audio end' event.
                    if event_response.is_empty() {
                        continue;
                    }

                    if event_response.len() != 6 {
                        debug!(
                            "Unexpected Event Response Length: {}: {:?}",
                            event_response.len(),
                            event_response
                        );
                        continue;
                    }

                    if event_response[0] == 1 && event_response[1] == 1 && event_response[2] == 1 {
                        // This event indicates something waiting to be read. If a token is
                        // already waiting, the reader already knows, so never block here.
                        match callbacks.data_read.try_send(true) {
                            Ok(()) | Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => {
                                // Something's gone horribly wrong!
                                debug!("Data channel closed, stopping event loop");
                                terminator.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        continue;
                    }

                    if event_response[0] == 1
                        && event_response[1] == 1
                        && event_response[2] == 0
                        && let Some(identifier) = &*identifier.lock().unwrap()
                    {
                        // A button or fader interrupt has been received.
                        if callbacks.input_changed.capacity() > 0 {
                            let se = callbacks.input_changed.blocking_send(identifier.clone());
                            if se.is_err() {
                                warn!("Error sending Callback! {:?}", se.err());

                                // Something's gone horribly wrong!
                                terminator.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                }
            }

            if terminator.load(Ordering::Relaxed) {
                debug!("Terminator has told us to terminate.");
                break;
            }

            // This code simply validates our handle is good, and re-establishes if needed.
            if let Err(_error) = self.get_device_id_string(handle) {
                debug!("Invalid Handle Detected, attempting recovery..");

                // Handle appears broken, try making a new one..
                let new_handle =
                    TUSB_INTERFACE.open_device_by_identifier(device_identifier.clone());

                // This one is broken too!
                if new_handle.is_err() {
                    warn!("Unable to recover invalid handle, stopping handler");
                    // Flag ourself for stop..
                    terminator.store(true, Ordering::Relaxed);

                    // Break any read locks which might be currently occurring, and tell then we're dead.
                    let _ = callbacks.data_read.try_send(false);

                    // Also trigger an 'input_changed' event, so the main handler can discover that
                    // we're dead if it wasn't previously active..
                    if let Some(identifier) = &*identifier.lock().unwrap() {
                        // Trigger a callback to the main event handler, so it can check..
                        let _ = callbacks.input_changed.blocking_send(identifier.clone());
                    }
                    break;
                } else {
                    debug!("Handle Re-established, attempting to register for notifications..");
                    let _ = TUSB_INTERFACE.close_device(handle);
                    handle = new_handle?;

                    let result =
                        unsafe { (self.register_device_notification)(handle, u32::MAX, event, 0) };
                    if result != 0 {
                        let _ = TUSB_INTERFACE.close_device(handle);
                        bail!("Unable to register notifications");
                    }
                }
            }
        }
        let _ = TUSB_INTERFACE.close_device(handle);

        warn!("Event Thread Terminated");
        bail!("Thread Terminated!")
    }

    // This is the original pnp handler
    #[allow(dead_code)]
    pub fn spawn_pnp_handle(&self) -> Result<()> {
        let mut spawned = self.pnp_thread_running.lock().unwrap();
        if *spawned {
            bail!("Handler Thread already running..");
        }

        debug!("Spawning PnP Thread..");
        thread::spawn(|| -> Result<()> {
            let event = unsafe { CreateEventA(None, false, false, None)? };

            let result =
                unsafe { (TUSB_INTERFACE.register_pnp_notification)(event, event, 0, 0, 0) };
            if result != 0 {
                bail!("Unable to register notifications");
            }

            loop {
                let wait_result = unsafe { WaitForSingleObject(event, 1000) };
                if wait_result == WAIT_TIMEOUT {
                    // Timeout on wait, go again!
                    continue;
                }

                // We need to re-enumerate the devices..
                let _ = TUSB_INTERFACE.detect_devices();
            }
        });

        *spawned = true;
        Ok(())
    }

    pub fn spawn_pnp_handle_win32(&self) -> Result<()> {
        let mut spawned = self.pnp_thread_running.lock().unwrap();
        if *spawned {
            bail!("Handler Thread already running...");
        }
        debug!("Spawning Win32 PnP Handler Thread");
        let started = std::time::Instant::now();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();

        thread::spawn(move || -> Result<()> {
            let mut ready_sender = Some(ready_tx);

            loop {
                // The API writes the required size (a ULONG) into this value.
                let mut length: u32 = 0;
                let result = unsafe {
                    CM_Get_Device_Interface_List_SizeA(
                        &mut length,
                        &GOXLR_GUID as *const _,
                        None,
                        CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                    )
                };

                if result != CR_SUCCESS {
                    // This should only occur if the system is Out of Memory!
                    warn!("Error Fetching Interface List Size {:?}", result);
                    sleep(Duration::from_millis(200));
                    continue;
                }

                let mut output = vec![0_u8; length as usize];
                let result = unsafe {
                    CM_Get_Device_Interface_ListA(
                        &GOXLR_GUID as *const _,
                        None,
                        &mut output,
                        CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                    )
                };

                if result != CR_SUCCESS {
                    // This theoretically should only occur if the size has changed since we fetched it
                    warn!("Error Fetching Interface List {:?}", result);
                    sleep(Duration::from_millis(200));
                    continue;
                }

                let count = output.split(|&v| v == 0).filter(|a| !a.is_empty()).count();
                if count != TUSB_INTERFACE.get_devices().len() {
                    debug!("Device Change Detected.");
                    let _ = TUSB_INTERFACE.detect_devices();
                }

                if let Some(sender) = ready_sender.take() {
                    let _ = sender.send(true);
                }
                sleep(Duration::from_secs(1));
            }
        });

        // The thread is running, so mark it as spawned and release the lock before waiting, this
        // prevents other callers from being blocked while we wait for the first device scan.
        *spawned = true;
        drop(spawned);

        // Wait for the first scan to complete, but never forever..
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(_) => info!("Win32 PnP Handler Started in {:?}", started.elapsed()),
            Err(RecvTimeoutError::Timeout) => {
                warn!(
                    "Win32 PnP Handler has not completed its first scan after {:?}, continuing..",
                    started.elapsed()
                );
            }
            Err(RecvTimeoutError::Disconnected) => {
                *self.pnp_thread_running.lock().unwrap() = false;
                bail!("Win32 PnP Handler Thread stopped before it was ready");
            }
        }
        Ok(())
    }

    //#[allow(dead_code)]
    // pub fn spawn_pnp_handle_rusb(&self) -> Result<()> {
    //     // Comment for future me: Use CM_Register_Notification instead of rusb
    //
    //     let mut spawned = self.pnp_thread_running.lock().unwrap();
    //     if *spawned {
    //         bail!("Handler Thread already running..");
    //     }
    //
    //     debug!("Spawning RUSB PnP Thread");
    //
    //     // We should not return from this method until at least one run has been done by the
    //     // thread, this is primarily to prevent conflicts on startup when everything changes.
    //
    //     let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel::<bool>();
    //
    //     thread::spawn(move || -> Result<()> {
    //         let mut devices = vec![];
    //         let mut ready_sender = Some(ready_tx);
    //
    //         debug!("PnP Thread Spawned");
    //         loop {
    //             let mut found_devices = vec![];
    //
    //             if let Ok(devices) = rusb::devices() {
    //                 for device in devices.iter() {
    //                     if let Ok(descriptor) = device.device_descriptor() {
    //                         let bus_number = device.bus_number();
    //                         let address = device.address();
    //
    //                         if descriptor.vendor_id() == VID_GOXLR
    //                             && (descriptor.product_id() == PID_GOXLR_FULL
    //                                 || descriptor.product_id() == PID_GOXLR_MINI)
    //                         {
    //                             found_devices.push(USBDevice {
    //                                 bus_number,
    //                                 address,
    //                             });
    //                         }
    //                     }
    //                 }
    //             } else {
    //                 debug!("Unable to Poll Devices");
    //             }
    //
    //             // Make sure our two vecs are the same..
    //             if !iters_equal_anyorder(
    //                 devices.clone().into_iter(),
    //                 found_devices.clone().into_iter(),
    //             ) {
    //                 debug!("Device Change Detected");
    //                 let _ = TUSB_INTERFACE.detect_devices();
    //                 devices.clear();
    //                 devices.append(&mut found_devices);
    //             }
    //
    //             // If a driver takes a couple of hundred milliseconds to load, it's theoretically
    //             // possible that we'll have detected the device and run detect_devices() too early
    //             // leaving the detected device list empty and causing a desync in the lists.
    //             //
    //             // The following simply checks what's already been found, and if the list size
    //             // isn't the same as we have detected here, attempts to force a resync of the
    //             // devices from the API.
    //             if devices.len() != TUSB_INTERFACE.get_devices().len() {
    //                 debug!("Device Desync Detected, attempting to resync..");
    //                 let _ = TUSB_INTERFACE.detect_devices();
    //             }
    //
    //             if let Some(sender) = ready_sender.take() {
    //                 let _ = sender.send(true);
    //             }
    //             sleep(Duration::from_secs(1));
    //         }
    //     });
    //
    //     // Block until the 'ready' message has been sent..
    //     while ready_rx.try_recv().is_err() {
    //         sleep(Duration::from_millis(5));
    //     }
    //     debug!("RUSB PnP Handler Started");
    //
    //     *spawned = true;
    //     Ok(())
    // }
}

// #[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
// struct USBDevice {
//     pub(crate) bus_number: u8,
//     pub(crate) address: u8,
// }

pub struct DeviceHandle {
    handle: u32,
    closed: AtomicBool,
}

impl DeviceHandle {
    pub fn from_device(device: GoXLRDevice) -> Result<Self> {
        // This is simple enough, iterate the devices until we find the one we want..
        if let Some(identifier) = device.identifier {
            let handle = TUSB_INTERFACE.open_device_by_identifier(identifier)?;
            return Ok(Self {
                handle,
                closed: AtomicBool::new(false),
            });
        }
        bail!("Unable to Locate Device")
    }

    /// Closes the handle, only once: the driver may hand the same number to another device later.
    pub fn close_handle(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        TUSB_INTERFACE.close_device(self.handle)
    }

    pub fn send_request(&self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        // Ok, need to work out what all this is, but still..
        TUSB_INTERFACE.send_request(self.handle, request, value, index, data)
    }

    pub fn read_response(
        &self,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>> {
        TUSB_INTERFACE.read_response(self.handle, request, value, index, length)
    }

    pub fn get_device_id_string(&self) -> Result<String> {
        TUSB_INTERFACE.get_device_id_string(self.handle)
    }

    pub fn get_properties(&self) -> Result<Properties> {
        TUSB_INTERFACE.get_properties_by_handle(self.handle)
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        let _ = self.close_handle();
    }
}

/// Closes a Windows event handle when dropped.
struct EventGuard(HANDLE);

impl Drop for EventGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[repr(C)]
struct ApiVersion {
    major: u16,
    minor: u16,
}

#[repr(C)]
#[derive(Debug, Default)]
struct DriverInfo {
    api_major: u32,
    api_minor: u32,
    driver_major: u32,
    driver_minor: u32,
    driver_patch: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Debug)]
pub struct Properties {
    vendor_id: i32,
    product_id: i32,
    revision_number: i32,
    serial_number: [u16; 128],
    manufacturer: [u16; 128],
    model: [u16; 128],

    // These two are likely provided specially by the driver, as they don't match anything
    // in the USB Spec.
    unknown_number: i32,
    unknown_string: [u16; 128],
}

impl Properties {
    /// The device's release number (bcdDevice), as major, minor, sub minor.
    pub fn revision(&self) -> (u8, u8, u8) {
        let bcd = self.revision_number as u16;
        (
            (bcd >> 8) as u8,
            ((bcd >> 4) & 0x0f) as u8,
            (bcd & 0x0f) as u8,
        )
    }

    pub fn vendor_id(&self) -> i32 {
        self.vendor_id
    }
    pub fn product_id(&self) -> i32 {
        self.product_id
    }
    pub fn manufacturer(&self) -> Result<String> {
        // Convert this from wide String, to regular String..
        Ok(U16CStr::from_slice_truncate(&self.manufacturer)?.to_string_lossy())
    }
    pub fn model(&self) -> Result<String> {
        // Convert this from wide String, to regular String..
        Ok(U16CStr::from_slice_truncate(&self.model)?.to_string_lossy())
    }
}

impl Default for Properties {
    fn default() -> Self {
        Self {
            vendor_id: 0,
            product_id: 0,
            revision_number: 0,
            serial_number: [0; 128],
            manufacturer: [0; 128],
            model: [0; 128],
            unknown_number: 0,
            unknown_string: [0; 128],
        }
    }
}

pub fn get_devices() -> Vec<GoXLRDevice> {
    let _ = TUSB_INTERFACE.spawn_pnp_handle_win32();
    let mut list = Vec::new();

    // Ok, this is slightly different now..
    let devices = TUSB_INTERFACE.get_devices();
    for device in devices {
        list.push(GoXLRDevice {
            bus_number: 0,
            address: 0,
            identifier: Some(device),
        })
    }
    list
}

pub fn get_version() -> Option<VersionNumber> {
    TUSB_INTERFACE.get_driver_version()
}

pub struct EventChannelReceiver {
    // A std channel so the reader can wait with a timeout instead of spinning, inside a Mutex
    // because the device must be Sync (only accessed through &mut, so it never contends).
    pub(crate) data_read: Mutex<std::sync::mpsc::Receiver<bool>>,
}

pub struct EventChannelSender {
    pub(crate) ready_notifier: tokio::sync::oneshot::Sender<bool>,
    pub(crate) data_read: SyncSender<bool>,
    pub(crate) input_changed: Sender<String>,
}

#[allow(dead_code)]
fn iters_equal_anyorder<T: Eq + Hash>(
    i1: impl Iterator<Item = T>,
    i2: impl Iterator<Item = T>,
) -> bool {
    fn get_lookup<T: Eq + Hash>(iter: impl Iterator<Item = T>) -> HashMap<T, usize> {
        let mut lookup = HashMap::<T, usize>::new();
        for value in iter {
            match lookup.entry(value) {
                Entry::Occupied(entry) => {
                    *entry.into_mut() += 1;
                }
                Entry::Vacant(entry) => {
                    entry.insert(0);
                }
            }
        }
        lookup
    }
    get_lookup(i1) == get_lookup(i2)
}
