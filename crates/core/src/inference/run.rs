//! One classification pass over the library.
//!
//! ## Shape of a run
//!
//! Posts are selected in SQL: everything filed under a different taxonomy or a
//! different model, plus anything never filed at all. A run is therefore
//! resumable by construction — a post that failed has no row, so the next run
//! picks it up, and editing one folder's definition re-runs exactly the posts
//! whose verdict was reached under the old definition.
//!
//! Requests go out in waves of [`WAVES_PER_WORKER`] × `workers`, each wave on
//! scoped threads, and the answers are written on the calling thread. SQLite
//! is single-writer, so that split is what keeps four in-flight requests from
//! turning into four connections fighting over a write lock.
//!
//! ## What is stored
//!
//! The verdict, the confidence that came with it, the centrality score, the
//! model that said so and the taxonomy it was answered under. Not the
//! probabilities: they are a distribution over folders that will not exist
//! after the next rename, and the per-folder counts the UI shows do not need
//! them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::categories;
use crate::error::{Error, Result};

use super::{criteria_of, plan, taxonomy_hash, Client, Outcome, Plan};

/// Requests in flight per wave, per worker. Eight is enough to hide latency
/// without piling up a hundred queued posts behind one rate limit.
const WAVES_PER_WORKER: usize = 4;

/// How many free-tier windows a run waits out before it gives up.
///
/// A rate limit is not a failure — the posts are fine, the account is early.
/// Waiting is the difference between a run that files 190 posts and one that
/// files 190 posts and then apologises.
const RATE_STALLS: u32 = 3;
const RATE_PAUSE: Duration = Duration::from_secs(60);
/// The pause is spent in ticks so the panel can count it down — and so Stop
/// works while the run is waiting, which is when a person is most likely to
/// press it.
const PAUSE_TICK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Re-run posts whose verdict is already current.
    pub force: bool,
    /// Send image bytes for image posts. Off makes every post text-only.
    pub send_images: bool,
    pub workers: usize,
    /// Stop after this many posts; `None` means the whole library.
    pub limit: Option<usize>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self { force: false, send_images: true, workers: 4, limit: None }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    /// Posts a request was made for.
    pub considered: u64,
    /// Posts filed successfully.
    pub filed: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The model that answered, as the provider named it.
    pub model: String,
    /// The first failure, in the provider's words. The rest are counted only;
    /// a run that fails 400 times for one reason should not send 400 sentences.
    pub first_error: Option<String>,
    /// Filed into "Unsorted": the model's way of saying nothing fit.
    pub unsorted: u64,
    /// A rate limit was waited out (once or more) rather than ending the run.
    pub rate_limited: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: u64,
    pub considered: u64,
    pub failed: u64,
    /// The provider's words for the first failure, or what the run is waiting
    /// on. A counter that rises while nothing explains it reads as a freeze.
    pub message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub total: i64,
    pub filed: i64,
    /// Bookmarks with no verdict, or one reached under a folder set or model
    /// that is no longer current.
    pub pending: i64,
    pub last_at: Option<i64>,
}

/// What a run needs to know about the library right now.
pub fn status(conn: &Connection, hash: &str, model: &str) -> Result<Status> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))?;
    let filed: i64 = conn.query_row("SELECT COUNT(*) FROM post_categories", [], |r| r.get(0))?;
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM bookmarks b
           JOIN posts p ON p.id = b.post_id
           LEFT JOIN post_categories pc ON pc.post_id = p.id
          WHERE pc.post_id IS NULL
             OR IFNULL(pc.taxonomy_hash, '') <> ?1
             OR IFNULL(pc.model, '') <> ?2",
        params![hash, model],
        |r| r.get(0),
    )?;
    let last_at: Option<i64> = conn.query_row("SELECT MAX(classified_at) FROM post_categories", [], |r| {
        r.get::<_, Option<i64>>(0)
    })?;
    Ok(Status { total, filed, pending, last_at })
}

struct Candidate {
    id: String,
    text: String,
    quoted: Option<String>,
    image: Option<Image>,
}

struct Image {
    /// Already mapped to JEV's vocabulary: `png`, `jpg`, `jpeg`.
    format: String,
    /// File name inside the library's media directory.
    path: PathBuf,
}

/// Classify every post whose verdict is not current.
pub fn run(
    conn: &Connection,
    media_dir: &Path,
    client: &Client,
    cats: &[crate::categories::Category],
    opts: &RunOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<RunReport> {
    let criteria = criteria_of(cats)?;
    let hash = taxonomy_hash(cats);
    let model = client.config().model.clone();

    let mut by_label: BTreeMap<&str, &str> = BTreeMap::new();
    for cat in cats {
        by_label.insert(cat.label.trim(), cat.id.as_str());
    }

    let mut candidates = select(conn, opts.force, &hash, &model)?;
    if let Some(limit) = opts.limit {
        candidates.truncate(limit);
    }
    let considered = candidates.len() as u64;

    let mut report = RunReport {
        considered,
        model: model.clone(),
        ..RunReport::default()
    };

    // The closure below is `move`, so it must capture a *reference* to the
    // criteria map rather than the map itself; otherwise the first wave takes
    // ownership and the second cannot be built.
    let criteria = &criteria;

    let workers = opts.workers.clamp(1, 16);
    let wave_size = workers * WAVES_PER_WORKER;
    let mut done = 0u64;
    let mut stalls = 0u32;

    for wave in candidates.chunks(wave_size) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }

        let mut wave_filed = 0u64;
        let mut wave_failed = 0u64;

        append_log(
            media_dir,
            &serde_json::json!({ "at": crate::now(), "wave": wave.len(), "begin": true }),
        );

        // Results stream back over a channel and are written on this thread as
        // they arrive. Collecting a whole wave first would make the panel wait
        // for the *last* post of sixteen — which on a one-a-minute account is a
        // quarter of an hour of a bar that has not moved.
        {
            // A write can fail (disk, lock, a folder deleted mid-run). Capture
            // it, stop consuming, and let the scope join before returning it:
            // `?` is not available inside a thread-scope closure.
            let mut write_error: Option<Error> = None;
            let (tx, rx) = std::sync::mpsc::channel::<(String, Result<Outcome>)>();
            std::thread::scope(|scope| {
                for worker in 0..workers {
                    let slice: Vec<&Candidate> =
                        wave.iter().skip(worker).step_by(workers).collect();
                    if slice.is_empty() {
                        continue;
                    }
                    let tx = tx.clone();
                    scope.spawn(move || {
                        for cand in slice {
                            if cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let outcome =
                                classify_one(client, media_dir, cand, &criteria, opts.send_images);
                            if tx.send((cand.id.clone(), outcome)).is_err() {
                                break;
                            }
                        }
                    });
                }
                drop(tx);

                for (post_id, outcome) in rx {
                    done += 1;
                    match outcome {
                        Ok(outcome) => {
                            let Some(category_id) =
                                by_label.get(outcome.choice.as_str()).copied()
                            else {
                                wave_failed += 1;
                                report.failed += 1;
                                report.first_error.get_or_insert_with(|| {
                                    format!(
                                        "the provider chose “{}”, which is not a folder",
                                        outcome.choice
                                    )
                                });
                                progress(&Progress {
                                    done,
                                    considered,
                                    failed: report.failed,
                                    message: report.first_error.clone(),
                                });
                                continue;
                            };
                            if let Err(e) = categories::file(
                                conn,
                                &post_id,
                                category_id,
                                Some(outcome.confidence),
                                outcome.score,
                                &outcome.model,
                                &hash,
                            ) {
                                write_error = Some(e);
                                break;
                            }
                            wave_filed += 1;
                            report.filed += 1;
                            if category_id == categories::UNSORTED_ID {
                                report.unsorted += 1;
                            }
                            report.input_tokens += outcome.input_tokens;
                            report.output_tokens += outcome.output_tokens;
                        }
                        Err(e) => {
                            wave_failed += 1;
                            report.failed += 1;
                            let message = e.to_string();
                            append_log(
                                media_dir,
                                &serde_json::json!({
                                    "at": crate::now(),
                                    "post": post_id,
                                    "error": message.chars().take(300).collect::<String>(),
                                }),
                            );
                            if report.first_error.is_none() {
                                report.first_error = Some(message);
                            }
                        }
                    }
                    progress(&Progress {
                        done,
                        considered,
                        failed: report.failed,
                        message: report.first_error.clone(),
                    });
                }
            });
            if let Some(e) = write_error {
                return Err(e);
            }
        }

        append_log(
            media_dir,
            &serde_json::json!({
                "at": crate::now(),
                "wave": wave.len(),
                "done": done,
                "considered": considered,
                "filed": report.filed,
                "failed": report.failed,
                "error": report.first_error,
            }),
        );

        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }

        if wave_filed == 0 && wave_failed > 0 {
            // A wave that fails completely is a bad key, a dead endpoint, or a
            // rate limit. The first two will not improve by waiting, so they
            // stop the run; the third is exactly what waiting fixes.
            if is_rate_limit(report.first_error.as_deref().unwrap_or("")) && stalls < RATE_STALLS {
                stalls += 1;
                report.rate_limited = true;
                append_log(media_dir, &serde_json::json!({
                    "at": crate::now(),
                    "paused_seconds": RATE_PAUSE.as_secs(),
                    "reason": report.first_error,
                }));
                let reason = report.first_error.clone().unwrap_or_default();
                let mut waited = 0u64;
                while waited < RATE_PAUSE.as_secs() {
                    if cancel.load(Ordering::Relaxed) {
                        report.cancelled = true;
                        break;
                    }
                    let left = RATE_PAUSE.as_secs() - waited;
                    progress(&Progress {
                        done,
                        considered,
                        failed: report.failed,
                        message: Some(if waited == 0 {
                            format!("{reason} — retrying in {left}s")
                        } else {
                            format!("Retrying in {left}s")
                        }),
                    });
                    std::thread::sleep(PAUSE_TICK);
                    waited += PAUSE_TICK.as_secs();
                }
                if report.cancelled {
                    break;
                }
                continue;
            }
            report.cancelled = true;
            break;
        }
    }

    append_log(
        media_dir,
        &serde_json::json!({
            "at": crate::now(),
            "end": true,
            "considered": considered,
            "filed": report.filed,
            "unsorted": report.unsorted,
            "failed": report.failed,
            "rate_limited": report.rate_limited,
            "cancelled": report.cancelled,
            "error": report.first_error,
        }),
    );

    Ok(report)
}

/// True when the provider's sentence is about pace rather than permission.
fn is_rate_limit(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("429")
        || m.contains("rate limit")
        || m.contains("rate-limit")
        || m.contains("too many requests")
        || m.contains("quota")
}

/// Append one line to `classify.log` beside the library.
///
/// Without it, "it stopped after about 280" is a guess: the report lives in a
/// webview that the next click replaces. A log makes the next report a fact.
fn append_log(media_dir: &Path, record: &serde_json::Value) {
    use std::io::Write;
    let Some(dir) = media_dir.parent() else { return };
    if let Ok(mut file) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("classify.log"))
    {
        let _ = writeln!(file, "{record}");
    }
}

fn classify_one(
    client: &Client,
    media_dir: &Path,
    cand: &Candidate,
    criteria: &BTreeMap<String, String>,
    send_images: bool,
) -> Result<Outcome> {
    let plan = plan(&cand.text, cand.quoted.as_deref(), cand.image.is_some(), send_images);
    match &plan {
        Plan::Image { .. } => {
            let Some(image) = &cand.image else {
                return client.classify(&plan, criteria, None);
            };
            let path = media_dir.join(&image.path);
            let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
            client.classify(&plan, criteria, Some((&image.format, &bytes)))
        }
        Plan::Text { .. } => client.classify(&plan, criteria, None),
    }
}

fn select(conn: &Connection, force: bool, hash: &str, model: &str) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.text, q.text, pc.post_id
           FROM bookmarks b
           JOIN posts p ON p.id = b.post_id
           LEFT JOIN posts q ON q.id = p.quoted_id
           LEFT JOIN post_categories pc ON pc.post_id = p.id
          WHERE ?1 = 1
             OR pc.post_id IS NULL
             OR IFNULL(pc.taxonomy_hash, '') <> ?2
             OR IFNULL(pc.model, '') <> ?3
          ORDER BY p.created_at DESC",
    )?;
    let mut rows = stmt.query(params![if force { 1 } else { 0 }, hash, model])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        out.push(Candidate {
            image: image_of(conn, &id)?,
            id,
            text: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            quoted: row.get(2)?,
        });
    }
    Ok(out)
}

/// The preview to send for a post: a photo if it has one, else a video poster.
fn image_of(conn: &Connection, post_id: &str) -> Result<Option<Image>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT mb.blob_hash, mb.mime
               FROM media m
               JOIN media_blobs mb ON mb.url = m.url
              WHERE m.post_id = ?1 AND m.kind IN ('photo', 'video')
              ORDER BY (m.kind = 'photo') DESC, m.position
              LIMIT 1",
            params![post_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let Some((hash, mime)) = row else { return Ok(None) };
    let Some(format) = format_for(&mime) else { return Ok(None) };
    let ext = if format == "png" { "png" } else { "jpg" };
    let path = media_ext(&hash, ext);
    Ok(Some(Image { format: format.to_string(), path }))
}

fn format_for(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        // Anything else (webp, gif) is not a JEV asset format, and sending it
        // as a mislabelled png would put bytes on the wire that the endpoint
        // has to reject. The post goes as text instead.
        _ => None,
    }
}

/// The file name of a stored preview: `<sha256>.<ext>`.
///
/// Relative to the library's media directory, which the caller joins on. The
/// media store names files by content hash and the extension follows the bytes,
/// so the name is derivable from what is already in the database.
fn media_ext(hash: &str, ext: &str) -> PathBuf {
    PathBuf::from(format!("{hash}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_post_whose_verdict_is_current_is_not_selected_again() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::configure(&conn).unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO posts (id, text, first_seen_at, source) VALUES ('p1', 'hi', 0, 'capture')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO bookmarks (post_id, observed_at) VALUES ('p1', 0)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO categories (id, label, criteria, created_at) VALUES ('c1', 'AI', '', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO post_categories (post_id, category_id, model, taxonomy_hash, classified_at)
             VALUES ('p1', 'c1', 'jev-x', 'abc', 1)",
            [],
        )
        .unwrap();

        assert!(select(&conn, false, "abc", "jev-x").unwrap().is_empty());
        // A different taxonomy re-opens it.
        assert_eq!(select(&conn, false, "def", "jev-x").unwrap().len(), 1);
        // A different model re-opens it.
        assert_eq!(select(&conn, false, "abc", "jev-y").unwrap().len(), 1);
        // Force re-opens everything.
        assert_eq!(select(&conn, true, "abc", "jev-x").unwrap().len(), 1);
    }

    #[test]
    fn a_webp_preview_is_not_sent_as_an_asset() {
        assert_eq!(format_for("image/webp"), None);
        assert_eq!(format_for("image/jpeg"), Some("jpg"));
        assert_eq!(format_for("image/png"), Some("png"));
    }
}
