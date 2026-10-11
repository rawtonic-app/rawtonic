//! The persistent export queue (G44/G66): jobs and their photos in SQLite,
//! so an export survives a restart, failed photos can be retried and new
//! exports wait behind a running one (Capture One's process queue).
//!
//! The queue stores state only; rendering stays with the caller. Each photo
//! records its planned output path before rendering. Outputs are published
//! atomically and never overwrite (see [`crate::output::write_new_output`]),
//! so after a crash an existing planned output is a finished photo.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    /// Waiting behind the running job.
    Queued,
    Running,
    /// Every photo exported.
    Done,
    /// Finished with failed photos (retryable).
    Failed,
    /// Cancelled by the user; the remaining photos can be resumed.
    Cancelled,
    /// The app quit or crashed while the job ran; resumable.
    Interrupted,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::Interrupted => "interrupted",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "done" => JobState::Done,
            "failed" => JobState::Failed,
            "cancelled" => JobState::Cancelled,
            "interrupted" => JobState::Interrupted,
            other => anyhow::bail!("Unknown export job state {other}"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemState {
    Pending,
    Running,
    Done,
    Failed,
}

impl ItemState {
    fn as_str(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Running => "running",
            ItemState::Done => "done",
            ItemState::Failed => "failed",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "pending" => ItemState::Pending,
            "running" => ItemState::Running,
            "done" => ItemState::Done,
            "failed" => ItemState::Failed,
            other => anyhow::bail!("Unknown export item state {other}"),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub index: usize,
    pub path: String,
    pub state: ItemState,
    pub output: Option<PathBuf>,
    pub error: Option<String>,
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub label: String,
    pub state: JobState,
    /// The caller's export request (settings, destination, recipes), as JSON.
    /// Not sent to the interface (it can carry whole recipes).
    #[serde(skip_serializing)]
    pub request: serde_json::Value,
    pub created_at: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
}

const QUEUE_VERSION: u32 = 1;

pub struct ExportQueue {
    conn: Connection,
}

impl ExportQueue {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Cannot open the export queue {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= QUEUE_VERSION,
            "The export queue is from a newer Rawtonic (version {version})"
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs(
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL UNIQUE,
               label TEXT NOT NULL,
               state TEXT NOT NULL,
               request TEXT NOT NULL,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS items(
               job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
               idx INTEGER NOT NULL,
               path TEXT NOT NULL,
               state TEXT NOT NULL DEFAULT 'pending',
               output TEXT,
               error TEXT,
               attempts INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(job_id, idx));
             PRAGMA user_version=1;",
        )?;
        Ok(Self { conn })
    }

    /// Adds a job for `paths` (one item each, in order) behind the others.
    pub fn enqueue(
        &self,
        label: &str,
        request: &serde_json::Value,
        paths: &[String],
    ) -> Result<String> {
        ensure!(!paths.is_empty(), "Nothing to export");
        let id = uuid::Uuid::new_v4().to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO jobs(id,label,state,request) VALUES(?,?,'queued',?)",
            params![id, label, serde_json::to_string(request)?],
        )?;
        {
            let mut insert = tx.prepare("INSERT INTO items(job_id,idx,path) VALUES(?,?,?)")?;
            for (index, path) in paths.iter().enumerate() {
                insert.execute(params![id, index as i64, path])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// The oldest queued job, marked running.
    pub fn start_next(&self) -> Result<Option<Job>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM jobs WHERE state='queued' ORDER BY seq LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        self.set_state(&id, JobState::Running)?;
        self.job(&id).map(Some)
    }

    /// Records a photo's planned output before it is rendered.
    pub fn item_started(&self, job: &str, index: usize, output: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET state=?, output=?, attempts=attempts+1 WHERE job_id=? AND idx=?",
            params![
                ItemState::Running.as_str(),
                output.to_string_lossy(),
                job,
                index as i64
            ],
        )?;
        Ok(())
    }

    pub fn item_done(&self, job: &str, index: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET state=?, error=NULL WHERE job_id=? AND idx=?",
            params![ItemState::Done.as_str(), job, index as i64],
        )?;
        Ok(())
    }

    pub fn item_failed(&self, job: &str, index: usize, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET state=?, error=? WHERE job_id=? AND idx=?",
            params![ItemState::Failed.as_str(), error, job, index as i64],
        )?;
        Ok(())
    }

    /// Ends the running job: done, failed (some photos failed) or cancelled.
    /// Photos that never finished go back to pending, for a later resume.
    pub fn finish(&self, job: &str, cancelled: bool) -> Result<Job> {
        self.conn.execute(
            "UPDATE items SET state='pending' WHERE job_id=? AND state='running'",
            [job],
        )?;
        let current = self.job(job)?;
        let state = if cancelled {
            JobState::Cancelled
        } else if current.failed > 0 {
            JobState::Failed
        } else if current.done == current.total {
            JobState::Done
        } else {
            JobState::Interrupted
        };
        self.set_state(job, state)?;
        self.job(job)
    }

    /// After a restart: running jobs become interrupted; photos whose
    /// planned output exists are finished. Returns the interrupted jobs.
    pub fn recover(&self) -> Result<Vec<Job>> {
        let ids: Vec<String> =
            self.ids("SELECT id FROM jobs WHERE state='running' ORDER BY seq")?;
        for id in &ids {
            for item in self.items(id)? {
                if item.state != ItemState::Running {
                    continue;
                }
                let finished = item.output.as_deref().is_some_and(Path::is_file);
                self.conn.execute(
                    "UPDATE items SET state=? WHERE job_id=? AND idx=?",
                    params![
                        if finished {
                            ItemState::Done
                        } else {
                            ItemState::Pending
                        }
                        .as_str(),
                        id,
                        item.index as i64
                    ],
                )?;
            }
            self.set_state(id, JobState::Interrupted)?;
        }
        ids.iter().map(|id| self.job(id)).collect()
    }

    /// Queues the photos of a failed, cancelled or interrupted job that did
    /// not export, behind the other jobs. Returns how many.
    pub fn retry(&self, job: &str) -> Result<usize> {
        let current = self.job(job)?;
        ensure!(
            matches!(
                current.state,
                JobState::Failed | JobState::Cancelled | JobState::Interrupted
            ),
            "Only failed, cancelled or interrupted exports can be resumed"
        );
        let tx = self.conn.unchecked_transaction()?;
        let count = tx.execute(
            "UPDATE items SET state='pending', error=NULL WHERE job_id=? AND state!='done'",
            [job],
        )?;
        // To the back of the queue.
        tx.execute(
            "UPDATE jobs SET state='queued', seq=(SELECT max(seq)+1 FROM jobs) WHERE id=?",
            [job],
        )?;
        tx.commit()?;
        Ok(count)
    }

    /// Removes a job that is not running (finished ones, or ones not to resume).
    pub fn remove(&self, job: &str) -> Result<()> {
        let current = self.job(job)?;
        ensure!(
            current.state != JobState::Running,
            "A running export is cancelled, not removed"
        );
        self.conn.execute("DELETE FROM jobs WHERE id=?", [job])?;
        Ok(())
    }

    /// Removes finished jobs beyond the newest `keep`.
    pub fn prune_done(&self, keep: usize) -> Result<()> {
        self.conn.execute(
            "DELETE FROM jobs WHERE state='done' AND id NOT IN
               (SELECT id FROM jobs WHERE state='done' ORDER BY seq DESC LIMIT ?)",
            [keep as i64],
        )?;
        Ok(())
    }

    /// Jobs in queue order, oldest first.
    pub fn jobs(&self) -> Result<Vec<Job>> {
        let ids = self.ids("SELECT id FROM jobs ORDER BY seq")?;
        ids.iter().map(|id| self.job(id)).collect()
    }

    pub fn job(&self, id: &str) -> Result<Job> {
        let (label, state, request, created_at): (String, String, String, String) = self
            .conn
            .query_row(
                "SELECT label,state,request,created_at FROM jobs WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .with_context(|| format!("No export job {id}"))?;
        let (total, done, failed): (i64, i64, i64) = self.conn.query_row(
            "SELECT count(*), coalesce(sum(state='done'),0), coalesce(sum(state='failed'),0)
             FROM items WHERE job_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Job {
            id: id.to_string(),
            label,
            state: JobState::parse(&state)?,
            request: serde_json::from_str(&request)?,
            created_at,
            total: total as usize,
            done: done as usize,
            failed: failed as usize,
        })
    }

    pub fn items(&self, job: &str) -> Result<Vec<Item>> {
        let mut q = self.conn.prepare(
            "SELECT idx,path,state,output,error,attempts FROM items WHERE job_id=? ORDER BY idx",
        )?;
        let rows = q.query_map([job], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (index, path, state, output, error, attempts) = row?;
            Ok(Item {
                index: index as usize,
                path,
                state: ItemState::parse(&state)?,
                output: output.map(PathBuf::from),
                error,
                attempts: attempts as u32,
            })
        })
        .collect()
    }

    fn ids(&self, sql: &str) -> Result<Vec<String>> {
        let mut q = self.conn.prepare(sql)?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn set_state(&self, id: &str, state: JobState) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET state=? WHERE id=?",
            params![state.as_str(), id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn queue() -> (tempfile::TempDir, ExportQueue) {
        let dir = tempfile::tempdir().unwrap();
        let queue = ExportQueue::open(&dir.path().join("export-queue.sqlite")).unwrap();
        (dir, queue)
    }

    fn paths(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("/photos/{i}.RAF")).collect()
    }

    #[test]
    fn jobs_run_in_order_and_finish_by_their_photos() {
        let (_dir, q) = queue();
        let first = q
            .enqueue("Export 2", &json!({"format":"jpg"}), &paths(2))
            .unwrap();
        let second = q.enqueue("Export 1", &json!({}), &paths(1)).unwrap();
        let job = q.start_next().unwrap().unwrap();
        assert_eq!(job.id, first);
        assert_eq!(job.request["format"], "jpg");
        assert!(q.start_next().unwrap().is_some_and(|j| j.id == second));
        q.item_started(&first, 0, Path::new("/out/0.jpg")).unwrap();
        q.item_done(&first, 0).unwrap();
        q.item_started(&first, 1, Path::new("/out/1.jpg")).unwrap();
        q.item_failed(&first, 1, "decode failed").unwrap();
        let job = q.finish(&first, false).unwrap();
        assert_eq!(
            (job.state, job.done, job.failed, job.total),
            (JobState::Failed, 1, 1, 2)
        );
        let items = q.items(&first).unwrap();
        assert_eq!(items[1].error.as_deref(), Some("decode failed"));
        assert_eq!(items[1].attempts, 1);
        q.item_done(&second, 0).unwrap();
        assert_eq!(q.finish(&second, false).unwrap().state, JobState::Done);
    }

    #[test]
    fn retry_requeues_only_what_did_not_export_at_the_back() {
        let (_dir, q) = queue();
        let job = q.enqueue("Export 3", &json!({}), &paths(3)).unwrap();
        q.start_next().unwrap();
        q.item_done(&job, 0).unwrap();
        q.item_failed(&job, 1, "x").unwrap();
        q.finish(&job, false).unwrap();
        let other = q.enqueue("Other", &json!({}), &paths(1)).unwrap();
        assert_eq!(q.retry(&job).unwrap(), 2);
        let order: Vec<String> = q.jobs().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(order, vec![other.clone(), job.clone()]);
        assert_eq!(q.start_next().unwrap().unwrap().id, other);
        let items = q.items(&job).unwrap();
        assert_eq!(
            items.iter().map(|i| i.state).collect::<Vec<_>>(),
            vec![ItemState::Done, ItemState::Pending, ItemState::Pending]
        );
        assert!(q.retry(&other).is_err(), "a running job is not retried");
    }

    #[test]
    fn a_cancelled_job_keeps_its_unfinished_photos_for_a_resume() {
        let (_dir, q) = queue();
        let job = q.enqueue("Export 2", &json!({}), &paths(2)).unwrap();
        q.start_next().unwrap();
        q.item_started(&job, 0, Path::new("/out/0.jpg")).unwrap();
        let done = q.finish(&job, true).unwrap();
        assert_eq!(done.state, JobState::Cancelled);
        assert_eq!(q.items(&job).unwrap()[0].state, ItemState::Pending);
        assert_eq!(q.retry(&job).unwrap(), 2);
    }

    #[test]
    fn recovery_finds_published_outputs_and_interrupts_the_rest() {
        let (dir, q) = queue();
        let job = q.enqueue("Export 3", &json!({}), &paths(3)).unwrap();
        q.start_next().unwrap();
        // Photo 0 was published before the crash, photo 1 was rendering.
        let published = dir.path().join("0.jpg");
        std::fs::write(&published, "jpeg").unwrap();
        q.item_started(&job, 0, &published).unwrap();
        q.item_started(&job, 1, &dir.path().join("1.jpg")).unwrap();
        drop(q);
        let q = ExportQueue::open(&dir.path().join("export-queue.sqlite")).unwrap();
        let interrupted = q.recover().unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].state, JobState::Interrupted);
        assert_eq!(interrupted[0].done, 1);
        let states: Vec<_> = q.items(&job).unwrap().iter().map(|i| i.state).collect();
        assert_eq!(
            states,
            vec![ItemState::Done, ItemState::Pending, ItemState::Pending]
        );
        assert!(q.recover().unwrap().is_empty(), "recovery runs once");
        assert_eq!(q.retry(&job).unwrap(), 2);
    }

    #[test]
    fn running_jobs_cannot_be_removed_and_old_finished_ones_are_pruned() {
        let (_dir, q) = queue();
        let mut done = Vec::new();
        for _ in 0..3 {
            let id = q.enqueue("Export", &json!({}), &paths(1)).unwrap();
            q.start_next().unwrap();
            q.item_done(&id, 0).unwrap();
            q.finish(&id, false).unwrap();
            done.push(id);
        }
        let running = q.enqueue("Export", &json!({}), &paths(1)).unwrap();
        q.start_next().unwrap();
        assert!(q.remove(&running).is_err());
        q.prune_done(1).unwrap();
        let left: Vec<String> = q.jobs().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(left, vec![done[2].clone(), running]);
        q.remove(&done[2]).unwrap();
        assert_eq!(q.jobs().unwrap().len(), 1);
    }
}
