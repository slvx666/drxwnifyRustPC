//! Links the heavy dependencies Mini will need, so the size/RAM spike measures them honestly.
use std::hint::black_box;

pub fn touch_all() -> String {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA cache_size=-512; CREATE TABLE t(x TEXT); INSERT INTO t VALUES('ok');").unwrap();
    let v: String = db.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();

    let rt = rquickjs::Runtime::new().unwrap();
    let ctx = rquickjs::Context::full(&rt).unwrap();
    let js: i32 = ctx.with(|c| c.eval("[1,2,3].map(x=>x*2).reduce((a,b)=>a+b)").unwrap());

    let _client = black_box(reqwest::Client::builder().build().unwrap());
    let _rt = black_box(tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap());
    let _dec = black_box(opus::Decoder::new(48000, opus::Channels::Stereo).unwrap());
    let probe = black_box(symphonia::default::get_probe());
    let _ = black_box(&probe);
    let j = serde_json::json!({"db": v, "js": js});
    j.to_string()
}
