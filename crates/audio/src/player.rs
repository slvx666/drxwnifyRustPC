//! The player: turns byte sources into voices for the mixer and keeps the playback clock.
//!
//! Each track gets a decoder thread (decode → resample to 48 kHz → ring buffer of ~2 s). The app decides *what* to
//! play (queue, crossfade timing, same-album gapless rule); the player does *how*: start, queue for gapless, crossfade,
//! seek, pause, volume, and reports when tracks start and end.

use crate::decode::{ByteSource, Decoder};
use crate::mixer::{MixCmd, MixEvent, Mixer, RATE, VoiceIn, VoiceShared};
use crate::output::{OutCmd, Output};
use crate::resample::Resampler;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ring buffer per voice: 2 seconds of stereo audio.
const RING_SAMPLES: usize = RATE as usize * 2 * 2;
/// Release the output device after this long with nothing playing.
const IDLE_RELEASE: Duration = Duration::from_secs(5);

pub type TrackId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Started(TrackId),
    /// The track played to its end (or was replaced).
    Ended(TrackId),
    /// Decoding failed; the app may skip to the next track (Android: autoSkipNextOnError).
    Failed {
        track: TrackId,
        error: String,
    },
    Idle,
}

/// Where playback is. The UI extrapolates from it instead of asking every frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackClock {
    pub track: Option<TrackId>,
    pub position_s: f64,
    pub duration_s: Option<f64>,
    pub playing: bool,
    pub at: Instant,
}

enum DecCmd {
    Seek(f64),
}

struct Track {
    id: TrackId,
    voice: u64,
    shared: Arc<VoiceShared>,
    ctl: Sender<DecCmd>,
    /// Track time at which the current voice started (changes on seek).
    offset_s: f64,
    duration_s: Option<f64>,
    gain: f32,
}

struct State {
    current: Option<Track>,
    queued: Option<Track>,
    paused: bool,
    idle_since: Option<Instant>,
}

pub struct Player {
    cmds: Mutex<rtrb::Producer<MixCmd>>,
    output: Output,
    state: Arc<Mutex<State>>,
    next_id: AtomicU64,
    events_rx: Mutex<Receiver<PlayerEvent>>,
    /// Voice-id → track-id, so mixer events map to tracks.
    voices: Arc<Mutex<Vec<(u64, TrackId)>>>,
    seek_tx: Sender<(TrackId, u64, Arc<VoiceShared>, f64, VoiceIn)>,
}

impl Player {
    /// Opens the default output device.
    pub fn new() -> Result<Arc<Self>, String> {
        let (cp, cc) = rtrb::RingBuffer::new(64);
        let (ep, ec) = rtrb::RingBuffer::new(256);
        let mixer = Arc::new(Mutex::new(Mixer::new(cc, ep)));
        let output = Output::start(mixer)?;
        let (ev_tx, ev_rx) = channel();
        let (seek_tx, seek_rx) = channel();
        let state = Arc::new(Mutex::new(State {
            current: None,
            queued: None,
            paused: false,
            idle_since: None,
        }));
        let player = Arc::new(Self {
            cmds: Mutex::new(cp),
            output,
            state: state.clone(),
            next_id: AtomicU64::new(1),
            events_rx: Mutex::new(ev_rx),
            voices: Arc::default(),
            seek_tx,
        });
        let p = Arc::downgrade(&player);
        std::thread::Builder::new()
            .name("audio-events".into())
            .spawn(move || event_loop(p, ec, ev_tx, seek_rx))
            .map_err(|e| e.to_string())?;
        Ok(player)
    }

    fn cmd(&self, c: MixCmd) {
        let mut q = self.cmds.lock().expect("cmd lock");
        let mut c = Some(c);
        // The mixer drains the queue every ~10 ms; wait briefly if it is full.
        for _ in 0..200 {
            match q.push(c.take().expect("command")) {
                Ok(()) => return,
                Err(rtrb::PushError::Full(back)) => {
                    c = Some(back);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
        tracing::error!("mixer command queue stuck");
    }

    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Decodes `source` on a new thread starting at `start_s`. Returns the track and its first voice.
    fn spawn_track<S: ByteSource + 'static>(
        &self,
        source: S,
        ext: Option<&str>,
        start_s: f64,
        gain: f32,
    ) -> Result<(Track, VoiceIn), String> {
        let mut dec = Decoder::open(source, ext).map_err(|e| e.to_string())?;
        let duration_s = dec.duration_s();
        let offset_s = if start_s > 0.0 {
            dec.seek(start_s).map_err(|e| e.to_string())?
        } else {
            0.0
        };
        let id = self.new_id();
        let voice = self.new_id();
        let shared = Arc::new(VoiceShared::default());
        let (prod, cons) = rtrb::RingBuffer::new(RING_SAMPLES);
        let (ctl_tx, ctl_rx) = channel();
        let seek_tx = self.seek_tx.clone();
        let next_id = self.next_id_handle();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || decode_loop(dec, prod, sh, ctl_rx, seek_tx, id, next_id))
            .map_err(|e| e.to_string())?;
        self.voices.lock().expect("voices").push((voice, id));
        let track = Track {
            id,
            voice,
            shared: shared.clone(),
            ctl: ctl_tx,
            offset_s,
            duration_s,
            gain,
        };
        Ok((
            track,
            VoiceIn {
                id: voice,
                ring: cons,
                shared,
                gain,
            },
        ))
    }

    fn next_id_handle(&self) -> Arc<AtomicU64> {
        // Seek voices need fresh ids from the decoder thread; share a counter starting far above normal ids.
        static SEEK_IDS: std::sync::OnceLock<Arc<AtomicU64>> = std::sync::OnceLock::new();
        SEEK_IDS
            .get_or_init(|| Arc::new(AtomicU64::new(1 << 40)))
            .clone()
    }

    /// Plays a track now, replacing what plays. `gain` is linear (normalisation).
    pub fn play<S: ByteSource + 'static>(
        &self,
        source: S,
        ext: Option<&str>,
        start_s: f64,
        gain: f32,
    ) -> Result<TrackId, String> {
        let (track, voice) = self.spawn_track(source, ext, start_s, gain)?;
        let id = track.id;
        {
            let mut st = self.state.lock().expect("state");
            st.current = Some(track);
            st.queued = None;
            st.paused = false;
            st.idle_since = None;
        }
        self.cmd(MixCmd::Pause(false));
        self.cmd(MixCmd::Play(voice));
        self.output.send(OutCmd::Run);
        Ok(id)
    }

    /// Queues the next track to start exactly when the current one ends (gapless).
    pub fn queue_next<S: ByteSource + 'static>(
        &self,
        source: S,
        ext: Option<&str>,
        gain: f32,
    ) -> Result<TrackId, String> {
        let (track, voice) = self.spawn_track(source, ext, 0.0, gain)?;
        let id = track.id;
        self.state.lock().expect("state").queued = Some(track);
        self.cmd(MixCmd::Queue(voice));
        Ok(id)
    }

    /// Starts a crossfade into a new track over `secs`.
    pub fn crossfade_to<S: ByteSource + 'static>(
        &self,
        source: S,
        ext: Option<&str>,
        gain: f32,
        secs: f32,
    ) -> Result<TrackId, String> {
        let (track, voice) = self.spawn_track(source, ext, 0.0, gain)?;
        let id = track.id;
        {
            let mut st = self.state.lock().expect("state");
            st.current = Some(track);
            st.queued = None;
        }
        self.cmd(MixCmd::Crossfade {
            voice,
            frames: (secs.max(0.05) * RATE as f32) as u32,
        });
        Ok(id)
    }

    pub fn pause(&self) {
        let mut st = self.state.lock().expect("state");
        st.paused = true;
        st.idle_since = Some(Instant::now());
        drop(st);
        self.cmd(MixCmd::Pause(true));
    }

    pub fn resume(&self) {
        let mut st = self.state.lock().expect("state");
        st.paused = false;
        st.idle_since = None;
        drop(st);
        self.output.send(OutCmd::Run);
        self.cmd(MixCmd::Pause(false));
    }

    pub fn stop(&self) {
        let mut st = self.state.lock().expect("state");
        let ids: Vec<u64> = st
            .current
            .iter()
            .chain(st.queued.iter())
            .map(|t| t.voice)
            .collect();
        st.current = None;
        st.queued = None;
        st.idle_since = Some(Instant::now());
        drop(st);
        for v in ids {
            self.cmd(MixCmd::Remove(v));
        }
    }

    /// Seeks the current track.
    pub fn seek(&self, secs: f64) {
        let st = self.state.lock().expect("state");
        if let Some(t) = &st.current {
            let _ = t.ctl.send(DecCmd::Seek(secs.max(0.0)));
        }
    }

    /// Master volume 0..1 (the app's volume slider).
    pub fn set_volume(&self, v: f32) {
        self.cmd(MixCmd::SetMaster(v));
    }

    /// Changes a track's linear gain (normalisation, story fades).
    pub fn set_gain(&self, track: TrackId, gain: f32) {
        let st = self.state.lock().expect("state");
        if let Some(t) = st
            .current
            .iter()
            .chain(st.queued.iter())
            .find(|t| t.id == track)
        {
            self.cmd(MixCmd::SetGain { id: t.voice, gain });
        }
    }

    pub fn clock(&self) -> PlaybackClock {
        let st = self.state.lock().expect("state");
        match &st.current {
            Some(t) => PlaybackClock {
                track: Some(t.id),
                position_s: t.offset_s
                    + t.shared.played.load(Ordering::Relaxed) as f64 / RATE as f64,
                duration_s: t.duration_s,
                playing: !st.paused,
                at: Instant::now(),
            },
            None => PlaybackClock {
                track: None,
                position_s: 0.0,
                duration_s: None,
                playing: false,
                at: Instant::now(),
            },
        }
    }

    /// Next player event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<PlayerEvent> {
        self.events_rx
            .lock()
            .expect("events")
            .recv_timeout(timeout)
            .ok()
    }
}

fn decode_loop(
    mut dec: Decoder,
    mut prod: rtrb::Producer<f32>,
    mut shared: Arc<VoiceShared>,
    ctl: Receiver<DecCmd>,
    seek_tx: Sender<(TrackId, u64, Arc<VoiceShared>, f64, VoiceIn)>,
    track: TrackId,
    ids: Arc<AtomicU64>,
) {
    let mut rs = (dec.sample_rate() != RATE).then(|| Resampler::new(dec.sample_rate(), RATE));
    let mut pending: Vec<f32> = Vec::new();
    let mut ended = false;
    loop {
        // Seek: new voice with a fresh ring, so old buffered audio is dropped at once.
        match ctl.try_recv() {
            Ok(DecCmd::Seek(s)) => {
                let at = dec.seek(s).unwrap_or(s);
                rs = (dec.sample_rate() != RATE).then(|| Resampler::new(dec.sample_rate(), RATE));
                pending.clear();
                ended = false;
                let (p, c) = rtrb::RingBuffer::new(RING_SAMPLES);
                prod = p;
                shared = Arc::new(VoiceShared::default());
                let voice = ids.fetch_add(1, Ordering::Relaxed);
                let vin = VoiceIn {
                    id: voice,
                    ring: c,
                    shared: shared.clone(),
                    gain: 1.0,
                };
                if seek_tx
                    .send((track, voice, shared.clone(), at, vin))
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if prod.is_abandoned() {
            return;
        }
        if pending.is_empty() && !ended {
            match dec.next_chunk() {
                Ok(Some(c)) => match &mut rs {
                    Some(r) => r.push(c.samples, &mut pending),
                    None => pending.extend_from_slice(c.samples),
                },
                Ok(None) => {
                    if let Some(r) = &mut rs {
                        r.finish(&mut pending);
                    }
                    ended = true;
                }
                Err(e) => {
                    tracing::warn!("decode error: {e}");
                    ended = true;
                }
            }
        }
        if !pending.is_empty() {
            let n = prod.slots().min(pending.len());
            if n > 0 {
                if let Ok(chunk) = prod.write_chunk_uninit(n) {
                    chunk.fill_from_iter(pending.drain(..n));
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
            continue;
        }
        if ended {
            shared.input_done.store(true, Ordering::Release);
            // Stay alive for seeks until the track is dropped.
            match ctl.recv_timeout(Duration::from_millis(200)) {
                Ok(DecCmd::Seek(s)) => {
                    // Re-run the loop with this seek.
                    let at = dec.seek(s).unwrap_or(s);
                    rs = (dec.sample_rate() != RATE)
                        .then(|| Resampler::new(dec.sample_rate(), RATE));
                    ended = false;
                    let (p, c) = rtrb::RingBuffer::new(RING_SAMPLES);
                    prod = p;
                    shared = Arc::new(VoiceShared::default());
                    let voice = ids.fetch_add(1, Ordering::Relaxed);
                    let vin = VoiceIn {
                        id: voice,
                        ring: c,
                        shared: shared.clone(),
                        gain: 1.0,
                    };
                    if seek_tx
                        .send((track, voice, shared.clone(), at, vin))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if prod.is_abandoned() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }
}

fn event_loop(
    player: std::sync::Weak<Player>,
    mut ec: rtrb::Consumer<MixEvent>,
    tx: Sender<PlayerEvent>,
    seek_rx: Receiver<(TrackId, u64, Arc<VoiceShared>, f64, VoiceIn)>,
) {
    loop {
        let Some(p) = player.upgrade() else { return };
        // Seeks finished by decoder threads: swap the voice in.
        while let Ok((track, voice, shared, at, mut vin)) = seek_rx.try_recv() {
            let mut st = p.state.lock().expect("state");
            if let Some(t) = st.current.as_mut().filter(|t| t.id == track) {
                let old = t.voice;
                t.voice = voice;
                t.shared = shared;
                t.offset_s = at;
                vin.gain = t.gain;
                drop(st);
                p.voices.lock().expect("voices").push((voice, track));
                p.cmd(MixCmd::Play(vin));
                // The old voice is gone; don't report its end as the track's end.
                p.voices.lock().expect("voices").retain(|(v, _)| *v != old);
            }
        }
        while let Ok(e) = ec.pop() {
            let map = |v: u64| {
                p.voices
                    .lock()
                    .expect("voices")
                    .iter()
                    .find(|(x, _)| *x == v)
                    .map(|(_, t)| *t)
            };
            match e {
                MixEvent::Started(v) => {
                    if let Some(t) = map(v) {
                        let mut st = p.state.lock().expect("state");
                        if st.queued.as_ref().is_some_and(|q| q.id == t) {
                            st.current = st.queued.take();
                        }
                        drop(st);
                        let _ = tx.send(PlayerEvent::Started(t));
                    }
                }
                MixEvent::Ended(v) => {
                    if let Some(t) = map(v) {
                        p.voices.lock().expect("voices").retain(|(x, _)| *x != v);
                        let mut st = p.state.lock().expect("state");
                        if st
                            .current
                            .as_ref()
                            .is_some_and(|c| c.id == t && c.voice == v)
                        {
                            st.current = None;
                        }
                        drop(st);
                        let _ = tx.send(PlayerEvent::Ended(t));
                    }
                }
                MixEvent::Idle => {
                    p.state
                        .lock()
                        .expect("state")
                        .idle_since
                        .get_or_insert_with(Instant::now);
                    let _ = tx.send(PlayerEvent::Idle);
                }
            }
        }
        // Release the device after a while with nothing to play.
        let release = {
            let st = p.state.lock().expect("state");
            st.idle_since.is_some_and(|t| t.elapsed() >= IDLE_RELEASE)
        };
        if release {
            p.output.send(OutCmd::Suspend);
            p.state.lock().expect("state").idle_since = None;
        }
        drop(p);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, secs: f32) -> std::io::Cursor<Vec<u8>> {
        let frames = (rate as f32 * secs) as usize;
        let data_len = frames * 4;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 4).to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let s =
                ((i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
            v.extend_from_slice(&s.to_le_bytes());
            v.extend_from_slice(&s.to_le_bytes());
        }
        std::io::Cursor::new(v)
    }

    /// Plays through the real output device at volume 0 (silent). Skipped when there is no device (CI).
    #[test]
    fn plays_gapless_queue_on_device() {
        let Ok(p) = Player::new() else { return };
        p.set_volume(0.0);
        let a = p.play(wav(44_100, 0.6), Some("wav"), 0.0, 1.0).unwrap();
        let b = p.queue_next(wav(48_000, 0.4), Some("wav"), 1.0).unwrap();
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(e) = p.next_event(Duration::from_millis(100)) {
                let done = e == PlayerEvent::Ended(b);
                seen.push(e);
                if done {
                    break;
                }
            }
        }
        assert!(seen.contains(&PlayerEvent::Started(a)), "{seen:?}");
        assert!(seen.contains(&PlayerEvent::Ended(a)), "{seen:?}");
        assert!(seen.contains(&PlayerEvent::Started(b)), "{seen:?}");
        assert!(seen.contains(&PlayerEvent::Ended(b)), "{seen:?}");
    }

    #[test]
    fn seek_moves_the_clock() {
        let Ok(p) = Player::new() else { return };
        p.set_volume(0.0);
        p.play(wav(48_000, 10.0), Some("wav"), 0.0, 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        p.seek(6.0);
        std::thread::sleep(Duration::from_millis(400));
        let c = p.clock();
        assert!(c.position_s > 6.0 && c.position_s < 7.5, "{c:?}");
        p.pause();
        let a = p.clock().position_s;
        std::thread::sleep(Duration::from_millis(300));
        let b = p.clock().position_s;
        assert!(
            (b - a).abs() < 0.05,
            "paused clock must not move: {a} -> {b}"
        );
    }
}
