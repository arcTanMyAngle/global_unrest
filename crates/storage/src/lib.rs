//! DuckDB analytics storage (actor thread owning the connection) plus
//! rusqlite-backed app settings. DuckDB is single-writer per file: the
//! desktop app owns its database exclusively through M3.
