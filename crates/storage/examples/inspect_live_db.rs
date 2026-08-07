use duckdb::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("database path required")?;
    let conn = Connection::open(path)?;
    let mut stmt =
        conn.prepare("SELECT source, count(*) FROM events GROUP BY source ORDER BY source")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (source, count) = row?;
        println!("{source}={count}");
    }
    let fixtures: i64 = conn.query_row(
        "SELECT count(*) FROM events WHERE source = 'fixtures'",
        [],
        |row| row.get(0),
    )?;
    println!("fixture_rows={fixtures}");
    Ok(())
}
