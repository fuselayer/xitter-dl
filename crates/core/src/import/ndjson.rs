//! The NDJSON handoff format.
//!
//! ## Why newline-delimited JSON
//!
//! This is the wire format between the userscript and the app, and the fallback
//! file format. It is append-only and line-oriented, which buys three things a
//! single JSON array cannot:
//!
//! 1. **It can be appended to.** The userscript buffers captures and flushes
//!    them; appending a line needs no read-modify-write of the whole file.
//! 2. **A truncated file is still mostly readable.** If the app is killed
//!    mid-flush, every complete line before the cut survives. A truncated
//!    array is unparseable and loses everything.
//! 3. **It streams.** Records can be ingested as they arrive rather than
//!    after the last one.
//!
//! ## The three record kinds
//!
//! ```text
//! {"v":1,"kind":"page",    ..., "raw":      { …full GraphQL envelope, once… }}
//! {"v":1,"kind":"bookmark",..., "raw_tweet":{ …tweet_results.result subtree… }}
//! {"v":1,"kind":"media",   ..., "url":      "https://pbs.twimg.com/…",
//!                               "mime":"image/jpeg", "data":"<base64>"}
//! ```
//!
//! A `page` record carries a whole timeline response and everything in it. A
//! `bookmark` record carries one tweet on its own — which is what continuous
//! capture produces, because a single save is not a page. A `media` record
//! carries one thumbnail's bytes, because the raw envelopes never contain
//! them: X sends image URLs, and the picture itself is a separate request that
//! only the browser can make (PRD §7.5).
//!
//! Splitting them this way is what keeps the "envelope stored once, not per
//! post" rule true on the wire as well as in the database.

use std::path::Path;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

use crate::db::write::{self, SOURCE_CAPTURE};
use crate::error::{Error, Result};
use crate::x;

use super::ImportSummary;

/// The current wire version. Bump only for a breaking change; a reader that
/// sees a higher number should warn and try anyway rather than refuse, since
/// an old app meeting a new script is the recoverable direction.
pub const WIRE_VERSION: u32 = 1;

/// The largest thumbnail we will decode from one record.
///
/// A `?name=thumb` photo is ~6 kB and a video poster ~60 kB, so this is two
/// orders of magnitude of headroom. It exists because the record's `data` is
/// decoded into memory before anything else looks at it, and the bridge's body
/// limit alone would let 8 MB of JSON become 6 MB of allocation per line.
pub const MAX_MEDIA_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Line {
    /// A whole captured page.
    Page {
        #[serde(default)]
        v: Option<u32>,
        #[serde(default)]
        captured_at: Option<Value>,
        #[serde(default)]
        op: Option<String>,
        raw: Value,
    },
    /// A single tweet, e.g. from continuous capture.
    Bookmark {
        #[serde(default)]
        v: Option<u32>,
        #[serde(default)]
        captured_at: Option<Value>,
        #[serde(default)]
        op: Option<String>,
        #[serde(default)]
        raw_tweet: Option<Value>,
        /// Our normalised form, when the script had no raw payload. Accepted
        /// so a future stub-only protocol does not need a new wire version.
        #[serde(default)]
        record: Option<Value>,
        #[serde(default)]
        sort_index: Option<String>,
        /// Only continuous capture can supply this — we watched the save.
        #[serde(default)]
        bookmarked_at: Option<i64>,
    },
    /// One thumbnail's bytes, already fetched by the browser.
    ///
    /// Keyed by `url`, which is the `media_url_https` from the payload rather
    /// than the variant that was actually downloaded: the variant is our
    /// choice and may change, while the payload's URL is what the post's own
    /// media rows still carry. `post_id` is for diagnostics only.
    Media {
        #[serde(default)]
        v: Option<u32>,
        #[serde(default)]
        captured_at: Option<Value>,
        #[serde(default)]
        post_id: Option<String>,
        url: String,
        /// From the response's `content-type`. Sniffed when absent.
        #[serde(default)]
        mime: Option<String>,
        /// Standard base64, padded.
        data: String,
    },
}

/// Import an NDJSON file.
pub fn import_file(conn: &Connection, path: &Path, now: i64) -> Result<ImportSummary> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    import_str(conn, &text, now)
}

/// Import NDJSON from a string.
pub fn import_str(conn: &Connection, text: &str, now: i64) -> Result<ImportSummary> {
    // Callers that already wrapped this in a transaction (the bridge does) must
    // not get a nested BEGIN, so only an autocommit connection is opened here.
    if !conn.is_autocommit() {
        return import_lines(conn, text, now);
    }
    // One transaction for the whole batch, so the write lock is taken once
    // instead of once per record.
    let tx = conn.unchecked_transaction()?;
    let summary = import_lines(&tx, text, now)?;
    tx.commit()?;
    Ok(summary)
}

/// The body of [`import_str`], run inside its transaction.
///
/// Without the wrapper every record committed on its own — a page of twenty
/// posts and forty previews is sixty separate writes, and each one takes the
/// same write lock the window's `list_bookmarks` and its search are queued
/// behind. That is why typing went sticky while a scroll was ingesting.
fn import_lines(conn: &Connection, text: &str, now: i64) -> Result<ImportSummary> {
    let mut summary = ImportSummary {
        files: 1,
        ..Default::default()
    };

    // Lines that parsed into a record kind this build understands, whether or
    // not they changed anything. Distinct from `stats`, which counts effects:
    // a file whose thumbnails are all already stored is a legitimate no-op,
    // not the "nothing recognisable here" case the error below is for.
    let mut recognised = 0usize;

    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        match serde_json::from_str::<Line>(line) {
            Ok(Line::Page {
                v,
                captured_at,
                op,
                raw,
            }) => {
                recognised += 1;
                if let Some(w) = warn_version(v) {
                    summary.problems.push(format!("line {}: {w}", lineno + 1));
                }
                let at = stamp(captured_at, now);
                // Re-serialize the envelope. It round-trips through serde_json
                // faithfully for the types X actually sends (no floats with
                // exotic precision in a bookmark payload), and this is the
                // import path, not the live-capture path, so the stored
                // envelope being a re-serialization is acceptable.
                let body = serde_json::to_string(&raw)?;
                match write::store_payload(
                    conn,
                    &body,
                    op.as_deref().or(Some("Bookmarks")),
                    SOURCE_CAPTURE,
                    at,
                ) {
                    Ok(s) => summary.stats.merge(s),
                    Err(e) => summary.problems.push(format!("line {}: {e}", lineno + 1)),
                }
            }

            Ok(Line::Bookmark {
                v,
                captured_at,
                op,
                raw_tweet,
                record,
                sort_index,
                bookmarked_at,
            }) => {
                recognised += 1;
                if let Some(w) = warn_version(v) {
                    summary.problems.push(format!("line {}: {w}", lineno + 1));
                }
                let at = stamp(captured_at, now);

                // Prefer the raw subtree: it is X's bytes, and storing it means
                // this record benefits from every future parser fix.
                let value = raw_tweet.or(record);
                let Some(value) = value else {
                    summary
                        .problems
                        .push(format!("line {}: bookmark with no raw_tweet or record", lineno + 1));
                    continue;
                };

                // A `bookmark` record is by definition something the user
                // saved, regardless of which operation produced it.
                let _ = op;
                match write::store_tweet_value(
                    conn,
                    &value,
                    SOURCE_CAPTURE,
                    at,
                    true,
                    sort_index.as_deref(),
                    bookmarked_at,
                ) {
                    Ok(Some((id, is_new))) => {
                        summary.stats.seen += 1;
                        if is_new {
                            summary.stats.new += 1;
                        } else {
                            summary.stats.updated += 1;
                        }
                        summary.stats.touched.push(id);
                    }
                    Ok(None) => summary
                        .problems
                        .push(format!("line {}: not a recognisable tweet", lineno + 1)),
                    Err(e) => summary.problems.push(format!("line {}: {e}", lineno + 1)),
                }
            }

            Ok(Line::Media {
                v,
                captured_at,
                post_id,
                url,
                mime,
                data,
            }) => {
                recognised += 1;
                if let Some(w) = warn_version(v) {
                    summary.problems.push(format!("line {}: {w}", lineno + 1));
                }
                let at = stamp(captured_at, now);
                let who = post_id
                    .as_deref()
                    .map(|p| format!(" for post {p}"))
                    .unwrap_or_default();

                // An in-memory library has nowhere to put bytes. That is a real
                // configuration, not a crash: `--dry-run` and most tests.
                let Some(store) = crate::media::MediaStore::for_connection(conn) else {
                    summary.problems.push(format!(
                        "line {}: media{who}, but this library has no directory beside it",
                        lineno + 1
                    ));
                    continue;
                };

                let Some(bytes) = crate::media::base64_decode(&data) else {
                    summary
                        .problems
                        .push(format!("line {}: media{who} is not valid base64", lineno + 1));
                    continue;
                };
                if bytes.is_empty() {
                    summary
                        .problems
                        .push(format!("line {}: media{who} carried no bytes", lineno + 1));
                    continue;
                }
                if bytes.len() > MAX_MEDIA_BYTES {
                    summary.problems.push(format!(
                        "line {}: media{who} is {} bytes, over the {MAX_MEDIA_BYTES} byte limit",
                        lineno + 1,
                        bytes.len()
                    ));
                    continue;
                }

                // Trust the response's content type when it has one, sniff
                // otherwise. A type we do not store is refused by `put`, which
                // is the one place that decides what can become a file name.
                let mime = match mime.filter(|m| !m.is_empty()) {
                    Some(m) => m,
                    None => match crate::media::sniff_image_mime(&bytes) {
                        Some(m) => m.to_string(),
                        None => {
                            summary.problems.push(format!(
                                "line {}: media{who} is not a recognisable image",
                                lineno + 1
                            ));
                            continue;
                        }
                    },
                };

                match store.put(&mime, &bytes) {
                    Ok((hash, len)) => {
                        match write::attach_media_blob(conn, &url, &hash, &mime, len as i64, at) {
                            // Counted only when the mapping actually moved: new
                            // URL, or a sharper preview replacing a known one.
                            // A re-scroll re-sends what it already stored, and a
                            // counter that went up on each would be measuring
                            // scrolling.
                            Ok(true) => {
                                summary.stats.media += 1;
                                match post_id.as_deref() {
                                    Some(p) => summary.stats.touched.push(p.to_string()),
                                    // A profile picture belongs to no single
                                    // post; every post by that account has to
                                    // repaint, or the avatar only shows up
                                    // after a restart.
                                    None => summary
                                        .stats
                                        .touched
                                        .extend(write::post_ids_for_avatar(conn, &url)?),
                                }
                            }
                            Ok(false) => {}
                            Err(e) => {
                                summary.problems.push(format!("line {}: {e}", lineno + 1))
                            }
                        }
                    }
                    Err(e) => summary.problems.push(format!("line {}: {e}", lineno + 1)),
                }
            }

            Err(e) => {
                // A malformed line does not abort the file. With an append-only
                // format the realistic corruption is a half-written final line
                // after a crash, and losing the other 4,000 records to it would
                // be absurd.
                summary
                    .problems
                    .push(format!("line {}: {e}", lineno + 1));
            }
        }
    }

    if recognised == 0 && summary.problems.is_empty() {
        return Err(Error::NoTweets {
            context: "NDJSON file".into(),
        });
    }

    Ok(summary)
}

/// `captured_at` may arrive as an ISO string or as Unix seconds depending on
/// which side of the wire wrote it.
fn stamp(value: Option<Value>, fallback: i64) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(fallback),
        Some(Value::String(s)) => x::parse_x_timestamp(&s).unwrap_or(fallback),
        _ => fallback,
    }
}

fn warn_version(v: Option<u32>) -> Option<String> {
    match v {
        Some(n) if n > WIRE_VERSION => Some(format!(
            "record claims wire version {n}, this build understands {WIRE_VERSION}; imported anyway"
        )),
        _ => None,
    }
}

/// Serialize a record for writing, used by the CLI's `--emit` and by tests.
pub fn page_line(op: &str, captured_at: i64, raw: &Value) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "v": WIRE_VERSION,
        "kind": "page",
        "captured_at": captured_at,
        "op": op,
        "raw": raw,
    }))?)
}

/// As [`page_line`], for a single tweet.
pub fn bookmark_line(raw_tweet: &Value, captured_at: i64) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "v": WIRE_VERSION,
        "kind": "bookmark",
        "captured_at": captured_at,
        "raw_tweet": raw_tweet,
    }))?)
}

/// As [`bookmark_line`], for one thumbnail's bytes.
pub fn media_line(url: &str, mime: &str, bytes: &[u8], captured_at: i64) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "v": WIRE_VERSION,
        "kind": "media",
        "captured_at": captured_at,
        "url": url,
        "mime": mime,
        "data": crate::media::base64_encode(bytes),
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::read;
    use serde_json::json;

    fn tweet(id: &str, text: &str) -> Value {
        json!({
            "__typename": "Tweet", "rest_id": id,
            "core": { "user_results": { "result": {
                "rest_id": "42",
                "core": { "name": "Ada", "screen_name": "ada" },
                "legacy": { "name": "Ada", "screen_name": "ada" }
            }}},
            "legacy": { "id_str": id, "full_text": text, "entities": { "urls": [] } }
        })
    }

    fn tweet_with_photo(id: &str, text: &str, url: &str) -> Value {
        let mut v = tweet(id, text);
        v["legacy"]["extended_entities"] = json!({
            "media": [{
                "type": "photo",
                "media_url_https": url,
                "original_info": { "width": 1200, "height": 800 }
            }]
        });
        v
    }

    fn page_of(raw_tweet: Value) -> String {
        page_line(
            "Bookmarks",
            1000,
            &json!({ "data": { "tweetResult": { "result": raw_tweet } } }),
        )
        .unwrap()
    }

    /// A file-backed library, because thumbnails need a directory to live in.
    fn temp_library(name: &str) -> (crate::db::Library, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("xdl-ndjson-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = crate::db::Library::open(dir.join("library.sqlite")).unwrap();
        (lib, dir)
    }

    const JPEG: [u8; 9] = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49];

    #[test]
    fn imports_a_page_record() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = page_line(
            "Bookmarks",
            1000,
            &json!({ "data": { "tweetResult": { "result": tweet("1", "page tweet") } } }),
        )
        .unwrap();

        let s = import_str(l.conn(), &line, 2000).unwrap();
        assert_eq!(s.stats.new, 1);
        assert_eq!(l.count_bookmarks().unwrap(), 1);
    }

    #[test]
    fn imports_a_single_bookmark_record_and_keeps_its_observed_save_time() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = json!({
            "v": 1, "kind": "bookmark", "captured_at": 5000,
            "raw_tweet": tweet("2", "saved live"),
            "bookmarked_at": 4999
        })
        .to_string();

        import_str(l.conn(), &line, 6000).unwrap();

        let post = read::get_post(l.conn(), "2").unwrap().unwrap();
        // Continuous capture is the only source of a real save time.
        assert_eq!(post.bookmarked_at, Some(4999));
        assert!(post.tweet.text.contains("saved live"));
    }

    #[test]
    fn a_backfill_page_leaves_the_save_time_null() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = page_line(
            "Bookmarks",
            1000,
            &json!({ "data": { "tweetResult": { "result": tweet("3", "bulk scrolled") } } }),
        )
        .unwrap();

        import_str(l.conn(), &line, 2000).unwrap();

        let post = read::get_post(l.conn(), "3").unwrap().unwrap();
        assert_eq!(post.bookmarked_at, None, "backfill must not invent a save time");
    }

    #[test]
    fn accepts_an_iso_timestamp_as_well_as_unix() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = json!({
            "v": 1, "kind": "bookmark",
            "captured_at": "2018-10-10T20:19:24Z",
            "raw_tweet": tweet("4", "iso stamped")
        })
        .to_string();

        import_str(l.conn(), &line, 6000).unwrap();
        let post = read::get_post(l.conn(), "4").unwrap().unwrap();
        assert_eq!(post.first_seen_at, 1_539_202_764);
    }

    #[test]
    fn a_corrupt_line_does_not_lose_the_rest_of_the_file() {
        // The realistic corruption: a half-written final line after a crash.
        let l = crate::db::Library::open_in_memory().unwrap();
        let good1 = bookmark_line(&tweet("10", "first"), 1000).unwrap();
        let good2 = bookmark_line(&tweet("11", "second"), 1001).unwrap();
        let text = format!("{good1}\n{good2}\n{{\"v\":1,\"kind\":\"bookm");

        let s = import_str(l.conn(), &text, 2000).unwrap();
        assert_eq!(s.stats.new, 2, "good lines should survive a corrupt one");
        assert_eq!(s.problems.len(), 1);
        assert!(s.problems[0].starts_with("line 3"));
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let text = format!(
            "# a comment the script wrote\n\n{}\n\n",
            bookmark_line(&tweet("12", "x"), 1000).unwrap()
        );
        let s = import_str(l.conn(), &text, 2000).unwrap();
        assert_eq!(s.stats.new, 1);
        assert!(s.problems.is_empty());
    }

    #[test]
    fn a_newer_wire_version_warns_but_still_imports() {
        // An old app meeting a new script is the recoverable direction, so we
        // try rather than refuse.
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = json!({
            "v": 99, "kind": "bookmark", "captured_at": 1000,
            "raw_tweet": tweet("13", "from the future")
        })
        .to_string();

        let s = import_str(l.conn(), &line, 2000).unwrap();
        assert_eq!(s.stats.new, 1, "a future version should still import");
        assert!(s.problems[0].contains("wire version 99"));
    }

    #[test]
    fn an_unknown_record_kind_is_reported_not_fatal() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let text = format!(
            "{}\n{}\n",
            json!({ "v": 1, "kind": "some_future_thing", "data": 1 }),
            bookmark_line(&tweet("14", "fine"), 1000).unwrap()
        );
        let s = import_str(l.conn(), &text, 2000).unwrap();
        assert_eq!(s.stats.new, 1);
        assert_eq!(s.problems.len(), 1);
    }

    #[test]
    fn a_completely_empty_file_is_reported() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let err = import_str(l.conn(), "\n\n# nothing here\n", 1000).unwrap_err();
        assert!(matches!(err, Error::NoTweets { .. }));
    }

    #[test]
    fn re_importing_the_same_ndjson_is_a_no_op() {
        let l = crate::db::Library::open_in_memory().unwrap();
        let text = format!(
            "{}\n{}\n",
            bookmark_line(&tweet("15", "one"), 1000).unwrap(),
            bookmark_line(&tweet("16", "two"), 1001).unwrap()
        );

        let a = import_str(l.conn(), &text, 2000).unwrap();
        let b = import_str(l.conn(), &text, 3000).unwrap();
        assert_eq!(a.stats.new, 2);
        assert_eq!(b.stats.new, 0);
        assert_eq!(b.stats.updated, 2);
        assert_eq!(l.count_bookmarks().unwrap(), 2);
    }

    // ── thumbnails ──────────────────────────────────────────────────────────

    #[test]
    fn a_media_record_lands_beside_the_library_and_shows_on_the_post() {
        let (l, dir) = temp_library("attach");
        let url = "https://pbs.twimg.com/media/MEDIA001.jpg";

        let text = format!(
            "{}\n{}",
            page_of(tweet_with_photo("20", "a photo", url)),
            media_line(url, "image/jpeg", &JPEG, 1001).unwrap()
        );
        let s = import_str(l.conn(), &text, 2000).unwrap();
        assert_eq!(s.stats.new, 1);
        assert_eq!(s.stats.media, 1);
        assert!(s.problems.is_empty(), "{:?}", s.problems);

        let hash = crate::media::sha256_hex(&JPEG);
        assert!(
            dir.join("media").join(format!("{hash}.jpg")).exists(),
            "the blob should be a file beside the database"
        );

        let post = read::get_post(l.conn(), "20").unwrap().unwrap();
        let blob = post.tweet.media[0].blob.as_ref().expect("blob should be attached");
        assert_eq!(blob.hash, hash);
        assert_eq!(blob.mime, "image/jpeg");
        assert_eq!(blob.bytes, JPEG.len() as i64);

        // And the bytes come back out intact, which is the whole point.
        let store = crate::media::MediaStore::for_connection(l.conn()).unwrap();
        assert_eq!(store.read(&blob.hash, &blob.mime).unwrap().as_deref(), Some(&JPEG[..]));

        // The two steps `media_thumb` takes: the MIME type by hash (so the
        // caller never supplies an extension) and the data URL the webview
        // draws.
        assert_eq!(read::blob_mime(l.conn(), &blob.hash).unwrap().as_deref(), Some("image/jpeg"));
        let url = store.data_url(&blob.hash, &blob.mime).unwrap().unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"), "{url}");
        assert_eq!(
            crate::media::base64_decode(&url["data:image/jpeg;base64,".len()..]).as_deref(),
            Some(&JPEG[..])
        );
        // A hash that is not a hash is refused before any file is opened.
        assert_eq!(read::blob_mime(l.conn(), "../../etc/passwd").unwrap(), None);
    }

    #[test]
    fn a_thumbnail_survives_the_page_being_re_imported() {
        // The regression that matters: `replace_media` deletes and rewrites a
        // post's media rows on every re-import, and a thumbnail keyed to a row
        // would vanish on the next scroll of the bookmarks page.
        let (l, _dir) = temp_library("survives");
        let url = "https://pbs.twimg.com/media/MEDIA002.jpg";

        let first = format!(
            "{}\n{}",
            page_of(tweet_with_photo("21", "first capture", url)),
            media_line(url, "image/jpeg", &JPEG, 1001).unwrap()
        );
        import_str(l.conn(), &first, 2000).unwrap();

        // A different page body carrying the same post — which is what a
        // re-scroll produces, since sortIndex and counts move.
        let second = page_of(tweet_with_photo("21", "second capture", url));
        import_str(l.conn(), &second, 3000).unwrap();

        let post = read::get_post(l.conn(), "21").unwrap().unwrap();
        assert_eq!(post.tweet.text, "second capture");
        assert!(
            post.tweet.media[0].blob.is_some(),
            "re-importing the page must not drop the thumbnail"
        );
    }

    #[test]
    fn a_thumbnail_can_arrive_before_the_post_it_belongs_to() {
        let (l, _dir) = temp_library("before");
        let url = "https://pbs.twimg.com/media/MEDIA003.jpg";

        let early = media_line(url, "image/jpeg", &JPEG, 1001).unwrap();
        let s = import_str(l.conn(), &early, 2000).unwrap();
        assert_eq!(s.stats.media, 1, "bytes are keyed by URL, not by a post row");

        // The post turns up later and finds its preview already there.
        import_str(l.conn(), &page_of(tweet_with_photo("22", "late", url)), 3000).unwrap();
        let post = read::get_post(l.conn(), "22").unwrap().unwrap();
        assert!(post.tweet.media[0].blob.is_some());
    }

    #[test]
    fn re_sending_a_thumbnail_counts_nothing_and_adds_no_file() {
        let (l, dir) = temp_library("resend");
        let url = "https://pbs.twimg.com/media/MEDIA004.jpg";
        let line = media_line(url, "image/jpeg", &JPEG, 1001).unwrap();

        let a = import_str(l.conn(), &line, 2000).unwrap();
        let b = import_str(l.conn(), &line, 3000).unwrap();

        assert_eq!(a.stats.media, 1);
        assert_eq!(b.stats.media, 0, "a re-scroll must not inflate the counter");
        assert_eq!(std::fs::read_dir(dir.join("media")).unwrap().count(), 1);
    }

    #[test]
    fn a_sharper_preview_at_the_same_url_replaces_the_mapping_and_counts() {
        let (l, dir) = temp_library("upgrade");
        let url = "https://pbs.twimg.com/media/MEDIA005.jpg";
        // The same media kept at a different size: the URL never changes, only
        // the bytes X serves for it. It has to replace the mapping *and* count,
        // because the file changing without the counter moving is exactly the
        // case where the window keeps drawing the old picture until a restart.
        let thumb = media_line(url, "image/jpeg", &JPEG, 1001).unwrap();
        let small: [u8; 10] = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x2A];
        let bigger = media_line(url, "image/jpeg", &small, 2002).unwrap();

        let a = import_str(l.conn(), &thumb, 2000).unwrap();
        let b = import_str(l.conn(), &bigger, 3000).unwrap();

        assert_eq!(a.stats.media, 1);
        assert_eq!(b.stats.media, 1, "a sharper preview is work, not a duplicate");

        let hash: String = l
            .conn()
            .query_row(
                "SELECT blob_hash FROM media_blobs WHERE url = ?1",
                rusqlite::params![url],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hash, crate::media::sha256_hex(&small));
        assert!(dir.join("media").join(format!("{hash}.jpg")).exists());
    }

    #[test]
    fn a_thumbnail_that_is_not_an_image_is_reported_and_not_stored() {
        let (l, dir) = temp_library("not-an-image");

        let not_base64 = json!({
            "v": 1, "kind": "media", "url": "https://pbs.twimg.com/media/BAD1.jpg",
            "mime": "image/jpeg", "data": "!!! not base64 !!!"
        })
        .to_string();
        let wrong_type = json!({
            "v": 1, "kind": "media", "url": "https://pbs.twimg.com/media/BAD2.jpg",
            "mime": "text/html", "data": crate::media::base64_encode(b"<script>")
        })
        .to_string();
        let no_type_no_magic = json!({
            "v": 1, "kind": "media", "url": "https://pbs.twimg.com/media/BAD3.jpg",
            "data": crate::media::base64_encode(b"just some bytes")
        })
        .to_string();

        for line in [not_base64, wrong_type, no_type_no_magic] {
            let s = import_str(l.conn(), &line, 2000).unwrap();
            assert_eq!(s.stats.media, 0);
            assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        }

        assert!(
            !dir.join("media").exists() || std::fs::read_dir(dir.join("media")).unwrap().count() == 0,
            "nothing should have been written"
        );
    }

    #[test]
    fn a_thumbnail_without_a_content_type_is_sniffed() {
        let (l, _dir) = temp_library("sniff");
        let url = "https://pbs.twimg.com/media/MEDIA005.png";
        // The PNG magic, spelled numerically: a byte-string literal with
        // escapes is one editing round trip away from becoming a real newline.
        let png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ];

        let line = json!({
            "v": 1, "kind": "media", "url": url,
            "data": crate::media::base64_encode(&png)
        })
        .to_string();

        let s = import_str(l.conn(), &line, 2000).unwrap();
        assert_eq!(s.stats.media, 1);
        assert!(s.problems.is_empty(), "{:?}", s.problems);

        let hash = crate::media::sha256_hex(&png);
        let store = crate::media::MediaStore::for_connection(l.conn()).unwrap();
        assert!(store.root().join(format!("{hash}.png")).exists());
    }

    #[test]
    fn an_in_memory_library_reports_a_thumbnail_it_cannot_store() {
        // `--dry-run` and most tests run in memory; a media record there must
        // say why it was skipped rather than silently disappear.
        let l = crate::db::Library::open_in_memory().unwrap();
        let line = media_line("https://pbs.twimg.com/media/MEDIA006.jpg", "image/jpeg", &JPEG, 1001).unwrap();

        let s = import_str(l.conn(), &line, 2000).unwrap();
        assert_eq!(s.stats.media, 0);
        assert!(s.problems[0].contains("no directory beside it"), "{:?}", s.problems);
    }
}
