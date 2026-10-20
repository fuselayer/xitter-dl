//! The thumbnail store: content-addressed image bytes on disk.
//!
//! ## Why files and not a BLOB column
//!
//! Two reasons. A blob is shareable — the same image posted twice, or the same
//! video poster seen on two bookmarks, is one file — and the library file stays
//! one file the user can back up or move without it having quietly grown by
//! hundreds of megabytes of JPEG. The table records the *mapping*; the bytes
//! live beside it.
//!
//! ## The layout
//!
//! `media/<sha256>.<ext>`, where the extension comes from an allowlisted MIME
//! type. The hash is computed here from the bytes we are about to write; it is
//! never supplied by the wire, so nothing arriving over the loopback can name
//! a path. That is the whole of the traversal defence: an input that cannot be
//! a path cannot walk out of the directory.
//!
//! ## Writes are idempotent and atomic
//!
//! The same thumbnail arriving twice is the normal case (a re-scroll, a
//! re-import, both builds touching one library). An existing file is left
//! alone. A new one is written to a temporary name and renamed into place, so
//! a reader — or the other build — never sees a half-written image under the
//! final name.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// MIME types we will store, and the extension each maps to.
///
/// An allowlist rather than a sanitiser: the extension is derived from a
/// *constant* table, so a hostile `mime` field in a capture record cannot
/// become a path component. X serves JPEG for photos and posters; PNG and
/// WebP are here because X does occasionally serve them and a refused-but-
/// harmless type is worse than a stored one.
const MIME_EXT: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// The extension for an allowed MIME type, or `None` if we do not store it.
pub fn mime_extension(mime: &str) -> Option<&'static str> {
    MIME_EXT
        .iter()
        .find(|(m, _)| m.eq_ignore_ascii_case(mime))
        .map(|(_, ext)| *ext)
}

/// Is this something we are willing to write to disk?
pub fn mime_is_allowed(mime: &str) -> bool {
    mime_extension(mime).is_some()
}

/// Recognise an image from its first bytes.
///
/// Only used when a capture record arrives without a `content-type` — a CDN
/// that omits it, or a userscript manager that does not expose response
/// headers. Sniffing beats guessing `jpeg` for a PNG, which would write a .jpg
/// the webview then declines to draw with no error anywhere.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Hex SHA-256 of a byte string. Lowercase, 64 characters.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True for exactly 64 lowercase hex characters.
///
/// Checked before any hash is joined to a path. Hashes we produce always pass;
/// a value read from a database that someone else wrote may not.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A directory of content-addressed images.
#[derive(Debug, Clone)]
pub struct MediaStore {
    root: PathBuf,
}

impl MediaStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store beside a connection's database file, when it has one.
    ///
    /// Deriving it from the connection rather than threading it through every
    /// importer is what keeps the two frontends honest: the bridge, the CLI and
    /// the paste box all store thumbnails next to the same library, and none of
    /// them can be configured to put them somewhere the app cannot read. An
    /// in-memory library has no directory, hence `None`.
    pub fn for_connection(conn: &rusqlite::Connection) -> Option<Self> {
        conn.path()
            // SQLite reports an empty name for a temporary or in-memory
            // database. `Path::new("").parent()` is already `None`, but saying
            // it out loud is cheaper than a reader having to know that.
            .filter(|p| !p.is_empty())
            .and_then(|p| Path::new(p).parent())
            .map(|dir| MediaStore::new(dir.join("media")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store `bytes`, returning `(sha256 hex, length)`.
    ///
    /// An unknown MIME type is refused rather than guessed: guessing is how a
    /// file gets an extension that does not match its contents, and the
    /// webview then refuses to draw it with no explanation anywhere.
    pub fn put(&self, mime: &str, bytes: &[u8]) -> Result<(String, usize)> {
        let ext = mime_extension(mime).ok_or_else(|| {
            Error::invalid(format!("refusing to store {mime}: not an allowed image type"))
        })?;

        let hash = sha256_hex(bytes);
        let path = self.root.join(format!("{hash}.{ext}"));
        if path.exists() {
            return Ok((hash, bytes.len()));
        }

        fs::create_dir_all(&self.root).map_err(|e| Error::create_dir(&self.root, e))?;

        // Write beside the target, then rename. Two builds can be writing the
        // same blob at the same time; the pid keeps their temporaries apart,
        // and rename is atomic on every platform we ship.
        let tmp = self
            .root
            .join(format!("{hash}.{ext}.{}.tmp", std::process::id()));
        fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))?;

        Ok((hash, bytes.len()))
    }

    /// Read a blob back. A missing file is `None`, not an error — the library
    /// can legitimately describe a blob whose file the user deleted.
    pub fn read(&self, hash: &str, mime: &str) -> Result<Option<Vec<u8>>> {
        if !is_sha256_hex(hash) {
            return Ok(None);
        }
        let Some(ext) = mime_extension(mime) else {
            return Ok(None);
        };
        let path = self.root.join(format!("{hash}.{ext}"));
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// The blob as a `data:` URL, which is how the webview draws it.
    ///
    /// The app's CSP is `img-src 'self' data:` and deliberately not `asset:`.
    /// A data URL needs no capability, no scope entry and no CSP change — and
    /// it keeps the file read on the Rust side, where the path guard lives.
    pub fn data_url(&self, hash: &str, mime: &str) -> Result<Option<String>> {
        Ok(self
            .read(hash, mime)?
            .map(|bytes| format!("data:{mime};base64,{}", base64_encode(&bytes))))
    }
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode standard base64. `None` for anything malformed.
///
/// Hand-rolled rather than a crate: this is the only base64 in the project,
/// and adding a dependency to the crate whose *point* is that its dependency
/// list is auditable is the wrong trade for forty lines with tests. Strict on
/// purpose — padding only at the end, no whitespace, no URL-safe alphabet —
/// because the only producer is our own script and a lenient decoder hides a
/// broken encoder.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();

    let end = bytes.iter().rposition(|c| *c != b'=').map(|i| i + 1)?;
    let padding = bytes.len() - end;
    if padding > 2 {
        return None;
    }
    if bytes[..end].contains(&b'=') {
        return None;
    }
    // Padding must be exactly what the data length calls for. `Zm9v=` decodes
    // to "foo" if you let it, but it is not base64 our encoder can produce,
    // and accepting it would hide a broken encoder behind a working decoder.
    if padding != (4 - end % 4) % 4 {
        return None;
    }
    if end % 4 == 1 {
        return None;
    }

    let mut out = Vec::with_capacity(end / 4 * 3 + 2);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;

    for &c in &bytes[..end] {
        acc = (acc << 6) | b64_value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }

    // A non-zero tail means the encoder wrote bits that decode to nothing;
    // that is not canonical base64 and a round trip would not reproduce it.
    if acc != 0 {
        return None;
    }

    Some(out)
}

/// Encode standard base64, padded.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(B64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xdl-media-{}-{}-{name}",
            std::process::id(),
            crate::now()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn base64_round_trips_at_every_length() {
        // Length 0 is deliberately absent: an empty body is not an image, and
        // `base64_decode("")` refusing it is the guard that says so.
        for len in 1..=8usize {
            let bytes: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37).wrapping_add(11)).collect();
            let encoded = base64_encode(&bytes);
            assert_eq!(base64_decode(&encoded).as_deref(), Some(&bytes[..]), "len {len}");
            assert_eq!(encoded.len() % 4, 0, "output is padded");
        }
    }

    #[test]
    fn base64_matches_a_known_vector() {
        // RFC 4648 test vectors. If these disagree, the encoder and the
        // decoder are wrong in the same direction and the round-trip test
        // above would still pass.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_rejects_what_a_script_would_not_send() {
        assert!(base64_decode("Zm9v\nYmFy").is_none(), "no whitespace");
        assert!(base64_decode("Zg==Zg==").is_none(), "padding only at the end");
        assert!(base64_decode("Z").is_none(), "one stray character is not a group");
        assert!(base64_decode("Zm9v-").is_none(), "url-safe alphabet is refused");
        assert!(base64_decode("Zm9v=").is_none(), "wrong padding for the length");
        assert!(base64_decode("").is_none(), "an empty string is not an image");
    }

    #[test]
    fn hashes_are_lowercase_hex_of_the_right_length() {
        let h = sha256_hex(b"abc");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_hex(&h));
        assert!(!is_sha256_hex(&h.to_uppercase()));
        assert!(!is_sha256_hex(".."));
        assert!(!is_sha256_hex(""));
        assert!(!is_sha256_hex(&format!("{h}0")));
    }

    #[test]
    fn stores_idempotently_and_reads_back() {
        let store = MediaStore::new(scratch("put"));
        let bytes = b"\xff\xd8 pretend these are jpeg bytes";

        let (hash, len) = store.put("image/jpeg", bytes).unwrap();
        assert_eq!(len, bytes.len());
        assert_eq!(hash, sha256_hex(bytes));
        assert!(store.root().join(format!("{hash}.jpg")).exists());

        // The same bytes again: same hash, no second file, no error.
        let (hash2, _) = store.put("image/jpeg", bytes).unwrap();
        assert_eq!(hash, hash2);
        let files = fs::read_dir(store.root()).unwrap().count();
        assert_eq!(files, 1, "a repeat write must not leave a second file");

        assert_eq!(store.read(&hash, "image/jpeg").unwrap().as_deref(), Some(&bytes[..]));
        let url = store.data_url(&hash, "image/jpeg").unwrap().unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn an_unknown_mime_type_is_refused_rather_than_extensioned() {
        let store = MediaStore::new(scratch("mime"));
        let err = store.put("text/html", b"<script>").unwrap_err();
        assert!(err.to_string().contains("not an allowed image type"));
        // And nothing was created for it.
        assert!(!store.root().exists() || fs::read_dir(store.root()).unwrap().count() == 0);
    }

    #[test]
    fn a_missing_blob_is_none_and_a_bad_hash_is_none() {
        let store = MediaStore::new(scratch("missing"));
        let hash = sha256_hex(b"never stored");
        assert!(store.read(&hash, "image/jpeg").unwrap().is_none());
        // A traversal attempt through the hash column resolves to nothing.
        assert!(store.read("../../etc/passwd", "image/jpeg").unwrap().is_none());
        assert!(store.data_url("../../etc/passwd", "image/jpeg").unwrap().is_none());
    }

    #[test]
    fn sniffs_the_four_types_x_serves() {
        assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff_image_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"<html>not an image</html>"), None);
        assert_eq!(sniff_image_mime(b""), None);
    }
}
