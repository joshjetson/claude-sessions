//! Every `glab` and `git` command this crate runs, as a pure function.
//!
//! Nothing here starts a process. That is the point: the flag combinations, the
//! URL encoding and the query string are the parts that go wrong, and they are
//! all testable with no GitLab, no network and no `glab` binary on the machine.

use std::path::Path;

/// The `glab` binary. Named once so a test can assert on it.
pub const GLAB: &str = "glab";
pub const GIT: &str = "git";

/// Branches a merge request is never opened *from*. They are where merge
/// requests land, so "open one from here" means something went wrong upstream.
pub const PROTECTED_BRANCHES: [&str; 3] = ["development", "main", "master"];

/// What `git rev-parse --abbrev-ref HEAD` says on a detached HEAD.
pub const DETACHED: &str = "HEAD";

/// The branch used when neither config nor the Odoo project's linked repository
/// names one. Same fallback as the Node original.
pub const FALLBACK_TARGET: &str = "development";

/// A merge request, as a project path plus its per-project number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrRef {
    pub project_path: String,
    pub iid: i64,
}

/// Which merge requests the "my open MRs" listing asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MrScope {
    #[default]
    CreatedByMe,
    AssignedToMe,
}

impl MrScope {
    pub fn as_str(self) -> &'static str {
        match self {
            MrScope::CreatedByMe => "created_by_me",
            MrScope::AssignedToMe => "assigned_to_me",
        }
    }
}

/// Merge flags. Both default to off, matching `glab`'s own defaults and the
/// Node wrapper's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeOptions {
    pub squash: bool,
    pub remove_source_branch: bool,
}

/// `encodeURIComponent`, which is what the Node wrapper used to build the
/// project-path segment.
///
/// The load-bearing character is `/`: a GitLab project path is
/// `group/subgroup/repo` and the API wants it as one path segment, so every
/// slash has to become `%2F` or the request addresses a route that is not
/// there.
pub fn encode_uri_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The API path for one merge request.
pub fn mr_api_path(project_path: &str, iid: i64) -> String {
    format!(
        "projects/{}/merge_requests/{iid}",
        encode_uri_component(project_path)
    )
}

/// The API path for the current user's open merge requests, newest-updated
/// first. Not URL-encoded as a whole: `glab api` takes the query string
/// verbatim, exactly as the Node wrapper passed it.
pub fn open_mrs_api_path(scope: MrScope) -> String {
    format!(
        "merge_requests?state=opened&scope={}&order_by=updated_at&per_page=100",
        scope.as_str()
    )
}

/// `glab api <path>`.
pub fn api_args(path: &str) -> Vec<String> {
    vec!["api".to_string(), path.to_string()]
}

/// `glab mr merge <iid> --repo <path> --yes [--squash] [--remove-source-branch]`.
///
/// `--repo` rather than a working directory: the Deploy tab merges MRs that
/// belong to repositories the dashboard has never checked out.
pub fn merge_args(project_path: &str, iid: i64, options: MergeOptions) -> Vec<String> {
    let mut args = vec![
        "mr".to_string(),
        "merge".to_string(),
        iid.to_string(),
        "--repo".to_string(),
        project_path.to_string(),
        "--yes".to_string(),
    ];
    if options.squash {
        args.push("--squash".to_string());
    }
    if options.remove_source_branch {
        args.push("--remove-source-branch".to_string());
    }
    args
}

/// `git -C <cwd> rev-parse --abbrev-ref HEAD`.
///
/// `-C` rather than a spawn working directory so the command is self-describing
/// in a log and in a test assertion.
pub fn current_branch_args(cwd: &Path) -> Vec<String> {
    vec![
        "-C".to_string(),
        cwd.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "--abbrev-ref".to_string(),
        "HEAD".to_string(),
    ]
}

/// `git -C <cwd> push -u origin <branch>` — a merge request cannot be opened
/// from a branch the remote has never seen.
pub fn push_args(cwd: &Path, branch: &str) -> Vec<String> {
    vec![
        "-C".to_string(),
        cwd.to_string_lossy().into_owned(),
        "push".to_string(),
        "-u".to_string(),
        "origin".to_string(),
        branch.to_string(),
    ]
}

/// `glab mr create --fill --yes --source-branch <b> --target-branch <t>`.
///
/// `--fill` takes the title and description from the commits, and `--yes`
/// skips the interactive confirmation — this runs with nobody watching.
pub fn create_mr_args(source_branch: &str, target_branch: &str) -> Vec<String> {
    vec![
        "mr".to_string(),
        "create".to_string(),
        "--fill".to_string(),
        "--yes".to_string(),
        "--source-branch".to_string(),
        source_branch.to_string(),
        "--target-branch".to_string(),
        target_branch.to_string(),
    ]
}

/// Whether a merge request can be opened from this branch at all.
///
/// A detached HEAD has no branch to push, and the three protected names are
/// where merge requests land rather than where they come from.
pub fn can_open_mr_from(branch: &str) -> bool {
    !branch.is_empty() && branch != DETACHED && !PROTECTED_BRANCHES.contains(&branch)
}

/// Pull the GitLab project path and the MR number out of a merge-request web
/// URL — `https://<host>/<group>/<repo>/-/merge_requests/403`.
///
/// The `/-/` separator is what makes this unambiguous: everything before it is
/// the project path however many subgroups deep it is.
pub fn parse_mr_url(url: &str) -> Option<MrRef> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Past the host.
    let (_, path) = rest.split_once('/')?;
    let (project_path, tail) = path.split_once("/-/merge_requests/")?;
    if project_path.is_empty() {
        return None;
    }
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let iid = digits.parse().ok()?;
    Some(MrRef {
        project_path: project_path.to_string(),
        iid,
    })
}

/// The project path a listing row belongs to: its `references.full`
/// (`group/repo!403`) when GitLab sent one, else recovered from the web URL.
pub fn project_from_ref(reference: Option<&str>, url: &str) -> String {
    if let Some(reference) = reference {
        if let Some((project, _)) = reference.split_once('!') {
            return project.to_string();
        }
    }
    parse_mr_url(url)
        .map(|parsed| parsed.project_path)
        .unwrap_or_default()
}
