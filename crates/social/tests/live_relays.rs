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
    let sk = vault.shared("friends", "sk").expect("friends key in the vault");
    let keys = Keys::from_hex(sk).expect("key");
    let me = keys.public_hex();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = Pool::new(rt.handle());
    pool.set_signer(Some(keys.clone()));
    let own = rt.block_on(pool.query(
        vec![json!({"kinds":[KIND_APP_DATA],"authors":[me],"#d":["drxw:profile"]})],
        Duration::from_secs(12),
    ));
    eprintln!("own profile events: {} (all valid: {})", own.len(), own.iter().all(|e| e.is_valid()));
    let personal = rt.block_on(pool.query(
        vec![json!({"kinds":[KIND_APP_DATA],"#p":[me],"limit":20})],
        Duration::from_secs(12),
    ));
    let decrypted = personal
        .iter()
        .filter(|e| !e.content.is_empty())
        .filter(|e| keys.decrypt(&e.pubkey, &e.content).is_ok())
        .count();
    eprintln!("fields addressed to me: {}, decrypted: {decrypted}", personal.len());
    assert!(!own.is_empty(), "the phone's profile should be on the relays");
}
