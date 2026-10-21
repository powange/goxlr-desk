//! Each GoXLR runs in its own task, which owns its `Device`: it reads the device's inputs, runs
//! the periodic update and the jobs sent to it, then sends its status back. A slow device (a USB
//! timeout, a firmware step) no longer holds up the other devices or the daemon's main loop.

use crate::device::Device;
use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;
use goxlr_ipc::MixerStatus;
use log::{debug, warn};
use tokio::sync::{mpsc, oneshot};

// Ticks (50 ms) between two status refreshes when nothing reported a change
const STATUS_REFRESH_TICKS: u32 = 20;

/// An answer to whoever asked for a job, given once the status it changed is in place.
pub type Reply = Box<dyn FnOnce() + Send>;

/// Something to run on the device: whether the status may have changed, and the answer.
type Job = Box<dyn for<'d> FnOnce(&'d mut Device) -> BoxFuture<'d, (bool, Option<Reply>)> + Send>;

/// A device's status, sent by its task after anything that may have changed it. The worker
/// applies the status, then gives the reply: a client asking for the status right after its
/// command sees the change.
pub struct DeviceUpdate {
    pub serial: String,
    pub status: MixerStatus,
    pub reply: Option<Reply>,
}

pub struct DeviceActor {
    jobs: mpsc::UnboundedSender<Job>,
    // Input polls and periodic updates coalesce: one pending is enough, the next run sees
    // everything (a busy device doesn't pile them up)
    inputs: mpsc::Sender<()>,
    ticks: mpsc::Sender<()>,
}

impl DeviceActor {
    pub fn spawn(device: Device, updates: mpsc::Sender<DeviceUpdate>) -> Self {
        let (jobs, mut job_rx) = mpsc::unbounded_channel::<Job>();
        let (inputs, mut input_rx) = mpsc::channel::<()>(1);
        let (ticks, mut tick_rx) = mpsc::channel::<()>(1);

        tokio::spawn(async move {
            let mut device = device;
            let serial = device.serial().to_string();
            let mut ticks_since_status = 0;
            loop {
                let (changed, reply) = tokio::select! {
                    job = job_rx.recv() => match job {
                        Some(job) => job(&mut device).await,
                        // The worker dropped this device (disconnected, or shutting down)
                        None => break,
                    },
                    Some(()) = input_rx.recv() => match device.monitor_inputs().await {
                        Ok(changed) => (changed, None),
                        Err(error) => {
                            warn!("Error Received from {}: {}", serial, error);
                            (false, None)
                        }
                    },
                    Some(()) = tick_rx.recv() => match device.update_state().await {
                        // Once a second the status is sent anyway, so nothing a change
                        // forgot to report stays out of it (the worker only sends differences)
                        Ok(changed) => {
                            ticks_since_status += 1;
                            (changed || ticks_since_status >= STATUS_REFRESH_TICKS, None)
                        }
                        Err(error) => {
                            warn!("Error Received from {} while updating state: {}", serial, error);
                            (false, None)
                        }
                    },
                };
                if changed {
                    ticks_since_status = 0;
                }
                if !changed {
                    if let Some(reply) = reply {
                        reply();
                    }
                    continue;
                }
                let update = DeviceUpdate {
                    serial: serial.clone(),
                    status: device.status().await,
                    reply,
                };
                if let Err(unsent) = updates.send(update).await {
                    // The worker has stopped, answer anyway
                    if let Some(reply) = unsent.0.reply {
                        reply();
                    }
                    break;
                }
            }
            debug!("Device task for {} stopped", serial);
        });

        Self {
            jobs,
            inputs,
            ticks,
        }
    }

    /// The device has new input state to read (a poll, or a button event).
    pub fn input_event(&self) {
        let _ = self.inputs.try_send(());
    }

    /// Time for the periodic update (held buttons, sampler, unsaved changes).
    pub fn tick(&self) {
        let _ = self.ticks.try_send(());
    }

    fn queue(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            warn!("The device task has stopped, the job is dropped");
        }
    }

    /// Runs `job` on the device without waiting for it; it returns whether the status changed.
    pub fn send<F>(&self, job: F)
    where
        F: for<'d> FnOnce(&'d mut Device) -> BoxFuture<'d, bool> + Send + 'static,
    {
        self.queue(Box::new(move |device| {
            Box::pin(async move { (job(device).await, None) })
        }));
    }

    /// Runs `job` on the device without waiting for it, and sends its result to `reply` once
    /// the status is up to date (`changes`: whether the job may change the status at all).
    pub fn send_reply<R, F>(&self, changes: bool, reply: oneshot::Sender<R>, job: F)
    where
        R: Send + 'static,
        F: for<'d> FnOnce(&'d mut Device) -> BoxFuture<'d, R> + Send + 'static,
    {
        self.queue(Box::new(move |device| {
            Box::pin(async move {
                let result = job(device).await;
                let answer: Reply = Box::new(move || {
                    let _ = reply.send(result);
                });
                (changes, Some(answer))
            })
        }));
    }

    /// Runs `job` on the device and waits for its result. For the worker itself: the result
    /// comes straight from the device task (waiting for the worker to pass it on would wait
    /// for ever), and the new status follows.
    pub async fn run<R, F>(&self, job: F) -> Result<R>
    where
        R: Send + 'static,
        F: for<'d> FnOnce(&'d mut Device) -> BoxFuture<'d, R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.queue(Box::new(move |device| {
            Box::pin(async move {
                let _ = tx.send(job(device).await);
                (true, None)
            })
        }));
        rx.await.map_err(|_| anyhow!("The device task has stopped"))
    }
}
