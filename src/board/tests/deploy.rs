//! The Deploy tab's rows. The MR badge leads: it is the answer the tab exists
//! to give, and anything after the task name gets clipped in a narrow pane.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::board::{
    build_deploy_tree, deploy_item_key, deploy_project_key, format_deploy_item, DeployItem, Role,
    Row,
};
use crate::types::{
    DeployBoard, DeployProjectState, DeployRun, DeployRunStatus, DeployTask, MergeRequest,
};

use super::{now, role_of, text};

/// Board, expansion set and runs owned together, because the rows borrow all
/// three.
struct Fixture {
    board: DeployBoard,
    expanded: HashSet<String>,
    runs: HashMap<String, DeployRun>,
}

impl Fixture {
    fn new(tasks: Vec<DeployTask>, command: &str) -> Self {
        let mut projects = BTreeMap::new();
        projects.insert(
            "Aurora".to_string(),
            DeployProjectState {
                project_id: 3,
                missing: false,
                command: command.to_string(),
                target_branch: "main".to_string(),
                tasks,
            },
        );
        Fixture {
            board: DeployBoard {
                configured: true,
                project_names: vec!["Aurora".to_string()],
                projects,
            },
            expanded: HashSet::new(),
            runs: HashMap::new(),
        }
    }

    fn open(mut self) -> Self {
        self.expanded.insert(deploy_project_key("Aurora"));
        self
    }

    fn missing(mut self) -> Self {
        self.board.projects.get_mut("Aurora").unwrap().missing = true;
        self
    }

    fn running(mut self, status: DeployRunStatus, exit_code: Option<i32>) -> Self {
        self.runs.insert(
            "Aurora".to_string(),
            DeployRun {
                status,
                exit_code,
                started_at: "2026-09-16T11:00:00.000Z".to_string(),
            },
        );
        self
    }

    fn items(&self) -> Vec<DeployItem<'_>> {
        build_deploy_tree(&self.board, &self.expanded, &self.runs)
    }

    fn row(&self, index: usize) -> Row {
        format_deploy_item(&self.items()[index], now())
    }
}

fn deploy_task(id: i64, name: &str) -> DeployTask {
    DeployTask {
        id,
        name: name.to_string(),
        state: "01_in_progress".to_string(),
        state_label: "In Progress".to_string(),
        project_name: "Aurora".to_string(),
        project_id: 3,
        stage_name: "Deployed".to_string(),
        ..DeployTask::default()
    }
}

fn with_mr(id: i64, name: &str, mr: MergeRequest) -> DeployTask {
    DeployTask {
        mr_url: mr.url.clone(),
        mr_iid: Some(mr.iid),
        mr_project_path: "group/repo".to_string(),
        mr_state: mr.state.clone(),
        mr: Some(mr),
        ..deploy_task(id, name)
    }
}

fn merge_request(state: &str) -> MergeRequest {
    MergeRequest {
        iid: 403,
        url: "https://git.example/group/repo/-/merge_requests/403".to_string(),
        title: "Fix the export".to_string(),
        state: state.to_string(),
        source_branch: "task-5944-fix".to_string(),
        target_branch: "main".to_string(),
        merge_status: "mergeable".to_string(),
        ..MergeRequest::default()
    }
}

#[test]
fn nothing_configured_is_its_own_row() {
    let empty = DeployBoard::default();
    let expanded = HashSet::new();
    let runs = HashMap::new();
    let items = build_deploy_tree(&empty, &expanded, &runs);
    assert_eq!(items.len(), 1);
    assert_eq!(
        text(&format_deploy_item(&items[0], now())),
        "  No projects configured for deploy. Press c to add one."
    );
}

#[test]
fn a_project_row_counts_what_is_outstanding_and_names_the_target_branch() {
    let fixture = Fixture::new(
        vec![with_mr(5944, "Fix the export", merge_request("opened"))],
        "./deploy.sh",
    );
    let row = fixture.row(0);
    assert_eq!(text(&row), "▶ 🚀 Aurora  1 outstanding  → main");
    assert_eq!(role_of(&row, "1 outstanding"), Some(Role::Warn));

    match &fixture.items()[0] {
        DeployItem::Project { blocked_count, .. } => assert_eq!(*blocked_count, 1),
        other => panic!("expected a project row, got {other:?}"),
    }
}

#[test]
fn a_project_with_no_deploy_command_says_so() {
    let fixture = Fixture::new(vec![], "");
    let row = fixture.row(0);
    assert_eq!(
        text(&row),
        "▶ 🚀 Aurora  clear to deploy  no deploy command"
    );
    assert_eq!(role_of(&row, "no deploy command"), Some(Role::Danger));
}

#[test]
fn a_running_deploy_is_shown_on_the_project_row() {
    let fixture = Fixture::new(vec![], "./deploy.sh").running(DeployRunStatus::Running, None);
    assert!(text(&fixture.row(0)).ends_with("  ⟳ deploying…"));

    let failed = Fixture::new(vec![], "./deploy.sh").running(DeployRunStatus::Fail, Some(2));
    let row = failed.row(0);
    assert!(text(&row).ends_with("  ✗ deploy failed (2)"));
    assert_eq!(role_of(&row, "deploy failed"), Some(Role::Danger));

    let ok = Fixture::new(vec![], "./deploy.sh").running(DeployRunStatus::Ok, Some(0));
    assert!(text(&ok.row(0)).ends_with("  ✓ deployed"));
}

#[test]
fn an_expanded_project_with_nothing_outstanding_says_it_is_clear() {
    let fixture = Fixture::new(vec![], "./deploy.sh").open();
    assert_eq!(
        text(&fixture.row(1)),
        "     ✓ Nothing outstanding in Deployed — clear to ship."
    );
    assert_eq!(deploy_item_key(&fixture.items()[1]), "di:clear:Aurora");
}

#[test]
fn a_project_missing_from_odoo_is_named() {
    let fixture = Fixture::new(vec![], "./deploy.sh").open().missing();
    assert_eq!(
        text(&fixture.row(1)),
        "     No Odoo project named \"Aurora\"."
    );
    assert_eq!(
        text(&fixture.row(0)),
        "▼ 🚀 Aurora  not found in Odoo  → main"
    );
}

#[test]
fn the_mr_badge_leads_the_task_row() {
    let fixture = Fixture::new(
        vec![with_mr(5944, "Fix the export", merge_request("merged"))],
        "./deploy.sh",
    )
    .open();
    let row = fixture.row(1);
    assert_eq!(
        text(&row),
        "  ✓ !403 merged  #5944  Fix the export  In Progress"
    );
    assert_eq!(row[1].text, "✓ !403 merged ", "the badge must lead the row");
    assert_eq!(row[1].style.role, Role::Ok);
    assert_eq!(deploy_item_key(&fixture.items()[1]), "dt:5944");
}

#[test]
fn every_mr_state_gets_its_own_verdict() {
    let cases: Vec<(DeployTask, &str, Role)> = vec![
        (
            with_mr(1, "Ready", merge_request("opened")),
            "⇅ !403 ready ",
            Role::Ready,
        ),
        (
            with_mr(2, "Closed", merge_request("closed")),
            "✗ !403 closed",
            Role::Danger,
        ),
        (
            with_mr(
                3,
                "Draft",
                MergeRequest {
                    draft: true,
                    ..merge_request("opened")
                },
            ),
            "⚠ !403 draft ",
            Role::Warn,
        ),
        (
            with_mr(
                4,
                "Conflicted",
                MergeRequest {
                    conflicts: true,
                    ..merge_request("opened")
                },
            ),
            "✗ !403 conflic",
            Role::Danger,
        ),
        (
            with_mr(
                5,
                "Checking",
                MergeRequest {
                    merge_status: "ci_still_running".to_string(),
                    ..merge_request("opened")
                },
            ),
            "⚠ !403 ci stil…",
            Role::Warn,
        ),
        (deploy_task(6, "No MR"), "○ no MR", Role::Dim),
    ];

    for (task, expected, role) in cases {
        let fixture = Fixture::new(vec![task], "./deploy.sh").open();
        let row = fixture.row(1);
        assert!(
            row[1].text.starts_with(expected),
            "expected {expected:?}, got {:?}",
            row[1].text
        );
        assert_eq!(row[1].style.role, role);
    }
}

#[test]
fn an_mr_that_could_not_be_read_says_so_rather_than_looking_merged() {
    let task = DeployTask {
        mr_error: Some("glab: not found".to_string()),
        mr_url: "https://git.example/group/repo/-/merge_requests/403".to_string(),
        mr_iid: Some(403),
        ..deploy_task(5944, "Fix the export")
    };
    let fixture = Fixture::new(vec![task], "./deploy.sh").open();
    let row = fixture.row(1);
    assert!(row[1].text.starts_with("✗ MR error"));
    assert_eq!(row[1].style.role, Role::Danger);
}

#[test]
fn an_unloaded_mr_shows_its_iid_and_a_question_mark() {
    let task = DeployTask {
        mr_iid: Some(403),
        ..deploy_task(5944, "Fix the export")
    };
    let fixture = Fixture::new(vec![task], "./deploy.sh").open();
    assert!(fixture.row(1)[1].text.starts_with("○ !403 ?"));
}

#[test]
fn the_pipeline_dot_follows_the_badge() {
    let cases = [
        ("success", Role::Ok, "●"),
        ("failed", Role::Danger, "●"),
        ("running", Role::Warn, "◐"),
        ("skipped", Role::Dim, "○"),
    ];
    for (status, role, glyph) in cases {
        let task = with_mr(
            5944,
            "Fix the export",
            MergeRequest {
                pipeline: status.to_string(),
                ..merge_request("opened")
            },
        );
        let fixture = Fixture::new(vec![task], "./deploy.sh").open();
        let row = fixture.row(1);
        assert_eq!(row[3].text, glyph, "{status}");
        assert_eq!(row[3].style.role, role, "{status}");
    }
}

#[test]
fn a_deploy_row_keeps_the_priority_column_and_shows_the_deadline() {
    let task = DeployTask {
        priority: Some("1".to_string()),
        deadline: Some("2026-09-01".to_string()),
        ..with_mr(5944, "Fix the export", merge_request("opened"))
    };
    let fixture = Fixture::new(vec![task], "./deploy.sh").open();
    let row = fixture.row(1);
    assert!(text(&row).contains(" ★Fix the export"));
    assert!(text(&row).ends_with(" ⚠ 09-01"));
    assert_eq!(role_of(&row, "⚠ 09-01"), Some(Role::Danger));
}

#[test]
fn a_collapsed_project_hides_its_tasks() {
    let fixture = Fixture::new(
        vec![with_mr(5944, "Fix the export", merge_request("opened"))],
        "./deploy.sh",
    );
    let items = fixture.items();
    assert_eq!(items.len(), 1);
    assert_eq!(deploy_item_key(&items[0]), "dp:Aurora");
}
