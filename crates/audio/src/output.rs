//! The audio output device. One thread owns the cpal stream (some backends' streams cannot move between threads)
//! and runs the [`Mixer`] in its callback. The stream is paused when nothing plays, so the PC is not woken every
//! 10 ms and can sleep; it is rebuilt on the new default device when the old one disappears.
//!
//! Output is 48 kHz stereo `f32`. On Windows cpal opens WASAPI shared mode with AUTOCONVERTPCM, so Windows converts to
//! the device rate if it differs.

use crate::mixer::{Mixer, RATE};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum OutCmd {
    /// Start (or keep) the stream running.
    Run,
    /// Pause the stream (device released).
    Suspend,
    /// Rebuild on the current default device.
    Rebuild,
    Stop,
}

#[derive(Clone)]
pub struct Output {
    tx: Sender<OutCmd>,
}

impl Output {
    /// Starts the output thread. Fails if there is no output device at all.
    pub fn start(mixer: Arc<Mutex<Mixer>>) -> Result<Self, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let tx2 = tx.clone();
        std::thread::Builder::new()
            .name("audio-output".into())
            .spawn(move || run(mixer, rx, tx2, ready_tx))
            .map_err(|e| e.to_string())?;
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "audio output did not start".to_string())??;
        Ok(Self { tx })
    }

    pub fn send(&self, c: OutCmd) {
        let _ = self.tx.send(c);
    }
}

fn build(mixer: &Arc<Mutex<Mixer>>, tx: &Sender<OutCmd>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let m = mixer.clone();
    let err_tx = tx.clone();
    device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| match m.try_lock() {
                Ok(mut mixer) => mixer.render(data),
                Err(_) => data.fill(0.0),
            },
            move |e| {
                tracing::warn!("audio output error: {e}");
                // Device unplugged or default changed: rebuild on the new default device.
                let _ = err_tx.send(OutCmd::Rebuild);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn run(
    mixer: Arc<Mutex<Mixer>>,
    rx: Receiver<OutCmd>,
    tx: Sender<OutCmd>,
    ready: Sender<Result<(), String>>,
) {
    let mut stream = match build(&mixer, &tx) {
        Ok(s) => {
            let _ = ready.send(Ok(()));
            Some(s)
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut running = false;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            OutCmd::Run => {
                if !running && let Some(s) = &stream {
                    running = s.play().is_ok();
                }
            }
            OutCmd::Suspend => {
                if running && let Some(s) = &stream {
                    let _ = s.pause();
                    running = false;
                }
            }
            OutCmd::Rebuild => {
                drop(stream.take());
                std::thread::sleep(Duration::from_millis(300));
                stream = build(&mixer, &tx).ok();
                if running && let Some(s) = &stream {
                    running = s.play().is_ok();
                }
            }
            OutCmd::Stop => return,
        }
    }
}
