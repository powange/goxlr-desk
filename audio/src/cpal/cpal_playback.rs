use crate::audio::{AudioOutput, AudioSpecification, OpenOutputStream};
use crate::cpal::cpal_config::CpalConfiguration;
use anyhow::{Result, anyhow, bail};
use cpal::Stream;
use cpal::traits::{DeviceTrait, StreamTrait};
use log::{debug, warn};
use rb::{Producer, RB, RbConsumer, RbInspector, RbProducer, SpscRb};
use rubato::{FftFixedIn, Resampler};

// How long the device may not take any sample before playback gives up
const STALL_TIMEOUT: Duration = Duration::from_secs(2);
use std::panic::catch_unwind;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// Create a 50ms Buffer Size for playback, this should be short enough to ensure there aren't
// any obvious delays when playing samples.
const BUFFER_SIZE: usize = 50;

pub(crate) struct CpalPlayback {
    stream: Option<Stream>,
    stream_closed: Arc<AtomicBool>,

    buffer: SpscRb<f32>,
    buffer_producer: Producer<f32>,

    // Resampler Related Variables..
    resampler: Option<CpalResampler>,
}

struct CpalResampler {
    resampler: FftFixedIn<f32>,
    input_buffer: Vec<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl OpenOutputStream for CpalPlayback {
    fn open(spec: AudioSpecification) -> Result<Box<dyn AudioOutput>> {
        let device = match catch_unwind(|| CpalConfiguration::get_device(spec.device, false)) {
            Ok(Ok(device)) => device,
            Ok(Err(e)) => bail!("Error Fetching Device: {}", e),
            Err(e) => bail!("PANIC attempting to Fetch Device! {:#?}", e),
        };

        let config = if cfg!(target_os = "windows") {
            // Windows expects the file to be resampled to the output config, so we can't use the
            // input audio. Instead, we gotta resample.
            match catch_unwind(|| device.default_output_config()) {
                Ok(Ok(output)) => output.config(),
                Ok(Err(e)) => bail!("Unable to get Default Stream: {}", e),
                Err(e) => bail!("PANIC Attempting to Default Stream: {:#?}", e),
            }
        } else {
            // MacOS will resample inside CoreAudio, so we send the samples directly.
            cpal::StreamConfig {
                channels: spec.spec.channels.count() as cpal::ChannelCount,
                sample_rate: cpal::SampleRate(spec.spec.rate),
                buffer_size: cpal::BufferSize::Fixed(64),
            }
        };

        // Before we go any further, is the channel count of the audio correct?
        if spec.spec.channels.count() != 2 {
            bail!("Only stereo audio is supported");
        }

        // Calculate the buffer size based on the sample count in BUFFER_SIZE milliseconds..
        let size = (BUFFER_SIZE * config.sample_rate.0 as usize) / 1000;
        let buffer_length = size * config.channels as usize;

        // Create the Actual Buffer
        let buffer = SpscRb::<f32>::new(buffer_length);
        let buffer_producer = buffer.producer();
        let buffer_consumer = buffer.consumer();

        // Prepare a bool to close the reader if CPAL throws an error..
        let stream_closed = Arc::new(AtomicBool::new(false));
        let stream_closed_inner = stream_closed.clone();

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Read from the ring buffer, and write them to the data array
                let written = buffer_consumer.read(data).unwrap_or(0);

                // Data expects a certain number of samples, if we didn't get enough from above,
                // mute anything afterwards as we're probably EoS
                data[written..].iter_mut().for_each(|s| *s = 0.0);
            },
            move |e| {
                warn!("Error on Playback Stream, Stopping.. {}", e);
                stream_closed_inner.store(true, Ordering::Relaxed);
            },
            Some(Duration::from_millis(500)),
        )?;
        stream.play()?;

        // Do we need to resample?
        let resampler = if spec.spec.rate != config.sample_rate.0 {
            debug!(
                "Creating Resampler from {} to {} using Buffer: {}",
                spec.spec.rate, config.sample_rate.0, spec.buffer
            );

            // Create a resampler..
            let resampler = FftFixedIn::<f32>::new(
                spec.spec.rate as usize,
                config.sample_rate.0 as usize,
                spec.buffer,
                2,
                spec.spec.channels.count(),
            )?;

            // Create a buffer to hold samples until we can resample..
            let input_buffer = Vec::with_capacity(spec.buffer * spec.spec.channels.count());

            // Allocate the Input and Output Buffers..
            let input = vec![vec![0_f32; spec.buffer]; spec.spec.channels.count()];
            let output = Resampler::output_buffer_allocate(&resampler, true);

            Some(CpalResampler {
                resampler,
                input_buffer,
                input,
                output,
            })
        } else {
            None
        };

        Ok(Box::new(Self {
            stream: Some(stream),
            stream_closed,

            buffer,
            buffer_producer,

            resampler,
        }))
    }
}

impl AudioOutput for CpalPlayback {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        if self.stream_closed.load(Ordering::Relaxed) {
            bail!("Stream has been closed");
        }

        // Do nothing if there are no samples sent..
        if samples.is_empty() {
            return Ok(());
        }

        let resampled;
        let out_samples: &[f32] = if let Some(resampler) = &mut self.resampler {
            // Queue the samples, then resample in blocks of exactly the size the resampler takes.
            // What's left waits for the next packet: packets vary in size (Vorbis, the last one of
            // a file), and dropping the excess made clicks and jumps.
            resampler.input_buffer.extend(samples);
            let channels = resampler.input.len();
            let mut output = vec![];
            loop {
                let required = resampler.resampler.input_frames_next() * channels;
                if resampler.input_buffer.len() < required {
                    break;
                }
                let block: Vec<f32> = resampler.input_buffer.drain(..required).collect();

                // The samples are interleaved, the resampler wants one list per channel.
                for (channel, input) in resampler.input.iter_mut().enumerate() {
                    input.clear();
                    input.extend(block.iter().skip(channel).step_by(channels).copied());
                }

                // A failure is an error: playing the block unresampled would be at the wrong rate
                let (_, frames) = resampler
                    .resampler
                    .process_into_buffer(&resampler.input, &mut resampler.output, None)
                    .map_err(|e| anyhow!("Resampling failed: {}", e))?;

                // Interleave the result again
                output.reserve(frames * channels);
                for frame in 0..frames {
                    for channel in resampler.output.iter() {
                        output.push(channel[frame]);
                    }
                }
            }
            resampled = output;
            &resampled
        } else {
            // No resampler needed, send back the samples
            samples
        };

        // Waits for room in the playback buffer, but not for ever: a device that stops calling
        // back without an error would otherwise freeze this player (and never see a stop).
        let mut position = 0;
        let mut waiting_since = Instant::now();
        while position < out_samples.len() {
            if self.stream_closed.load(Ordering::Relaxed) {
                bail!("The audio stream was closed");
            }
            match self
                .buffer_producer
                .write_blocking_timeout(&out_samples[position..], Duration::from_millis(100))
            {
                Ok(Some(written)) => {
                    position += written;
                    waiting_since = Instant::now();
                }
                Ok(None) => break,
                Err(_) if waiting_since.elapsed() < STALL_TIMEOUT => {}
                Err(_) => bail!("The audio device stopped taking samples"),
            }
        }

        Ok(())
    }

    fn flush(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            let length = resampler.input_buffer.len();
            let capacity = resampler.input_buffer.capacity();

            if length != 0 {
                // There's some stuff left in the buffer, fill the buffer to capacity to
                // flush it through to the output buffer.
                let filling_samples = vec![0.; capacity - length];
                let _ = self.write(&filling_samples);
            }
        }

        // Make sure the playback buffer is empty, to prevent premature pausing at
        // the end of playback
        let started = Instant::now();
        while !self.buffer.is_empty() {
            // Make sure the Stream hasn't closed while handling the buffer, or stalled..
            if self.stream_closed.load(Ordering::Relaxed) || started.elapsed() > STALL_TIMEOUT {
                return;
            }

            // Wait briefly for samples to flush
            std::thread::sleep(Duration::from_millis(5));
        }

        if let Some(stream) = &self.stream {
            let _ = stream.pause();
        }
    }

    fn stop(&mut self) {
        // We're going to take the stream and drop it, this should stop playback immediately.
        self.stream_closed.store(true, Ordering::Relaxed);
        self.stream.take();
    }
}
