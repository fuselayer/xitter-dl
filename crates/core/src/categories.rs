//! Folders, and where a post was filed.
//!
//! A category is a folder: it has a label, a sentence saying what belongs in it,
//! and optionally a parent. The classifier reads the label and the sentence, so
//! both travel together — a bare label like "markets" is not a definition.
//!
//! ## Why the tree lives in SQL rather than in the window
//!
//! Dragging a folder into another folder is a move, and a move that produces a
//! cycle would make the tree unrenderable and the recursive filter non-
//! terminating. The check belongs next to the write, where every caller gets it,
//! rather than in one component's drop handler.
//!
//! ## Counts
//!
//! `own` is how many posts sit in this folder; `total` includes everything below
//! it, because a folder that shows zero while holding a subfolder full of posts
//! reads as broken.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: String,
    pub parent_id: Option<String>,
    pub label: String,
    /// What belongs here, in a sentence. Read by the classifier.
    pub criteria: String,
    pub position: i64,
    /// Posts filed directly in this folder.
    pub own: i64,
    /// Posts filed here or anywhere below.
    pub total: i64,
}

/// Every folder, flat, ordered the way the tree is drawn.
///
/// The window builds the tree itself: one flat pass is cheaper over IPC than a
/// nested structure, and a flat list keeps "drag into" a single-row update.
/// Where the classifier files a post that fits nothing else.
///
/// A choice with no way out is not a choice: given only real folders, a model
/// asked to pick one will pick one — confidently — even for a bare link. This
/// folder is that way out, and it is an ordinary folder so the posts that land
/// there can be read, renamed and argued with.
pub const UNSORTED_ID: &str = "unsorted";
pub const UNSORTED_LABEL: &str = "Unsorted";
pub const UNSORTED_CRITERIA: &str = "The post fits none of the other folders, or there is too little of it to judge: a bare link, a bare image, or a reply with no subject.";

/// Make sure the escape hatch exists. Idempotent, and never touches the
/// user's own folders — if it was renamed, the id still holds the verdict.
pub fn ensure_unsorted(conn: &Connection) -> Result<()> {
    let exists = conn
        .query_row("SELECT 1 FROM categories WHERE id = ?1", params![UNSORTED_ID], |_| Ok(()))
        .optional()?
        .is_some();
    if exists {
        return Ok(());
    }
    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE parent_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO categories (id, parent_id, label, criteria, position, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
        params![UNSORTED_ID, UNSORTED_LABEL, UNSORTED_CRITERIA, position, crate::now()],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<Category>> {
    let mut counts = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT category_id, COUNT(*) FROM post_categories GROUP BY category_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let n: i64 = row.get(1)?;
            counts.insert(id, n);
        }
    }

    let mut out = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, label, criteria, position
               FROM categories
              ORDER BY COALESCE(parent_id, ''), position, label",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            out.push(Category {
                own: counts.get(&id).copied().unwrap_or(0),
                id,
                parent_id: row.get(1)?,
                label: row.get(2)?,
                criteria: row.get(3)?,
                position: row.get(4)?,
                total: 0,
            });
        }
    }

    // Roll up: every folder's own count is added to each ancestor. Done here
    // rather than in SQL because a recursive join per node would be a query per
    // folder, and the tree is small enough that one pass over it is free.
    if !out.is_empty() {
        let parent_of: std::collections::HashMap<&str, Option<&str>> = out
            .iter()
            .map(|c| (c.id.as_str(), c.parent_id.as_deref()))
            .collect();
        let mut totals: std::collections::HashMap<String, i64> = out
            .iter()
            .map(|c| (c.id.clone(), c.own))
            .collect();
        for c in &out {
            if c.own == 0 {
                continue;
            }
            let mut cursor = c.parent_id.as_deref();
            let mut guard = 0;
            while let Some(parent) = cursor {
                guard += 1;
                if guard > 64 {
                    break; // a cycle that predates the guard; do not spin
                }
                *totals.entry(parent.to_string()).or_default() += c.own;
                cursor = parent_of.get(parent).copied().flatten();
            }
        }
        for c in &mut out {
            c.total = totals.get(&c.id).copied().unwrap_or(c.own);
        }
    }

    // Sorting by parent then position groups children under their parent, which
    // is what the window wants for indentation; order within a group is the
    // user's drag order.
    out.sort_by(|a, b| {
        a.parent_id
            .clone()
            .unwrap_or_default()
            .cmp(&b.parent_id.clone().unwrap_or_default())
            .then(a.position.cmp(&b.position))
            .then(a.label.cmp(&b.label))
    });
    Ok(out)
}

/// Create a folder at the end of its parent's list.
pub fn create(
    conn: &Connection,
    parent_id: Option<&str>,
    label: &str,
    criteria: &str,
) -> Result<Category> {
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::Invalid("a category needs a name".into()));
    }
    let id = next_id(conn)?;
    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM categories
          WHERE parent_id IS ?1",
        params![parent_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO categories (id, parent_id, label, criteria, position, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, parent_id, label, criteria.trim(), position, crate::now()],
    )?;
    Ok(Category {
        id,
        parent_id: parent_id.map(str::to_owned),
        label: label.to_string(),
        criteria: criteria.trim().to_string(),
        position,
        own: 0,
        total: 0,
    })
}

/// Rename a folder and rewrite what belongs in it.
pub fn update(conn: &Connection, id: &str, label: &str, criteria: &str) -> Result<()> {
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::Invalid("a category needs a name".into()));
    }
    let n = conn.execute(
        "UPDATE categories SET label = ?2, criteria = ?3 WHERE id = ?1",
        params![id, label, criteria.trim()],
    )?;
    if n == 0 {
        return Err(Error::Invalid("no such category".into()));
    }
    Ok(())
}

/// Move a folder under another one (or back to the root), at `position`.
///
/// Refuses to move a folder into itself or into one of its own descendants:
/// that would detach the subtree from the root and leave a cycle in the
/// recursive filter.
pub fn move_to(
    conn: &Connection,
    id: &str,
    parent_id: Option<&str>,
    position: Option<i64>,
) -> Result<()> {
    if parent_id == Some(id) {
        return Err(Error::Invalid("a folder cannot contain itself".into()));
    }
    if let Some(parent) = parent_id {
        let mut cursor = Some(parent.to_string());
        let mut guard = 0;
        while let Some(current) = cursor {
            guard += 1;
            if guard > 128 {
                return Err(Error::Invalid("category tree is cyclic".into()));
            }
            if current == id {
                return Err(Error::Invalid(
                    "a folder cannot be moved into its own contents".into(),
                ));
            }
            cursor = conn
                .query_row(
                    "SELECT parent_id FROM categories WHERE id = ?1",
                    params![current],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
        }
    }

    let position = match position {
        Some(p) => p,
        None => conn.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM categories
              WHERE parent_id IS ?1 AND id <> ?2",
            params![parent_id, id],
            |r| r.get(0),
        )?,
    };

    let previous: Option<String> = conn
        .query_row(
            "SELECT parent_id FROM categories WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    let n = conn.execute(
        "UPDATE categories SET parent_id = ?2, position = ?3 WHERE id = ?1",
        params![id, parent_id, position],
    )?;
    if n == 0 {
        return Err(Error::Invalid("no such category".into()));
    }
    resequence(conn, parent_id)?;
    if previous.as_deref() != parent_id {
        resequence(conn, previous.as_deref())?;
    }
    Ok(())
}

/// Renumber a parent's children `0..n` in their current order.
///
/// The window says "drop it third" as an index, and an index only means
/// something if the siblings are dense. Doing it on every write keeps the
/// numbers stable without the UI having to reason about gaps.
fn resequence(conn: &Connection, parent_id: Option<&str>) -> Result<()> {
    let mut ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM categories WHERE parent_id IS ?1
              ORDER BY position, rowid",
        )?;
        let mut rows = stmt.query(params![parent_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row.get(0)?);
        }
        out
    };
    for (index, id) in ids.drain(..).enumerate() {
        conn.execute(
            "UPDATE categories SET position = ?2 WHERE id = ?1",
            params![id, index as i64],
        )?;
    }
    Ok(())
}

/// Delete a folder, its subfolders, and every filing inside them.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    // `ON DELETE CASCADE` handles the subtree and the filings, but only because
    // the connection turns foreign keys on; see `db::open`.
    conn.execute("DELETE FROM categories WHERE id = ?1", params![id])?;
    Ok(())
}

/// File a post in a folder, replacing whatever it said before.
pub fn assign(conn: &Connection, post_id: &str, category_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO post_categories (post_id, category_id, classified_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(post_id) DO UPDATE SET
           category_id = excluded.category_id,
           classified_at = excluded.classified_at",
        params![post_id, category_id, crate::now()],
    )?;
    Ok(())
}

/// File a post with the evidence that produced the verdict.
///
/// Separate from [`assign`] because a classifier's answer is not a bare
/// assignment: the confidence, the model and the folder set it was answered
/// under are what make a re-run possible and a wrong filing explicable.
/// Remove every verdict, leaving the folders untouched.
///
/// The run's own candidate query treats a missing verdict as "not yet filed", so
/// this is the same thing as starting from scratch — without rebuilding the
/// tree someone spent time describing.
pub fn clear_all(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("DELETE FROM post_categories", [])?)
}

pub fn file(
    conn: &Connection,
    post_id: &str,
    category_id: &str,
    confidence: Option<f64>,
    score: Option<f64>,
    model: &str,
    taxonomy_hash: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO post_categories
             (post_id, category_id, confidence, score, model, taxonomy_hash, classified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(post_id) DO UPDATE SET
           category_id   = excluded.category_id,
           confidence    = excluded.confidence,
           score         = excluded.score,
           model         = excluded.model,
           taxonomy_hash = excluded.taxonomy_hash,
           classified_at = excluded.classified_at",
        params![post_id, category_id, confidence, score, model, taxonomy_hash, crate::now()],
    )?;
    Ok(())
}

fn next_id(conn: &Connection) -> Result<String> {
    // Readable ids keep hand-edited databases and CLI output legible; the
    // tree is small, so a counter per prefix is fine.
    let n: i64 = conn.query_row("SELECT COUNT(*) + 1 FROM categories", [], |r| r.get(0))?;
    let mut candidate = format!("c{n:04}");
    loop {
        let taken: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM categories WHERE id = ?1",
                params![candidate],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_none() {
            return Ok(candidate);
        }
        candidate = format!("c{}", crate::now() * 1000 + n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::configure(&conn).unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn
    }

    #[test]
    fn the_escape_hatch_is_created_once_and_never_overwritten() {
        let conn = db();

        ensure_unsorted(&conn).unwrap();
        let first = list(&conn).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, UNSORTED_ID);
        assert!(first[0].criteria.contains("fits none"));

        // Renaming it is the user's business; the id is what a verdict stores,
        // so a second call must not clobber the name.
        update(&conn, UNSORTED_ID, "Cherries", "odd ones").unwrap();
        ensure_unsorted(&conn).unwrap();
        let again = list(&conn).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].label, "Cherries");
    }

    #[test]
    fn a_folder_can_hold_folders_and_counts_roll_up() {
        let conn = db();
        conn.execute(
            "INSERT INTO posts (id, text, first_seen_at, source)
             VALUES ('p1', 'hello', 0, 'capture'), ('p2', 'world', 0, 'capture')",
            [],
        )
        .unwrap();
        let tech = create(&conn, None, "Tech", "Software").unwrap();
        let ai = create(&conn, Some(&tech.id), "AI", "Models").unwrap();
        assign(&conn, "p1", &tech.id).unwrap();
        assign(&conn, "p2", &ai.id).unwrap();

        let all = list(&conn).unwrap();
        let parent = all.iter().find(|c| c.id == tech.id).unwrap();
        let child = all.iter().find(|c| c.id == ai.id).unwrap();
        assert_eq!(parent.own, 1);
        assert_eq!(parent.total, 2, "parent count includes the child's posts");
        assert_eq!(child.total, 1);
    }

    #[test]
    fn a_folder_cannot_be_moved_into_itself_or_its_child() {
        let conn = db();
        let a = create(&conn, None, "A", "").unwrap();
        let b = create(&conn, Some(&a.id), "B", "").unwrap();
        let c = create(&conn, Some(&b.id), "C", "").unwrap();

        assert!(move_to(&conn, &a.id, Some(&a.id), None).is_err());
        assert!(move_to(&conn, &a.id, Some(&b.id), None).is_err());
        assert!(move_to(&conn, &a.id, Some(&c.id), None).is_err());
        // The legal direction is fine.
        assert!(move_to(&conn, &c.id, None, None).is_ok());
    }

    #[test]
    fn deleting_a_folder_takes_its_children_and_their_filings() {
        let conn = db();
        conn.execute(
            "INSERT INTO posts (id, text, first_seen_at, source)
             VALUES ('p1', 'hi', 0, 'capture')",
            [],
        )
        .unwrap();
        let a = create(&conn, None, "A", "").unwrap();
        let b = create(&conn, Some(&a.id), "B", "").unwrap();
        assign(&conn, "p1", &b.id).unwrap();

        delete(&conn, &a.id).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM categories", [], |r| r.get(0))
            .unwrap();
        let filed: i64 = conn
            .query_row("SELECT COUNT(*) FROM post_categories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        assert_eq!(filed, 0);
        // The post itself outlives the folder; folders are a view of it.
        let posts: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(posts, 1);
    }
}
