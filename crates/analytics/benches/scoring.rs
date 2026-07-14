//! Scoring/aggregation throughput at M3-scale volumes (plan §11: 100k
//! events ≈ one GDELT day). Run with `cargo bench -p analytics`.

use std::hint::black_box;

use analytics::{ScoreEvent, compose_window, score_buckets};
use core_types::{BUCKET_SECS, EventKind};
use criterion::{Criterion, criterion_group, criterion_main};

/// Deterministic synthetic events: `n` records spread over ~200 cells and
/// 35 days, roughly fixture-shaped (¾ attention, ¼ discrete events).
fn synth_events(n: usize) -> Vec<ScoreEvent> {
    // SplitMix64, same generator family as the fixture generator.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let outlets = [
        "globalwire.example",
        "daily-ledger.example",
        "worldpost.example",
        "signal-times.example",
    ];
    let themes = ["protest", "flood", "transport", "elections"];
    (0..n)
        .map(|_| {
            let r = next();
            let attention = r % 4 != 0;
            ScoreEvent {
                h3_cell: 0x83_0000_0000_0000 | (r % 200), // synthetic cell keys
                ts_epoch_s: (r % (35 * 86_400)) as i64,
                kind: if attention {
                    EventKind::NewsAttention
                } else {
                    EventKind::Protest
                },
                article_count: (r % 40) as u32 + 1,
                distinct_source_count: (r % 4) as u32 + 1,
                location_confidence: 0.85,
                severity: (!attention).then_some(((r % 10) as f32) * 0.1),
                renders_as_point: r % 5 != 0,
                themes: vec![themes[(r % themes.len() as u64) as usize].to_string()],
                outlet_domains: vec![outlets[(r % outlets.len() as u64) as usize].to_string()],
            }
        })
        .collect()
}

fn bench_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("scoring");
    group.sample_size(20);
    for n in [10_000usize, 100_000] {
        let events = synth_events(n);
        group.bench_function(format!("score_buckets_{n}"), |b| {
            b.iter(|| score_buckets(black_box(&events)))
        });
    }

    let scored = score_buckets(&synth_events(100_000));
    let window = (28 * 86_400, 35 * 86_400);
    let cell_buckets: Vec<_> = scored
        .buckets
        .iter()
        .filter(|b| {
            b.h3_cell == scored.buckets[0].h3_cell
                && b.bucket_start >= window.0
                && b.bucket_start < window.1
        })
        .copied()
        .collect();
    assert!(!cell_buckets.is_empty());
    assert_eq!(window.0 % BUCKET_SECS, 0);
    group.bench_function("compose_window_7d", |b| {
        b.iter(|| compose_window(black_box(&cell_buckets), black_box(window)))
    });
    group.finish();
}

criterion_group!(benches, bench_scoring);
criterion_main!(benches);
