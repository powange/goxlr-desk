use crate::audio::{AudioOutput, AudioSpecification, OpenOutputStream};
use anyhow::{Result, anyhow};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use std::thread;
use std::time::Duration;

pub struct PulsePlayback {
    pulse_simple: Simple,
    // Reused for each write, rather than allocating one per packet
    bytes: Vec<u8>,
}

// How much audio the server keeps ahead: 20 ms, a scheduling hiccup doesn't make it underrun
const TARGET_LATENCY_MS: u32 = 20;

impl OpenOutputStream for PulsePlayback {
    fn open(spec: AudioSpecification) -> Result<Box<dyn AudioOutput>> {
        let pulse_spec = Spec {
            format: Format::FLOAT32NE,
            channels: spec.spec.channels.count() as u8,
            rate: spec.spec.rate,
        };

        if !pulse_spec.is_valid() {
            // Invalid Specification, Error Out..
            return Err(anyhow!("Invalid Pulse Specification"));
        }

        let device_string;
        let device_str = match spec.device {
            None => None,
            Some(value) => {
                device_string = value;
                Some(device_string.as_str())
            }
        };

        // We need to maintain a relatively small buffer..
        let bytes_per_second = pulse_spec.rate * pulse_spec.channels as u32 * 4;
        let pulse_buffer_attributes = BufferAttr {
            maxlength: u32::MAX,
            tlength: bytes_per_second * TARGET_LATENCY_MS / 1000,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };

        // Create the Connection (Use Pulse Simple for this, because, well, it's simple!)
        let pulse = Simple::new(
            None,
            "GoXLR Utility",
            Direction::Playback,
            device_str,
            "Media",
            &pulse_spec,
            Default::default(),
            Some(&pulse_buffer_attributes),
        );

        match pulse {
            Ok(pulse_simple) => {
                thread::sleep(Duration::from_millis(75));
                Ok(Box::new(PulsePlayback {
                    pulse_simple,
                    bytes: vec![],
                }))
            }
            Err(_) => Err(anyhow!("Unable to Connect to Pulse")),
        }
    }
}

impl AudioOutput for PulsePlayback {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        // The stream is FLOAT32NE: native byte order
        self.bytes.clear();
        for sample in samples {
            self.bytes.extend_from_slice(&sample.to_ne_bytes());
        }

        // A dead stream is an error, otherwise the player would run through the file at full
        // speed without playing anything.
        self.pulse_simple
            .write(self.bytes.as_slice())
            .map_err(|e| anyhow!("Unable to play on PulseAudio: {}", e))
    }

    fn flush(&mut self) {
        let _ = self.pulse_simple.drain();
    }

    fn stop(&mut self) {}
}
