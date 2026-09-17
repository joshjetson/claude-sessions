//! Turning `glab`'s JSON into the shapes the Deploy tab draws.
//!
//! GitLab's merge-request payload carries the same fact under two names in
//! several places (`draft` and `work_in_progress`, `detailed_merge_status` and
//! `merge_status`, `head_pipeline` and `pipeline`), because the API kept both
//! through a deprecation. Every one of those pairs is collapsed here so nothing
//! above this file has to know which GitLab version answered.

use serde_json::Value;

use crate::types::MergeRequest;

use super::argv::project_from_ref;

/// Mergeability verdicts that mean "GitLab has not finished checking yet".
///
/// Reading a merge request is what KICKS the check, so the first answer is
/// routinely one of these — see [`super::Gitlab::fetch_mr`], which retries once.
pub const PENDING_STATUSES: [&str; 3] = ["checking", "unchecked", "preparing"];

pub fn is_pending_check(status: &str) -> bool {
    PENDING_STATUSES.contains(&status)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `detailed_merge_status` where GitLab sends it, else the older
/// `merge_status`. Both are consulted because the precise reason ("ci_still
/// running", "not_approved") only lives on the newer one.
pub fn merge_status_of(record: &Value) -> String {
    let detailed = text(record, "detailed_merge_status");
    if detailed.is_empty() {
        text(record, "merge_status")
    } else {
        detailed
    }
}

/// The head pipeline's status, under either of the two keys GitLab uses.
fn pipeline_of(record: &Value) -> String {
    for key in ["head_pipeline", "pipeline"] {
        if let Some(pipeline) = record.get(key).filter(|value| value.is_object()) {
            let status = text(pipeline, "status");
            if !status.is_empty() {
                return status;
            }
        }
    }
    String::new()
}

/// One merge request.
///
/// The project path is deliberately not read back out of the payload: the
/// caller already knows which path it asked about, and that is the one every
/// subsequent call (merge, open in a browser) is addressed with.
pub fn to_merge_request(record: &Value) -> MergeRequest {
    MergeRequest {
        iid: record
            .get("iid")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        url: text(record, "web_url"),
        title: text(record, "title"),
        state: text(record, "state"),
        // `work_in_progress` is the pre-14.0 spelling of `draft`.
        draft: flag(record, "draft") || flag(record, "work_in_progress"),
        source_branch: text(record, "source_branch"),
        target_branch: text(record, "target_branch"),
        // Only meaningful once the mergeability check finished; the status
        // beside it carries the precise reason when it blocks.
        conflicts: flag(record, "has_conflicts"),
        merge_status: merge_status_of(record),
        pipeline: pipeline_of(record),
        // Why GitLab is consulted at all: the MR's author is routinely not the
        // task's assignee, so Odoo's copy of this cannot be trusted.
        author: record
            .get("author")
            .map(|author| text(author, "username"))
            .unwrap_or_default(),
    }
}

/// One row of the "my open merge requests" listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenMr {
    pub iid: i64,
    pub title: String,
    pub url: String,
    /// `group/subgroup/repo`, recovered from `references.full` or the URL.
    pub project: String,
    pub target_branch: String,
    pub draft: bool,
    pub updated_at: String,
}

pub fn to_open_mr(record: &Value) -> OpenMr {
    let url = text(record, "web_url");
    let reference = record
        .get("references")
        .and_then(|refs| refs.get("full"))
        .and_then(Value::as_str);
    OpenMr {
        iid: record
            .get("iid")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        title: text(record, "title"),
        project: project_from_ref(reference, &url),
        url,
        target_branch: text(record, "target_branch"),
        draft: flag(record, "draft") || flag(record, "work_in_progress"),
        updated_at: text(record, "updated_at"),
    }
}

/// A list endpoint's body. A non-array answer means "nothing usable" rather
/// than an error — the Node wrapper's rule, kept because `glab` prints a bare
/// object when the token cannot see anything.
pub fn to_open_mrs(body: &Value) -> Vec<OpenMr> {
    body.as_array()
        .map(|rows| rows.iter().map(to_open_mr).collect())
        .unwrap_or_default()
}

/// The first URL in a command's output — how `glab mr create` reports where the
/// merge request landed.
pub fn first_url(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|word| {
        (word.starts_with("https://") || word.starts_with("http://")).then(|| {
            word.trim_end_matches(['.', ',', ')', '"', '\''])
                .to_string()
        })
    })
}
