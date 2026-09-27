use super::*;
use serde::{Deserialize, Serialize};

const MAX_DRAFTS_PER_PROJECT: i64 = 32;
const MAX_PROJECT_BYTES: i64 = 32_000_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditorDraft {
    pub path: String,
    pub base: String,
    pub draft: String,
    pub base_hash: String,
    pub revision: String,
    pub updated_at: f64,
}

impl Store {
    pub fn editor_drafts(&self, workspace: &Path) -> Result<Vec<EditorDraft>> {
        let db = self.lock()?;
        let mut query = db.prepare(
            "SELECT path,base,draft,base_hash,revision,updated_at FROM editor_drafts WHERE workspace=? ORDER BY updated_at DESC LIMIT 33",
        )?;
        let rows = query.query_map([workspace.to_string_lossy().as_ref()], |row| {
            Ok(EditorDraft {
                path: row.get(0)?,
                base: row.get(1)?,
                draft: row.get(2)?,
                base_hash: row.get(3)?,
                revision: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        let drafts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            drafts.len() <= MAX_DRAFTS_PER_PROJECT as usize,
            "Too many saved editor drafts"
        );
        Ok(drafts)
    }

    /// A record revision prevents a second app window from silently replacing
    /// or clearing a newer unsaved draft. `missing` is the only create token.
    pub fn put_editor_draft(
        &self,
        workspace: &Path,
        path: &str,
        base: &str,
        draft: &str,
        base_hash: &str,
        expected_revision: &str,
    ) -> Result<EditorDraft> {
        ensure!(base != draft, "Only unsaved changes need a recovery draft");
        ensure!(
            base.len() <= crate::workspace::MAX_FILE_BYTES
                && draft.len() <= crate::workspace::MAX_FILE_BYTES,
            "Editor draft exceeds the 4 MB file limit"
        );
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT revision FROM editor_drafts WHERE workspace=? AND path=?",
                params![workspace.to_string_lossy(), path],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            old.as_deref().unwrap_or("missing") == expected_revision,
            "The recovery draft changed in another window; this editor keeps its unsaved text"
        );
        let (count, other_bytes): (i64, i64) = tx.query_row(
            "SELECT count(*),COALESCE(sum(length(CAST(base AS BLOB))+length(CAST(draft AS BLOB))),0) FROM editor_drafts WHERE workspace=? AND path<>?",
            params![workspace.to_string_lossy(), path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            count < MAX_DRAFTS_PER_PROJECT,
            "Too many open editor drafts"
        );
        ensure!(
            other_bytes + base.len() as i64 + draft.len() as i64 <= MAX_PROJECT_BYTES,
            "Saved editor drafts exceed the 32 MB project limit"
        );
        let record = EditorDraft {
            path: path.to_owned(),
            base: base.to_owned(),
            draft: draft.to_owned(),
            base_hash: base_hash.to_owned(),
            revision: id(),
            updated_at: now(),
        };
        tx.execute(
            "INSERT INTO editor_drafts(workspace,path,revision,base_hash,base,draft,updated_at) VALUES(?,?,?,?,?,?,?) \
             ON CONFLICT(workspace,path) DO UPDATE SET revision=excluded.revision,base_hash=excluded.base_hash,base=excluded.base,draft=excluded.draft,updated_at=excluded.updated_at",
            params![workspace.to_string_lossy(), record.path, record.revision, record.base_hash, record.base, record.draft, record.updated_at],
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn delete_editor_draft(
        &self,
        workspace: &Path,
        path: &str,
        expected_revision: &str,
    ) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT revision FROM editor_drafts WHERE workspace=? AND path=?",
                params![workspace.to_string_lossy(), path],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            old.as_deref().unwrap_or("missing") == expected_revision,
            "The recovery draft changed in another window; it was not removed"
        );
        tx.execute(
            "DELETE FROM editor_drafts WHERE workspace=? AND path=?",
            params![workspace.to_string_lossy(), path],
        )?;
        tx.commit()?;
        Ok(())
    }
}
