//! One-time interactive Telegram login. Run this once per account:
//!
//! ```sh
//! cargo run -p source-telegram --features live --example login_setup
//! ```
//!
//! Reads `TELEGRAM_API_ID` / `TELEGRAM_API_HASH` / `LES_TELEGRAM_SESSION_FILE`
//! from `.env` (same as the rest of this workspace), prompts for a phone
//! number and the code Telegram sends to it, and saves a local SQLite
//! session file at `LES_TELEGRAM_SESSION_FILE`. `TelegramSource` (the real,
//! long-running adapter) only ever *opens* that file — it never performs
//! this interactive flow itself, since a GUI app or headless worker has
//! nowhere to put a phone-number/code prompt.
//!
//! This tool never prints message text or channel content — it only proves
//! login succeeded by naming the account it signed in as.

#[cfg(not(feature = "live"))]
fn main() {
    eprintln!("build with --features live");
}

#[cfg(feature = "live")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use std::sync::Arc;

    use grammers_client::{Client, SignInError};
    use grammers_mtsender::SenderPool;
    use grammers_session::storages::SqliteSession;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let _ = dotenvy::dotenv();

    let api_id: i32 = std::env::var("TELEGRAM_API_ID")
        .expect("TELEGRAM_API_ID not set (see .env.example)")
        .trim()
        .parse()
        .expect("TELEGRAM_API_ID must be an integer");
    let api_hash =
        std::env::var("TELEGRAM_API_HASH").expect("TELEGRAM_API_HASH not set (see .env.example)");
    let session_path = std::env::var("LES_TELEGRAM_SESSION_FILE")
        .expect("LES_TELEGRAM_SESSION_FILE not set (see .env.example)");

    let session = Arc::new(
        SqliteSession::open(&session_path)
            .await
            .unwrap_or_else(|e| panic!("opening session file `{session_path}`: {e}")),
    );
    let SenderPool { runner, handle, .. } = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(handle);
    let _runner = tokio::spawn(runner.run());

    if client.is_authorized().await.expect("checking session") {
        println!("already logged in — session `{session_path}` is ready to use.");
        return;
    }

    println!("Logging in to Telegram. This account will be used read-only,");
    println!("to read public channel history — it will never post or join anything.");
    let phone = prompt("Phone number (international format, e.g. +15551234567): ");
    let token = client
        .request_login_code(&phone, &api_hash)
        .await
        .expect("requesting login code");
    let code = prompt("Code Telegram just sent you: ");
    match client.sign_in(&token, &code).await {
        Ok(_) => {}
        Err(SignInError::PasswordRequired(password_token)) => {
            let hint = password_token.hint().unwrap_or("none");
            let password = prompt(&format!("Two-factor password (hint: {hint}): "));
            client
                .check_password(password_token, password.trim())
                .await
                .expect("checking 2FA password");
        }
        Err(e) => panic!("sign-in failed: {e}"),
    }

    println!("Signed in. Session saved to `{session_path}`.");
    println!(
        "The live source will now read this file on every poll — nothing further to do here."
    );
}

#[cfg(feature = "live")]
fn prompt(message: &str) -> String {
    use std::io::{self, BufRead as _, Write as _};
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(message.as_bytes()).unwrap();
    stdout.flush().unwrap();
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).unwrap();
    line.trim().to_owned()
}
