//! Flattening the board, and the keys rows are remembered by.

use std::collections::HashSet;

use crate::board::{
    board_item_key, build_board_tree, project_key, stage_key, subtask_key, BoardItem,
};

use super::{board, task};

fn expanded(keys: &[String]) -> HashSet<String> {
    keys.iter().cloned().collect()
}

#[test]
fn a_collapsed_project_is_one_row() {
    let board = board(vec![(
        "In Progress",
        2,
        vec![task(1, "One"), task(2, "Two")],
    )]);
    let items = build_board_tree(&board, &HashSet::new());
    assert_eq!(items.len(), 1);
    match &items[0] {
        BoardItem::Project {
            name,
            task_count,
            expanded,
            ..
        } => {
            assert_eq!(*name, "Aurora");
            assert_eq!(*task_count, 2);
            assert!(!expanded);
        }
        other => panic!("expected a project row, got {other:?}"),
    }
}

#[test]
fn stages_come_out_in_sequence_order_then_by_name() {
    let board = board(vec![
        ("Quality Assurance", 3, vec![task(1, "One")]),
        ("Approved to Start", 1, vec![task(2, "Two")]),
        ("Also Third", 3, vec![task(3, "Three")]),
    ]);
    let items = build_board_tree(&board, &expanded(&[project_key("Aurora")]));
    let stages: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            BoardItem::Stage { stage_name, .. } => Some(*stage_name),
            _ => None,
        })
        .collect();
    assert_eq!(
        stages,
        vec!["Approved to Start", "Also Third", "Quality Assurance"]
    );
}

#[test]
fn tasks_appear_only_under_an_expanded_stage() {
    let board = board(vec![("In Progress", 2, vec![task(5944, "Fix the export")])]);
    let collapsed = build_board_tree(&board, &expanded(&[project_key("Aurora")]));
    assert!(!collapsed
        .iter()
        .any(|item| matches!(item, BoardItem::Task { .. })));

    let open = build_board_tree(
        &board,
        &expanded(&[project_key("Aurora"), stage_key("Aurora", "In Progress")]),
    );
    assert!(matches!(open[2], BoardItem::Task { task, .. } if task.id == 5944));
}

#[test]
fn subtasks_appear_only_when_their_parent_is_expanded() {
    let mut parent = task(5944, "Parent");
    parent.subtasks = vec![task(8801, "Child one"), task(8802, "Child two")];
    let board = board(vec![("In Progress", 2, vec![parent])]);

    let keys = vec![project_key("Aurora"), stage_key("Aurora", "In Progress")];
    let closed = build_board_tree(&board, &expanded(&keys));
    match &closed[2] {
        BoardItem::Task {
            sub_count,
            sub_expanded,
            ..
        } => {
            assert_eq!(*sub_count, 2);
            assert!(!sub_expanded);
        }
        other => panic!("expected a task row, got {other:?}"),
    }
    assert_eq!(closed.len(), 3);

    let mut with_subs = keys.clone();
    with_subs.push(subtask_key(5944));
    let open = build_board_tree(&board, &expanded(&with_subs));
    let subs: Vec<i64> = open
        .iter()
        .filter_map(|item| match item {
            BoardItem::Subtask { task, parent_id } => {
                assert_eq!(*parent_id, 5944);
                Some(task.id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(subs, vec![8801, 8802]);
}

#[test]
fn a_task_with_no_subtasks_never_expands() {
    let board = board(vec![("In Progress", 2, vec![task(5944, "Alone")])]);
    let keys = vec![
        project_key("Aurora"),
        stage_key("Aurora", "In Progress"),
        subtask_key(5944),
    ];
    let items = build_board_tree(&board, &expanded(&keys));
    assert!(matches!(
        items[2],
        BoardItem::Task {
            sub_expanded: false,
            ..
        }
    ));
}

#[test]
fn an_empty_board_is_no_rows() {
    assert!(build_board_tree(&crate::types::Board::default(), &HashSet::new()).is_empty());
}

#[test]
fn keys_identify_a_row_across_refreshes() {
    let notif = crate::types::Notification {
        id: "n1".to_string(),
        title: String::new(),
        message: String::new(),
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: None,
        level: crate::types::NotificationLevel::Info,
        ts: String::new(),
        status: crate::types::NotificationStatus::Unread,
    };
    let one = task(5944, "One");
    let cases: Vec<(BoardItem<'_>, &str)> = vec![
        (
            BoardItem::Project {
                name: "Aurora",
                project_id: 3,
                task_count: 1,
                expanded: false,
            },
            "bp:Aurora",
        ),
        (
            BoardItem::Stage {
                project_name: "Aurora",
                stage_name: "In Progress",
                stage_id: 2,
                count: 1,
                expanded: false,
            },
            "bs:Aurora:In Progress",
        ),
        (
            BoardItem::Task {
                task: &one,
                project_name: "Aurora",
                stage_name: "In Progress",
                sub_count: 0,
                sub_expanded: false,
            },
            "bt:5944",
        ),
        (
            BoardItem::Subtask {
                task: &one,
                parent_id: 1,
            },
            "st:5944",
        ),
        (BoardItem::Info { name: "empty" }, "info:empty"),
        (BoardItem::Separator, "nsep"),
        (
            BoardItem::NotificationHeader {
                unread: 1,
                total: 2,
            },
            "nhdr",
        ),
        (BoardItem::Notification { notif: &notif }, "n:n1"),
    ];
    for (item, expected) in cases {
        assert_eq!(board_item_key(&item), expected);
    }
}
