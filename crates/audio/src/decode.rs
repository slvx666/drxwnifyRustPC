//! Decoding any supported stream to interleaved stereo `f32`.
//!
//! Containers: MP4/M4A, WebM/Matroska, Ogg, MP3, FLAC, WAV (symphonia). Codecs: AAC-LC, MP3, FLAC, Vorbis, PCM
//! (symphonia) and Opus (libopus — symphonia has no Opus decoder). MPEG-TS (VK HLS) and HE-AAC come later.
//!
//! Output is always stereo: mono is duplicated, more channels are reduced to front left/right.
//! Opus pre-skip (from `OpusHead`) is dropped at the start so tracks begin exactly on the first real sample.

use std::io::{Read, Seek};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unsupported stream: {0}")]
    Unsupported(String),
    #[error("stream: {0}")]
    Stream(#[from] SymError),
    #[error("opus: {0}")]
    Opus(#[from] opus::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type DecodeResult<T> = std::result::Result<T, DecodeError>;

/// A readable, seekable byte source. Files, memory and the HTTP cache source implement it.
pub trait ByteSource: Read + Seek + Send + Sync {
    fn byte_len(&self) -> Option<u64>;
}

struct SourceAdapter<S: ByteSource>(S);

impl<S: ByteSource> Read for SourceAdapter<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl<S: ByteSource> Seek for SourceAdapter<S> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}
impl<S: ByteSource> MediaSource for SourceAdapter<S> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.0.byte_len()
    }
}

impl ByteSource for std::fs::File {
    fn byte_len(&self) -> Option<u64> {
        self.metadata().ok().map(|m| m.len())
    }
}
impl ByteSource for Box<dyn ByteSource> {
    fn byte_len(&self) -> Option<u64> {
        (**self).byte_len()
    }
}
impl ByteSource for std::io::Cursor<Vec<u8>> {
    fn byte_len(&self) -> Option<u64> {
        Some(self.get_ref().len() as u64)
    }
}

/// Opus frames are at most 120 ms; at 48 kHz stereo that is 5760 frames × 2.
const OPUS_MAX_SAMPLES: usize = 5760 * 2;

enum Inner {
    Sym(Box<dyn AudioDecoder>),
    Opus {
        dec: opus::Decoder,
        pre_skip: usize,
        skip_left: usize,
    },
}

/// A decoded block of audio.
pub struct Chunk<'a> {
    /// Start time of the block in seconds, when the container knows it.
    pub start_s: Option<f64>,
    /// Interleaved stereo samples.
    pub samples: &'a [f32],
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    track_id: u32,
    time_base: Option<TimeBase>,
    inner: Inner,
    rate: u32,
    channels: usize,
    duration_s: Option<f64>,
    raw: Vec<f32>,
    out: Vec<f32>,
}

impl Decoder {
    /// Opens a stream. `ext` is a hint like "m4a", "webm", "mp3" (speeds up probing; may be None).
    pub fn open<S: ByteSource + 'static>(source: S, ext: Option<&str>) -> DecodeResult<Self> {
        let mss = MediaSourceStream::new(Box::new(SourceAdapter(source)), Default::default());
        let mut hint = Hint::new();
        if let Some(e) = ext {
            hint.with_extension(e);
        }
        let format = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| DecodeError::Unsupported("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| DecodeError::Unsupported("no audio parameters".into()))?
            .clone();
        let track_id = track.id;
        let time_base = track.time_base;
        let duration_s = match (track.num_frames, params.sample_rate) {
            (Some(n), Some(r)) if r > 0 => Some(n as f64 / r as f64),
            _ => track.duration.zip(time_base).and_then(|(d, tb)| {
                tb.calc_time(Timestamp::new(d.get() as i64))
                    .map(|t| t.as_secs_f64())
            }),
        };

        let (inner, rate, channels) = if params.codec == CODEC_ID_OPUS {
            let head = params.extra_data.as_deref().unwrap_or_default();
            let (channels, pre_skip) = parse_opus_head(head)?;
            if channels > 2 {
                return Err(DecodeError::Unsupported(format!(
                    "Opus with {channels} channels"
                )));
            }
            let dec = opus::Decoder::new(48_000, opus::Channels::Stereo)?;
            (
                Inner::Opus {
                    dec,
                    pre_skip,
                    skip_left: pre_skip,
                },
                48_000,
                2,
            )
        } else {
            let dec = symphonia::default::get_codecs()
                .make_audio_decoder(&params, &AudioDecoderOptions::default())?;
            let rate = params
                .sample_rate
                .ok_or_else(|| DecodeError::Unsupported("unknown sample rate".into()))?;
            let channels = params
                .channels
                .as_ref()
                .map(|c| c.count())
                .unwrap_or(2)
                .max(1);
            (Inner::Sym(dec), rate, channels)
        };
        Ok(Self {
            format,
            track_id,
            time_base,
            inner,
            rate,
            channels,
            duration_s,
            raw: Vec::new(),
            out: Vec::new(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn duration_s(&self) -> Option<f64> {
        self.duration_s
    }

    /// Decodes the next block. `None` at the end of the stream. Corrupt packets are skipped.
    pub fn next_chunk(&mut self) -> DecodeResult<Option<Chunk<'_>>> {
        loop {
            let Some(packet) = self.format.next_packet()? else {
                return Ok(None);
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let start_s = self
                .time_base
                .and_then(|tb| tb.calc_time(packet.pts))
                .map(|t| t.as_secs_f64());
            match &mut self.inner {
                Inner::Sym(dec) => {
                    let buf = match dec.decode(&packet) {
                        Ok(b) => b,
                        Err(SymError::DecodeError(e)) => {
                            tracing::debug!("skipping corrupt packet: {e}");
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    };
                    if buf.frames() == 0 {
                        continue;
                    }
                    buf.copy_to_vec_interleaved(&mut self.raw);
                    to_stereo(&self.raw, self.channels, &mut self.out);
                }
                Inner::Opus { dec, skip_left, .. } => {
                    self.out.resize(OPUS_MAX_SAMPLES, 0.0);
                    let frames = match dec.decode_float(&packet.data, &mut self.out, false) {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::debug!("skipping corrupt opus packet: {e}");
                            continue;
                        }
                    };
                    self.out.truncate(frames * 2);
                    // Drop the OpusHead pre-skip ourselves. Symphonia's Ogg packet trims disagree with the Ogg Opus
                    // granule rules by one pre-skip (checked in a test), so they are ignored for Opus; the end padding
                    // (under one frame) stays.
                    let drop = (*skip_left).min(frames);
                    *skip_left -= drop;
                    self.out.drain(..drop * 2);
                    if self.out.is_empty() {
                        continue;
                    }
                }
            }
            return Ok(Some(Chunk {
                start_s,
                samples: &self.out,
            }));
        }
    }

    /// Seeks to `secs`. Returns the time actually reached (the container may land on an earlier packet).
    pub fn seek(&mut self, secs: f64) -> DecodeResult<f64> {
        let time = Time::try_from_secs_f64(secs.max(0.0)).unwrap_or(Time::ZERO);
        let track_id = Some(self.track_id);
        let to = self
            .format
            .seek(SeekMode::Coarse, SeekTo::Time { time, track_id })
            .or_else(|_| self.format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id }))?;
        match &mut self.inner {
            Inner::Sym(d) => d.reset(),
            Inner::Opus {
                dec,
                pre_skip,
                skip_left,
            } => {
                dec.reset_state()?;
                // After a seek to the very start the pre-skip applies again.
                *skip_left = if to.actual_ts.get() <= 0 {
                    *pre_skip
                } else {
                    0
                };
            }
        }
        Ok(self
            .time_base
            .and_then(|tb| tb.calc_time(to.actual_ts))
            .map(|t| t.as_secs_f64())
            .unwrap_or(secs))
    }
}

/// `OpusHead`: magic, version, channel count (byte 9), pre-skip (bytes 10-11, little endian).
fn parse_opus_head(h: &[u8]) -> DecodeResult<(usize, usize)> {
    if h.len() < 19 || &h[..8] != b"OpusHead" {
        // Some WebM files omit CodecPrivate; assume stereo with the usual 312-sample pre-skip.
        return Ok((2, 312));
    }
    Ok((h[9] as usize, u16::from_le_bytes([h[10], h[11]]) as usize))
}

/// Interleaved `n`-channel samples to interleaved stereo.
fn to_stereo(src: &[f32], n: usize, out: &mut Vec<f32>) {
    out.clear();
    match n {
        1 => {
            out.reserve(src.len() * 2);
            for &s in src {
                out.push(s);
                out.push(s);
            }
        }
        2 => out.extend_from_slice(src),
        _ => {
            out.reserve(src.len() / n * 2);
            for f in src.chunks_exact(n) {
                out.push(f[0]);
                out.push(f[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
        let data_len = frames * channels as usize * 2;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let s =
                ((i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * 16000.0) as i16;
            for _ in 0..channels {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        v
    }

    #[test]
    fn decodes_mono_wav_to_stereo() {
        let mut d = Decoder::open(Cursor::new(wav(44_100, 1, 44_100)), Some("wav")).unwrap();
        assert_eq!(d.sample_rate(), 44_100);
        assert!((d.duration_s().unwrap() - 1.0).abs() < 0.01);
        let mut total = 0;
        while let Some(c) = d.next_chunk().unwrap() {
            assert_eq!(c.samples.len() % 2, 0);
            total += c.samples.len() / 2;
        }
        assert_eq!(total, 44_100);
    }

    #[test]
    fn seeks_in_wav() {
        let mut d = Decoder::open(Cursor::new(wav(48_000, 2, 48_000 * 3)), Some("wav")).unwrap();
        let t = d.seek(2.0).unwrap();
        assert!((t - 2.0).abs() < 0.05, "landed at {t}");
        let mut total = 0;
        while let Some(c) = d.next_chunk().unwrap() {
            total += c.samples.len() / 2;
        }
        assert!(
            (total as i64 - 48_000).abs() < 2_000,
            "{total} frames after seek"
        );
    }

    #[test]
    fn decodes_ogg_opus_with_pre_skip() {
        let bytes = crate::oggopus::tests::encode_sine_ogg_opus(48_000);
        let mut d = Decoder::open(Cursor::new(bytes), Some("opus")).unwrap();
        assert_eq!(d.sample_rate(), 48_000);
        let mut total = 0;
        let mut peak = 0f32;
        while let Some(c) = d.next_chunk().unwrap() {
            total += c.samples.len() / 2;
            peak = c.samples.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        // One second of audio; the encoder pads the last frame, the pre-skip is removed.
        assert!(
            (48_000..48_000 + 960).contains(&total),
            "{total} frames: pre-skip removed, padding under one frame"
        );
        assert!(peak > 0.2, "audio is not silent: {peak}");
    }
}
