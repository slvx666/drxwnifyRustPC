//! The real-time mixer that runs inside the audio output callback.
//!
//! Voices are decoded tracks arriving through lock-free ring buffers. One voice is *active*; the next track can wait
//! as *queued* and starts on the very next sample when the active one ends (gapless). A crossfade plays both, the old
//! one fading out `(1 − p)²` and the new one fading in `1 − (1 − p)²` (Android curves, docs/specs/audio.md), per sample
//! instead of Android's 20 volume steps. Pause and resume ramp over 10 ms so there are no clicks.
//!
//! The callback never locks; commands come in and events go out through `rtrb` queues.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const RATE: u32 = 48_000;
const PAUSE_RAMP_FRAMES: u32 = RATE / 100;

/// Per-voice counters shared with the controller.
#[derive(Default)]
pub struct VoiceShared {
    /// Frames this voice has sent to the output.
    pub played: AtomicU64,
    /// Set by the decoder thread when no more samples will come.
    pub input_done: AtomicBool,
    /// Track frames the output does not show one to one (silence cut, tempo): added to `played` for the clock.
    pub adjust: std::sync::atomic::AtomicI64,
}

pub struct VoiceIn {
    pub id: u64,
    pub ring: rtrb::Consumer<f32>,
    pub shared: Arc<VoiceShared>,
    /// Linear gain (normalisation and story volume), on top of the master volume.
    pub gain: f32,
}

pub enum MixCmd {
    /// Make this the active voice, replacing any active one immediately.
    Play(VoiceIn),
    /// Queue a voice to start when the active one ends.
    Queue(VoiceIn),
    /// Crossfade from the active voice to this one over `frames`.
    Crossfade {
        voice: VoiceIn,
        frames: u32,
    },
    /// Drop a voice (skip, seek replaces it with a new one).
    Remove(u64),
    /// Promote the queued voice to active immediately (fast skip without re-buffering).
    PromoteQueued,
    SetMaster(f32),
    SetGain {
        id: u64,
        gain: f32,
    },
    Pause(bool),
    /// Equaliser and balance on the mix (None = off).
    SetPostFx(Option<Box<crate::dsp::PostFx>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixEvent {
    Started(u64),
    Ended(u64),
    /// Nothing is playing and nothing is queued.
    Idle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Active,
    Queued,
    FadingOut,
}

struct Voice {
    id: u64,
    ring: rtrb::Consumer<f32>,
    shared: Arc<VoiceShared>,
    gain: f32,
    role: Role,
    /// Crossfade progress: (done, total) frames; None = full volume.
    fade: Option<(u32, u32)>,
}

impl Voice {
    fn finished(&self) -> bool {
        self.ring.is_empty() && self.shared.input_done.load(Ordering::Acquire)
    }
    fn fade_gain(&self) -> f32 {
        match self.fade {
            None => 1.0,
            Some((d, t)) => {
                let p = (d as f32 / t.max(1) as f32).min(1.0);
                match self.role {
                    Role::FadingOut => (1.0 - p) * (1.0 - p),
                    _ => 1.0 - (1.0 - p) * (1.0 - p),
                }
            }
        }
    }
}

pub struct Mixer {
    voices: Vec<Voice>,
    master: f32,
    paused: bool,
    /// 0..=PAUSE_RAMP_FRAMES; full volume at the top.
    pause_level: u32,
    cmds: rtrb::Consumer<MixCmd>,
    events: rtrb::Producer<MixEvent>,
    idle_sent: bool,
    post: Option<Box<crate::dsp::PostFx>>,
}

impl Mixer {
    pub fn new(cmds: rtrb::Consumer<MixCmd>, events: rtrb::Producer<MixEvent>) -> Self {
        Self {
            voices: Vec::with_capacity(8),
            master: 1.0,
            paused: false,
            pause_level: PAUSE_RAMP_FRAMES,
            cmds,
            events,
            idle_sent: true,
            post: None,
        }
    }

    fn emit(&mut self, e: MixEvent) {
        let _ = self.events.push(e);
    }

    fn apply_commands(&mut self) {
        while let Ok(cmd) = self.cmds.pop() {
            match cmd {
                MixCmd::Play(v) => {
                    let ended: Vec<u64> = self
                        .voices
                        .iter()
                        .filter(|v| v.role != Role::Queued)
                        .map(|v| v.id)
                        .collect();
                    self.voices.retain(|v| v.role == Role::Queued);
                    for id in ended {
                        self.emit(MixEvent::Ended(id));
                    }
                    let id = v.id;
                    self.voices.push(Voice {
                        id,
                        ring: v.ring,
                        shared: v.shared,
                        gain: v.gain,
                        role: Role::Active,
                        fade: None,
                    });
                    self.emit(MixEvent::Started(id));
                    self.idle_sent = false;
                }
                MixCmd::Queue(v) => {
                    self.voices.retain(|x| x.role != Role::Queued);
                    self.voices.push(Voice {
                        id: v.id,
                        ring: v.ring,
                        shared: v.shared,
                        gain: v.gain,
                        role: Role::Queued,
                        fade: None,
                    });
                    self.idle_sent = false;
                }
                MixCmd::Crossfade { voice, frames } => {
                    for x in self.voices.iter_mut() {
                        if x.role == Role::Active {
                            x.role = Role::FadingOut;
                            x.fade = Some((0, frames));
                        }
                    }
                    self.voices
                        .retain(|x| x.role != Role::Queued || x.id == voice.id);
                    let id = voice.id;
                    self.voices.push(Voice {
                        id,
                        ring: voice.ring,
                        shared: voice.shared,
                        gain: voice.gain,
                        role: Role::Active,
                        fade: Some((0, frames)),
                    });
                    self.emit(MixEvent::Started(id));
                    self.idle_sent = false;
                }
                MixCmd::Remove(id) => {
                    if let Some(i) = self.voices.iter().position(|v| v.id == id) {
                        let v = self.voices.remove(i);
                        if v.role != Role::Queued {
                            self.emit(MixEvent::Ended(id));
                        }
                    }
                }
                MixCmd::PromoteQueued => {
                    let ended: Vec<u64> = self
                        .voices
                        .iter()
                        .filter(|v| v.role != Role::Queued)
                        .map(|v| v.id)
                        .collect();
                    self.voices.retain(|v| v.role == Role::Queued);
                    for id in ended {
                        self.emit(MixEvent::Ended(id));
                    }
                    if let Some(q) = self.voices.iter_mut().find(|x| x.role == Role::Queued) {
                        q.role = Role::Active;
                        let qid = q.id;
                        self.emit(MixEvent::Started(qid));
                        self.idle_sent = false;
                    }
                }
                MixCmd::SetMaster(m) => self.master = m.clamp(0.0, 1.0),
                MixCmd::SetGain { id, gain } => {
                    if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
                        v.gain = gain;
                    }
                }
                MixCmd::Pause(p) => self.paused = p,
                MixCmd::SetPostFx(fx) => self.post = fx,
            }
        }
    }

    /// Fills `out` (interleaved stereo) with the mix.
    pub fn render(&mut self, out: &mut [f32]) {
        self.apply_commands();
        out.fill(0.0);
        let frames = out.len() / 2;
        // Fully paused: output silence and leave the rings untouched.
        if self.paused && self.pause_level == 0 {
            return;
        }
        for f in 0..frames {
            if self.paused {
                if self.pause_level == 0 {
                    // Faded out: stop consuming so resume continues from here.
                    break;
                }
                self.pause_level -= 1;
            } else if self.pause_level < PAUSE_RAMP_FRAMES {
                self.pause_level += 1;
            }
            let pause_gain = self.pause_level as f32 / PAUSE_RAMP_FRAMES as f32;
            let (mut l, mut r) = (0.0f32, 0.0f32);
            let mut i = 0;
            while i < self.voices.len() {
                let v = &mut self.voices[i];
                if v.role == Role::Queued {
                    i += 1;
                    continue;
                }
                if v.ring.slots() >= 2 {
                    let g = v.gain * v.fade_gain();
                    let a = v.ring.pop().unwrap_or(0.0);
                    let b = v.ring.pop().unwrap_or(0.0);
                    l += a * g;
                    r += b * g;
                    v.shared.played.fetch_add(1, Ordering::Relaxed);
                    if let Some((d, t)) = v.fade.as_mut() {
                        *d += 1;
                        if *d >= *t {
                            if v.role == Role::FadingOut {
                                let id = v.id;
                                self.voices.remove(i);
                                self.emit(MixEvent::Ended(id));
                                continue;
                            }
                            v.fade = None;
                        }
                    }
                } else if v.finished() {
                    let (id, role) = (v.id, v.role);
                    self.voices.remove(i);
                    self.emit(MixEvent::Ended(id));
                    if role == Role::Active
                        && let Some(q) = self.voices.iter_mut().find(|x| x.role == Role::Queued)
                    {
                        // Gapless: the queued track starts on this very frame.
                        q.role = Role::Active;
                        let qid = q.id;
                        self.emit(MixEvent::Started(qid));
                        i = 0;
                    }
                    continue;
                }
                // else: buffer underrun; the decoder is late. Output silence for this voice.
                i += 1;
            }
            let m = self.master * pause_gain;
            out[f * 2] = l * m;
            out[f * 2 + 1] = r * m;
        }
        if let Some(fx) = self.post.as_mut() {
            fx.process(out);
        }
        if self.voices.is_empty() && !self.idle_sent {
            self.idle_sent = true;
            self.emit(MixEvent::Idle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(id: u64, frames: usize, value: f32) -> (VoiceIn, Arc<VoiceShared>) {
        let (mut p, c) = rtrb::RingBuffer::new(frames * 2 + 16);
        for _ in 0..frames * 2 {
            p.push(value).unwrap();
        }
        let shared = Arc::new(VoiceShared::default());
        shared.input_done.store(true, Ordering::Release);
        (
            VoiceIn {
                id,
                ring: c,
                shared: shared.clone(),
                gain: 1.0,
            },
            shared,
        )
    }

    fn mixer() -> (Mixer, rtrb::Producer<MixCmd>, rtrb::Consumer<MixEvent>) {
        let (cp, cc) = rtrb::RingBuffer::new(32);
        let (ep, ec) = rtrb::RingBuffer::new(64);
        (Mixer::new(cc, ep), cp, ec)
    }

    fn drain(ev: &mut rtrb::Consumer<MixEvent>) -> Vec<MixEvent> {
        let mut v = Vec::new();
        while let Ok(e) = ev.pop() {
            v.push(e);
        }
        v
    }

    #[test]
    fn balance_on_the_mix() {
        let (mut m, mut cmd, _ev) = mixer();
        let (a, _) = voice(1, 50, 0.5);
        cmd.push(MixCmd::Play(a)).ok();
        cmd.push(MixCmd::SetPostFx(Some(Box::new(crate::dsp::PostFx {
            eq: None,
            balance: 0.5,
        }))))
        .ok();
        let mut out = vec![0.0; 20 * 2];
        m.render(&mut out);
        assert!(
            out.chunks(2)
                .all(|f| (f[0] - 0.25).abs() < 1e-6 && (f[1] - 0.5).abs() < 1e-6)
        );
        cmd.push(MixCmd::SetPostFx(None)).ok();
        m.render(&mut out);
        assert!(out.chunks(2).all(|f| f[0] == 0.5 && f[1] == 0.5));
    }

    #[test]
    fn gapless_switch_on_the_next_frame() {
        let (mut m, mut cmd, mut ev) = mixer();
        let (a, _) = voice(1, 100, 0.25);
        let (b, _) = voice(2, 100, 0.5);
        cmd.push(MixCmd::Play(a)).ok();
        cmd.push(MixCmd::Queue(b)).ok();
        let mut out = vec![0.0; 300 * 2];
        m.render(&mut out);
        assert!(out[..200].iter().all(|&s| s == 0.25));
        assert!(
            out[200..400].iter().all(|&s| s == 0.5),
            "no gap between tracks"
        );
        assert!(out[400..].iter().all(|&s| s == 0.0));
        assert_eq!(
            drain(&mut ev),
            vec![
                MixEvent::Started(1),
                MixEvent::Ended(1),
                MixEvent::Started(2),
                MixEvent::Ended(2),
                MixEvent::Idle
            ]
        );
    }

    #[test]
    fn crossfade_uses_android_curves() {
        let (mut m, mut cmd, _ev) = mixer();
        let (a, _) = voice(1, 2000, 1.0);
        let (b, _) = voice(2, 2000, 1.0);
        cmd.push(MixCmd::Play(a)).ok();
        let mut out = vec![0.0; 2];
        m.render(&mut out);
        cmd.push(MixCmd::Crossfade {
            voice: b,
            frames: 1000,
        })
        .ok();
        let mut out = vec![0.0; 1000 * 2];
        m.render(&mut out);
        // At p = 0.5: out (1-p)² = 0.25 plus in 1-(1-p)² = 0.75 → 1.0 total.
        let mid = out[500 * 2];
        assert!((mid - 1.0).abs() < 0.01, "{mid}");
        // Early: mostly the old track.
        let early = out[100 * 2];
        let p: f32 = 0.1;
        assert!((early - ((1.0 - p).powi(2) + 1.0 - (1.0 - p).powi(2))).abs() < 0.01);
    }

    #[test]
    fn pause_ramps_and_keeps_samples() {
        let (mut m, mut cmd, _ev) = mixer();
        let (a, shared) = voice(1, 48_000, 1.0);
        cmd.push(MixCmd::Play(a)).ok();
        let mut out = vec![0.0; 1000 * 2];
        m.render(&mut out);
        cmd.push(MixCmd::Pause(true)).ok();
        m.render(&mut out);
        assert!(
            out[0] > 0.9 && out[out.len() - 1] < 0.6,
            "fades out over 10 ms"
        );
        let played = shared.played.load(Ordering::Relaxed);
        m.render(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(
            shared.played.load(Ordering::Relaxed),
            played,
            "paused: nothing consumed"
        );
    }
}
