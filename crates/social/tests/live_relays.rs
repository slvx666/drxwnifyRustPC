//! Live check against the real relays with an imported profile's friends key (DRX_TEST_PROFILE). Prints counts
//! and yes/no only. Not run in CI.

use drx_social::nostr::{KIND_APP_DATA, Keys};
use drx_social::relay::Pool;
use serde_json::json;
use std::time::Duration;

#[test]
fn reads_android_events() {
    let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
        return;
    };
    let profile = drx_core::paths::Profile::at(dir);
    let vault = drx_core::secrets::Vault::load(&profile).expect("vault");
    let sk = vault
        .shared("friends", "sk")
        .expect("friends key in the vault");
    let keys = Keys::from_hex(sk).expect("key");
    let me = keys.public_hex();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = Pool::new(rt.handle());
    pool.set_signer(Some(keys.clone()));
    let own = rt.block_on(pool.query(
        vec![json!({"kinds":[KIND_APP_DATA],"authors":[me],"#d":["drxw:profile"]})],
        Duration::from_secs(12),
    ));
    eprintln!(
        "own profile events: {} (all valid: {})",
        own.len(),
        own.iter().all(|e| e.is_valid())
    );
    let personal = rt.block_on(pool.query(
        vec![json!({"kinds":[KIND_APP_DATA],"#p":[me],"limit":20})],
        Duration::from_secs(12),
    ));
    let decrypted = personal
        .iter()
        .filter(|e| !e.content.is_empty())
        .filter(|e| keys.decrypt(&e.pubkey, &e.content).is_ok())
        .count();
    eprintln!(
        "fields addressed to me: {}, decrypted: {decrypted}",
        personal.len()
    );
    assert!(
        !own.is_empty(),
        "the phone's profile should be on the relays"
    );
}

#[test]
fn follows_friends() {
    let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
        return;
    };
    let profile = drx_core::paths::Profile::at(dir);
    let vault = drx_core::secrets::Vault::load(&profile).expect("vault");
    let keys = Keys::from_hex(vault.shared("friends", "sk").unwrap()).unwrap();
    let xml = std::fs::read_to_string(profile.shared_prefs("friends")).unwrap();
    let sp = drx_core::shared_prefs::SharedPrefs::parse(&xml).unwrap();
    let saved = drx_social::friends::Saved {
        friends_json: sp.get_str("friends").map(str::to_string),
        privacy_json: sp.get_str("privacy").map(str::to_string),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();
    let pool = Pool::new(rt.handle());
    let friends = drx_social::friends::Friends::new(keys, pool, saved);
    friends.start_live();
    rt.block_on(tokio::time::sleep(Duration::from_secs(15)));
    let st = friends.state().borrow().clone();
    let count = |f: &dyn Fn(&drx_social::friends::FriendData) -> bool| {
        st.data.values().filter(|d| f(d)).count()
    };
    eprintln!(
        "friends {} | with data {} | name {} | avatar {} | now {} | history {} | likes {} | requests {}",
        st.friends.len(),
        st.data.len(),
        count(&|d| d.name.is_some()),
        count(&|d| d.avatar.is_some()),
        count(&|d| d.now.is_some()),
        count(&|d| d.history.is_some()),
        count(&|d| d.likes.is_some()),
        st.requests.len()
    );
    assert!(st.data.len() > 1);
}

#[test]
fn reads_chats_both_ways() {
    let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
        return;
    };
    let profile = drx_core::paths::Profile::at(dir);
    let vault = drx_core::secrets::Vault::load(&profile).expect("vault");
    let keys = Keys::from_hex(vault.shared("friends", "sk").unwrap()).unwrap();
    let me = keys.public_hex();
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 30 * 86_400;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = Pool::new(rt.handle());
    pool.set_signer(Some(keys.clone()));
    let events = rt.block_on(pool.query(
        vec![
            json!({"kinds":[4],"#p":[me],"since":since}),
            json!({"kinds":[4],"authors":[me],"since":since}),
        ],
        Duration::from_secs(15),
    ));
    let words: Vec<_> = events
        .iter()
        .filter_map(|e| drx_social::chat::decode(&keys, e))
        .collect();
    let msgs = |mine: bool| {
        words
            .iter()
            .filter(|w| {
                w.mine == mine && matches!(w.payload, drx_social::chat::Payload::Message { .. })
            })
            .count()
    };
    let peers: std::collections::HashSet<_> = words.iter().map(|w| w.peer.clone()).collect();
    eprintln!(
        "dm events {} | decoded {} | messages in {} out {} | chats {}",
        events.len(),
        words.len(),
        msgs(false),
        msgs(true),
        peers.len()
    );
}
