use crate::commands::Command;
use crate::device::base::{
    AttachGoXLR, ExecutableGoXLR, FullGoXLRDevice, GoXLRCommands, GoXLRDevice, Response,
    ResponseError, UsbData, decode_response, encode_request,
};
use crate::{PID_GOXLR_FULL, PID_GOXLR_MINI, VID_GOXLR};
use anyhow::{Error, Result, anyhow, bail};
use goxlr_types::{DriverInterface, VersionNumber};
use log::{debug, error, info, warn};
use rusb::Error::Pipe;
use rusb::{
    Device, DeviceDescriptor, DeviceHandle, Direction, GlobalContext, Language, Recipient,
    RequestType,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::task;

pub struct GoXLRUSB {
    handle: DeviceHandle<GlobalContext>,
    device: Device<GlobalContext>,
    descriptor: DeviceDescriptor,

    disconnect_sender: Sender<String>,
    event_sender: Sender<String>,
    identifier: Option<String>,

    pause_polling: Arc<AtomicBool>,
    stop_polling: Arc<AtomicBool>,
    // Milliseconds between two polls, longer while the device is idle
    poll_millis: Arc<AtomicU64>,

    stopping: Arc<AtomicBool>,
    disconnecting: bool,

    language: Language,
    command_count: u16,
    timeout: Duration,
}

// How often the buttons, faders and encoders are read: often while they're used, less once
// nothing has moved for a while (the first move is then seen within POLL_IDLE_MILLIS)
const POLL_ACTIVE_MILLIS: u64 = 20;
const POLL_IDLE_MILLIS: u64 = 60;

// The polling task stops with the device
impl Drop for GoXLRUSB {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
    }
}

impl GoXLRUSB {
    fn find_device(device: GoXLRDevice) -> Result<(Device<GlobalContext>, DeviceDescriptor)> {
        if let Ok(devices) = rusb::devices() {
            for usb_device in devices.iter() {
                if usb_device.bus_number() == device.bus_number
                    && usb_device.address() == device.address
                    && let Ok(descriptor) = usb_device.device_descriptor()
                {
                    return Ok((usb_device, descriptor));
                }
            }
        }
        bail!("Specified Device not Found!")
    }

    /// Tells the daemon the device is gone (when it is). Problems here are only logged: the
    /// caller returns the USB error that led here, which is the one that matters.
    fn trigger_disconnect(&mut self) {
        if let Err(e) = self.try_disconnect() {
            warn!("Unable to report the device's disconnection: {}", e);
        }
    }

    fn try_disconnect(&mut self) -> Result<()> {
        // If this function has already been called further up the stack, don't run it.
        if self.disconnecting {
            return Ok(());
        }

        // Flag this device as possibly disconnecting..
        self.disconnecting = true;

        // Perform a connection Check, and reset if needed..
        if self.is_connected() {
            // We're still connected, reset the disconnecting flag
            self.disconnecting = false;
            return Ok(());
        }

        if let Some(identifier) = &self.identifier {
            // Polling only stops once the daemon has been told, otherwise the device would be
            // left half gone (no polling, still listed).
            if let Err(e) = self.disconnect_sender.try_send(identifier.clone()) {
                self.disconnecting = false;
                bail!("{}", e);
            }
            self.stopping.store(true, Ordering::Relaxed);
            return Ok(());
        }
        self.disconnecting = false;
        bail!("Unable to Disconnect, Identifier not Found!");
    }

    pub(crate) fn write_class_control(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), rusb::Error> {
        self.handle.write_control(
            rusb::request_type(Direction::Out, RequestType::Class, Recipient::Interface),
            request,
            value,
            index,
            data,
            self.timeout,
        )?;

        Ok(())
    }

    pub(crate) fn write_control(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), rusb::Error> {
        self.handle.write_control(
            rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Interface),
            request,
            value,
            index,
            data,
            self.timeout,
        )?;

        Ok(())
    }

    pub(crate) fn read_control(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>, rusb::Error> {
        let mut buf = vec![0; length];
        let response_length = self.handle.read_control(
            rusb::request_type(Direction::In, RequestType::Vendor, Recipient::Interface),
            request,
            value,
            index,
            &mut buf,
            self.timeout,
        )?;
        buf.truncate(response_length);
        Ok(buf)
    }
}

impl AttachGoXLR for GoXLRUSB {
    fn from_device(
        device: GoXLRDevice,
        disconnect_sender: Sender<String>,
        event_sender: Sender<String>,
        _skip_pause: bool,
    ) -> Result<Box<dyn FullGoXLRDevice>> {
        // Firstly, we need to locate the USB device based on the location..
        let (device, descriptor) = GoXLRUSB::find_device(device)?;
        let handle = device.open()?;

        let timeout = Duration::from_secs(1);

        let languages = handle.read_languages(timeout)?;
        let language = languages
            .first()
            .ok_or_else(|| anyhow!("Not GoXLR?"))?
            .to_owned();

        let device = handle.device();
        info!("Connected to possible GoXLR device at {:?}", device);

        let device_is_claimed = handle.claim_interface(0).is_ok();

        let mut goxlr = Self {
            device: handle.device(),
            handle,
            descriptor,
            language,
            disconnect_sender,
            event_sender,
            identifier: None,
            command_count: 0,
            stopping: Arc::new(AtomicBool::new(false)),
            disconnecting: false,
            timeout,
            pause_polling: Arc::new(AtomicBool::new(false)),
            stop_polling: Arc::new(AtomicBool::new(false)),
            poll_millis: Arc::new(AtomicU64::new(POLL_ACTIVE_MILLIS)),
        };

        // Resets the state of the device (unconfirmed - Might just be the command id counter)
        let result = goxlr.write_control(1, 0, 0, &[]);

        if result == Err(Pipe) {
            // The GoXLR is not initialised, we need to fix that..
            info!("Found uninitialised GoXLR, attempting initialisation..");

            // Before we claim the interface, give things like Pipewire a change to finish up..
            sleep(Duration::from_millis(1500));

            if device_is_claimed {
                goxlr.handle.release_interface(0)?;
            }
            goxlr.handle.set_auto_detach_kernel_driver(true)?;

            if goxlr.handle.claim_interface(0).is_err() {
                return Err(anyhow!("Unable to Claim Device"));
            }

            debug!("Activating Vendor Interface...");
            goxlr.read_control(0, 0, 0, 24)?;

            // Now activate audio..
            debug!("Activating Audio...");
            goxlr.write_class_control(1, 0x0100, 0x2900, &[0x80, 0xbb, 0x00, 0x00])?;

            // Reset the device, so ALSA can pick it up again..
            goxlr.handle.reset()?;

            // The reset can make the device enumerate again: this handle is then gone
            // (NotFound). That's expected, the device is found again on the next detection.
            match goxlr.handle.release_interface(0) {
                Err(rusb::Error::NotFound) | Err(rusb::Error::NoDevice) => {
                    bail!("The GoXLR is re-enumerating after its initialisation, retrying shortly");
                }
                result => result?,
            }

            // Reattempt the reset..
            match goxlr.write_control(1, 0, 0, &[]) {
                Err(rusb::Error::NotFound) | Err(rusb::Error::NoDevice) => {
                    bail!("The GoXLR is re-enumerating after its initialisation, retrying shortly");
                }
                result => result?,
            }

            warn!(
                "Initialisation complete. If you are using the JACK script, you may need to reboot for audio to work."
            );
        }

        // Force command pipe activation in all cases.
        debug!("Handling initial request");
        goxlr.read_control(3, 0, 0, 1040)?;

        // Set the local serial number..
        Ok(Box::new(goxlr))
    }

    fn set_unique_identifier(&mut self, identifier: String) {
        // One polling task per device
        let first = self.identifier.is_none();
        let event_id = identifier.clone();
        self.identifier = Some(identifier);
        if !first {
            return;
        }

        let sender = self.event_sender.clone();
        let stopping = self.stopping.clone();
        let paused = self.pause_polling.clone();
        let stopped = self.stop_polling.clone();
        let poll_millis = self.poll_millis.clone();

        task::spawn(async move {
            loop {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }

                if paused.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(POLL_ACTIVE_MILLIS)).await;
                    continue;
                }
                if stopped.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(POLL_IDLE_MILLIS)).await;
                    continue;
                }

                let event = event_id.clone();

                // Only send an event if we have the capacity to do so; the daemon closing its end
                // (shutting down) ends this task.
                if sender.capacity() > 0 && sender.send(event).await.is_err() {
                    warn!("Sender Closed for {}", event_id);
                    break;
                }

                let millis = poll_millis.load(Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(millis)).await;
            }
        });
    }

    fn is_connected(&mut self) -> bool {
        debug!("Checking Disconnect for device: {:?}", self.device);
        let active_configuration = self.handle.active_configuration();
        if active_configuration.is_ok() {
            let result = self.request_data(Command::ResetCommandIndex, &[]);
            return if result.is_ok() {
                debug!("Device {:?} is still connected", self.device);
                true
            } else {
                debug!("Device {:?} has been disconnected", self.device);
                false
            };
        }
        false
    }

    fn set_idle(&mut self, idle: bool) {
        let millis = if idle {
            POLL_IDLE_MILLIS
        } else {
            POLL_ACTIVE_MILLIS
        };
        self.poll_millis.store(millis, Ordering::Relaxed);
    }

    fn set_is_polling(&mut self, polling: bool) {
        warn!("GoXLR Interaction Polling: {}", polling);
        self.stop_polling.store(!polling, Ordering::Relaxed);
    }
}

impl ExecutableGoXLR for GoXLRUSB {
    fn perform_request(&mut self, command: Command, body: &[u8], retry: bool) -> Result<Vec<u8>> {
        // Polling waits while a request is in flight, and resumes however this returns
        let _paused = PollingPause::new(self.pause_polling.clone());

        if command == Command::ResetCommandIndex {
            self.command_count = 0;
        } else {
            if self.command_count == u16::MAX {
                self.request_data(Command::ResetCommandIndex, &[])?;
            }
            self.command_count += 1;
        }

        let command_index = self.command_count;
        let full_request = encode_request(command, command_index, body)?;

        if let Err(error) = self.write_control(2, 0, 0, &full_request) {
            debug!("Error when attempting to write control.");
            self.trigger_disconnect();
            bail!(error);
        }

        // The full fat GoXLR can handle requests incredibly quickly..
        let mut sleep_time = Duration::from_millis(3);
        if self.descriptor.product_id() == PID_GOXLR_MINI {
            // The mini, however, cannot.
            sleep_time = Duration::from_millis(10);
        }
        sleep(sleep_time);

        // Some firmware update messages take longer to process, give more time here and don't
        // spam about them being delayed.
        let (count, ignore_warn) = match command {
            Command::ExecuteFirmwareUpdateAction(_) => (80, true),
            Command::ExecuteFirmwareUpdateCommand(_) => (80, true),
            _ => (20, false),
        };

        for i in 0..count {
            let raw = match self.read_control(3, 0, 0, 1040) {
                Ok(raw) => raw,
                Err(Pipe) if i < count - 1 => {
                    if !ignore_warn {
                        debug!(
                            "Response not arrived yet for {:?}, sleeping and retrying (Attempt {} of {})",
                            command,
                            i + 1,
                            count
                        );
                    }
                    sleep(sleep_time);
                    continue;
                }
                Err(Pipe) => {
                    // We can't read from this GoXLR, flag as disconnected.
                    self.trigger_disconnect();
                    warn!(
                        "Failed to receive response (Attempt {} of {}), possible Dead GoXLR?",
                        count, count
                    );
                    return Err(Error::from(Pipe));
                }
                Err(err) => {
                    debug!("Error Occurred during packet read: {}", err);
                    self.trigger_disconnect();
                    return Err(Error::from(err));
                }
            };

            return match decode_response(raw, command_index) {
                Ok(Response::Body(body)) => Ok(body),
                Ok(Response::OtherIndex(received)) => {
                    debug!(
                        "Mismatched Command Indexes, expected {}, received: {}",
                        command_index, received
                    );
                    debug!("Full Request: {:?}", full_request);
                    if !retry {
                        debug!("Attempting Resync and Retry");
                        self.perform_request(Command::ResetCommandIndex, &[], true)?;

                        debug!("Resync complete, retrying Command..");
                        self.perform_request(command, body, true)
                    } else {
                        debug!("Resync Failed, Throwing Error..");
                        self.trigger_disconnect();
                        Err(Error::from(rusb::Error::Other))
                    }
                }
                Err(e @ ResponseError::ShortHeader(_)) => {
                    error!("{}", e);
                    self.trigger_disconnect();
                    Err(Error::from(Pipe))
                }
                Err(e) => Err(e.into()),
            };
        }
        // Only reached when count is 0
        bail!("No response read")
    }

    fn get_descriptor(&self) -> Result<UsbData> {
        // The device's own release number (bcdDevice), not the USB specification it follows
        let version = self.descriptor.device_version();
        let device_version = (version.0, version.1, version.2);

        let device_manufacturer = self.handle.read_manufacturer_string(
            self.language,
            &self.descriptor,
            Duration::from_millis(100),
        )?;

        let product_name = self.handle.read_product_string(
            self.language,
            &self.descriptor,
            Duration::from_millis(100),
        )?;

        Ok(UsbData {
            vendor_id: self.descriptor.vendor_id(),
            product_id: self.descriptor.product_id(),
            device_version,
            device_manufacturer,
            product_name,
        })
    }
}

impl GoXLRCommands for GoXLRUSB {}
impl FullGoXLRDevice for GoXLRUSB {}

pub fn find_devices() -> Vec<GoXLRDevice> {
    let mut found_devices: Vec<GoXLRDevice> = Vec::new();

    if let Ok(devices) = rusb::devices() {
        for device in devices.iter() {
            if let Ok(descriptor) = device.device_descriptor() {
                let bus_number = device.bus_number();
                let address = device.address();

                if descriptor.vendor_id() == VID_GOXLR
                    && (descriptor.product_id() == PID_GOXLR_FULL
                        || descriptor.product_id() == PID_GOXLR_MINI)
                {
                    found_devices.push(GoXLRDevice {
                        bus_number,
                        address,
                        identifier: None,
                    });
                }
            }
        }
    }

    found_devices
}

pub fn get_interface_version() -> (DriverInterface, Option<VersionNumber>) {
    let version = rusb::version();
    (
        DriverInterface::LIBUSB,
        Some(VersionNumber(
            version.major() as u32,
            version.minor() as u32,
            Some(version.micro() as u32),
            None,
        )),
    )
}

/// Pauses the polling while it lives (a request in flight), resumes it when dropped.
struct PollingPause(Arc<AtomicBool>);

impl PollingPause {
    fn new(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self(flag)
    }
}

impl Drop for PollingPause {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}
