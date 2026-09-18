//! Which QA runs are being watched, so one survives quitting the dashboard.
//!
//! A run is a grouping the reviewer made — these tasks, watched together, with
//! this coordinator. The sessions it groups are real processes that outlive the
//! TUI easily; the grouping was the only part that did not, so quitting turned
//! a seven-agent run back into seven unrelated rows.
//!
//! Written whole every time it changes. A run has a handful of fields and a
//! reviewer has one or two runs, so a diff would be more code than it saves and
//! one more thing to get wrong.

use rusqlite::Row;

use crate::qarun::{QaRun, RunMode};

use super::Db;

fn ids_to_text(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Parses a stored id list, skipping anything that is not a number.
///
/// Lenient on purpose: a row this cannot fully read is still a run worth
/// showing, and refusing the whole row would lose the reviewer's grouping over
/// one bad character.
fn ids_from_text(text: &str) -> Vec<i64> {
    text.split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect()
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<QaRun> {
    Ok(QaRun {
        id: row.get("id")?,
        project_name: row.get("project_name")?,
        stage_name: row.get("stage_name")?,
        task_ids: ids_from_text(&row.get::<_, String>("task_ids")?),
        spawned: ids_from_text(&row.get::<_, String>("spawned")?),
        started_at: row.get("started_at")?,
        // 0 is stored as NULL, because 0 means "no cap" everywhere else and a
        // stored 0 would read as a cap of zero lanes.
        lane_limit: row
            .get::<_, Option<i64>>("lane_limit")?
            .filter(|limit| *limit > 0)
            .map(|limit| limit as usize),
        mode: match row.get::<_, String>("mode")?.as_str() {
            "shadow" => RunMode::Shadow,
            _ => RunMode::Triage,
        },
        coordinator_started: row.get::<_, i64>("coordinator_started")? != 0,
    })
}

impl Db {
    /// Every watched run, oldest first, so the list is in the order they were
    /// started rather than whatever order SQLite returns.
    pub fn qa_runs(&self) -> Vec<QaRun> {
        self.rows(
            "qa_runs",
            "SELECT id, project_name, stage_name, task_ids, spawned, started_at, \
             lane_limit, mode, coordinator_started FROM qa_runs ORDER BY started_at, id",
            [],
            from_row,
        )
    }

    /// Replace the stored runs with this exact set.
    ///
    /// Replace rather than upsert: stopping a run has to remove its row, and a
    /// delete-then-insert says that once instead of in two places that can
    /// disagree.
    pub fn save_qa_runs(&self, runs: &[QaRun]) {
        self.exec("qa_runs.save", |conn| {
            // A plain batch, not a transaction: `with_conn` hands out a shared
            // reference, and one run list is small enough that a partial write
            // is corrected by the next save rather than worth reworking the
            // connection API for.
            conn.execute("DELETE FROM qa_runs", [])?;

            {
                let mut stmt = conn.prepare(
                    "INSERT INTO qa_runs \
                     (id, project_name, stage_name, task_ids, spawned, started_at, \
                      lane_limit, mode, coordinator_started) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )?;
                for run in runs {
                    stmt.execute(rusqlite::params![
                        run.id,
                        run.project_name,
                        run.stage_name,
                        ids_to_text(&run.task_ids),
                        ids_to_text(&run.spawned),
                        run.started_at,
                        run.lane_limit.map(|limit| limit as i64),
                        match run.mode {
                            RunMode::Shadow => "shadow",
                            RunMode::Triage => "triage",
                        },
                        i64::from(run.coordinator_started),
                    ])?;
                }
            }
            Ok(())
        });
    }
}
