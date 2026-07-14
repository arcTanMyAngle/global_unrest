//! Deterministic synthetic fixture generator.
//!
//! Produces ~35 days of GDELT-shaped attention observations and ACLED-shaped
//! event records around a fixed set of world locations, with scripted spikes
//! so the UI has something to show and M2 spike detection has something to
//! find. Everything is synthetic: headlines are tagged `[synthetic]`, outlet
//! domains use the reserved `.example` TLD, and the time span ends at a fixed
//! anchor date so output is byte-stable for a given seed.
//!
//! Usage: cargo run -p source-fixtures --bin generate-fixtures
//!        [-- --out <path>] [--days N] [--seed N]

use std::fmt::Write as _;
use std::io::Write as _;

use chrono::{DateTime, Datelike, TimeZone, Utc};

/// SplitMix64: tiny, seedable, and stable across releases — unlike `rand`,
/// whose distributions may change between versions. Determinism of committed
/// fixtures matters more than statistical quality here.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in [0, n).
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Knuth Poisson sampler; fine for the small λ used here.
    fn poisson(&mut self, lambda: f64) -> u32 {
        let l = (-lambda).exp();
        let mut k = 0u32;
        let mut p = 1.0;
        loop {
            p *= self.f64();
            if p <= l {
                return k;
            }
            k += 1;
            if k > 1000 {
                return k; // safety valve; unreachable for our λ
            }
        }
    }
}

struct Spot {
    name: &'static str,
    iso: &'static str,
    admin1: Option<&'static str>,
    lat: f64,
    lon: f64,
    /// "city" or "country" (country = deliberate centroid records that
    /// exercise the precision rendering contract).
    precision: &'static str,
    /// Expected attention observations per 6h bucket.
    att_rate: f64,
    /// Expected discrete events per 6h bucket.
    evt_rate: f64,
    themes: &'static [&'static str],
    /// Dominant event type for ACLED-shaped records.
    event_types: &'static [&'static str],
}

const SPOTS: &[Spot] = &[
    Spot {
        name: "Paris",
        iso: "FRA",
        admin1: Some("Île-de-France"),
        lat: 48.8566,
        lon: 2.3522,
        precision: "city",
        att_rate: 4.0,
        evt_rate: 0.40,
        themes: &["PROTEST", "LABOR", "TRANSPORT"],
        event_types: &["Protests", "Riots"],
    },
    Spot {
        name: "Berlin",
        iso: "DEU",
        admin1: Some("Berlin"),
        lat: 52.5200,
        lon: 13.4050,
        precision: "city",
        att_rate: 3.2,
        evt_rate: 0.24,
        themes: &["PROTEST", "ENERGY", "ECON_INFLATION"],
        event_types: &["Protests"],
    },
    Spot {
        name: "London",
        iso: "GBR",
        admin1: Some("England"),
        lat: 51.5074,
        lon: -0.1278,
        precision: "city",
        att_rate: 4.0,
        evt_rate: 0.24,
        themes: &["PROTEST", "LABOR", "TRANSPORT"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Nairobi",
        iso: "KEN",
        admin1: Some("Nairobi"),
        lat: -1.2921,
        lon: 36.8219,
        precision: "city",
        att_rate: 3.6,
        evt_rate: 0.48,
        themes: &["ELECTIONS", "PROTEST"],
        event_types: &["Protests", "Riots"],
    },
    Spot {
        name: "Jakarta",
        iso: "IDN",
        admin1: Some("Jakarta"),
        lat: -6.2088,
        lon: 106.8456,
        precision: "city",
        att_rate: 3.6,
        evt_rate: 0.40,
        themes: &["FLOOD", "TRANSPORT", "INFRASTRUCTURE"],
        event_types: &["Strategic developments", "Protests"],
    },
    Spot {
        name: "Santiago",
        iso: "CHL",
        admin1: Some("Santiago Metropolitan"),
        lat: -33.4489,
        lon: -70.6693,
        precision: "city",
        att_rate: 2.8,
        evt_rate: 0.32,
        themes: &["PROTEST", "EDUCATION"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Bogotá",
        iso: "COL",
        admin1: Some("Bogotá"),
        lat: 4.7110,
        lon: -74.0721,
        precision: "city",
        att_rate: 3.2,
        evt_rate: 0.40,
        themes: &["PROTEST", "SECURITY"],
        event_types: &["Protests", "Violence against civilians"],
    },
    Spot {
        name: "Cairo",
        iso: "EGY",
        admin1: Some("Cairo"),
        lat: 30.0444,
        lon: 31.2357,
        precision: "city",
        att_rate: 3.2,
        evt_rate: 0.32,
        themes: &["ECON_INFLATION", "PROTEST"],
        event_types: &["Protests"],
    },
    Spot {
        name: "New Delhi",
        iso: "IND",
        admin1: Some("Delhi"),
        lat: 28.6139,
        lon: 77.2090,
        precision: "city",
        att_rate: 4.4,
        evt_rate: 0.48,
        themes: &["PROTEST", "LABOR", "AGRICULTURE"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Manila",
        iso: "PHL",
        admin1: Some("Metro Manila"),
        lat: 14.5995,
        lon: 120.9842,
        precision: "city",
        att_rate: 3.2,
        evt_rate: 0.40,
        themes: &["STORM", "TRANSPORT", "PROTEST"],
        event_types: &["Strategic developments", "Protests"],
    },
    Spot {
        name: "Lagos",
        iso: "NGA",
        admin1: Some("Lagos"),
        lat: 6.5244,
        lon: 3.3792,
        precision: "city",
        att_rate: 3.6,
        evt_rate: 0.48,
        themes: &["FUEL", "PROTEST", "SECURITY"],
        event_types: &["Protests", "Violence against civilians"],
    },
    Spot {
        name: "São Paulo",
        iso: "BRA",
        admin1: Some("São Paulo"),
        lat: -23.5505,
        lon: -46.6333,
        precision: "city",
        att_rate: 3.6,
        evt_rate: 0.32,
        themes: &["PROTEST", "TRANSPORT"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Mexico City",
        iso: "MEX",
        admin1: Some("CDMX"),
        lat: 19.4326,
        lon: -99.1332,
        precision: "city",
        att_rate: 3.6,
        evt_rate: 0.40,
        themes: &["PROTEST", "SECURITY", "WATER"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Kyiv",
        iso: "UKR",
        admin1: Some("Kyiv"),
        lat: 50.4501,
        lon: 30.5234,
        precision: "city",
        att_rate: 4.8,
        evt_rate: 0.60,
        themes: &["CONFLICT", "ENERGY", "AIR_DEFENSE"],
        event_types: &["Battles", "Explosions/Remote violence"],
    },
    Spot {
        name: "Karachi",
        iso: "PAK",
        admin1: Some("Sindh"),
        lat: 24.8607,
        lon: 67.0011,
        precision: "city",
        att_rate: 3.2,
        evt_rate: 0.40,
        themes: &["ENERGY", "PROTEST"],
        event_types: &["Protests", "Violence against civilians"],
    },
    // Near the antimeridian on purpose: exercises boundary splitting.
    Spot {
        name: "Suva",
        iso: "FJI",
        admin1: Some("Central"),
        lat: -18.1248,
        lon: 178.4501,
        precision: "city",
        att_rate: 1.2,
        evt_rate: 0.12,
        themes: &["CYCLONE", "INFRASTRUCTURE"],
        event_types: &["Strategic developments"],
    },
    Spot {
        name: "Auckland",
        iso: "NZL",
        admin1: Some("Auckland"),
        lat: -36.8485,
        lon: 174.7633,
        precision: "city",
        att_rate: 1.6,
        evt_rate: 0.12,
        themes: &["TRANSPORT", "LABOR"],
        event_types: &["Protests"],
    },
    Spot {
        name: "Seattle",
        iso: "USA",
        admin1: Some("Washington"),
        lat: 47.6062,
        lon: -122.3321,
        precision: "city",
        att_rate: 2.8,
        evt_rate: 0.20,
        themes: &["LABOR", "TECH", "PROTEST"],
        event_types: &["Protests"],
    },
    // Country centroids: deliberately coarse geocoding. These must never
    // render as point markers (precision contract, docs/DATA_MODEL.md).
    Spot {
        name: "Russia (centroid)",
        iso: "RUS",
        admin1: None,
        lat: 61.5240,
        lon: 105.3188,
        precision: "country",
        att_rate: 2.8,
        evt_rate: 0.20,
        themes: &["CONFLICT", "ENERGY"],
        event_types: &["Battles"],
    },
    Spot {
        name: "Brazil (centroid)",
        iso: "BRA",
        admin1: None,
        lat: -14.2350,
        lon: -51.9253,
        precision: "country",
        att_rate: 2.4,
        evt_rate: 0.16,
        themes: &["DEFORESTATION", "PROTEST"],
        event_types: &["Protests"],
    },
    Spot {
        name: "DR Congo (centroid)",
        iso: "COD",
        admin1: None,
        lat: -4.0383,
        lon: 21.7587,
        precision: "country",
        att_rate: 2.4,
        evt_rate: 0.32,
        themes: &["CONFLICT", "DISPLACEMENT"],
        event_types: &["Battles", "Violence against civilians"],
    },
    Spot {
        name: "Australia (centroid)",
        iso: "AUS",
        admin1: None,
        lat: -25.2744,
        lon: 133.7751,
        precision: "country",
        att_rate: 2.0,
        evt_rate: 0.12,
        themes: &["WILDFIRE", "MINING"],
        event_types: &["Strategic developments"],
    },
];

/// Scripted spikes: (spot name, start day, end day inclusive, multiplier,
/// theme override). Day 0 is the oldest day.
const SPIKES: &[(&str, u32, u32, f64, Option<&str>)] = &[
    ("Paris", 20, 23, 6.0, Some("PROTEST")),
    ("Nairobi", 10, 12, 5.0, Some("ELECTIONS")),
    ("Jakarta", 28, 29, 7.0, Some("FLOOD")),
    ("Lagos", 31, 32, 4.0, Some("FUEL")),
];

const OUTLETS: &[&str] = &[
    "globalwire.example",
    "daily-ledger.example",
    "worldpost.example",
    "signal-times.example",
    "meridian-news.example",
    "the-observer.example",
    "newsgrid.example",
    "horizon-daily.example",
    "civic-monitor.example",
    "open-dispatch.example",
];

/// Fixed anchor so regeneration with the same seed is byte-identical.
/// 2026-07-01T00:00:00Z, exclusive end of the generated span.
fn anchor(days: u32) -> (DateTime<Utc>, DateTime<Utc>) {
    let end = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    (end - chrono::Duration::days(i64::from(days)), end)
}

fn weekday_factor(dt: DateTime<Utc>) -> f64 {
    // Mon..Sun — quieter weekends make M2 baselines interesting.
    [1.0, 1.05, 1.1, 1.05, 1.0, 0.8, 0.7][dt.weekday().num_days_from_monday() as usize]
}

const BUCKET_FACTOR: [f64; 4] = [0.7, 1.2, 1.3, 0.8];

fn spike_multiplier(spot: &Spot, day: u32) -> (f64, Option<&'static str>) {
    for &(name, from, to, mult, theme) in SPIKES {
        if name == spot.name && (from..=to).contains(&day) {
            return (mult, theme);
        }
    }
    (1.0, None)
}

fn main() {
    let mut out_path = String::from("fixtures/generated/events_35d.json");
    let mut days: u32 = 35;
    let mut seed: u64 = 42;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out_path = args.get(i + 1).expect("--out needs a value").clone();
                i += 2;
            }
            "--days" => {
                days = args
                    .get(i + 1)
                    .expect("--days needs a value")
                    .parse()
                    .expect("--days");
                i += 2;
            }
            "--seed" => {
                seed = args
                    .get(i + 1)
                    .expect("--seed needs a value")
                    .parse()
                    .expect("--seed");
                i += 2;
            }
            other => panic!("unknown argument `{other}`"),
        }
    }

    let mut rng = Rng(seed);
    let (start, _end) = anchor(days);
    let mut records: Vec<(i64, String)> = Vec::new();
    let mut gdoc_seq = 0u64;
    let mut aevt_seq = 0u64;
    let (mut n_attention, mut n_events) = (0u64, 0u64);

    for day in 0..days {
        for bucket in 0..4u32 {
            let bucket_start = start
                + chrono::Duration::days(i64::from(day))
                + chrono::Duration::hours(i64::from(bucket) * 6);
            for spot in SPOTS {
                let (mult, spike_theme) = spike_multiplier(spot, day);
                let base = weekday_factor(bucket_start) * BUCKET_FACTOR[bucket as usize] * mult;

                // --- attention observations (gdelt_doc shape) ---
                for _ in 0..rng.poisson(spot.att_rate * base) {
                    let ts = bucket_start + chrono::Duration::seconds(rng.below(6 * 3600) as i64);
                    let theme = spike_theme
                        .unwrap_or(spot.themes[rng.below(spot.themes.len() as u64) as usize]);
                    let articles = 1 + (-rng.f64().max(1e-9).ln() * 6.0 * mult.sqrt()) as u64;
                    let articles = articles.min(80);
                    let n_outlets = 1 + rng.below(articles.min(6));
                    let mut domains: Vec<&str> = Vec::new();
                    while (domains.len() as u64) < n_outlets {
                        let d = OUTLETS[rng.below(OUTLETS.len() as u64) as usize];
                        if !domains.contains(&d) {
                            domains.push(d);
                        }
                    }
                    gdoc_seq += 1;
                    n_attention += 1;
                    let id = format!("gdoc-{gdoc_seq:06}");
                    let title = format!(
                        "[synthetic] Coverage of {} activity in {}",
                        theme.to_lowercase().replace('_', " "),
                        spot.name
                    );
                    let mut rec = String::with_capacity(512);
                    write!(
                        rec,
                        r#"{{"shape":"gdelt_doc","record_id":"{id}","seendate":"{}","themes":["{theme}"],"lat":{},"lon":{},"geo_precision":"{}","country_iso":"{}""#,
                        ts.format("%Y-%m-%dT%H:%M:%SZ"),
                        spot.lat,
                        spot.lon,
                        if spot.precision == "city" { "city" } else { "country" },
                        spot.iso
                    )
                    .unwrap();
                    if let Some(a1) = spot.admin1 {
                        write!(rec, r#","admin1":"{a1}""#).unwrap();
                    }
                    let domains_json = domains
                        .iter()
                        .map(|d| format!("\"{d}\""))
                        .collect::<Vec<_>>()
                        .join(",");
                    write!(
                        rec,
                        r#","num_articles":{articles},"num_sources":{n_outlets},"title":"{title}","domains":[{domains_json}],"urls":["https://{}/a/{gdoc_seq}"]}}"#,
                        domains[0]
                    )
                    .unwrap();
                    records.push((ts.timestamp(), rec));
                }

                // --- discrete events (acled_event shape) ---
                for _ in 0..rng.poisson(spot.evt_rate * base) {
                    let ts = bucket_start + chrono::Duration::seconds(rng.below(6 * 3600) as i64);
                    let event_type =
                        spot.event_types[rng.below(spot.event_types.len() as u64) as usize];
                    let theme = spike_theme
                        .unwrap_or(spot.themes[rng.below(spot.themes.len() as u64) as usize]);
                    let fatalities = if matches!(
                        event_type,
                        "Battles" | "Explosions/Remote violence" | "Violence against civilians"
                    ) {
                        rng.below(12)
                    } else if event_type == "Riots" {
                        rng.below(3)
                    } else {
                        0
                    };
                    let geo_precision = if spot.precision == "city" {
                        if rng.f64() < 0.8 { 1 } else { 2 }
                    } else {
                        3
                    };
                    aevt_seq += 1;
                    n_events += 1;
                    let id = format!("aevt-{aevt_seq:06}");
                    let headline = format!(
                        "[synthetic] {event_type} reported in {} ({})",
                        spot.name,
                        theme.to_lowercase().replace('_', " ")
                    );
                    let mut rec = String::with_capacity(512);
                    write!(
                        rec,
                        r#"{{"shape":"acled_event","record_id":"{id}","event_date":"{}","event_time":"{}","event_type":"{event_type}","lat":{},"lon":{},"geo_precision":{geo_precision},"country_iso":"{}""#,
                        ts.format("%Y-%m-%d"),
                        ts.format("%H:%M:%S"),
                        spot.lat,
                        spot.lon,
                        spot.iso
                    )
                    .unwrap();
                    if let Some(a1) = spot.admin1 {
                        write!(rec, r#","admin1":"{a1}""#).unwrap();
                    }
                    write!(
                        rec,
                        r#","fatalities":{fatalities},"source_count":{},"notes_headline":"{headline}","tags":["{theme}"]}}"#,
                        1 + rng.below(5)
                    )
                    .unwrap();
                    records.push((ts.timestamp(), rec));
                }
            }
        }
    }

    // Deliberately malformed records to exercise ingest_log (never crash):
    // out-of-range coordinates, and a record with no shape at all.
    records.push((
        i64::MAX - 1,
        r#"{"shape":"gdelt_doc","record_id":"bad-coords-000001","seendate":"2026-06-15T12:00:00Z","themes":["PROTEST"],"lat":999.0,"lon":0.0,"geo_precision":"city","country_iso":"ZZZ","num_articles":3,"num_sources":1,"title":"[synthetic] deliberately malformed record","domains":["globalwire.example"],"urls":[]}"#.to_string(),
    ));
    records.push((
        i64::MAX,
        r#"{"record_id":"no-shape-000001","note":"[synthetic] deliberately shapeless record for ingest_log"}"#.to_string(),
    ));

    records.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let path = std::path::Path::new(&out_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create output file"));
    writeln!(f, "{{").unwrap();
    writeln!(f, r#"  "schema": "les-fixture-v1","#).unwrap();
    writeln!(
        f,
        r#"  "generator": {{"seed": {seed}, "days": {days}, "anchor_end": "2026-07-01T00:00:00Z"}},"#
    )
    .unwrap();
    writeln!(f, r#"  "records": ["#).unwrap();
    let last = records.len().saturating_sub(1);
    for (i, (_, rec)) in records.iter().enumerate() {
        let comma = if i == last { "" } else { "," };
        writeln!(f, "    {rec}{comma}").unwrap();
    }
    writeln!(f, "  ]").unwrap();
    writeln!(f, "}}").unwrap();
    f.flush().unwrap();

    println!(
        "wrote {} records ({} attention, {} events, 2 malformed) to {}",
        records.len(),
        n_attention,
        n_events,
        out_path
    );
}
