//! JSON-file [`Session`] storage for the Telegram account session.
//!
//! **Why this exists instead of `grammers_session::storages::SqliteSession`:**
//! that storage is gated behind `grammers-session`'s `sqlite-storage` feature,
//! which pulls `libsql-ffi` — a second statically-vendored copy of SQLite
//! alongside the one `rusqlite`/`libsqlite3-sys` already brings in for
//! `storage`'s settings store. Linking both into `global-signal-desktop`
//! fails with dozens of duplicate `sqlite3_*` symbols (LNK2005). `cargo
//! check`/`cargo clippy` do not link, so nothing but a real `cargo build` of
//! the binary can catch it.
//!
//! Turning that feature off leaves `MemorySession`, which by design persists
//! nothing — and re-logging-in on every start is exactly what Telegram
//! punishes with flood waits. [`Session`] is a small trait, so this file
//! implements it directly over a JSON file instead.
//!
//! `SessionData` carries the whole state and has all-public fields, but it
//! has **no serde derives in 0.10.0** (its component types do, behind the
//! crate's `serde` feature) — hence the hand-written [`PersistedSession`]
//! mirror rather than serializing `SessionData` itself.
//!
//! The file holds a live Telegram login: treat it as a credential. It is
//! covered by `.gitignore`'s `*.session` / `*.session-*` rules.

use std::collections::hash_map::Entry;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use serde::{Deserialize, Serialize};

/// Appended to the session file name for the write-then-rename temp file.
/// Chosen so the temp file matches `.gitignore`'s existing `*.session-*` rule
/// even if a crash leaves one behind.
const TEMP_SUFFIX: &str = "-tmp";

/// A [`Session`] backed by a JSON file, saved whenever the state changes.
pub struct FileSession {
    path: PathBuf,
    data: Mutex<SessionData>,
    /// When set, state changes stay in memory and the file is never written.
    /// See [`FileSession::load_read_only`].
    read_only: bool,
}

#[derive(Debug)]
pub enum FileSessionError {
    Poisoned,
    Io(std::io::Error),
    Format(serde_json::Error),
}

impl std::error::Error for FileSessionError {}

impl fmt::Display for FileSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Poisoned => write!(f, "session lock is poisoned"),
            Self::Io(e) => write!(f, "session file i/o: {e}"),
            Self::Format(e) => write!(f, "session file is not valid session JSON: {e}"),
        }
    }
}

impl FileSession {
    /// Load the session at `path`, or start a fresh one if the file does not
    /// exist yet (which is what the first `login_setup` run does).
    ///
    /// A file that exists but does not parse is an **error**, never a silent
    /// fresh session: quietly discarding a session the user believes is
    /// logged in would send them back through an SMS login without saying
    /// why.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, FileSessionError> {
        Self::open(path.into(), false)
    }

    /// Load the session at `path` but never write it back.
    ///
    /// For a **second** client sharing one login — the desktop's on-demand
    /// media lookup runs alongside the ingest poller, and both would otherwise
    /// hold the same file. Each [`FileSession::save`] writes the *whole* state,
    /// so two writers take turns overwriting each other's peer cache; the
    /// dropped entries come back as extra `resolve_username` calls, which is
    /// exactly what Telegram answers with a flood wait. Making the reader
    /// read-only leaves one writer and removes the race.
    ///
    /// Nothing is lost by it: the auth key and home DC are already on disk
    /// (login wrote them), and neither client runs the update loop, so the only
    /// state this drops is a peer cache that the writer keeps anyway.
    pub fn load_read_only(path: impl Into<PathBuf>) -> Result<Self, FileSessionError> {
        Self::open(path.into(), true)
    }

    fn open(path: PathBuf, read_only: bool) -> Result<Self, FileSessionError> {
        let data = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<PersistedSession>(&text)
                .map_err(FileSessionError::Format)?
                .into(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SessionData::default(),
            Err(e) => return Err(FileSessionError::Io(e)),
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
            read_only,
        })
    }

    fn data(&self) -> Result<MutexGuard<'_, SessionData>, FileSessionError> {
        self.data.lock().map_err(|_| FileSessionError::Poisoned)
    }

    /// Write the whole state out. Called with the state lock held, so a
    /// concurrent mutation can never interleave a half-written file.
    ///
    /// Writes a sibling temp file and renames it over the target, so an
    /// interrupted write leaves the previous (still valid) session intact
    /// rather than a truncated file that `load` would reject.
    fn save(&self, data: &SessionData) -> Result<(), FileSessionError> {
        if self.read_only {
            return Ok(());
        }
        let json = serde_json::to_string(&PersistedSession::from(data))
            .map_err(FileSessionError::Format)?;
        let temp = temp_path(&self.path);
        std::fs::write(&temp, json).map_err(FileSessionError::Io)?;
        std::fs::rename(&temp, &self.path).map_err(FileSessionError::Io)
    }
}

/// `foo.session` -> `foo.session-tmp` (not `with_extension`, which would
/// replace `.session` and escape the `.gitignore` rules).
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(TEMP_SUFFIX);
    path.with_file_name(name)
}

/// Serializable mirror of [`SessionData`].
///
/// The maps become sequences because their keys are integers and JSON object
/// keys are not — both key types are recoverable from the values themselves
/// (`DcOption::id`, `PeerInfo::id()`).
///
/// The field *types* are upstream's, so the on-disk format is tied to
/// `grammers-session`'s own serde representation: a `grammers-*` bump that
/// changes it invalidates existing session files, and the only recovery is
/// re-running `examples/login_setup.rs`. [`FileSession::load`] surfaces that
/// as a parse error naming the file rather than as a silent logout.
#[derive(Serialize, Deserialize)]
struct PersistedSession {
    home_dc: i32,
    dc_options: Vec<DcOption>,
    peer_infos: Vec<PeerInfo>,
    updates_state: UpdatesState,
}

impl From<&SessionData> for PersistedSession {
    fn from(data: &SessionData) -> Self {
        Self {
            home_dc: data.home_dc,
            dc_options: data.dc_options.values().cloned().collect(),
            peer_infos: data.peer_infos.values().cloned().collect(),
            updates_state: data.updates_state.clone(),
        }
    }
}

impl From<PersistedSession> for SessionData {
    fn from(persisted: PersistedSession) -> Self {
        Self {
            home_dc: persisted.home_dc,
            dc_options: persisted
                .dc_options
                .into_iter()
                .map(|dc_option| (dc_option.id, dc_option))
                .collect(),
            peer_infos: persisted
                .peer_infos
                .into_iter()
                .map(|peer| (peer.id(), peer))
                .collect(),
            updates_state: persisted.updates_state,
        }
    }
}

/// Mirrors `MemorySession`'s in-memory semantics exactly (including
/// `cache_peer`'s merge-into-existing via `PeerInfo::extend_info`), adding a
/// file write after any call that actually changed something. The
/// no-change guards matter: the client re-asserts unchanged update state
/// often, and this state is only worth a disk write when it moves.
impl Session for FileSession {
    type Error = FileSessionError;

    fn home_dc_id(&self) -> Result<i32, FileSessionError> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), FileSessionError>> {
        Box::pin(async move {
            let mut data = self.data()?;
            if data.home_dc == dc_id {
                return Ok(());
            }
            data.home_dc = dc_id;
            self.save(&data)
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, FileSessionError> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), FileSessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let mut data = self.data()?;
            if data.dc_options.get(&dc_option.id) == Some(&dc_option) {
                return Ok(());
            }
            data.dc_options.insert(dc_option.id, dc_option);
            self.save(&data)
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, FileSessionError>> {
        Box::pin(async move { Ok(self.data()?.peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), FileSessionError>> {
        let peer = peer.clone();
        Box::pin(async move {
            let mut data = self.data()?;
            // `extend_info` reports whether the peers matched in type and id,
            // not whether any field moved — re-caching an already-complete
            // peer returns `true`. Compare instead, so the common case (every
            // channel re-resolved on every sweep) does not rewrite the file.
            let changed = match data.peer_infos.entry(peer.id()) {
                Entry::Occupied(mut existing) => {
                    let before = existing.get().clone();
                    existing.get_mut().extend_info(&peer);
                    *existing.get() != before
                }
                Entry::Vacant(slot) => {
                    slot.insert(peer);
                    true
                }
            };
            if changed { self.save(&data) } else { Ok(()) }
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, FileSessionError>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), FileSessionError>> {
        Box::pin(async move {
            let mut data = self.data()?;
            let before = data.updates_state.clone();

            match update {
                UpdateState::All(updates_state) => {
                    data.updates_state = updates_state;
                }
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => {
                    data.updates_state.qts = qts;
                }
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|c| c.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }

            if data.updates_state == before {
                return Ok(());
            }
            self.save(&data)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddrV4, SocketAddrV6};

    use grammers_session::types::PeerId;

    use super::*;

    const HOME_DC: i32 = 4;
    const OTHER_DC: i32 = 2;
    const AUTH_KEY_BYTE: u8 = 0xab;
    const CHANNEL_ID: i64 = 1_234_567;
    const CHANNEL_PTS: i32 = 99;
    const PRIMARY_PTS: i32 = 17;
    const PRIMARY_DATE: i32 = 1_700_000_000;
    const PRIMARY_SEQ: i32 = 3;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "les-telegram-session-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dc_option(id: i32, auth_key: Option<[u8; 256]>) -> DcOption {
        DcOption {
            id,
            ipv4: SocketAddrV4::new([149, 154, 167, 51].into(), 443),
            ipv6: SocketAddrV6::new([0; 16].into(), 443, 0, 0),
            auth_key,
        }
    }

    /// The whole point of persisting: an auth key survives a reload, so a
    /// restart does not mean another SMS login.
    #[tokio::test]
    async fn auth_key_and_home_dc_survive_a_reload() {
        let path = temp_dir().join("roundtrip.session");
        let session = FileSession::load(&path).unwrap();
        session.set_home_dc_id(HOME_DC).await.unwrap();
        session
            .set_dc_option(&dc_option(HOME_DC, Some([AUTH_KEY_BYTE; 256])))
            .await
            .unwrap();

        let reloaded = FileSession::load(&path).unwrap();
        assert_eq!(reloaded.home_dc_id().unwrap(), HOME_DC);
        assert_eq!(
            reloaded.dc_option(HOME_DC).unwrap().unwrap().auth_key,
            Some([AUTH_KEY_BYTE; 256])
        );
        // Statically-known options are still there alongside the saved one.
        assert!(reloaded.dc_option(OTHER_DC).unwrap().is_some());
    }

    #[tokio::test]
    async fn update_state_survives_a_reload() {
        let path = temp_dir().join("updates.session");
        let session = FileSession::load(&path).unwrap();
        session
            .set_update_state(UpdateState::Primary {
                pts: PRIMARY_PTS,
                date: PRIMARY_DATE,
                seq: PRIMARY_SEQ,
            })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Channel {
                id: CHANNEL_ID,
                pts: CHANNEL_PTS,
            })
            .await
            .unwrap();

        let state = FileSession::load(&path)
            .unwrap()
            .updates_state()
            .await
            .unwrap();
        assert_eq!(state.pts, PRIMARY_PTS);
        assert_eq!(state.date, PRIMARY_DATE);
        assert_eq!(state.seq, PRIMARY_SEQ);
        assert_eq!(state.channels.len(), 1);
        assert_eq!(state.channels[0].id, CHANNEL_ID);
        assert_eq!(state.channels[0].pts, CHANNEL_PTS);
    }

    #[tokio::test]
    async fn cached_peers_survive_a_reload() {
        let path = temp_dir().join("peers.session");
        let peer = PeerInfo::Channel {
            id: CHANNEL_ID,
            auth: None,
            kind: None,
        };
        let session = FileSession::load(&path).unwrap();
        session.cache_peer(&peer).await.unwrap();

        let reloaded = FileSession::load(&path).unwrap();
        assert_eq!(
            reloaded
                .peer(PeerId::channel(CHANNEL_ID).unwrap())
                .await
                .unwrap(),
            Some(peer)
        );
    }

    /// A missing file is a fresh session; a present-but-unparseable one is an
    /// error, so a stale/foreign file is never silently treated as a logout.
    #[test]
    fn missing_file_starts_fresh_but_a_corrupt_one_errors() {
        let dir = temp_dir();
        let missing = dir.join("absent.session");
        assert!(FileSession::load(&missing).is_ok());
        assert!(!missing.exists(), "load must not create the file");

        let corrupt = dir.join("corrupt.session");
        std::fs::write(&corrupt, b"SQLite format 3\0not json").unwrap();
        assert!(matches!(
            FileSession::load(&corrupt),
            Err(FileSessionError::Format(_))
        ));
    }

    /// A read-only session still serves what login wrote, and still tracks
    /// changes in memory — it just never touches the file the writer owns.
    #[tokio::test]
    async fn a_read_only_session_reads_the_file_but_never_writes_it() {
        let path = temp_dir().join("read-only.session");
        let writer = FileSession::load(&path).unwrap();
        writer.set_home_dc_id(HOME_DC).await.unwrap();
        let written = std::fs::read_to_string(&path).unwrap();

        let reader = FileSession::load_read_only(&path).unwrap();
        assert_eq!(reader.home_dc_id().unwrap(), HOME_DC);
        reader.set_home_dc_id(OTHER_DC).await.unwrap();
        assert_eq!(reader.home_dc_id().unwrap(), OTHER_DC);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
    }

    #[test]
    fn temp_path_keeps_the_gitignored_session_suffix() {
        assert_eq!(
            temp_path(Path::new("./telegram.session")),
            PathBuf::from("./telegram.session-tmp")
        );
    }
}
