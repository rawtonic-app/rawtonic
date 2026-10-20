//! A library's ingest jobs (G66), so an ingest a quit or crash interrupted
//! is finished when the library is opened again, without copying a file
//! twice or leaving one out. A job (`tasks`, kind `ingest`) keeps the
//! ingest's options as the app gave them and one item per photograph
//! (`ingest_items`: the source, its companion, its number for `{seq}`) in
//! the order they are imported. Before a photograph is copied its planned
//! copies are recorded (`copying`), so copies published by a run that was
//! cut off before they were recorded in the library are found and recorded
//! then (and left to it meanwhile: `planned_ingest_copies`). Finished and
//! cancelled jobs are kept for a while (the newest `KEPT_JOBS`). Nothing
//! here touches files.
use crate::Library;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Finished and cancelled ingest jobs kept (the newest).
const KEPT_JOBS: usize = 20;

/// Where an ingest job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Being imported, or cut off by a quit or crash (`Library::open`
    /// cannot tell: the app knows the one it runs).
    Running,
    /// Stopped with photographs left whose source was not there (a card
    /// taken out); finished when it is.
    Interrupted,
    Done,
    /// Stopped by the user; not resumed.
    Cancelled,
}

/// Where one photograph of an ingest job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    Pending,
    /// Its copy (`IngestItem::destination`) may be published.
    Copying,
    Done,
    Failed,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "running" => Self::Running,
            "interrupted" => Self::Interrupted,
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            other => bail!("Unknown ingest job state {other}"),
        })
    }
}

impl ItemState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Copying => "copying",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "pending" => Self::Pending,
            "copying" => Self::Copying,
            "done" => Self::Done,
            "failed" => Self::Failed,
            other => bail!("Unknown ingest item state {other}"),
        })
    }
    /// Still to be imported.
    pub fn open(self) -> bool {
        matches!(self, Self::Pending | Self::Copying)
    }
}

/// One photograph of an ingest job.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestItem {
    pub position: usize,
    /// Its number among the job's photographs (`{seq}` counts from the
    /// first), a pair's one.
    pub number: u64,
    pub source: PathBuf,
    pub companion: Option<PathBuf>,
    pub state: ItemState,
    /// The copy planned or made.
    pub destination: Option<PathBuf>,
    /// The companion's copy planned or made.
    pub companion_destination: Option<PathBuf>,
    pub error: Option<String>,
}

/// An ingest job with its photographs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestJob {
    pub id: String,
    pub state: JobState,
    /// The ingest's options, as the caller stored them.
    pub options: Value,
    pub created_at: String,
    pub items: Vec<IngestItem>,
}

impl IngestJob {
    /// Photographs still to be imported.
    pub fn open_items(&self) -> impl Iterator<Item = &IngestItem> {
        self.items.iter().filter(|item| item.state.open())
    }
}

fn text(path: &Path) -> Result<&str> {
    path.to_str().context("Paths must be valid UTF-8")
}

impl Library {
    /// Records a new ingest job (`Running`) of `items` (each photograph's
    /// source and companion, in import order, numbered from 0) with
    /// `options`, in one transaction. Returns its id.
    pub fn create_ingest_job(
        &self,
        options: &Value,
        items: &[(PathBuf, Option<PathBuf>)],
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM tasks WHERE kind='ingest' AND state IN ('done','cancelled') AND id NOT IN
             (SELECT id FROM tasks WHERE kind='ingest' AND state IN ('done','cancelled')
              ORDER BY rowid DESC LIMIT ?)",
            [KEPT_JOBS as i64],
        )?;
        tx.execute(
            "INSERT INTO tasks(id,kind,state,payload) VALUES(?,'ingest','running',?)",
            params![id, serde_json::to_string(options)?],
        )?;
        for (position, (source, companion)) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO ingest_items(job_id,position,number,source,companion,state)
                 VALUES(?,?,?,?,?,'pending')",
                params![
                    id,
                    position as i64,
                    position as i64,
                    text(source)?,
                    companion.as_deref().map(text).transpose()?
                ],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn ingest_job(&self, id: &str) -> Result<IngestJob> {
        let (state, payload, created_at): (String, String, String) = self
            .conn
            .query_row(
                "SELECT state,payload,created_at FROM tasks WHERE id=? AND kind='ingest'",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .with_context(|| format!("No ingest job {id}"))?;
        let mut query = self.conn.prepare(
            "SELECT position,number,source,companion,state,destination,companion_destination,error
             FROM ingest_items WHERE job_id=? ORDER BY position",
        )?;
        let items = query
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })?
            .map(|row| {
                let (position, number, source, companion, state, destination, planned, error) =
                    row?;
                Ok(IngestItem {
                    position: position as usize,
                    number: number as u64,
                    source: source.into(),
                    companion: companion.map(Into::into),
                    state: ItemState::parse(&state)?,
                    destination: destination.map(Into::into),
                    companion_destination: planned.map(Into::into),
                    error,
                })
            })
            .collect::<Result<_>>()?;
        Ok(IngestJob {
            id: id.into(),
            state: JobState::parse(&state)?,
            options: serde_json::from_str(&payload)?,
            created_at,
            items,
        })
    }

    /// Ingest jobs with photographs still to import that were not
    /// cancelled (running ones included: the caller knows which it runs),
    /// oldest first.
    pub fn unfinished_ingest_jobs(&self) -> Result<Vec<IngestJob>> {
        let ids: Vec<String> = self
            .conn
            .prepare(
                "SELECT t.id FROM tasks t WHERE t.kind='ingest' AND t.state IN ('running','interrupted')
                 AND EXISTS(SELECT 1 FROM ingest_items i WHERE i.job_id=t.id AND i.state IN ('pending','copying'))
                 ORDER BY t.created_at, t.rowid",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.iter().map(|id| self.ingest_job(id)).collect()
    }

    /// Records where photograph `position` of job `id` is: its state, its
    /// copy and its companion's (each kept when `None`) and why it failed.
    pub fn set_ingest_item(
        &self,
        id: &str,
        position: usize,
        state: ItemState,
        destination: Option<&Path>,
        companion_destination: Option<&Path>,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE ingest_items SET state=?,destination=coalesce(?,destination),
             companion_destination=coalesce(?,companion_destination),error=?
             WHERE job_id=? AND position=?",
            params![
                state.as_str(),
                destination.map(text).transpose()?,
                companion_destination.map(text).transpose()?,
                error,
                id,
                position as i64
            ],
        )?;
        ensure!(changed == 1, "No photograph {position} in ingest job {id}");
        Ok(())
    }

    /// The copies unfinished ingest jobs planned and have not recorded yet:
    /// such a file is left to its job (which records it with its companion)
    /// rather than registered on its own.
    pub fn planned_ingest_copies(&self) -> Result<HashSet<PathBuf>> {
        let mut query = self.conn.prepare(
            "SELECT i.destination,i.companion_destination FROM ingest_items i JOIN tasks t ON t.id=i.job_id
             WHERE t.state IN ('running','interrupted') AND i.state='copying'",
        )?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?;
        let mut planned = HashSet::new();
        for row in rows {
            let (photo, companion) = row?;
            planned.extend(photo.into_iter().chain(companion).map(PathBuf::from));
        }
        Ok(planned)
    }

    pub fn set_ingest_job_state(&self, id: &str, state: JobState) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tasks SET state=? WHERE id=? AND kind='ingest'",
            params![state.as_str(), id],
        )?;
        ensure!(changed == 1, "No ingest job {id}");
        Ok(())
    }
}
