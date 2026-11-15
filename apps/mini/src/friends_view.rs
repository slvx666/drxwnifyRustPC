//! The friends page (pc/ FriendsScreen): one's own profile with the code and invite link, requests, friends'
//! cards (live first), and a friend's page with history, likes and playlists to play. Follows the relays while
//! the app runs; the page redraws at most every half second.

use crate::{FriendCardItem, FriendRequestItem, MainWindow, SongItem};
use drx_app::App;
use drx_social::friends::{DEVELOPER_PUBKEY, FriendData, FriendsState, TrackRef};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;

thread_local! {
    /// The friend whose page is open, and the tracks its list shows.
    static OPEN: RefCell<Option<String>> = const { RefCell::new(None) };
    static TAB: RefCell<String> = RefCell::new("history".into());
    static TRACKS: RefCell<Vec<drx_catalog::model::Track>> = const { RefCell::new(Vec::new()) };
    static WATCHING: RefCell<bool> = const { RefCell::new(false) };
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// «2 мин назад», «3 ч назад», «вчера», «5 дн назад».
pub fn ago(ms: i64) -> String {
    let s = ((now_ms() - ms) / 1000).max(0);
    match s {
        0..=59 => "только что".into(),
        60..=3599 => format!("{} мин назад", s / 60),
        3600..=86_399 => format!("{} ч назад", s / 3600),
        86_400..=172_799 => "вчера".into(),
        _ => format!("{} дн назад", s / 86_400),
    }
}

/// A friend's picture: the base64 JPEG written once to the cache and loaded like a cover.
fn avatar(app: &App, pubkey: &str, b64: Option<&str>, size: f32) -> Option<slint::Image> {
    use base64::Engine;
    let b64 = b64?;
    let dir = app.profile().cache_dir().join("avatars");
    let path = dir.join(format!("{}-{}.jpg", &pubkey[..pubkey.len().min(16)], b64.len()));
    if !path.exists() {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(&path, bytes).ok()?;
    }
    let url = format!("{}{}", drx_core::images::LOCAL_PREFIX, path.display());
    crate::cover_r(app, &url, (size * 2.0) as u32, crate::theme_r(size, (size * 2.0) as u32, size))
}

fn initial(name: &str) -> String {
    name.trim().chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

pub fn wire(ui: &MainWindow, app: &App) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friends_copy(move |what| {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some(f) = app2.friends() else { return };
        let me = f.me();
        let st = f.state().borrow().clone();
        let name = st.data.get(&me).and_then(|d| d.name.clone()).unwrap_or_default();
        let (text, done) = if what == "code" {
            (drx_social::nostr::bech32_encode("npub", &hex_bytes(&me)), "Код скопирован")
        } else {
            (f.invite_link(&name), "Ссылка-приглашение скопирована")
        };
        ui.set_toast(if drx_platform::copy_text(&text) { done } else { "Не удалось скопировать" }.into());
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_open(move |p| {
        let Some(ui) = ui_w.upgrade() else { return };
        OPEN.with(|o| *o.borrow_mut() = Some(p.to_string()));
        TAB.with(|t| *t.borrow_mut() = "history".into());
        render(&ui, &app2);
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_back(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        OPEN.with(|o| *o.borrow_mut() = None);
        render(&ui, &app2);
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_set_tab(move |t| {
        let Some(ui) = ui_w.upgrade() else { return };
        TAB.with(|x| *x.borrow_mut() = t.to_string());
        render(&ui, &app2);
    });
    let app2 = app.clone();
    ui.on_friend_play(move |i| {
        let tracks = TRACKS.with(|t| t.borrow().clone());
        if (i as usize) < tracks.len() {
            app2.play_tracks(tracks, i as usize);
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_accept(move |p| {
        let Some(ui) = ui_w.upgrade() else { return };
        let name = app2
            .friends_state()
            .and_then(|rx| rx.borrow().requests.iter().find(|r| r.pubkey == p.as_str()).map(|r| r.name.clone()))
            .unwrap_or_default();
        add(&ui, &app2, p.to_string(), name);
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_dismiss(move |p| {
        app2.dismiss_request(&p);
        if let Some(ui) = ui_w.upgrade() {
            render(&ui, &app2);
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_add(move |text| {
        let Some(ui) = ui_w.upgrade() else { return };
        let t = text.trim().to_string();
        if t.is_empty() {
            return;
        }
        // An invite link may carry the name: …/add/<npub>?n=<name>.
        let name = t.split("?n=").nth(1).map(|n| n.replace("%20", " ")).unwrap_or_default();
        add(&ui, &app2, t, name);
    });
}

fn add(ui: &MainWindow, app: &App, code: String, name: String) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = app2.add_friend(&code, &name).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match res {
                Ok(()) => {
                    ui.set_friends_add_text("".into());
                    ui.set_toast("Заявка отправлена".into());
                }
                Err(e) => ui.set_toast(format!("Не получилось: {e}").into()),
            }
            render(&ui, &app2);
        });
    });
}

fn hex_bytes(h: &str) -> Vec<u8> {
    (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()).collect()
}

/// The page opened: follow the relays (once) and draw.
pub fn show(ui: &MainWindow, app: &App) {
    ui.set_view_title("Друзья".into());
    ui.set_friends_available(app.friends().is_some());
    OPEN.with(|o| *o.borrow_mut() = None);
    render(ui, app);
    if WATCHING.with(|w| std::mem::replace(&mut *w.borrow_mut(), true)) {
        return;
    }
    app.start_friends_live();
    let Some(mut rx) = app.friends_state() else { return };
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        while rx.changed().await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let (app3, ui_w) = (app2.clone(), ui_w.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_w.upgrade()
                    && ui.get_view_friends()
                {
                    render(&ui, &app3);
                }
            });
        }
    });
}

/// Pictures arrived: draw again with them.
pub fn refresh(ui: &MainWindow, app: &App) {
    render(ui, app);
}

fn status(d: Option<&FriendData>) -> (String, bool) {
    match d.and_then(|d| d.now.as_ref()) {
        None => ("ничего не слышно".into(), false),
        Some(n) if n.is_live(now_ms()) => ("слушает сейчас".into(), true),
        Some(n) => (format!("слушал(а) {}", ago(n.at)), false),
    }
}

fn render(ui: &MainWindow, app: &App) {
    let Some(f) = app.friends() else { return };
    let st: FriendsState = f.state().borrow().clone();
    let me = f.me();
    let mine = st.data.get(&me);
    let my_name = mine.and_then(|d| d.name.clone()).unwrap_or_default();
    ui.set_friends_me_initial(initial(&my_name).into());
    ui.set_friends_me_name(my_name.into());
    let npub = drx_social::nostr::bech32_encode("npub", &hex_bytes(&me));
    ui.set_friends_me_code(format!("{}…{}", &npub[..16.min(npub.len())], &npub[npub.len().saturating_sub(6)..]).into());
    match avatar(app, &me, mine.and_then(|d| d.avatar.as_deref()), 72.0) {
        Some(i) => {
            ui.set_friends_me_avatar(i);
            ui.set_friends_me_has_avatar(true);
        }
        None => ui.set_friends_me_has_avatar(false),
    }
    let requests: Vec<FriendRequestItem> = st
        .requests
        .iter()
        .map(|r| FriendRequestItem {
            pubkey: r.pubkey.clone().into(),
            initial: initial(&r.name).into(),
            name: r.name.clone().into(),
            when: ago(r.at).into(),
        })
        .collect();
    ui.set_friends_requests(ModelRc::new(VecModel::from(requests)));

    let mut friends = st.friends.clone();
    let t = now_ms();
    friends.sort_by_key(|fr| {
        let n = st.data.get(&fr.pubkey).and_then(|d| d.now.as_ref());
        (std::cmp::Reverse(n.is_some_and(|n| n.is_live(t))), std::cmp::Reverse(n.map_or(0, |n| n.at)))
    });
    let cards: Vec<FriendCardItem> = friends
        .iter()
        .map(|fr| {
            let d = st.data.get(&fr.pubkey);
            let name = d
                .and_then(|d| d.name.clone())
                .filter(|n| !n.trim().is_empty())
                .or_else(|| Some(fr.name.clone()).filter(|n| !n.trim().is_empty()))
                .unwrap_or_else(|| "Друг".into());
            let (status, live) = status(d);
            let av = avatar(app, &fr.pubkey, d.and_then(|d| d.avatar.as_deref()), 44.0);
            let now = d.and_then(|d| d.now.as_ref());
            let cover = now
                .and_then(|n| n.track.cover.as_deref())
                .and_then(|u| crate::cover_r(app, u, crate::ROW_COVER, crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 46.0)));
            FriendCardItem {
                pubkey: fr.pubkey.clone().into(),
                initial: initial(&name).into(),
                name: name.into(),
                status: status.into(),
                live,
                has_avatar: av.is_some(),
                avatar: av.unwrap_or_default(),
                developer: fr.pubkey == DEVELOPER_PUBKEY,
                has_track: now.is_some(),
                track_title: now.map(|n| n.track.title.clone()).unwrap_or_default().into(),
                track_artist: now.map(|n| n.track.artist.clone()).unwrap_or_default().into(),
                has_cover: cover.is_some(),
                cover: cover.unwrap_or_default(),
            }
        })
        .collect();
    ui.set_friends_cards(ModelRc::new(VecModel::from(cards)));

    let open = OPEN.with(|o| o.borrow().clone());
    ui.set_friend_page(open.is_some());
    let Some(p) = open else { return };
    let d = st.data.get(&p);
    let name = d
        .and_then(|d| d.name.clone())
        .or_else(|| st.friends.iter().find(|x| x.pubkey == p).map(|x| x.name.clone()))
        .unwrap_or_else(|| "Друг".into());
    ui.set_friend_status(
        match d.and_then(|d| d.now.as_ref()) {
            Some(n) if n.is_live(t) => format!("слушает: {} — {}", n.track.title, n.track.artist),
            Some(n) => format!("последний трек {}", ago(n.at)),
            None => "ничего не слышно".into(),
        }
        .into(),
    );
    match avatar(app, &p, d.and_then(|d| d.avatar.as_deref()), 84.0) {
        Some(i) => {
            ui.set_friend_avatar(i);
            ui.set_friend_has_avatar(true);
        }
        None => ui.set_friend_has_avatar(false),
    }
    ui.set_friend_initial(initial(&name).into());
    ui.set_friend_name(name.into());
    ui.set_friend_likes(d.and_then(|d| d.likes.as_ref()).map(|l| l.total.to_string()).unwrap_or_default().into());
    let tab = TAB.with(|x| x.borrow().clone());
    ui.set_friend_tab(tab.clone().into());
    let (refs, empty): (Vec<TrackRef>, &str) = match tab.as_str() {
        "likes" => (
            d.and_then(|d| d.likes.as_ref()).map(|l| l.tracks.clone()).unwrap_or_default(),
            "Любимые появятся, когда друг ими поделится.",
        ),
        "playlists" => (
            d.and_then(|d| d.playlists.as_ref())
                .map(|ps| ps.iter().flat_map(|p| p.tracks.clone()).collect())
                .unwrap_or_default(),
            "Плейлистов не видно.",
        ),
        _ => (d.and_then(|d| d.history.clone()).unwrap_or_default(), "Друг ещё ничего не показал — или скрыл историю."),
    };
    ui.set_friend_empty(empty.into());
    let tracks: Vec<drx_catalog::model::Track> = refs.iter().map(drx_app::social::track_of).collect();
    app.remember_tracks(&tracks);
    let items: Vec<SongItem> = tracks
        .iter()
        .zip(&refs)
        .enumerate()
        .map(|(i, (tr, r))| {
            let cover = r
                .cover
                .as_deref()
                .and_then(|u| crate::cover_r(app, u, crate::ROW_COVER, crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0)));
            SongItem {
                id: tr.id.clone().into(),
                title: tr.title.clone().into(),
                artists: r.artist.clone().into(),
                duration: if r.duration_ms > 0 { crate::fmt_time(r.duration_ms as f64 / 1000.0) } else if r.at > 0 { ago(r.at) } else { String::new() }.into(),
                liked: false,
                index: i as i32 + 1,
                has_cover: cover.is_some(),
                cover: cover.unwrap_or_default(),
                artist_id: "".into(),
                album_id: "".into(),
                plays: "".into(),
            }
        })
        .collect();
    TRACKS.with(|t| *t.borrow_mut() = tracks);
    ui.set_friend_tracks(ModelRc::new(VecModel::from(items)));
}
