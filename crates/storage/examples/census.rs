//! TEMPORARY census probe — delete before gates.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("db path");
    let conn = duckdb::Connection::open(&path)?;

    const URLS: &str =
        "(SELECT source, headline, unnest(json_extract_string(urls, '$[*]')) AS u FROM events)";

    let video_pred = "(lower(u) LIKE '%youtube.com%' OR lower(u) LIKE '%youtu.be%' \
         OR lower(u) LIKE '%vimeo.com%' OR lower(u) LIKE '%twitch.tv%' \
         OR lower(u) LIKE '%tiktok.com%' OR lower(u) LIKE '%dailymotion.com%' \
         OR lower(u) LIKE '%streamable.com%' OR lower(u) LIKE '%rumble.com%' \
         OR lower(u) LIKE '%.mp4%' OR lower(u) LIKE '%.webm%' OR lower(u) LIKE '%.mov%' \
         OR lower(u) LIKE '%.m4v%' OR lower(u) LIKE '%.m3u8%')";

    let q1 = "SELECT source, count(*) FROM events GROUP BY 1 ORDER BY 2 DESC".to_string();
    let q2 =
        "SELECT source, count(*) FROM events WHERE urls <> '[]' GROUP BY 1 ORDER BY 2 DESC"
            .to_string();
    let q3 = format!(
        "SELECT regexp_extract(u, '^https?://([^/]+)', 1) AS host, count(*) c FROM {URLS} \
         GROUP BY 1 ORDER BY c DESC LIMIT 40"
    );
    let q4 = format!(
        "SELECT regexp_extract(u, '^https?://([^/]+)', 1) AS host, source, count(*) c \
         FROM {URLS} WHERE {video_pred} GROUP BY 1, 2 ORDER BY c DESC LIMIT 40"
    );
    let q5 = format!("SELECT source, headline, u FROM {URLS} WHERE {video_pred} LIMIT 15");
    let q6 = "SELECT source, kind, location_precision, headline FROM events \
         WHERE country_iso = 'COL' ORDER BY ts_epoch_s DESC LIMIT 15"
        .to_string();
    let q7 = "SELECT min(ts_epoch_s), max(ts_epoch_s), count(*) FROM events".to_string();

    let queries: &[(&str, &str)] = &[
        ("rows by source", &q1),
        ("rows with any url", &q2),
        ("distinct url hosts (top 40)", &q3),
        ("video-classified urls", &q4),
        ("sample video rows", &q5),
        ("colombia rows", &q6),
        ("time span / total", &q7),
    ];

    for (label, sql) in queries {
        println!("\n=== {label} ===");
        match conn.prepare(sql) {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                let mut n = 0;
                while let Some(row) = rows.next()? {
                    let mut out = Vec::new();
                    for i in 0.. {
                        match row.get_ref(i) {
                            Ok(v) => out.push(format!("{v:?}")),
                            Err(_) => break,
                        }
                    }
                    println!("{}", out.join(" | "));
                    n += 1;
                }
                if n == 0 {
                    println!("(no rows)");
                }
            }
            Err(e) => println!("ERR: {e}"),
        }
    }
    Ok(())
}
