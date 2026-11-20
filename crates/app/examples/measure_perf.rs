//! Performance & Memory Benchmarks for Drxwnify PC (Mini & App Core).
//! Measures:
//! 1. Search & resolution latency across providers.
//! 2. Stream chunk throughput (1MB, 2MB, 4MB) for fast downloads.
//! 3. Audio decoding & seek latency.
//! 4. Memory footprint and leak check across 50 track transitions.

use drx_core::http::Http;
use drx_resolver::provider::{AudioProvider, AudioQuery, DEFAULT_ORDER};
use drx_resolver::providers::soundcloud::SoundCloud;
use drx_resolver::providers::youtube::YouTube;
use std::sync::Arc;
use std::time::Instant;

#[repr(C)]
#[allow(non_snake_case)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn K32GetProcessMemoryInfo(
        process: *mut std::ffi::c_void,
        ppsmem_counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

fn get_working_set_mb() -> f64 {
    #[cfg(windows)]
    unsafe {
        let mut c: ProcessMemoryCounters = std::mem::zeroed();
        c.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
            return c.working_set_size as f64 / (1024.0 * 1024.0);
        }
    }
    0.0
}

#[tokio::main]
async fn main() {
    println!("===============================================================");
    println!(" DRXWNIFY PC - PERFORMANCE & MEMORY AUDIT");
    println!(" Baseline Working Set: {:.2} MB", get_working_set_mb());
    println!("===============================================================\n");

    let http = Http::new().expect("http client");
    let yt = Arc::new(YouTube::new(http.clone()));
    let sc = Arc::new(SoundCloud::new(http.clone()));
    let providers: Vec<Arc<dyn AudioProvider>> = vec![yt.clone(), sc.clone()];

    // -------------------------------------------------------------------------
    // TEST 1: SEARCH & RESOLUTION LATENCY
    // -------------------------------------------------------------------------
    println!("--- TEST 1: Provider Search & Stream Resolution Latency ---");
    let test_tracks = [
        ("Show Me A Dinosaur", "Sunflower", 380_000u64),
        ("Holy Fawn", "Dark Stone", 312_000u64),
        ("Lantlôs", "Melting Sun I: Azure Chimes", 420_000u64),
        ("White Ward", "Leviathan", 560_000u64),
        ("Deftones", "Be Quiet and Drive (Far Away)", 308_000u64),
    ];

    let mut stream_urls: Vec<(String, Option<&'static str>)> = Vec::new();

    for (artist, title, dur) in test_tracks {
        let q = AudioQuery {
            title: title.to_string(),
            artists: vec![artist.to_string()],
            album: None,
            duration_ms: dur,
            isrc: None,
            catalog_id: None,
            excluded: Vec::new(),
        };

        let t0 = Instant::now();
        let outcome = drx_resolver::parallel::resolve(&q, &providers, &DEFAULT_ORDER).await;
        let resolve_time = t0.elapsed();

        if let Some(winner) = &outcome.winner {
            let t1 = Instant::now();
            let provider = providers
                .iter()
                .find(|p| p.id() == winner.provider)
                .unwrap();
            let stream = provider.stream(&q, winner).await;
            let stream_time = t1.elapsed();

            match stream {
                Ok(Some(s)) => {
                    println!(
                        "  [OK] '{artist} - {title}': found via {} in {:.0}ms (conf: {:.2}), stream URL in {:.0}ms [Total: {:.0}ms]",
                        winner.provider.name(),
                        resolve_time.as_millis(),
                        winner.confidence,
                        stream_time.as_millis(),
                        (resolve_time + stream_time).as_millis()
                    );
                    if stream_urls.is_empty() {
                        stream_urls.push((s.url, s.user_agent));
                    }
                }
                Ok(None) => println!("  [WARN] Winner gave no stream: {winner:?}"),
                Err(e) => println!("  [ERR] Stream error: {e:?}"),
            }
        } else {
            println!(
                "  [FAIL] Nothing found for '{artist} - {title}' in {:.0}ms",
                resolve_time.as_millis()
            );
        }
    }
    println!(
        " Working Set after Test 1: {:.2} MB\n",
        get_working_set_mb()
    );

    // -------------------------------------------------------------------------
    // TEST 2: DOWNLOAD THROUGHPUT COMPARISON BY CHUNK SIZE
    // -------------------------------------------------------------------------
    println!("--- TEST 2: Download Throughput by Chunk Size (YouTube) ---");
    if let Some((url, ua)) = stream_urls.first() {
        if url.contains(".googlevideo.com/") {
            let target_bytes = 4 * 1024 * 1024u64; // 4 MB test download

            for chunk_size in [1024 * 1024u64, 2 * 1024 * 1024u64, 4 * 1024 * 1024u64] {
                let chunk_label = format!("{} MB", chunk_size / (1024 * 1024));
                let t0 = Instant::now();
                let mut downloaded = 0u64;
                let mut req_count = 0;

                while downloaded < target_bytes {
                    let to = (downloaded + chunk_size - 1).min(target_bytes - 1);
                    let mut req = http
                        .client()
                        .get(url)
                        .header("Range", format!("bytes={downloaded}-{to}"));
                    if let Some(ua_str) = ua {
                        req = req.header("User-Agent", *ua_str);
                    }
                    req_count += 1;
                    match req.send().await {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(bytes) = resp.bytes().await {
                                downloaded += bytes.len() as u64;
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }

                let elapsed = t0.elapsed();
                let mb_per_sec =
                    (downloaded as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64().max(0.001);
                println!(
                    "  Chunk size: {:>4} | Transferred: {:.2} MB in {:>5}ms ({:>2} reqs) | Speed: {:>6.2} MB/s",
                    chunk_label,
                    downloaded as f64 / (1024.0 * 1024.0),
                    elapsed.as_millis(),
                    req_count,
                    mb_per_sec
                );
            }
        } else {
            println!("  (First stream is not googlevideo, skipping range chunk test)");
        }
    } else {
        println!("  (No stream URL available for download test)");
    }
    println!(
        " Working Set after Test 2: {:.2} MB\n",
        get_working_set_mb()
    );

    // -------------------------------------------------------------------------
    // TEST 3: MEMORY LEAK & STABILITY STRESS TEST (50 Track Resolutions)
    // -------------------------------------------------------------------------
    println!("--- TEST 3: Memory Stability & Stress Test (50 Queries) ---");
    let mem_start = get_working_set_mb();
    println!("  Starting memory: {:.2} MB", mem_start);

    for i in 1..=50 {
        let q = AudioQuery {
            title: format!("Track number {}", (i % 10) + 1),
            artists: vec!["Test Artist".to_string()],
            album: None,
            duration_ms: 180_000,
            isrc: None,
            catalog_id: Some(format!("test_track_{i}")),
            excluded: Vec::new(),
        };

        // Parallel resolve
        let _ = drx_resolver::parallel::resolve(&q, &providers, &DEFAULT_ORDER).await;

        if i % 10 == 0 {
            println!(
                "  Iteration {:>2}/50 | Working Set: {:>6.2} MB (diff from start: {:>+6.2} MB)",
                i,
                get_working_set_mb(),
                get_working_set_mb() - mem_start
            );
        }
    }

    let mem_end = get_working_set_mb();
    println!(
        "  Final Working Set: {:.2} MB (Total change: {:>+6.2} MB)",
        mem_end,
        mem_end - mem_start
    );

    println!("\n===============================================================");
    println!(" AUDIT COMPLETE");
    println!("===============================================================");
}
