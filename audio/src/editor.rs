use anyhow::{Result, bail};
use std::fs::File;
use std::io::ErrorKind::UnexpectedEof;
use std::io::{Seek, Write};
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::errors::Error;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

/// A whole audio file decoded into memory, for editing.
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    // Interleaved samples, between -1.0 and 1.0
    pub samples: Vec<f32>,
    // The source's bit depth when it's integer PCM (WAV), kept when writing it back.
    pub bits_per_sample: Option<u16>,
}

impl DecodedAudio {
    /// Decodes a WAV, MP3, OGG or FLAC file.
    pub fn decode(path: &Path) -> Result<Self> {
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            hint.with_extension(extension);
        }

        let stream = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
        let probe = get_probe().format(&hint, stream, &Default::default(), &Default::default())?;
        let mut reader = probe.format;

        let Some(track) = reader.default_track() else {
            bail!("No audio track found");
        };
        let track_id = track.id;
        let params = track.codec_params.clone();

        let Some(sample_rate) = params.sample_rate else {
            bail!("Unable to determine the sample rate");
        };
        let channels = match params.channels {
            Some(channels) => channels.count() as u16,
            None => bail!("Unable to determine the channel count"),
        };
        let bits_per_sample = params
            .bits_per_sample
            .filter(|bits| matches!(bits, 16 | 24 | 32))
            .filter(|_| {
                params.codec == symphonia::core::codecs::CODEC_TYPE_PCM_S16LE
                    || params.codec == symphonia::core::codecs::CODEC_TYPE_PCM_S24LE
                    || params.codec == symphonia::core::codecs::CODEC_TYPE_PCM_S32LE
            })
            .map(|bits| bits as u16);

        let mut decoder = get_codecs().make(&params, &Default::default())?;
        let mut samples = Vec::new();
        let mut buffer: Option<SampleBuffer<f32>> = None;

        loop {
            let packet = match reader.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(error)) if error.kind() == UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            };
            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let buffer = buffer.get_or_insert_with(|| {
                        SampleBuffer::new(decoded.capacity() as u64, *decoded.spec())
                    });
                    if buffer.capacity() < decoded.capacity() * channels as usize {
                        *buffer = SampleBuffer::new(decoded.capacity() as u64, *decoded.spec());
                    }
                    buffer.copy_interleaved_ref(decoded);
                    samples.extend_from_slice(buffer.samples());
                }
                // A damaged packet is skipped, the rest of the file is still usable.
                Err(Error::DecodeError(_)) => continue,
                Err(error) => return Err(error.into()),
            }
        }

        if samples.is_empty() {
            bail!("The file doesn't contain any audio");
        }

        Ok(Self {
            sample_rate,
            channels,
            samples,
            bits_per_sample,
        })
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Removes parts of the audio, given as (start, end) times in seconds on the original
    /// timeline. Ranges may overlap or be in any order. Refuses to remove everything.
    pub fn remove_ranges(&mut self, ranges: &[(f64, f64)]) -> Result<()> {
        let frames = self.frames();
        let to_frame = |seconds: f64| -> usize {
            if !seconds.is_finite() || seconds <= 0.0 {
                return 0;
            }
            ((seconds * self.sample_rate as f64).round() as usize).min(frames)
        };

        let mut ranges: Vec<(usize, usize)> = ranges
            .iter()
            .map(|(start, end)| (to_frame(start.min(*end)), to_frame(start.max(*end))))
            .filter(|(start, end)| end > start)
            .collect();
        ranges.sort();

        // Keep every frame that isn't inside a range.
        let channels = self.channels as usize;
        let mut kept = Vec::with_capacity(self.samples.len());
        let mut position = 0;
        for (start, end) in ranges {
            if start > position {
                kept.extend_from_slice(&self.samples[position * channels..start * channels]);
            }
            position = position.max(end);
        }
        if position < frames {
            kept.extend_from_slice(&self.samples[position * channels..frames * channels]);
        }

        if kept.is_empty() {
            bail!("The edit would remove all of the audio");
        }
        self.samples = kept;
        Ok(())
    }

    /// Writes the audio as a WAV file, keeping the source's bit depth when it was integer PCM
    /// (24 bit otherwise).
    pub fn write_wav<W: Write + Seek>(&self, writer: W) -> Result<()> {
        let bits = self.bits_per_sample.unwrap_or(24);
        let spec = hound::WavSpec {
            channels: self.channels,
            sample_rate: self.sample_rate,
            bits_per_sample: bits,
            sample_format: hound::SampleFormat::Int,
        };

        let scale = ((1_i64 << (bits - 1)) - 1) as f32;
        let mut writer = hound::WavWriter::new(writer, spec)?;
        for sample in &self.samples {
            writer.write_sample((sample.clamp(-1.0, 1.0) * scale).round() as i32)?;
        }
        writer.finalize()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // One second of a stereo 16 bit 1kHz-ish ramp at 8kHz, written with hound.
    fn test_file(path: &Path) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..8000_i32 {
            let value = ((frame % 100) * 300) as i16;
            writer.write_sample(value).unwrap();
            writer.write_sample(-value).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn decode_edit_and_write() {
        let path = std::env::temp_dir().join(format!("goxlr-edit-{}.wav", std::process::id()));
        test_file(&path);

        let mut audio = DecodedAudio::decode(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            (audio.sample_rate, audio.channels, audio.bits_per_sample),
            (8000, 2, Some(16))
        );
        assert_eq!(audio.frames(), 8000);

        // Crop to 0.25s - 0.75s, then cut 0.1s out of the middle (overlapping ranges are fine)
        audio
            .remove_ranges(&[(0.0, 0.25), (0.75, 1.0), (0.4, 0.5), (0.45, 0.5)])
            .unwrap();
        assert_eq!(audio.frames(), 3200);
        assert!((audio.duration() - 0.4).abs() < 0.001);

        // The first kept frames are frames 2000 and 2001 of the original (values 0 and 300)
        assert!(audio.samples[0].abs() < 0.001);
        assert!((audio.samples[2] - 300.0 / 32767.0).abs() < 0.001);

        let mut written = Cursor::new(Vec::new());
        audio.write_wav(&mut written).unwrap();
        let reader = hound::WavReader::new(Cursor::new(written.into_inner())).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 16);
        assert_eq!(reader.len(), 3200 * 2);

        // Removing everything is refused
        assert!(audio.remove_ranges(&[(0.0, 10.0)]).is_err());
    }
}
