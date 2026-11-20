//! The «Слушать вместе» room card on the friends page: create, join by code or link, leave; the host lets people
//! in, hands the room over, removes people and takes guests' suggestions. Redrawn whenever the room changes.

use crate::{MainWindow, RoomAsk, RoomData, RoomPerson};
use drx_app::App;
use drx_together::{LINK_PREFIX, Role, Room, normalize_code};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;

thread_local! {
    /// The room last drawn (its status is said once more when the room ends).
    static LAST: RefCell<Option<Room>> = const { RefCell::new(None) };
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

pub fn wire(ui: &MainWindow, app: &App) {
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_room_action(move |action, arg| {
            let Some(ui) = ui_w.upgrade() else { return };
            let name = ui.get_friends_me_name().to_string();
            match action.as_str() {
                "create" => app2.room_start(&name, None),
                "join" => {
                    let code = normalize_code(&arg);
                    if code.len() < 4 {
                        ui.set_toast("Вставьте код комнаты или ссылку на неё".into());
                        return;
                    }
                    ui.set_room_join_text("".into());
                    app2.room_start(&name, Some(&code));
                }
                "leave" => app2.room_leave(),
                "link" => {
                    let code = app2
                        .room()
                        .map(|r| r.borrow().code.clone())
                        .unwrap_or_default();
                    if !code.is_empty() {
                        let ok = drx_platform::copy_text(&format!("{LINK_PREFIX}{code}"));
                        ui.set_toast(
                            if ok {
                                "Ссылка на комнату скопирована"
                            } else {
                                "Буфер обмена занят — попробуйте ещё раз"
                            }
                            .into(),
                        );
                    }
                }
                "resync" => app2.room_resync(),
                "approve" => app2.room_approve(&arg, true),
                "reject" => app2.room_approve(&arg, false),
                "kick" => app2.room_kick(&arg),
                "host" => app2.room_make_host(&arg),
                "take" => app2.room_answer_suggestion(&arg, true),
                "drop" => app2.room_answer_suggestion(&arg, false),
                _ => {}
            }
        });
    }
    // Follow the room: a new room (or none) on each version change, and every change of the room itself.
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let mut version = app2.room_version();
        loop {
            match app2.room() {
                Some(mut rx) => {
                    let room = rx.borrow_and_update().clone();
                    let _ = ui_w.upgrade_in_event_loop(move |ui| render(&ui, Some(room)));
                    tokio::select! {
                        r = rx.changed() => if r.is_err() { let _ = version.changed().await; },
                        r = version.changed() => if r.is_err() { return },
                    }
                }
                None => {
                    let _ = ui_w.upgrade_in_event_loop(move |ui| render(&ui, None));
                    if version.changed().await.is_err() {
                        return;
                    }
                }
            }
        }
    });
}

fn render(ui: &MainWindow, room: Option<Room>) {
    let Some(r) = room else {
        // The room ended: say why once (not let in, removed, no connection).
        if let Some(last) = LAST.with(|l| l.borrow_mut().take())
            && !last.status.is_empty()
            && last.role == Role::None
            && !last.status.ends_with('…')
        {
            ui.set_toast(last.status.clone().into());
        }
        ui.set_room(RoomData::default());
        ui.set_room_guest(false);
        return;
    };
    let name_of = |id: &str| {
        r.users
            .iter()
            .find(|u| u.user_id == id)
            .map(|u| u.username.clone())
            .unwrap_or_default()
    };
    let data = RoomData {
        active: true,
        code: r.code.clone().into(),
        role: match r.role {
            Role::Host => "host",
            Role::Guest => "guest",
            Role::None => "",
        }
        .into(),
        status: r.status.clone().into(),
        users: model(
            r.users
                .iter()
                .map(|u| RoomPerson {
                    id: u.user_id.clone().into(),
                    name: if u.username.is_empty() {
                        "Без имени".into()
                    } else {
                        u.username.clone().into()
                    },
                    host: u.user_id == r.host_id,
                    me: u.user_id == r.my_id,
                    online: u.is_connected || u.user_id == r.my_id,
                })
                .collect(),
        ),
        requests: model(
            r.requests
                .iter()
                .map(|(id, name)| RoomAsk {
                    id: id.clone().into(),
                    name: name.clone().into(),
                    title: "".into(),
                })
                .collect(),
        ),
        suggestions: model(
            r.suggestions
                .iter()
                .map(|s| RoomAsk {
                    id: s.id.clone().into(),
                    name: s.from.clone().into(),
                    title: format!("{} — {}", s.track.artist, s.track.title).into(),
                })
                .collect(),
        ),
        buffering: r
            .buffering
            .iter()
            .map(|id| name_of(id))
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    };
    ui.set_room_guest(r.role == Role::Guest);
    ui.set_room(data);
    LAST.with(|l| *l.borrow_mut() = Some(r));
}
