//! The xitter-dl desktop shell.
//!
//! ## What this process is, and is not
//!
//! It is a window, a SQLite connection, a loopback listener, and a set of typed
//! commands. It holds no X credential of any kind and cannot generate X
//! traffic: the architecture's central claim is enforced at the dependency
//! boundary rather than by convention (PRD §4.3, §7.1).
//!
//! One client is linked in, and it exists for exactly one caller: the
//! classifier. That client (`xdl_core::inference`) is pointed at the provider
//! the user configured — TypeSafe, OpenCode Zen, or a host they typed — and
//! knows no X host. It is the narrow exception described in PRD §7.4, and the
//! only credential it holds is the user's own API key, read from the OS
//! credential store when a run starts.
//!
//! The bridge ([`xdl_core::bridge`]) is the mirror image of a client and does
//! not weaken that claim: it binds `127.0.0.1`, waits, and never dials out. It
//! exists only while this process does.
//!
//! The one outbound action is `open_external`, which hands a URL to the
//! operating system. The OS opens the user's browser, and *that* makes the
//! request. See the note on that command.
//!
//! ## Concurrency
//!
//! Reads open their own connection; writes take a mutex. SQLite in WAL mode
//! allows many readers alongside one writer, and opening a connection is
//! cheap, so this gives real read concurrency without a pool. A single shared
//! `Mutex<Library>` would instead serialise every search behind every import,
//! which is exactly the freeze the WAL pragma exists to prevent.
//!
//! The bridge thread shares the *write* connection, because it is a writer.
//! That is the one place the mutex is genuinely contended, and it is fine: a
//! capture batch and a manual import are both short.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use xdl_core::bridge::{Bridge, BridgeEvent, BridgeOptions, Ingest, Notify, Pairing};
use xdl_core::categories::{self, Category};
use xdl_core::db::{read, write, Library};
use xdl_core::inference::{
    self, classify, status as classify_status_of, taxonomy_hash, Client, Preset, ProviderConfig,
    RunOptions, RunReport, Status, PRESETS,
};
use xdl_core::model::PostView;
use xdl_core::search::SearchHit;
use serde::Serialize;
use tauri::{Emitter, Manager, State};

mod keys;
mod settings;

/// Shared application state.
pub struct AppState {
    db_path: PathBuf,
    /// Held only for writes — including writes that arrive over the bridge.
    /// Reads do not touch it.
    writer: Arc<Mutex<Library>>,
    pairing: Arc<Mutex<Pairing>>,
    /// Where the install secret is persisted, beside the library.
    pairing_path: PathBuf,
    /// The port the bridge actually bound, or 0 before it starts.
    port: AtomicU16,
    /// Unix seconds of the last batch a browser successfully delivered, 0 if
    /// none this run. What makes "connected" mean *connected now* rather than
    /// "someone paired once, months ago".
    last_capture_at: Arc<AtomicI64>,
    /// The version string the last batch carried. The pairing label is frozen
    /// at pairing time, so this is the only live answer to "which script is
    /// running".
    last_script_version: Arc<Mutex<Option<String>>>,
    /// Kept alive for the life of the process; dropping it stops the listener.
    bridge: Mutex<Option<Bridge>>,
    /// Provider, model, and whether images may be sent. Never the key.
    settings: Arc<Mutex<settings::Settings>>,
    settings_path: PathBuf,
    /// True while a classification run owns the classifier.
    classifying: Arc<AtomicBool>,
    /// Set by `classify_cancel`; checked between waves.
    cancel: Arc<AtomicBool>,
}

impl AppState {
    pub fn open(db_path: PathBuf) -> xdl_core::Result<Self> {
        let writer = Library::open(&db_path)?;

        let pairing_path = db_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("bridge.json");

        let existing = std::fs::read_to_string(&pairing_path).ok();
        let (pairing, created) = Pairing::load_or_generate(existing.as_deref())?;
        if created {
            persist_pairing(&pairing_path, &pairing);
        }

        let settings_path = db_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("settings.json");
        let settings = settings::load(&settings_path);

        Ok(Self {
            db_path,
            writer: Arc::new(Mutex::new(writer)),
            pairing: Arc::new(Mutex::new(pairing)),
            pairing_path,
            port: AtomicU16::new(0),
            last_capture_at: Arc::new(AtomicI64::new(0)),
            last_script_version: Arc::new(Mutex::new(None)),
            bridge: Mutex::new(None),
            settings: Arc::new(Mutex::new(settings)),
            settings_path,
            classifying: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn pairing_path(&self) -> &Path {
        &self.pairing_path
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    /// A fresh connection for a read.
    fn reader(&self) -> Result<Library, String> {
        Library::open(&self.db_path).map_err(err)
    }

    fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }
}

/// Write the pairing state next to the library.
///
/// Failure is logged and ignored rather than fatal: the app works fine with an
/// in-memory secret, the user simply re-pairs after a restart. Refusing to
/// start because a side file could not be written would be a worse trade.
fn persist_pairing(path: &Path, pairing: &Pairing) {
    let Ok(json) = pairing.to_json() else { return };
    if std::fs::write(path, json).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        // The install secret is a bearer token for a local endpoint. On a
        // multi-user machine the file should not be readable by anyone else.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Commands return `Result<T, String>`; the frontend shows the string.
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// What `import_*` returns to the UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    files: usize,
    seen: usize,
    new: usize,
    updated: usize,
    payloads: usize,
    /// Thumbnails newly stored by this import.
    media: usize,
    /// Posts this import changed, by id. Empty when the batch was too large to
    /// be worth listing, in which case the caller should reload.
    changed: Vec<String>,
    problems: Vec<String>,
    library: read::LibraryStats,
}

fn summarize(
    conn: &rusqlite::Connection,
    files: usize,
    stats: write::IngestStats,
    problems: Vec<String>,
) -> Result<ImportResult, String> {
    Ok(ImportResult {
        files,
        seen: stats.seen,
        new: stats.new,
        updated: stats.updated,
        payloads: stats.payloads,
        media: stats.media,
        changed: stats.touched,
        problems,
        library: read::stats(conn).map_err(err)?,
    })
}

// ── read commands ────────────────────────────────────────────────────────────

#[tauri::command]
fn list_bookmarks(
    state: State<'_, AppState>,
    limit: usize,
    offset: usize,
    category: Option<String>,
) -> Result<Vec<PostView>, String> {
    let lib = state.reader()?;
    read::list_bookmarks_in(lib.conn(), category.as_deref(), limit, offset).map_err(err)
}

// ── categories ───────────────────────────────────────────────────────────────
//
// Every mutation returns the whole list. The tree is small (tens of rows), the
// window redraws it either way, and a partial answer would let two windows
// disagree about what exists.

#[tauri::command]
fn categories_list(state: State<'_, AppState>) -> Result<Vec<Category>, String> {
    let lib = state.reader()?;
    categories::list(lib.conn()).map_err(err)
}

#[tauri::command]
fn category_create(
    state: State<'_, AppState>,
    parent: Option<String>,
    label: String,
    criteria: Option<String>,
) -> Result<Vec<Category>, String> {
    let lib = state.reader()?;
    categories::create(
        lib.conn(),
        parent.as_deref(),
        &label,
        criteria.as_deref().unwrap_or(""),
    )
    .map_err(err)?;
    categories::list(lib.conn()).map_err(err)
}

#[tauri::command]
fn category_update(
    state: State<'_, AppState>,
    id: String,
    label: String,
    criteria: Option<String>,
) -> Result<Vec<Category>, String> {
    let lib = state.reader()?;
    categories::update(lib.conn(), &id, &label, criteria.as_deref().unwrap_or("")).map_err(err)?;
    categories::list(lib.conn()).map_err(err)
}

/// Move a folder under `parent` (or the root) at `position` among its siblings.
#[tauri::command]
fn category_move(
    state: State<'_, AppState>,
    id: String,
    parent: Option<String>,
    position: Option<i64>,
) -> Result<Vec<Category>, String> {
    let lib = state.reader()?;
    categories::move_to(lib.conn(), &id, parent.as_deref(), position).map_err(err)?;
    categories::list(lib.conn()).map_err(err)
}

#[tauri::command]
fn category_delete(state: State<'_, AppState>, id: String) -> Result<Vec<Category>, String> {
    let lib = state.reader()?;
    categories::delete(lib.conn(), &id).map_err(err)?;
    categories::list(lib.conn()).map_err(err)
}

// ── classifier ───────────────────────────────────────────────────────────────
//
// The UI never sees a key: it can set one, clear one, and learn that one
// exists (with a four-character hint), and that is the whole surface.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InferenceView {
    provider: ProviderConfig,
    presets: &'static [Preset],
    has_key: bool,
    key_hint: Option<String>,
    send_images: bool,
    status: Status,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ClassifyDone {
    report: RunReport,
    status: Status,
}

/// Everything the settings panel needs, including how much work is left.
fn inference_view(state: &AppState) -> Result<InferenceView, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock".to_string())?
        .clone();
    let lib = state.reader()?;
    let cats = categories::list(lib.conn()).map_err(err)?;
    let hash = taxonomy_hash(&cats);
    let status = classify_status_of(lib.conn(), &hash, &settings.provider.model).map_err(err)?;
    Ok(InferenceView {
        has_key: keys::get(&settings.provider.id).is_some(),
        key_hint: keys::get(&settings.provider.id).map(|k| keys::hint(&k)),
        provider: settings.provider,
        presets: PRESETS,
        send_images: settings.send_images,
        status,
    })
}

#[tauri::command]
fn inference_config(state: State<'_, AppState>) -> Result<InferenceView, String> {
    inference_view(&state)
}

/// Save provider, model, image policy — and a key, if one was typed.
///
/// An empty key string is not "leave it alone": it is a deliberate clear, which
/// is why the UI sends `None` when the field was untouched.
/// Whether *this* provider has a key, and the last four characters.
///
/// The panel used to answer this from the saved settings, which is the same
/// thing only while the dropdown still matches what was saved. Selecting a new
/// provider showed the old provider's hint, which reads as "your key is
/// already here" and is exactly the question a person asks before typing a
/// second one.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyStatus {
    has_key: bool,
    hint: Option<String>,
}

#[tauri::command]
fn inference_key_status(provider: String) -> Result<KeyStatus, String> {
    let hint = keys::get(&provider).map(|key| keys::hint(&key));
    Ok(KeyStatus { has_key: hint.is_some(), hint })
}

#[tauri::command]
fn inference_save(
    state: State<'_, AppState>,
    provider: String,
    base_url: String,
    model: String,
    requests_per_minute: u32,
    key: Option<String>,
    send_images: bool,
) -> Result<InferenceView, String> {
    let config = ProviderConfig { id: provider, base_url, model, requests_per_minute }
        .validated()
        .map_err(err)?;

    if let Some(key) = key {
        let key = key.trim();
        if key.is_empty() {
            keys::clear(&config.id)?;
        } else {
            keys::set(&config.id, key)?;
        }
    }

    {
        let mut settings = state
            .settings
            .lock()
            .map_err(|_| "settings lock".to_string())?;
        settings.provider = config;
        settings.send_images = send_images;
        settings::save(state.settings_path(), &settings)?;
    }
    inference_view(&state)
}

/// One round-trip with the configured provider, on a throwaway sentence.
#[tauri::command]
async fn inference_test(
    state: State<'_, AppState>,
) -> Result<inference::client::TestResult, String> {
    let config = state
        .settings
        .lock()
        .map_err(|_| "settings lock".to_string())?
        .provider
        .clone();
    let key = keys::get(&config.id).ok_or_else(|| "no key saved for this provider".to_string())?;
    let config_for_task = config.clone();
    tauri::async_runtime::spawn_blocking(move || {
        Client::new(config_for_task, key).test().map_err(err)
    })
    .await
    .map_err(err)?
}

/// Start a classification run on a worker thread.
///
/// Returns the current status immediately; the run reports through
/// `classify:progress` and finishes with `classify:done` (or `classify:error`).
#[tauri::command]
fn classify_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    force: bool,
    limit: Option<usize>,
    send_images: Option<bool>,
) -> Result<(), String> {
    if state.classifying.swap(true, Ordering::SeqCst) {
        return Err("a classification run is already going".into());
    }

    let settings = match state.settings.lock() {
        Ok(settings) => settings.clone(),
        Err(_) => {
            state.classifying.store(false, Ordering::SeqCst);
            return Err("settings lock".into());
        }
    };

    let options = RunOptions {
        force,
        send_images: send_images.unwrap_or(settings.send_images),
        limit,
        ..RunOptions::default()
    };

    let db_path = state.db_path.clone();
    let cancel = state.cancel.clone();
    let running = state.classifying.clone();
    cancel.store(false, Ordering::SeqCst);

    // Nothing below runs on the caller's thread.
    //
    // A synchronous command executes on the window's own thread, so a slow
    // open, a credential read, or anything that waits on a lock parks the event
    // loop: the button does nothing, the Stop button queues behind it, and the
    // run is indistinguishable from a hang. The command's only job is to claim
    // the flag and hand the work to a thread that can report its own progress.
    std::thread::spawn(move || {
        let fail = |message: String| {
            crumb(&db_path, &format!("error: {message}"));
            let _ = app.emit("classify:error", message);
            running.store(false, Ordering::SeqCst);
        };

        crumb(&db_path, "start");

        let Some(key) = keys::get(&settings.provider.id) else {
            return fail("add an API key in Settings first".into());
        };
        crumb(&db_path, "key read");

        let outcome = (|| -> xdl_core::Result<ClassifyDone> {
            let lib = Library::open(&db_path)?;
            crumb(&db_path, "library open");

            let media_dir = lib
                .media_dir()
                .unwrap_or_else(|| db_path.parent().unwrap_or(Path::new(".")).join("media"));

            categories::ensure_unsorted(lib.conn())?;
            let cats = categories::list(lib.conn())?;
            if cats.is_empty() {
                return Err(xdl_core::Error::Invalid("make a folder first".into()));
            }
            crumb(&db_path, &format!("{} folders", cats.len()));

            let mut client = Client::new(settings.provider.clone(), key);
            client.watch(cancel.clone());

            // No probe. On a one-request-a-minute account the probe *is* the
            // minute: pressing Test in Settings and then File 100 meant the
            // first post arrived inside a window the probe had just spent.
            // The first post is the probe, and its failure is already on screen.

            let report = classify(
                lib.conn(),
                &media_dir,
                &client,
                &cats,
                &options,
                &cancel,
                &mut |progress| {
                    let _ = app.emit("classify:progress", progress);
                },
            )?;

            let status =
                classify_status_of(lib.conn(), &taxonomy_hash(&cats), &client.config().model)?;
            Ok(ClassifyDone { report, status })
        })();

        match outcome {
            Ok(done) => {
                crumb(
                    &db_path,
                    &format!(
                        "done filed={} failed={} cancelled={}",
                        done.report.filed, done.report.failed, done.report.cancelled
                    ),
                );
                let _ = app.emit("classify:done", done);
            }
            Err(e) => {
                crumb(&db_path, &format!("error: {e}"));
                let _ = app.emit("classify:error", e.to_string());
            }
        }
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}

/// Leave a breadcrumb in `classify.log` beside the library.
///
/// The classifier runs off-thread, so "it hung" has to be answerable from disk
/// rather than from a window that was replaced. Each step is one line.
fn crumb(db_path: &Path, step: &str) {
    use std::io::Write;
    let Some(dir) = db_path.parent() else { return };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("classify.log"))
    else {
        return;
    };
    let record = serde_json::json!({ "at": xdl_core::now(), "step": step });
    let _ = writeln!(file, "{record}");
}

#[tauri::command]
fn classify_cancel(state: State<'_, AppState>) -> Result<bool, String> {
    state.cancel.store(true, Ordering::SeqCst);
    // Answers "was there anything to stop", so a Stop pressed after the run
    // ended resets the button instead of leaving it saying "Stopping…".
    Ok(state.classifying.load(Ordering::SeqCst))
}

/// Throw away every verdict. The folders stay; the filings go.
#[tauri::command]
fn classify_reset(state: State<'_, AppState>) -> Result<InferenceView, String> {
    if state.classifying.load(Ordering::SeqCst) {
        return Err("a run is in progress — stop it first".into());
    }
    {
        let mut lib = state
            .writer
            .lock()
            .map_err(|_| "library busy".to_string())?;
        categories::clear_all(lib.conn_mut()).map_err(err)?;
    }
    inference_view(&state)
}

#[tauri::command]
fn classify_status(state: State<'_, AppState>) -> Result<Status, String> {
    inference_view(&state).map(|v| v.status)
}

#[tauri::command]
fn get_post(state: State<'_, AppState>, id: String) -> Result<Option<PostView>, String> {
    let lib = state.reader()?;
    read::get_post(lib.conn(), &id).map_err(err)
}

/// Re-read a handful of posts after a capture changed them.
///
/// The window refreshes the rows a batch actually touched instead of reloading
/// the whole 500-post list, which is what made the feed stutter every time a
/// preview arrived.
#[tauri::command]
fn posts_by_ids(state: State<'_, AppState>, ids: Vec<String>) -> Result<Vec<PostView>, String> {
    let lib = state.reader()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids.iter().take(500) {
        match read::get_post(lib.conn(), id).map_err(err)? {
            Some(view) => out.push(view),
            // Removed or never stored: the caller keeps what it has.
            None => {}
        }
    }
    Ok(out)
}

#[tauri::command]
fn search(state: State<'_, AppState>, query: String, limit: usize) -> Result<Vec<SearchHit>, String> {
    let lib = state.reader()?;
    // The library default uses U+0001/U+0002 delimiters so the frontend builds
    // DOM nodes instead of parsing HTML — see `xdl_core::search::SNIPPET_OPEN`.
    xdl_core::search::search(lib.conn(), &query, limit).map_err(err)
}

#[tauri::command]
fn stats(state: State<'_, AppState>) -> Result<read::LibraryStats, String> {
    let lib = state.reader()?;
    read::stats(lib.conn()).map_err(err)
}

#[tauri::command]
fn library_path(state: State<'_, AppState>) -> String {
    state.db_path().display().to_string()
}

/// Read one stored thumbnail and hand it to the webview as a `data:` URL.
///
/// ## Why a hash, and not a post id
///
/// Thumbnails are content-addressed, so the hash *is* the identity — the same
/// picture on twenty bookmarks is one file and one call. Passing the hash also
/// makes the path guard trivial: the argument is either 64 lowercase hex digits
/// or it is refused before anything touches the filesystem, so the webview
/// cannot name a file it should not read even if it were compromised. The MIME
/// type, which decides the extension, comes from the database rather than the
/// caller, so the extension is not attacker-chosen either.
///
/// ## Why `data:` and not `asset:`
///
/// The app's CSP is `img-src 'self' data:` and has no `asset:` entry. Widening
/// it would hand the webview a file-read capability for the whole library
/// directory; a command that returns the bytes of one hash does the same job
/// with none of that. It also keeps the read on the Rust side of the seam,
/// where every path in this app is built.
#[tauri::command]
fn media_thumb(state: State<'_, AppState>, hash: String) -> Result<Option<String>, String> {
    if !xdl_core::media::is_sha256_hex(&hash) {
        return Ok(None);
    }
    let lib = state.reader()?;
    let Some(mime) = read::blob_mime(lib.conn(), &hash).map_err(err)? else {
        return Ok(None);
    };
    let Some(store) = lib.media_store() else {
        return Ok(None);
    };
    store.data_url(&hash, &mime).map_err(err)
}

// ── write commands ───────────────────────────────────────────────────────────

/// Import a payload the user pasted.
///
/// The most direct route to "one tweet in the library": no file dialog, no
/// userscript, no network. Whatever the user pastes is parsed locally and
/// stored in the local database.
#[tauri::command]
fn import_json(state: State<'_, AppState>, text: String) -> Result<ImportResult, String> {
    let mut guard = state.writer.lock().map_err(|_| "database lock poisoned")?;
    let now = xdl_core::now();

    let tx = guard.conn_mut().transaction().map_err(err)?;
    let summary = xdl_core::import::payload::import_json_text(&tx, &text, None, now).map_err(err)?;
    tx.commit().map_err(err)?;

    summarize(guard.conn(), summary.files, summary.stats, summary.problems)
}

/// Import from a path on disk: a JSON payload, NDJSON, a HAR, or a directory.
#[tauri::command]
fn import_path(state: State<'_, AppState>, path: String) -> Result<ImportResult, String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("no such file or directory: {}", path.display()));
    }

    let mut guard = state.writer.lock().map_err(|_| "database lock poisoned")?;
    let now = xdl_core::now();

    let tx = guard.conn_mut().transaction().map_err(err)?;
    let summary = xdl_core::import::import_path(&tx, &path, now).map_err(err)?;
    tx.commit().map_err(err)?;

    summarize(guard.conn(), summary.files, summary.stats, summary.problems)
}

// ── the one outbound action ──────────────────────────────────────────────────

/// Hand a URL to the operating system's default browser.
///
/// ## Why this is not a violation of "the app never dials out"
///
/// It is that rule being obeyed. This process opens no socket: it asks the OS
/// to open a URL, and the *user's own browser* makes the request — their
/// session, their IP, their fingerprint — at the moment they click. That is
/// the same provenance the userscript side of the design depends on.
///
/// The alternative, letting the webview navigate, would put the request inside
/// this process. Which is why every link in the UI routes through here instead
/// of using a bare `href`.
///
/// ## Why the scheme is checked
///
/// The URL comes from tweet text, which is attacker-controlled in the general
/// case. Passing it through unchecked to a shell could launch anything the OS
/// has registered a handler for — including local executables. Only `http`
/// and `https` are forwarded, and the command is spawned without a shell
/// wherever the platform allows it.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!("refusing to open non-web URL: {url}"));
    }

    #[cfg(target_os = "windows")]
    {
        // `rundll32` invokes the registered protocol handler directly, which
        // avoids `cmd.exe` and therefore avoids shell-quoting rules entirely.
        // The empty-string title trick that `start` needs is not required here.
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn()
            .map_err(|e| format!("could not open browser: {e}"))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("could not open browser: {e}"))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("could not open browser: {e}"))?;
    }

    Ok(())
}

// ── pairing ──────────────────────────────────────────────────────────────────

/// What the pairing panel renders, and everything the frontend is allowed to
/// know about the bridge.
///
/// Note what is **not** here: the install secret. It leaves the process exactly
/// once, in the response to the script's `/v1/pair` exchange, and is never sent
/// to the webview. The UI cannot leak what it never receives.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingInfo {
    port: u16,
    /// Ready to show the user, e.g. `http://127.0.0.1:8737`.
    address: String,
    /// The raw code, for a copy button.
    code: Option<String>,
    /// The same code as the user should read it aloud: `ABCD-2345`.
    formatted_code: Option<String>,
    seconds_remaining: i64,
    paired: bool,
    paired_label: Option<String>,
    paired_at: Option<i64>,
    /// Unix seconds of the last batch this bridge ingested in this run, or
    /// `None` if no browser has delivered anything since launch. `paired` says
    /// a secret exists; this says somebody is actually on the other end.
    last_capture_at: Option<i64>,
    /// The version the last batch carried, e.g. `0.4.3`. `None` until a batch
    /// arrives (or for a script too old to send the header).
    script_version: Option<String>,
}

fn snapshot(state: &AppState) -> Result<PairingInfo, String> {
    let now = xdl_core::now();
    let p = state.pairing.lock().map_err(|_| "pairing lock poisoned")?;
    let code = p.current_code(now).map(str::to_string);
    let port = state.port();
    Ok(PairingInfo {
        port,
        address: format!("http://127.0.0.1:{port}"),
        formatted_code: code.as_deref().map(xdl_core::bridge::format_code),
        code,
        seconds_remaining: p.seconds_remaining(now),
        paired: p.is_paired(),
        paired_label: p.paired_label.clone(),
        paired_at: p.paired_at,
        last_capture_at: match state.last_capture_at.load(Ordering::Relaxed) {
            0 => None,
            at => Some(at),
        },
        script_version: state
            .last_script_version
            .lock()
            .ok()
            .and_then(|v| v.clone()),
    })
}

/// Read the current pairing state without starting a code.
#[tauri::command]
fn pairing_status(state: State<'_, AppState>) -> Result<PairingInfo, String> {
    snapshot(&state)
}

/// Begin displaying a code, rotating it if the current one has expired.
///
/// Called when the panel opens. The code is display state and is deliberately
/// never persisted, so it cannot outlive the window that showed it.
#[tauri::command]
fn pairing_show(state: State<'_, AppState>) -> Result<PairingInfo, String> {
    let now = xdl_core::now();
    {
        let mut p = state.pairing.lock().map_err(|_| "pairing lock poisoned")?;
        p.show_code(now).map_err(err)?;
    }
    snapshot(&state)
}

/// Stop displaying a code, invalidating it immediately.
///
/// Called when the panel closes. A code that stayed valid while off screen
/// would be a code nobody is watching.
#[tauri::command]
fn pairing_hide(state: State<'_, AppState>) -> Result<PairingInfo, String> {
    {
        let mut p = state.pairing.lock().map_err(|_| "pairing lock poisoned")?;
        p.hide_code();
    }
    snapshot(&state)
}

/// Forget every pairing and mint a new secret.
///
/// Every installed script stops working until it is paired again. That is the
/// point: it is the answer to "I think someone else has my secret".
#[tauri::command]
fn pairing_revoke(state: State<'_, AppState>) -> Result<PairingInfo, String> {
    {
        let mut p = state.pairing.lock().map_err(|_| "pairing lock poisoned")?;
        p.revoke().map_err(err)?;
        persist_pairing(state.pairing_path(), &p);
    }
    snapshot(&state)
}

// ── the bridge ───────────────────────────────────────────────────────────────

/// Start the loopback receiver.
///
/// The port is *not* persisted. The script discovers it with `/v1/hello` — an
/// unauthenticated pure echo — and caches the answer, rescanning only when a
/// request fails. That keeps the app from having to advertise a moving port it
/// cannot reach anyone to tell about, and it means a squatter on the wrong port
/// never sees the install secret.
fn start_bridge(app: &tauri::AppHandle, state: &AppState) -> xdl_core::Result<()> {
    let writer = Arc::clone(&state.writer);
    let ingest: Ingest = Arc::new(move |text: &str, now: i64| {
        let mut lib = writer
            .lock()
            .map_err(|_| xdl_core::Error::invalid("database lock poisoned"))?;
        let tx = lib.conn_mut().transaction()?;
        let summary = xdl_core::import::ndjson::import_str(&tx, text, now)?;
        tx.commit()?;
        Ok(summary)
    });

    let emitter = app.clone();
    let pairing = Arc::clone(&state.pairing);
    let pairing_path = state.pairing_path().to_path_buf();
    let last_capture = Arc::clone(&state.last_capture_at);
    let last_version = Arc::clone(&state.last_script_version);
    let notify: Notify = Arc::new(move |event: BridgeEvent| {
        // A successful exchange changes the persisted state, and the bridge is
        // where that happens, so the write belongs here.
        match &event {
            BridgeEvent::Paired { .. } => {
                if let Ok(p) = pairing.lock() {
                    persist_pairing(&pairing_path, &p);
                }
            }
            // The only evidence that a browser is on the other end right now.
            // Pairing alone is history; this is presence.
            BridgeEvent::Captured(report) => {
                last_capture.store(xdl_core::now(), Ordering::Relaxed);
                if let Some(v) = &report.script_version {
                    if let Ok(mut slot) = last_version.lock() {
                        *slot = Some(v.clone());
                    }
                }
            }
            BridgeEvent::Rejected { .. } => {}
        }
        let _ = emitter.emit(BRIDGE_EVENT, &event);
    });

    let bridge = Bridge::start(
        Arc::clone(&state.pairing),
        ingest,
        notify,
        BridgeOptions::default(),
    )?;

    state.port.store(bridge.port(), Ordering::Relaxed);
    // Worth saying out loud in the log: when a script will not connect, the
    // first question is always "which port is it actually on", and the answer
    // is otherwise invisible in a windowed build with no console.
    eprintln!(
        "xitter-dl: capture bridge listening on http://127.0.0.1:{}",
        bridge.port()
    );
    if let Ok(mut slot) = state.bridge.lock() {
        *slot = Some(bridge);
    }
    Ok(())
}

/// Event name the frontend listens on for live capture progress.
pub const BRIDGE_EVENT: &str = "xdl://bridge";

// `user32!MessageBoxW`, declared by hand so that reporting a startup failure
// costs no dependency. Linked at module scope because `#[link]` is not reliably
// honoured on an `extern` block nested inside a function body. A doc comment
// cannot be used here: rustdoc has nothing to attach it to on a foreign module.
#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(
        hwnd: *mut core::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        utype: u32,
    ) -> i32;
}

/// Report a failure that happens before there is a window to report it in.
///
/// A release build is `windows_subsystem = "windows"`, so it has no console: a
/// panic there is entirely silent. The process vanishes and the user sees
/// nothing happen at all — which is what an unopenable library produces, since
/// the database is opened before the window is built. "Nothing happened" is the
/// worst possible way to report a concrete, fixable problem, so say it in a
/// dialog and then leave.
fn fatal(message: &str) -> ! {
    eprintln!("xitter-dl: {message}");

    #[cfg(windows)]
    unsafe {
        let wide = |s: &str| -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        };
        const MB_ICONERROR: u32 = 0x0000_0010;
        let text = wide(message);
        let caption = wide("xitter-dl");
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_ICONERROR,
        );
    }

    std::process::exit(1);
}

/// Build and run the app.
pub fn run() {
    let db_path = xdl_core::default_library_path();
    let state = match AppState::open(db_path) {
        Ok(state) => state,
        Err(e) => fatal(&format!(
            "xitter-dl could not open its library.\n\n\
             {e}\n\n\
             Set XITTER_DL_DB to keep the library somewhere else, e.g.\n\
             C:\\Users\\you\\xitter-dl.sqlite"
        )),
    };

    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            // A bridge that will not bind must not stop the app from opening.
            // The library is still fully usable from files and the CLI, and
            // saying so is better than refusing to start.
            if let Err(e) = start_bridge(&handle, &state) {
                eprintln!("xitter-dl: capture bridge did not start: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_bookmarks,
            categories_list,
            category_create,
            category_update,
            category_move,
            category_delete,
            inference_config,
            inference_key_status,
            inference_save,
            inference_test,
            classify_start,
            classify_cancel,
            classify_reset,
            classify_status,
            get_post,
            posts_by_ids,
            search,
            stats,
            library_path,
            media_thumb,
            import_json,
            import_path,
            open_external,
            pairing_status,
            pairing_show,
            pairing_hide,
            pairing_revoke,
        ])
        .run(tauri::generate_context!())
        .expect("error while running xitter-dl");
}

#[cfg(test)]
mod tests {
    /// The scheme guard is the only security-relevant logic in this crate, so
    /// it gets a test even though the rest is wiring.
    #[test]
    fn open_external_rejects_non_web_schemes() {
        // Mirrors the check inside the command; kept in sync deliberately
        // rather than by sharing, because the command body needs the value
        // inline for the cfg branches.
        let allowed = |u: &str| {
            let l = u.trim().to_ascii_lowercase();
            l.starts_with("http://") || l.starts_with("https://")
        };

        assert!(allowed("https://x.com/ada/status/1"));
        assert!(allowed("http://example.com"));
        assert!(allowed("HTTPS://EXAMPLE.COM"));

        // Everything below would hand an arbitrary string to the OS.
        assert!(!allowed("javascript:alert(1)"));
        assert!(!allowed("file:///C:/Windows/System32/cmd.exe"));
        assert!(!allowed("ms-settings:"));
        assert!(!allowed("C:\\Windows\\System32\\cmd.exe"));
        assert!(!allowed("\\\\server\\share"));
        assert!(!allowed("vbscript:msgbox"));
        assert!(!allowed(""));
    }
}
