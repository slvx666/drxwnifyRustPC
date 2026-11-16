//! Sound processing with the same numbers as the Android app (docs/specs/audio.md):
//! loudness normalisation gain, stereo balance, parametric equaliser (RBJ biquads, AutoEq profiles, soft limiter)
//! and the long-silence detector used by «instant silence skip».

/// Normalisation gain (linear) for a YouTube `loudnessDb`: `clamp(−loudness, −15 dB, +3 dB)`.
/// Android: `LoudnessEnhancer` with MIN_GAIN_MB = −1500, MAX_GAIN_MB = 300 (MusicService.kt).
pub fn normalization_gain(loudness_db: Option<f32>) -> f32 {
    match loudness_db {
        Some(l) => db_to_linear((-l).clamp(-15.0, 3.0)),
        None => 1.0,
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Stereo balance −1..1 on interleaved stereo (BalanceAudioProcessor.kt).
pub fn apply_balance(samples: &mut [f32], balance: f32) {
    if balance == 0.0 {
        return;
    }
    let left = if balance > 0.0 { 1.0 - balance } else { 1.0 };
    let right = if balance < 0.0 { 1.0 + balance } else { 1.0 };
    for f in samples.chunks_exact_mut(2) {
        f[0] *= left;
        f[1] *= right;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    /// Peaking.
    Pk,
    /// Low shelf.
    Lsc,
    /// High shelf.
    Hsc,
    /// Low pass — Android computes it as a peaking filter (BiquadFilter.kt `else` branch); kept identical.
    Lpq,
    /// High pass — same as above.
    Hpq,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub frequency: f64,
    pub gain_db: f64,
    pub q: f64,
    pub kind: FilterType,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EqProfile {
    pub preamp_db: f64,
    pub bands: Vec<Band>,
}

/// Android's limit (ParametricEQ.MAX_BANDS).
pub const MAX_BANDS: usize = 20;

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    // State per channel (direct form I).
    x1: [f64; 2],
    x2: [f64; 2],
    y1: [f64; 2],
    y2: [f64; 2],
}

impl Biquad {
    /// Robert Bristow-Johnson's cookbook, exactly as BiquadFilter.kt (shelf slope S = 1).
    fn new(b: &Band, rate: f64) -> Self {
        let omega = 2.0 * std::f64::consts::PI * b.frequency / rate;
        let (sin_w, cos_w) = omega.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match b.kind {
            FilterType::Lsc | FilterType::Hsc => {
                let a = 10f64.powf(b.gain_db / 20.0).sqrt();
                let s = 1.0;
                let alpha = sin_w / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let (ap1, am1) = (a + 1.0, a - 1.0);
                if b.kind == FilterType::Lsc {
                    (
                        a * (ap1 - am1 * cos_w + two_sqrt_a_alpha),
                        2.0 * a * (am1 - ap1 * cos_w),
                        a * (ap1 - am1 * cos_w - two_sqrt_a_alpha),
                        ap1 + am1 * cos_w + two_sqrt_a_alpha,
                        -2.0 * (am1 + ap1 * cos_w),
                        ap1 + am1 * cos_w - two_sqrt_a_alpha,
                    )
                } else {
                    (
                        a * (ap1 + am1 * cos_w + two_sqrt_a_alpha),
                        -2.0 * a * (am1 + ap1 * cos_w),
                        a * (ap1 + am1 * cos_w - two_sqrt_a_alpha),
                        ap1 - am1 * cos_w + two_sqrt_a_alpha,
                        2.0 * (am1 - ap1 * cos_w),
                        ap1 - am1 * cos_w - two_sqrt_a_alpha,
                    )
                }
            }
            _ => {
                let a = 10f64.powf(b.gain_db / 40.0);
                let alpha = sin_w / (2.0 * b.q);
                (
                    1.0 + alpha * a,
                    -2.0 * cos_w,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos_w,
                    1.0 - alpha / a,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: [0.0; 2],
            x2: [0.0; 2],
            y1: [0.0; 2],
            y2: [0.0; 2],
        }
    }

    #[inline]
    fn run(&mut self, ch: usize, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1[ch] + self.b2 * self.x2[ch]
            - self.a1 * self.y1[ch]
            - self.a2 * self.y2[ch];
        self.x2[ch] = self.x1[ch];
        self.x1[ch] = x;
        self.y2[ch] = self.y1[ch];
        self.y1[ch] = y;
        y
    }
}

/// −1 dBFS: where the soft limiter starts to bend the signal (CustomEqualizerAudioProcessor.kt KNEE).
const KNEE: f64 = 0.891;

fn soft_limit(s: f64) -> f64 {
    let m = s.abs();
    if m <= KNEE {
        s
    } else {
        s.signum() * (KNEE + (1.0 - KNEE) * ((m - KNEE) / (1.0 - KNEE)).tanh())
    }
}

pub struct Equalizer {
    filters: Vec<Biquad>,
    preamp: f64,
}

impl Equalizer {
    pub fn new(profile: &EqProfile, rate: u32) -> Self {
        let nyquist = rate as f64 / 2.0;
        let filters = profile
            .bands
            .iter()
            .filter(|b| b.enabled && b.frequency > 0.0 && b.frequency < nyquist)
            .take(MAX_BANDS)
            .map(|b| Biquad::new(b, rate as f64))
            .collect();
        Self {
            filters,
            preamp: 10f64.powf(profile.preamp_db / 20.0),
        }
    }

    /// Processes interleaved stereo in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if self.filters.is_empty() && self.preamp == 1.0 {
            return;
        }
        for f in samples.chunks_exact_mut(2) {
            for (ch, s) in f.iter_mut().enumerate() {
                let mut x = *s as f64;
                for b in self.filters.iter_mut() {
                    x = b.run(ch, x);
                }
                *s = soft_limit(x * self.preamp) as f32;
            }
        }
    }
}

/// What the mixer applies to the finished mix: the equaliser, then the stereo balance (Android: the custom
/// equaliser processor and BalanceAudioProcessor sit after the decoder for every track).
pub struct PostFx {
    pub eq: Option<Equalizer>,
    pub balance: f32,
}

impl PostFx {
    pub fn process(&mut self, samples: &mut [f32]) {
        if let Some(e) = self.eq.as_mut() {
            e.process(samples);
        }
        apply_balance(samples, self.balance);
    }
}

/// Parses an AutoEq "ParametricEQ.txt" profile (ParametricEQParser.kt):
/// `Preamp: -5.2 dB` and `Filter 1: ON PK Fc 105 Hz Gain 8.8 dB Q 0.70`.
pub fn parse_autoeq(text: &str) -> EqProfile {
    let mut p = EqProfile::default();
    for line in text.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("preamp:") {
            if let Some(v) = line[7..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
            {
                p.preamp_db = v;
            }
            continue;
        }
        if !lower.starts_with("filter") {
            continue;
        }
        let Some((_, rest)) = line.split_once(':') else {
            continue;
        };
        let w: Vec<&str> = rest.split_whitespace().collect();
        let enabled = w.first().is_some_and(|s| s.eq_ignore_ascii_case("ON"));
        let kind = match w.get(1).map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("PK") | Some("PEQ") => FilterType::Pk,
            Some("LSC") | Some("LS") => FilterType::Lsc,
            Some("HSC") | Some("HS") => FilterType::Hsc,
            Some("LPQ") | Some("LP") => FilterType::Lpq,
            Some("HPQ") | Some("HP") => FilterType::Hpq,
            _ => continue,
        };
        let after = |key: &str| -> Option<f64> {
            w.iter()
                .position(|s| s.eq_ignore_ascii_case(key))
                .and_then(|i| w.get(i + 1))
                .and_then(|v| v.parse().ok())
        };
        let Some(frequency) = after("Fc") else {
            continue;
        };
        p.bands.push(Band {
            frequency,
            gain_db: after("Gain").unwrap_or(0.0),
            q: after("Q").unwrap_or(1.41),
            kind,
            enabled,
        });
    }
    p
}

/// Detects ≥ 2 s of near-silence (peak < 256 of 32767 on every channel; SilenceDetectorAudioProcessor.kt).
pub struct SilenceDetector {
    rate: u32,
    silent_frames: u64,
    min_frames: u64,
    reported: bool,
}

impl SilenceDetector {
    pub const THRESHOLD: f32 = 256.0 / 32768.0;

    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            silent_frames: 0,
            min_frames: rate as u64 * 2,
            reported: false,
        }
    }

    pub fn reset(&mut self) {
        self.silent_frames = 0;
        self.reported = false;
    }

    /// True while inside a long silence.
    pub fn in_silence(&self) -> bool {
        self.silent_frames >= self.min_frames
    }

    /// Feeds interleaved stereo; returns true once when a long silence begins.
    pub fn feed(&mut self, samples: &[f32]) -> bool {
        let mut fired = false;
        for f in samples.chunks_exact(2) {
            if f[0].abs() < Self::THRESHOLD && f[1].abs() < Self::THRESHOLD {
                self.silent_frames += 1;
                if self.silent_frames >= self.min_frames && !self.reported {
                    self.reported = true;
                    fired = true;
                }
            } else {
                self.silent_frames = 0;
                self.reported = false;
            }
        }
        let _ = self.rate;
        fired
    }
}

/// «Пропускать тишину» (ExoPlayer's SilenceSkippingAudioProcessor with its defaults, as the phone's skipSilence):
/// a quiet run (every sample under 1024/32768) of 150 ms or more is cut, keeping 20 ms of it; shorter pauses stay.
pub struct SilenceSkipper {
    run: usize,
    held: Vec<f32>,
    min_frames: usize,
    pad_frames: usize,
}

impl SilenceSkipper {
    pub const THRESHOLD: f32 = 1024.0 / 32768.0;

    pub fn new(rate: u32) -> Self {
        Self { run: 0, held: Vec::new(), min_frames: rate as usize * 15 / 100, pad_frames: rate as usize / 50 }
    }

    pub fn reset(&mut self) {
        self.run = 0;
        self.held.clear();
    }

    /// Interleaved stereo in, what is kept appended to `out`; returns the frames cut.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> u64 {
        let mut cut = 0u64;
        for f in input.chunks_exact(2) {
            if f[0].abs() < Self::THRESHOLD && f[1].abs() < Self::THRESHOLD {
                self.run += 1;
                if self.run <= self.min_frames {
                    self.held.extend_from_slice(f);
                    if self.run == self.min_frames {
                        // Long enough: keep the padding, cut the rest of what was held.
                        out.extend_from_slice(&self.held[..self.pad_frames * 2]);
                        cut += (self.min_frames - self.pad_frames) as u64;
                        self.held.clear();
                    }
                } else {
                    cut += 1;
                }
            } else {
                if !self.held.is_empty() {
                    out.append(&mut self.held);
                }
                self.run = 0;
                out.extend_from_slice(f);
            }
        }
        cut
    }

    /// The stream ended: a short quiet tail is played after all.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        out.append(&mut self.held);
        self.run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_skipper_cuts_long_pauses_only() {
        let mut s = SilenceSkipper::new(1000); // 150 frames min, 20 padding
        let mut input = vec![0.5f32; 20];
        input.extend(vec![0.0; 2 * 100]); // a short pause: kept
        input.extend(vec![0.5; 20]);
        input.extend(vec![0.0; 2 * 400]); // a long one: 20 frames stay
        input.extend(vec![0.5; 20]);
        let mut out = Vec::new();
        let cut = s.process(&input, &mut out);
        assert_eq!(cut, 380);
        assert_eq!(out.len(), 20 + 200 + 20 + 40 + 20);
    }

    #[test]
    fn normalization_clamps_like_android() {
        assert!(
            (normalization_gain(Some(-6.0)) - db_to_linear(3.0)).abs() < 1e-6,
            "boost capped at +3 dB"
        );
        assert!(
            (normalization_gain(Some(20.0)) - db_to_linear(-15.0)).abs() < 1e-6,
            "cut capped at -15 dB"
        );
        assert!((normalization_gain(Some(2.0)) - db_to_linear(-2.0)).abs() < 1e-6);
        assert_eq!(normalization_gain(None), 1.0);
    }

    #[test]
    fn balance_matches_android() {
        let mut s = [1.0, 1.0];
        apply_balance(&mut s, 0.25);
        assert_eq!(s, [0.75, 1.0]);
        let mut s = [1.0, 1.0];
        apply_balance(&mut s, -0.5);
        assert_eq!(s, [1.0, 0.5]);
    }

    #[test]
    fn parses_autoeq() {
        let p = parse_autoeq(
            "Preamp: -5.2 dB\nFilter 1: ON LSC Fc 105 Hz Gain 8.8 dB Q 0.70\nFilter 2: ON PK Fc 70 Hz Gain -6.7 dB Q 0.29\nFilter 3: OFF PK Fc 1000 Hz Gain 1 dB Q 1\n",
        );
        assert_eq!(p.preamp_db, -5.2);
        assert_eq!(p.bands.len(), 3);
        assert_eq!(p.bands[0].kind, FilterType::Lsc);
        assert_eq!(p.bands[1].gain_db, -6.7);
        assert!(!p.bands[2].enabled);
    }

    fn tone_gain(eq: &mut Equalizer, freq: f32) -> f32 {
        let rate = 48_000.0;
        let mut s: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let v = (i as f32 / rate * freq * std::f32::consts::TAU).sin() * 0.1;
                [v, v]
            })
            .collect();
        eq.process(&mut s);
        let tail = &s[s.len() / 2..];
        tail.iter().fold(0f32, |m, v| m.max(v.abs())) / 0.1
    }

    #[test]
    fn peaking_band_boosts_its_frequency() {
        let p = EqProfile {
            preamp_db: 0.0,
            bands: vec![Band {
                frequency: 1000.0,
                gain_db: 6.0,
                q: 1.0,
                kind: FilterType::Pk,
                enabled: true,
            }],
        };
        let mut eq = Equalizer::new(&p, 48_000);
        let g = tone_gain(&mut eq, 1000.0);
        assert!((g - db_to_linear(6.0)).abs() < 0.05, "{g}");
        let mut eq = Equalizer::new(&p, 48_000);
        let far = tone_gain(&mut eq, 100.0);
        assert!((far - 1.0).abs() < 0.05, "{far}");
    }

    #[test]
    fn limiter_keeps_peaks_below_one() {
        assert!(soft_limit(3.0) <= 1.0 && soft_limit(-3.0) >= -1.0);
        assert!(soft_limit(0.95) < 0.95, "bends above the knee");
        assert_eq!(soft_limit(0.5), 0.5);
    }

    #[test]
    fn silence_detector_fires_after_two_seconds() {
        let mut d = SilenceDetector::new(48_000);
        let quiet = vec![0.001f32; 48_000 * 2];
        assert!(!d.feed(&quiet));
        assert!(d.feed(&quiet));
        assert!(d.in_silence());
        assert!(!d.feed(&quiet), "fires once per silence");
        d.feed(&[0.5, 0.5]);
        assert!(!d.in_silence());
    }
}
