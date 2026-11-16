//! The friends page (pc/ FriendsScreen): one's own profile with the code and invite link, requests, friends'
//! cards (live first), and a friend's page with the chat, history, likes and playlists to play. Follows the relays
//! while the app runs; the page redraws at most every half second, the chat when a message comes or changes.

use crate::{ChatItem, FriendCardItem, FriendRequestItem, MainWindow, ReactionChip, SongItem, StickerPic};
use drx_app::App;
use drx_social::friends::{DEVELOPER_PUBKEY, FriendData, FriendsState, TrackRef};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;

thread_local! {
    /// The friend whose page is open, and the tracks its list shows.
    static OPEN: RefCell<Option<String>> = const { RefCell::new(None) };
    static TAB: RefCell<String> = RefCell::new("chat".into());
    /// The music cards of the chat on screen, by row.
    static CHAT_MUSIC: RefCell<Vec<Option<drx_app::chats::ChatMusic>>> = const { RefCell::new(Vec::new()) };
    /// The chat's rows on screen: (message id, mine, text).
    static CHAT_ROWS: RefCell<Vec<(String, bool, String)>> = const { RefCell::new(Vec::new()) };
    /// The message being changed (its text is in the field), if any.
    static EDITING: RefCell<Option<String>> = const { RefCell::new(None) };
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

/// A message's time: «14:05» today, «вчера 14:05», «03.11 14:05» before, with the year when it is not this one.
fn clock(ms: i64) -> String {
    let off = drx_platform::utc_offset_seconds();
    let local = ms.div_euclid(1000) + off;
    let (day, sec) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let today = (now_ms().div_euclid(1000) + off).div_euclid(86_400);
    let hm = format!("{:02}:{:02}", sec / 3600, sec % 3600 / 60);
    if day == today {
        return hm;
    }
    if day == today - 1 {
        return format!("вчера {hm}");
    }
    let (y, m, d) = civil(day);
    let (ty, _, _) = civil(today);
    if y == ty { format!("{d:02}.{m:02} {hm}") } else { format!("{d:02}.{m:02}.{y} {hm}") }
}

/// Days since 1970-01-01 → (year, month, day) (H. Hinnant's civil_from_days).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

thread_local! {
    /// The open friend page's avatar (address, name) for the picture viewer.
    static SHOWN_AVATAR: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

/// The friend whose page is open.
pub fn open_peer() -> Option<String> {
    OPEN.with(|o| o.borrow().clone())
}

/// The friend page's avatar for the picture viewer: (address, name).
pub fn shown_avatar() -> Option<(String, String)> {
    SHOWN_AVATAR.with(|s| s.borrow().clone())
}

/// A friend's picture: the base64 JPEG written once to the cache and loaded like a cover.
fn avatar(app: &App, pubkey: &str, b64: Option<&str>, size: f32) -> Option<slint::Image> {
    let url = avatar_url(app, pubkey, b64)?;
    crate::cover_r(app, &url, (size * 2.0) as u32, crate::theme_r(size, (size * 2.0) as u32, size))
}

/// The cached file of a friend's picture as an image address (`file:` + path).
fn avatar_url(app: &App, pubkey: &str, b64: Option<&str>) -> Option<String> {
    use base64::Engine;
    let b64 = b64?;
    let dir = app.profile().cache_dir().join("avatars");
    let path = dir.join(format!("{}-{}.jpg", &pubkey[..pubkey.len().min(16)], b64.len()));
    if !path.exists() {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(&path, bytes).ok()?;
    }
    Some(format!("{}{}", drx_core::images::LOCAL_PREFIX, path.display()))
}

/// A name as the app's fonts can draw it: mathematical bold, italic, script… letters (U+1D400–U+1D7FF, which
/// the fonts lack, so the name came out blank) become plain ones; everything else stays as written.
pub(crate) fn plain(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.chars()
        .flat_map(|c| {
            let fancy = ('\u{1D400}'..='\u{1D7FF}').contains(&c);
            let v: Vec<char> = if fancy { c.to_string().nfkc().collect() } else { vec![c] };
            v
        })
        .collect()
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
        TAB.with(|t| *t.borrow_mut() = "chat".into());
        crate::portrait_view::reset();
        ui.set_friend_chat_text("".into());
        EDITING.with(|e| *e.borrow_mut() = None);
        ui.set_friend_chat_editing(false);
        render(&ui, &app2);
    });
    let app2 = app.clone();
    ui.on_friend_play_now(move |p| {
        // What the friend plays (or played last), from the row's right side.
        let now = app2.friends_state().and_then(|rx| rx.borrow().data.get(p.as_str()).and_then(|d| d.now.clone()));
        if let Some(n) = now {
            let t = drx_app::social::track_of(&n.track);
            app2.remember_tracks(std::slice::from_ref(&t));
            app2.play_tracks(vec![t], 0);
        }
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
    // The sticker panel: every sticker Mini draws; a pick goes out as a message of that sticker (as on the phone).
    ui.set_sticker_list(ModelRc::new(VecModel::from(
        drx_social::chat::mini_stickers()
            .iter()
            .enumerate()
            .map(|(i, (_, r))| StickerPic { pic: i as i32, ratio: *r })
            .collect::<Vec<_>>(),
    )));
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_friend_chat_sticker(move |i| {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some(peer) = OPEN.with(|o| o.borrow().clone()) else { return };
            let Some((id, _)) = drx_social::chat::mini_stickers().into_iter().nth(i as usize) else { return };
            if let Err(e) = app2.send_chat(&peer, &drx_social::chat::sticker_token(&id)) {
                ui.set_toast(format!("Не отправлено: {e}").into());
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_friend_chat_typing(move || {
            if let Some(peer) = OPEN.with(|o| o.borrow().clone()) {
                app2.chat_typing_now(&peer);
            }
        });
    }
    ui.on_friend_chat_send(move |text| {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some(peer) = OPEN.with(|o| o.borrow().clone()) else { return };
        if text.trim().is_empty() {
            return;
        }
        let editing = EDITING.with(|e| e.borrow().clone());
        let res = match &editing {
            Some(id) => app2.edit_chat(&peer, id, &text),
            None => app2.send_chat(&peer, &text),
        };
        match res {
            Ok(()) => {
                ui.set_friend_chat_text("".into());
                EDITING.with(|e| *e.borrow_mut() = None);
                ui.set_friend_chat_editing(false);
            }
            Err(e) => ui.set_toast(
                format!("{}: {e}", if editing.is_some() { "Не изменено" } else { "Не отправлено" }).into(),
            ),
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_chat_action(move |i, action| {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some(peer) = OPEN.with(|o| o.borrow().clone()) else { return };
        if action == "cancel-edit" {
            EDITING.with(|e| *e.borrow_mut() = None);
            ui.set_friend_chat_editing(false);
            ui.set_friend_chat_text("".into());
            return;
        }
        let Some((id, _mine, text)) = CHAT_ROWS.with(|r| r.borrow().get(i as usize).cloned()) else { return };
        let res = match action.as_str() {
            "edit" => {
                EDITING.with(|e| *e.borrow_mut() = Some(id));
                ui.set_friend_chat_text(text.into());
                ui.set_friend_chat_editing(true);
                Ok(())
            }
            "del" => app2.delete_chat(&peer, &id, false),
            "del-both" => app2.delete_chat(&peer, &id, true),
            a => match a.strip_prefix("r:") {
                Some(key) => app2.react_chat(&peer, &id, key),
                None => Ok(()),
            },
        };
        if let Err(e) = res {
            ui.set_toast(format!("Не получилось: {e}").into());
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_toggle_along(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some(peer) = OPEN.with(|o| o.borrow().clone()) else { return };
        if app2.listening_along().as_deref() == Some(peer.as_str()) {
            app2.stop_listen_along();
            ui.set_toast("Больше не слушаю вместе".into());
        } else if let Err(e) = app2.start_listen_along(&peer) {
            ui.set_toast(format!("Не получилось: {e}").into());
        }
        render(&ui, &app2);
    });
    let ui_w = ui.as_weak();
    ui.on_friend_remove(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        ui.set_dialog_title("Удалить из друзей?".into());
        ui.set_dialog_subtitle(format!("{} больше не увидит, что ты слушаешь; переписка останется", ui.get_friend_name()).into());
        ui.set_dialog_kind("unfriend".into());
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_chat_play(move |i| {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some(m) = CHAT_MUSIC.with(|c| c.borrow().get(i as usize).cloned().flatten()) else { return };
        match m.kind.as_str() {
            "album" => ui.invoke_open_album(m.id.into()),
            "artist" => ui.invoke_open_artist(m.id.into()),
            "playlist" if drx_app::playlists::is_editable_id(&m.id) => crate::nav::go_playlist(&ui, &app2, &m.id, &m.title),
            "playlist" => ui.set_toast("Этот плейлист открывается на телефоне".into()),
            _ => {
                let r = TrackRef {
                    title: m.title,
                    artist: m.by,
                    catalog_id: Some(m.id.clone()).filter(|id| !id.contains(':') || id.starts_with("mfb:")),
                    media_id: Some(m.id),
                    cover: m.cover,
                    duration_ms: m.duration_ms,
                    at: 0,
                };
                app2.play_tracks(vec![drx_app::social::track_of(&r)], 0);
            }
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

/// «Удалить» in the dialog: the open friend goes off the list (their data on the relays is cleared too).
pub fn remove_open(ui: &MainWindow, app: &App) {
    ui.set_dialog_kind("".into());
    let Some(peer) = OPEN.with(|o| o.borrow().clone()) else { return };
    let name = ui.get_friend_name().to_string();
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = app2.remove_friend(&peer).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match res {
                Ok(()) => {
                    OPEN.with(|o| *o.borrow_mut() = None);
                    ui.set_toast(format!("{name} больше не в друзьях").into());
                }
                Err(e) => ui.set_toast(format!("Не получилось: {e}").into()),
            }
            render(&ui, &app2);
        });
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
    app.start_chats_live();
    if let Some(mut rx) = app.chats_version() {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        app.runtime().spawn(async move {
            while rx.changed().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let (app3, ui_w) = (app2.clone(), ui_w.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_w.upgrade()
                        && ui.get_view_friends()
                        && (!ui.get_friend_page() || TAB.with(|t| t.borrow().as_str() == "chat"))
                    {
                        render(&ui, &app3);
                    }
                });
            }
        });
    }
    {
        let mut rx = app.listen_along_version();
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        app.runtime().spawn(async move {
            while rx.changed().await.is_ok() {
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
    redraw_when_typing_ends(ui, app);
    let Some(f) = app.friends() else { return };
    let st: FriendsState = f.state().borrow().clone();
    let me = f.me();
    let mine = st.data.get(&me);
    let my_name = plain(&mine.and_then(|d| d.name.clone()).unwrap_or_default());
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
            initial: initial(&plain(&r.name)).into(),
            name: plain(&r.name).into(),
            when: ago(r.at).into(),
        })
        .collect();
    ui.set_friends_requests(ModelRc::new(VecModel::from(requests)));

    let mut friends = st.friends.clone();
    let t = now_ms();
    let previews: std::collections::HashMap<String, drx_app::chats::ChatPreview> =
        app.chat_previews().into_iter().map(|p| (p.peer.clone(), p)).collect();
    // Unread chats first, then who plays now, then the most recent listeners.
    friends.sort_by_key(|fr| {
        let n = st.data.get(&fr.pubkey).and_then(|d| d.now.as_ref());
        let unread = previews.get(&fr.pubkey).is_some_and(|p| p.unread > 0);
        (
            std::cmp::Reverse(unread),
            std::cmp::Reverse(n.is_some_and(|n| n.is_live(t))),
            std::cmp::Reverse(n.map_or(0, |n| n.at)),
        )
    });
    let cards: Vec<FriendCardItem> = friends
        .iter()
        .map(|fr| {
            let d = st.data.get(&fr.pubkey);
            let name = d
                .and_then(|d| d.name.clone())
                .filter(|n| !n.trim().is_empty())
                .or_else(|| Some(fr.name.clone()).filter(|n| !n.trim().is_empty()))
                .map(|n| plain(&n))
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
                preview: if app.chat_typing(&fr.pubkey) {
                    "печатает…".into()
                } else {
                    previews.get(&fr.pubkey).map(preview_line).unwrap_or_default().into()
                },
                unread: previews.get(&fr.pubkey).map_or(0, |p| p.unread.min(999) as i32),
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
        .filter(|n| !n.trim().is_empty())
        .or_else(|| st.friends.iter().find(|x| x.pubkey == p).map(|x| x.name.clone()))
        .filter(|n| !n.trim().is_empty())
        .map(|n| plain(&n))
        .unwrap_or_else(|| "Друг".into());
    let along = app.listening_along().as_deref() == Some(p.as_str());
    ui.set_friend_along(along);
    ui.set_friend_can_along(d.and_then(|d| d.now.as_ref()).is_some_and(|n| n.is_live(t) && n.listen_along));
    let typing = app.chat_typing(&p);
    ui.set_friend_typing(typing);
    ui.set_friend_status(
        match d.and_then(|d| d.now.as_ref()) {
            _ if typing => "печатает…".to_string(),
            Some(n) if along => format!("слушаю вместе: {} — {}", n.track.title, n.track.artist),
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
    let shown = avatar_url(app, &p, d.and_then(|d| d.avatar.as_deref())).map(|u| (u, name.clone()));
    SHOWN_AVATAR.with(|s| *s.borrow_mut() = shown);
    ui.set_friend_initial(initial(&name).into());
    ui.set_friend_name(name.into());
    ui.set_friend_likes(d.and_then(|d| d.likes.as_ref()).map(|l| l.total.to_string()).unwrap_or_default().into());
    crate::portrait_view::tags(ui, d);
    let tab = TAB.with(|x| x.borrow().clone());
    ui.set_friend_tab(tab.clone().into());
    if tab == "chat" {
        render_chat(ui, app, &p);
        return;
    }
    if tab == "portrait" {
        let tracks = crate::portrait_view::render(ui, app, d);
        TRACKS.with(|t| *t.borrow_mut() = tracks);
        return;
    }
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

/// The conversation with `peer` (the newest 300 messages); opening it reads the friend's messages.
fn render_chat(ui: &MainWindow, app: &App, peer: &str) {
    app.chat_opened(peer);
    let rows = app.chat_messages(peer, 300);
    let items: Vec<ChatItem> = rows
        .iter()
        .map(|m| {
            let music = m.music.as_ref();
            let cover = music
                .and_then(|c| c.cover.as_deref())
                .and_then(|u| crate::cover_r(app, u, crate::ROW_COVER, crate::theme_r(8.0, crate::ROW_COVER, 44.0)));
            // Stickers alone, all with a picture here: drawn big (an animated one keeps the message as text).
            let pics: Vec<StickerPic> = drx_social::chat::sticker_only(&m.text)
                .and_then(|ids| {
                    ids.iter()
                        .map(|id| drx_social::chat::mini_sticker(id).map(|(i, r)| StickerPic { pic: i as i32, ratio: r }))
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or_default();
            ChatItem {
                id: m.id.clone().into(),
                mine: m.mine,
                text: if pics.is_empty() { drx_social::chat::plain_stickers(&m.text).into() } else { "".into() },
                sticker_ratio: pics.iter().map(|p| p.ratio).sum(),
                stickers: ModelRc::new(VecModel::from(pics)),
                time: clock(m.ts).into(),
                edited: m.edited,
                state: m.state as i32,
                read: m.read,
                has_music: music.is_some(),
                music_kind: music.map(|c| c.kind.clone()).unwrap_or_default().into(),
                music_title: music.map(|c| c.title.clone()).unwrap_or_default().into(),
                music_by: music.map(|c| c.by.clone()).unwrap_or_default().into(),
                has_music_cover: cover.is_some(),
                music_cover: cover.unwrap_or_default(),
                reactions: ModelRc::new(VecModel::from(chips(&m.reactions))),
            }
        })
        .collect();
    CHAT_ROWS.with(|c| *c.borrow_mut() = rows.iter().map(|m| (m.id.clone(), m.mine, m.text.clone())).collect());
    CHAT_MUSIC.with(|c| *c.borrow_mut() = rows.into_iter().map(|m| m.music).collect());
    ui.set_friend_chat(ModelRc::new(VecModel::from(items)));
}

/// While someone is typing, one more redraw once their «печатает…» has run out.
fn redraw_when_typing_ends(ui: &MainWindow, app: &App) {
    thread_local! { static WAITING: RefCell<bool> = const { RefCell::new(false) }; }
    let typing = app.friends_state().is_some_and(|rx| rx.borrow().friends.iter().any(|f| app.chat_typing(&f.pubkey)));
    if !typing || WAITING.with(|w| std::mem::replace(&mut *w.borrow_mut(), true)) {
        return;
    }
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    slint::Timer::single_shot(std::time::Duration::from_millis(6_500), move || {
        WAITING.with(|w| *w.borrow_mut() = false);
        if let Some(ui) = ui_w.upgrade()
            && ui.get_view_friends()
        {
            render(&ui, &app2);
        }
    });
}

/// «Вы: текст · 14:51» — the friends list's line for a chat, as ChatHub.preview (one line: newlines become spaces).
fn preview_line(p: &drx_app::chats::ChatPreview) -> String {
    let text: String = drx_social::chat::plain_stickers(&p.text).split_whitespace().collect::<Vec<_>>().join(" ");
    let body = match (&p.music, text.is_empty()) {
        (Some(m), false) => format!("♪ {m} · {text}"),
        (Some(m), true) => format!("♪ {m}"),
        (None, false) => text,
        (None, true) => "стикер".into(),
    };
    format!("{}{} · {}", if p.mine { "Вы: " } else { "" }, body, clock(p.ts))
}

/// Reactions grouped by key, in the order first put: «❤️ 2», mine marked.
fn chips(reactions: &[(String, bool)]) -> Vec<ReactionChip> {
    let mut out: Vec<ReactionChip> = Vec::new();
    for (key, mine) in reactions {
        match out.iter_mut().find(|c| c.key.as_str() == key) {
            Some(c) => {
                c.count += 1;
                c.mine |= *mine;
            }
            None => out.push(ReactionChip {
                label: key.strip_prefix("e:").map(str::to_string).unwrap_or_else(|| "стикер".into()).into(),
                pic: drx_social::chat::emoji_index(key).map_or(-1, |i| i as i32),
                sticker: key.strip_prefix("s:").and_then(drx_social::chat::mini_sticker).map_or(-1, |s| s.0 as i32),
                ratio: key.strip_prefix("s:").and_then(drx_social::chat::mini_sticker).map_or(1.0, |s| s.1),
                key: key.clone().into(),
                count: 1,
                mine: *mine,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates() {
        assert_eq!(super::civil(0), (1970, 1, 1));
        assert_eq!(super::civil(20_407), (2025, 11, 15));
        assert_eq!(super::civil(19_782), (2024, 2, 29));
        assert_eq!(super::plain("\u{1D403}\u{1D41E}\u{1D426}\u{1D428} ok"), "Demo ok");
        assert_eq!(super::plain("шура"), "шура");
    }
}
