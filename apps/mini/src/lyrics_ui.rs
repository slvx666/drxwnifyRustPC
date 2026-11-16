//! The lyrics view (the player bar's microphone): every line of the playing track's lyrics; synced ones follow the
//! song (a click on a line seeks there). The right panel's «ТЕКСТ» shows the sung line and the next one.

use crate::{LyricLine, MainWindow};
use drx_app::App;
use drx_lyrics::Lyrics;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    /// The lyrics on screen and the track they belong to, with when it started (for «still looking»).
    static SHOWN: RefCell<(Option<Arc<Lyrics>>, String, std::time::Instant)> =
        RefCell::new((None, String::new(), std::time::Instant::now()));
}

fn set_lines(ui: &MainWindow, lyrics: Option<&Lyrics>) {
    let (lines, synced): (Vec<LyricLine>, bool) = match lyrics {
        Some(Lyrics::Synced(ls)) => (
            ls.iter().map(|l| LyricLine { text: l.text.clone().into(), time_ms: l.time_ms.min(i32::MAX as u64) as i32 }).collect(),
            true,
        ),
        Some(Lyrics::Plain(text)) => (text.lines().map(|t| LyricLine { text: t.trim().into(), time_ms: 0 }).collect(), false),
        None => (Vec::new(), false),
    };
    ui.set_lyrics_synced(synced);
    ui.set_lyrics_current(-1);
    ui.set_lyrics_lines(ModelRc::new(VecModel::from(lines)));
}

/// The sung line now: the lyrics view's highlight and the right panel's two lines.
fn tick(ui: &MainWindow, app: &App) {
    let (lyrics, track, since) = SHOWN.with(|s| s.borrow().clone());
    match lyrics.as_deref() {
        Some(l @ Lyrics::Synced(lines)) => {
            let ms = (app.clock().position_s * 1000.0).max(0.0) as u64;
            let cur = l.line_at(ms);
            let i = cur.map_or(-1, |i| i as i32);
            if ui.get_lyrics_current() != i {
                ui.set_lyrics_current(i);
            }
            let now = cur.and_then(|i| lines.get(i)).map(|l| l.text.as_str()).unwrap_or("");
            let next = lines.get(cur.map_or(0, |i| i + 1)).map(|l| l.text.as_str()).unwrap_or("");
            if ui.get_lyric_now() != now {
                ui.set_lyric_now(now.into());
            }
            if ui.get_lyric_next() != next {
                ui.set_lyric_next(next.into());
            }
        }
        Some(Lyrics::Plain(_)) => {}
        None => {
            let status = if track.is_empty() {
                "Ничего не играет"
            } else if since.elapsed().as_secs() < 8 {
                "Ищу текст…"
            } else {
                "Для этого трека текста не нашлось"
            };
            if ui.get_lyrics_status() != status {
                ui.set_lyrics_status(status.into());
            }
        }
    }
}

pub fn wire(ui: &MainWindow, app: &App) {
    {
        let app2 = app.clone();
        ui.on_lyrics_seek(move |ms| app2.seek(f64::from(ms.max(0)) / 1000.0));
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_lyrics_toggled(move || {
            if let Some(ui) = ui_w.upgrade() {
                tick(&ui, &app2);
            }
        });
    }
    // New lyrics (or a new track): the lines change.
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let mut rx = app2.now_playing();
        let mut last: (Option<usize>, String) = (None, String::new());
        loop {
            let (lyrics, track) = {
                let n = rx.borrow_and_update();
                (n.lyrics.clone(), n.track.as_ref().map(|t| t.id.clone()).unwrap_or_default())
            };
            let key = (lyrics.as_ref().map(|l| Arc::as_ptr(l) as usize), track.clone());
            if key != last {
                let new_track = key.1 != last.1;
                last = key;
                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                    SHOWN.with(|s| {
                        let mut s = s.borrow_mut();
                        let since = if new_track { std::time::Instant::now() } else { s.2 };
                        *s = (lyrics.clone(), track, since);
                    });
                    set_lines(&ui, lyrics.as_deref());
                });
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    // The sung line, five times a second (cheap: a scan of a few dozen lines).
    let (app3, ui_w3) = (app.clone(), ui.as_weak());
    let timer = Box::leak(Box::new(slint::Timer::default()));
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(200), move || {
        if let Some(ui) = ui_w3.upgrade() {
            tick(&ui, &app3);
        }
    });
}
