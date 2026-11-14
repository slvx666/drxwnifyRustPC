//! Sample-rate conversion to the output rate (48 kHz) for tracks at other rates (AAC/MP3 at 44.1 kHz).
//! FFT resampler from `rubato` (fixed ratio, high quality). The resampler delay is removed at the start and the
//! tail is flushed at the end, so tracks stay sample-exact for gapless playback.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as _};

const CHUNK: usize = 1024;

pub struct Resampler {
    fft: Fft<f32>,
    pending: Vec<f32>,
    out_buf: Vec<f32>,
    /// Output frames still to drop (resampler delay).
    skip: usize,
    in_frames: u64,
    out_frames: u64,
    ratio: f64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let fft = Fft::<f32>::new(from as usize, to as usize, CHUNK, 2, FixedSync::Input).expect("valid resampler");
        let skip = fft.output_delay();
        let max = fft.output_frames_max();
        Self {
            fft,
            pending: Vec::with_capacity(CHUNK * 2),
            out_buf: vec![0.0; max * 2],
            skip,
            in_frames: 0,
            out_frames: 0,
            ratio: to as f64 / from as f64,
        }
    }

    fn run_chunk(&mut self, out: &mut Vec<f32>) {
        let cap = self.out_buf.len() / 2;
        let input = InterleavedSlice::new(&self.pending[..CHUNK * 2], 2, CHUNK).expect("input shape");
        let mut output = InterleavedSlice::new_mut(&mut self.out_buf, 2, cap).expect("output shape");
        let (_, written) = self.fft.process_into_buffer(&input, &mut output, None).expect("resample");
        self.pending.drain(..CHUNK * 2);
        let mut frames = &self.out_buf[..written * 2];
        if self.skip > 0 {
            let d = self.skip.min(written);
            frames = &frames[d * 2..];
            self.skip -= d;
        }
        // Never emit more than the input length allows (the tail of the last chunk is padding).
        let allowed = (self.in_frames as f64 * self.ratio).round() as u64;
        let n = ((frames.len() / 2) as u64).min(allowed.saturating_sub(self.out_frames)) as usize;
        out.extend_from_slice(&frames[..n * 2]);
        self.out_frames += n as u64;
    }

    /// Feeds interleaved stereo input; appends converted output to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.in_frames += (input.len() / 2) as u64;
        self.pending.extend_from_slice(input);
        while self.pending.len() >= CHUNK * 2 {
            self.run_chunk(out);
        }
    }

    /// Flushes everything still inside the resampler at the end of a track.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let target = (self.in_frames as f64 * self.ratio).round() as u64;
        let mut guard = 0;
        while self.out_frames < target && guard < 16 {
            // `push` leaves less than one chunk pending; pad it with silence to a full chunk.
            self.pending.resize(CHUNK * 2, 0.0);
            self.run_chunk(out);
            guard += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_length_exactly() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let input: Vec<f32> = (0..44_100 * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5).collect();
        for c in input.chunks(3000) {
            r.push(c, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len() / 2, 48_000);
    }

    #[test]
    fn keeps_a_tone() {
        // 1 kHz at 44.1k → 1 kHz at 48k: zero crossings per second stay ~2000.
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let input: Vec<f32> =
            (0..44_100).flat_map(|i| { let s = (i as f32 / 44_100.0 * 1000.0 * std::f32::consts::TAU).sin(); [s, s] }).collect();
        r.push(&input, &mut out);
        r.finish(&mut out);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let crossings = left.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((990..=1010).contains(&crossings), "{crossings}");
    }
}
