use anyhow::{Result, anyhow, bail};

use core::default::Default;
use ebur128::{EbuR128, Mode};
use log::{debug, warn};
use std::fs::File;
use std::io::ErrorKind::UnexpectedEof;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crate::AtomicF64;
use crate::audio::{AudioSpecification, get_output};
use symphonia::core::audio::{Layout, SampleBuffer, SignalSpec};
use symphonia::core::errors::Error;
use symphonia::core::formats::{SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::{Hint, ProbeResult};
use symphonia::default::get_codecs;

pub struct Player {
    file: PathBuf,
    probe: ProbeResult,

    volume: f32,

    // I should really introduce messaging for this..
    stopping: Arc<AtomicBool>,
    force_stop: Arc<AtomicBool>,
    restart_track: Arc<AtomicBool>,

    device: Option<String>,
    fade_duration: Option<u32>,
    start_pct: Option<f64>,
    stop_pct: Option<f64>,
    gain: Option<f64>,

    progress: Arc<AtomicU8>,
    error: Arc<Mutex<Option<String>>>,

    // Used for processing Gain..
    process_only: bool,
    normalized_gain: Arc<AtomicF64>,
}

impl Player {
    /// Load up the Player, and prepare for playback..
    pub fn new(
        file: &PathBuf,
        device: Option<String>,
        fade_duration: Option<u32>,
        start_pct: Option<f64>,
        stop_pct: Option<f64>,
        gain: Option<f64>,
    ) -> Result<Self> {
        let probe_result = Player::load_file(file);
        if probe_result.is_err() {
            return Err(anyhow!("Unable to Probe Audio File"));
        }

        Ok(Self {
            file: file.clone(),

            probe: probe_result.unwrap(),
            volume: 1.0_f32,
            stopping: Arc::new(AtomicBool::new(false)),
            force_stop: Arc::new(AtomicBool::new(false)),
            restart_track: Arc::new(AtomicBool::new(false)),

            progress: Arc::new(AtomicU8::new(0)),
            error: Arc::new(Mutex::new(None)),

            device,
            fade_duration,
            start_pct,
            stop_pct,
            gain,

            process_only: false,
            normalized_gain: Arc::new(AtomicF64::new(1.0)),
        })
    }

    fn load_file(file: &PathBuf) -> symphonia::core::errors::Result<ProbeResult> {
        // Use the file extension to get a type hint..
        let mut hint = Hint::new();
        if let Some(extension) = file.extension()
            && let Some(extension_str) = extension.to_str()
        {
            hint.with_extension(extension_str);
        }

        // A sample removed or unreadable is an error for this playback, not a panic
        let media_source = Box::new(File::open(file).map_err(Error::IoError)?);
        let stream = MediaSourceStream::new(media_source, Default::default());

        let format_options = Default::default();
        let metadata_options = Default::default();

        symphonia::default::get_probe().format(&hint, stream, &format_options, &metadata_options)
    }

    pub fn calculate_gain(&mut self) {
        self.process_only = true;

        let result = self.play();
        if let Err(error) = result {
            let mut res = self.error.lock().unwrap();
            *res = Some(error.to_string());
        }
    }

    pub fn play_loop(&mut self) -> Result<()> {
        self.play_internal(true)
    }

    pub fn play(&mut self) -> Result<()> {
        self.play_internal(false)
    }

    fn play_internal(&mut self, looping: bool) -> Result<()> {
        let reader = &mut self.probe.format;

        // Grab the Track and it's ID
        let track = match reader.default_track() {
            Some(track) => track,
            None => bail!("Unable to find Default Track"),
        };
        let track_id = track.id;

        // The per-sample volume change when fading.
        let mut fade_amount: Option<f32> = None;

        // Sample Start and Stop positions..
        let mut first_frame: Option<u64> = None;
        let mut stop_sample: Option<u64> = None;

        let sample_rate = track.codec_params.sample_rate;
        let frames = track.codec_params.n_frames;

        let mut ebu_r128 = None;

        let channels = match track.codec_params.channels {
            None => bail!("Unable to obtain channel count"),
            Some(channels) => channels.count(),
        };

        if channels > 2 {
            bail!("The Sample Player only Supports Mono and Stereo Samples");
        }

        if let Some(rate) = sample_rate {
            if self.process_only {
                // Measured as it's played: a mono file comes out on both channels.
                ebu_r128 = Some(EbuR128::new(channels.max(2) as u32, rate, Mode::I)?);
            } else {
                if let Some(fade_duration) = self.fade_duration {
                    // When fading out, we need work out the number of samples related to the fade
                    // duration, so (rate * duration) should give us the expected frame count, but
                    // we also need to multiply by the channel count to get the sample count.
                    //
                    // Once we have that calculated, we need to convert it to a volume multiplier,
                    // so for example, 48,000hz * 0.5 * 2 channels = 48,000 samples
                    // 1.0 / 48000 = 0.00002(ish), and we'll subtract that from the volume
                    // (between 1 and 0) for every sample received.
                    //
                    // This should have the volume at 0 after 48,000 iterations, at which point
                    // we terminate.
                    // The fade is applied to the output, which is stereo for a mono file.
                    let output_channels = channels.max(2);
                    fade_amount = Some(
                        1.0 / ((rate as f32 * (fade_duration as f32 / 1000f32))
                            * output_channels as f32),
                    );
                }

                if let Some(frames) = frames {
                    if let Some(start_pct) = self.start_pct {
                        // Calculate the first frame based on the percent..
                        first_frame = Some(((frames as f64 / 100.0) * start_pct).round() as u64);
                        debug!(
                            "Starting Sample: {}",
                            first_frame.unwrap() * channels as u64
                        );
                    }

                    if let Some(stop_pct) = self.stop_pct {
                        stop_sample = Some(
                            ((frames as f64 / 100.0) * stop_pct).round() as u64 * channels as u64,
                        );
                        debug!("Stop Sample: {}", stop_sample.unwrap());
                    }
                }
            }
        } else {
            bail!("Unable to Determine the Audio File's Sample Rate");
        }

        // Audio Output Device..
        let mut audio_output = None;

        // Create a Decoder..
        let decoder_opts = Default::default();
        let mut decoder = get_codecs().make(&track.codec_params, &decoder_opts)?;

        // Prepare the Sample Buffer..
        let mut sample_buffer = None;

        // Start the Processed Sample Count..
        let mut samples_processed = if let Some(frame) = first_frame {
            let seek_time = SeekTo::TimeStamp {
                ts: frame,
                track_id,
            };

            match reader.seek(SeekMode::Accurate, seek_time) {
                Ok(seeked_to) => seeked_to.actual_ts * channels as u64,
                Err(_) => 0,
            }
        } else {
            0
        };

        let mut mono_playback = false;

        // Keep the decoder and output stream alive when looping. Re-opening either at every EOF
        // leaves the output callback without data and produces an audible gap.
        let loop_start = first_frame.unwrap_or_default();

        // Loop over the input file..
        let result = 'main: loop {
            let packet = match reader.next_packet() {
                Ok(packet) => packet,
                Err(err) => {
                    if looping
                        && !self.stopping.load(Ordering::Relaxed)
                        && matches!(&err, Error::IoError(error) if error.kind() == UnexpectedEof)
                    {
                        let seek_time = SeekTo::TimeStamp {
                            ts: loop_start,
                            track_id,
                        };

                        match reader.seek(SeekMode::Accurate, seek_time) {
                            Ok(seeked_to) => {
                                decoder.reset();
                                samples_processed = seeked_to.actual_ts * channels as u64;
                                continue;
                            }
                            Err(seek_error) => break Err(seek_error),
                        }
                    }
                    break Err(err);
                }
            };

            if packet.track_id() != track_id {
                // Doesn't belong to us.
                continue;
            }

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    // Is this the first decoded packet?
                    if audio_output.is_none() && sample_buffer.is_none() {
                        let spec = *decoded.spec();
                        let mut output_spec = spec;

                        // This is a mono audio file, we need to replace the spec with a Stereo
                        // spec, then during processing, duplicate and interlace the samples.
                        if spec.channels.count() == 1 {
                            mono_playback = true;
                            output_spec = SignalSpec::new_with_layout(spec.rate, Layout::Stereo);
                        }

                        let capacity = decoded.capacity() as u64;
                        sample_buffer = Some(SampleBuffer::<f32>::new(capacity, spec));

                        if !self.process_only {
                            let audio_spec = AudioSpecification {
                                device: self.device.clone(),
                                spec: output_spec,
                                buffer: capacity as usize,
                            };

                            audio_output.replace(get_output(audio_spec)?);
                        }
                    }

                    if let Some(ref mut buf) = sample_buffer {
                        // Grab out the samples..
                        buf.copy_interleaved_ref(decoded);
                        let mut samples = buf.samples().to_vec();

                        // Positions (start, stop, progress) count the file's own samples, before
                        // a mono file is doubled to stereo.
                        let source_samples = samples.len() as u64;

                        if mono_playback {
                            let mut buffer = vec![];
                            samples.iter().for_each(|s| {
                                buffer.push(*s);
                                buffer.push(*s)
                            });
                            samples = buffer;
                        }

                        if let Some(ref mut ebu_r128) = ebu_r128 {
                            ebu_r128.add_frames_f32(samples.as_slice())?;
                            samples_processed += source_samples;

                            let progress = Player::processed(frames, samples_processed, channels);
                            if self.progress.load(Ordering::Relaxed) != progress {
                                self.progress.store(progress, Ordering::Relaxed);
                            }

                            // Skip straight to the next packet..
                            continue;
                        }

                        // Apply any gain to the samples..
                        if let Some(gain) = self.gain {
                            for sample in samples.iter_mut() {
                                *sample *= gain as f32;
                            }
                        }

                        if self.stopping.load(Ordering::Relaxed) {
                            if self.force_stop.load(Ordering::Relaxed) {
                                // Don't care about the buffer, just end it.
                                debug!("Force Stop Requested, terminating.");
                                break 'main Ok(());
                            }

                            if let Some(fade_amount) = fade_amount {
                                // Technically, this is a little weird, we don't do a 'per-channel' check on the samples,
                                // so each channel will have a slightly different volume, for now it's small enough to not
                                // actually notice :p

                                for sample in samples.iter_mut() {
                                    *sample *= self.volume;

                                    // Has the fade amount dropped below 0?
                                    self.volume -= fade_amount;
                                    if self.volume < 0.0 {
                                        break 'main Ok(());
                                    }
                                }
                            } else {
                                // No fade duration, clear out sample buffer and end.
                                debug!("Stop Requested, No Fade Out set, Stopping Playback.");
                                break 'main Ok(());
                            }
                        }

                        // Flush the samples to the Audio Stream..
                        // The device can go away (unplugged, default output changed): end this
                        // playback with an error rather than panic.
                        if let Some(audio_output) = &mut audio_output
                            && let Err(e) = audio_output.write(&samples)
                        {
                            break 'main Err(Error::IoError(std::io::Error::other(e.to_string())));
                        }

                        samples_processed += source_samples;

                        // Calculate the Current Processing Percent..
                        let progress = Player::processed(frames, samples_processed, channels);
                        if self.progress.load(Ordering::Relaxed) != progress {
                            self.progress.store(progress, Ordering::Relaxed);
                        }

                        if let Some(stop_sample) = stop_sample
                            && samples_processed >= stop_sample
                        {
                            if looping && !self.stopping.load(Ordering::Relaxed) {
                                let seek_time = SeekTo::TimeStamp {
                                    ts: loop_start,
                                    track_id,
                                };

                                match reader.seek(SeekMode::Accurate, seek_time) {
                                    Ok(seeked_to) => {
                                        decoder.reset();
                                        samples_processed = seeked_to.actual_ts * channels as u64;
                                        continue 'main;
                                    }
                                    Err(error) => break 'main Err(error),
                                }
                            }
                            break Ok(());
                        }

                        if self.restart_track.load(Ordering::Relaxed) {
                            // We've been prompted to restart the current track..
                            let start_frame = first_frame.unwrap_or_default();

                            let seek_time = SeekTo::TimeStamp {
                                ts: start_frame,
                                track_id,
                            };

                            // Seek the reader backwards to the start, and reset the decoder as the
                            // loops do. A failed seek leaves the position as it is.
                            match reader.seek(SeekMode::Accurate, seek_time) {
                                Ok(seeked_to) => {
                                    decoder.reset();
                                    samples_processed = seeked_to.actual_ts * channels as u64;
                                }
                                Err(e) => debug!("Error Seeking: {}", e),
                            }

                            // Set the back to FALSE
                            self.restart_track.store(false, Ordering::Relaxed);
                        }
                    }
                }
                // A damaged packet is skipped (Symphonia documents decode errors as
                // recoverable), the rest of the file still plays.
                Err(Error::DecodeError(e)) => {
                    warn!("Skipping a damaged audio packet: {}", e);
                    continue;
                }
                Err(err) => break Err(err),
            }
        };
        if !self.force_stop.load(Ordering::Relaxed)
            && let Some(ref mut audio_output) = audio_output
        {
            // We should always flush the last samples, unless forced to stop
            audio_output.flush();
        }

        // Stop the playback handler..
        if let Some(mut audio_output) = audio_output {
            audio_output.stop();
        }

        if let Some(ebu_r128) = ebu_r128 {
            // Calculate Gain..
            let mut loudness = ebu_r128.loudness_global()?;
            if loudness == f64::NEG_INFINITY {
                debug!("Unable to Obtain loudness in Mode I, trying M..");
                loudness = ebu_r128.loudness_momentary()?;
            }

            if loudness == f64::NEG_INFINITY {
                debug!("Unable to Obtain loudness in Mode M, Setting Default..");
                self.normalized_gain.store(1.0, Ordering::Relaxed);
            } else {
                let target = -23.0;
                let gain_db = target - loudness;
                let value = f64::powf(10., gain_db / 20.);

                self.normalized_gain.store(value, Ordering::Relaxed);
            }
        }
        decoder.finalize();

        // Symphonia's reader doesn't seem to have a way to check whether we've finished reading
        // bytes for the file, so will continue to try until it hits and EoF then drop an error.
        //
        // We're going to suppress that error here, with the assumption that if it's reached, the
        // file has ended, and playback is complete. It's not ideal, but we'll work with it.
        if let Err(error) = result {
            if let Error::IoError(ref error) = error
                && error.kind() == UnexpectedEof
            {
                return Ok(());
            }

            // Otherwise, throw this error up.
            bail!(error)
        }
        Ok(())
    }

    fn processed(total_frames: Option<u64>, current_frame: u64, channels: usize) -> u8 {
        // Calculate the Current Processing Percent..
        if let Some(frames) = total_frames {
            let frames_processed = current_frame / channels as u64;
            let processed_ratio = frames_processed as f64 / frames as f64;
            let calc_percent = (processed_ratio * 100.) as u8;

            return calc_percent;
        }
        0
    }

    pub fn get_state(&self) -> PlayerState {
        PlayerState {
            playing_file: self.file.clone(),
            stopping: self.stopping.clone(),
            force_stop: self.force_stop.clone(),
            restart_track: self.restart_track.clone(),
            progress: self.progress.clone(),
            error: self.error.clone(),
            calculated_gain: self.normalized_gain.clone(),
        }
    }
}

#[derive(Debug)]
pub struct PlayerState {
    // Note the file being played..
    pub playing_file: PathBuf,

    pub stopping: Arc<AtomicBool>,
    pub force_stop: Arc<AtomicBool>,

    // This is used for triggering a seek back the the beginning
    pub restart_track: Arc<AtomicBool>,

    // These are generally read only from the outside..
    pub progress: Arc<AtomicU8>,
    pub error: Arc<Mutex<Option<String>>>,

    // Specifically for calculating the gain..
    pub calculated_gain: Arc<AtomicF64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // One second of a 440 Hz tone at 8 kHz, as mono or as the same signal on both channels.
    fn tone(path: &std::path::Path, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..8000 {
            let value =
                ((frame as f32 * 440.0 * std::f32::consts::TAU / 8000.0).sin() * 8000.0) as i16;
            for _ in 0..channels {
                writer.write_sample(value).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    fn measure(path: &PathBuf) -> (f64, u8) {
        let mut player = Player::new(path, None, None, None, None, None).unwrap();
        let state = player.get_state();
        player.calculate_gain();
        assert!(state.error.lock().unwrap().is_none());
        (
            state.calculated_gain.load(Ordering::Relaxed),
            state.progress.load(Ordering::Relaxed),
        )
    }

    #[test]
    fn mono_files_are_measured_like_stereo() {
        let dir = std::env::temp_dir().join(format!("goxlr-player-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mono, stereo) = (dir.join("mono.wav"), dir.join("stereo.wav"));
        tone(&mono, 1);
        tone(&stereo, 2);

        let (mono_gain, mono_progress) = measure(&mono);
        let (stereo_gain, stereo_progress) = measure(&stereo);
        std::fs::remove_dir_all(&dir).unwrap();

        // Progress ends at 100%, not 200%, and the loudness doesn't depend on the layout.
        assert!((99..=100).contains(&mono_progress), "{mono_progress}");
        assert!((99..=100).contains(&stereo_progress), "{stereo_progress}");
        let ratio = mono_gain / stereo_gain;
        assert!((0.8..1.25).contains(&ratio), "{mono_gain} vs {stereo_gain}");
    }

    #[test]
    fn a_missing_file_is_an_error() {
        assert!(
            Player::new(
                &PathBuf::from("/nonexistent/sample.wav"),
                None,
                None,
                None,
                None,
                None
            )
            .is_err()
        );
    }
}
