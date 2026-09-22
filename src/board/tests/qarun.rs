//! The QA run block on the board: where its rows go, and whether they fit.
//!
//! The width assertions are not cosmetic. An ordinary task row is already about
//! 70 plain characters at its longest, and the list pane is 133 columns on a
//! 208-column terminal at the default conversation width, 50 on an 80-column
//! one. A status column appended without thought wraps every row, and a wrapped
//! row in a list pane is unreadable.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::{board, task, text};
use crate::board::{
    board_item_key, build_board_tree, build_board_tree_with_runs, collapsed_key, format_board_item,
    stage_key, BoardCtx, BoardItem,
};
use crate::qaden::{QaRunState, QaVerdict};
use crate::qarun::{QaRun, QaStatus, RunCtx, RunMode, QA_WIDE_MIN_COLS};
use crate::types::{Notification, NotificationKind, NotificationLevel, NotificationStatus};

const STAGE: &str = "Quality Assurance";

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn state(open: usize, closed: usize) -> QaRunState {
    QaRunState {
        exists: true,
        round: 1,
        head: Some("abc1234".to_string()),
        open_gaps: open,
        closed_gaps: closed,
        dir: PathBuf::from("/qa"),
        ..QaRunState::default()
    }
}

fn question() -> Notification {
    Notification {
        id: "q".to_string(),
        title: "which environment?".to_string(),
        message: String::new(),
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: Some(4101),
        level: NotificationLevel::Warn,
        kind: NotificationKind::Question,
        run_id: String::new(),
        ts: String::new(),
        status: NotificationStatus::Unread,
    }
}

fn tasks() -> Vec<crate::types::Task> {
    vec![
        task(4101, "Summary row shows the wrong total"),
        task(4102, "Description field ignores its length cap"),
        task(4103, "Number formatting differs between panels"),
        task(4104, "Cancelling an entry leaves it locked"),
    ]
}

fn run(task_ids: Vec<i64>) -> QaRun {
    QaRun {
        id: QaRun::id_for("Aurora", STAGE),
        project_name: "Aurora".to_string(),
        stage_name: STAGE.to_string(),
        task_ids,
        started_at: String::new(),
        lane_limit: None,
        coordinator_started: false,
        spawned: Vec::new(),
        mode: RunMode::Shadow,
    }
}

fn expanded() -> HashSet<String> {
    HashSet::from([
        crate::board::project_key("Aurora"),
        stage_key("Aurora", STAGE),
    ])
}

fn ctx_at(tree_cols: u16) -> BoardCtx<'static> {
    BoardCtx {
        blink_on: false,
        now: Some(super::now()),
        tree_cols,
        ..BoardCtx::default()
    }
}

/// A run over 4101 / 4102 / 4103, leaving 4104 loose in the stage.
fn fixture() -> (crate::types::Board, [QaRun; 1], Notification) {
    (
        board(vec![(STAGE, 1, tasks())]),
        [run(vec![4101, 4102, 4103])],
        question(),
    )
}

fn run_ctx<'a>(ask: &'a Notification) -> RunCtx<'a> {
    let mut passed = state(0, 9);
    passed.verdict = Some(QaVerdict::Pass);
    RunCtx {
        run_states: HashMap::from([
            (4101, {
                let mut s = state(1, 0);
                s.round = 2;
                s
            }),
            (4102, state(6, 3)),
            (4103, passed),
        ]),
        sessions: HashMap::new(),
        asks: HashMap::from([(4101, ask)]),
        now: Some(now()),
    }
}

// ------------------------------------------------------------------ tree ----

#[test]
fn the_run_leads_its_stage() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    let stage_at = items
        .iter()
        .position(|i| matches!(i, BoardItem::Stage { .. }))
        .unwrap();
    let run_at = items
        .iter()
        .position(|i| matches!(i, BoardItem::QaRun { .. }))
        .unwrap();
    let loose_at = items
        .iter()
        .position(|i| matches!(i, BoardItem::Task { .. }))
        .unwrap();

    assert!(run_at > stage_at, "the run should sit under its stage");
    assert!(run_at < loose_at, "the run should lead the stage's tasks");
}

#[test]
fn a_task_in_the_run_is_not_also_a_loose_row() {
    // Duplicated rows would double every count the reader makes.
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    let loose: Vec<i64> = items
        .iter()
        .filter_map(|i| match i {
            BoardItem::Task { task, .. } => Some(task.id),
            _ => None,
        })
        .collect();
    assert_eq!(loose, vec![4104]);
}

#[test]
fn a_collapsed_run_still_claims_its_tasks() {
    // Otherwise collapsing makes them reappear below, which reads as the run
    // having released them.
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let mut open = expanded();
    open.insert(collapsed_key(&runs[0].id));

    let items = build_board_tree_with_runs(&board, &open, &runs, Some(&ctx));
    assert_eq!(
        items
            .iter()
            .filter(|i| matches!(i, BoardItem::QaRunTask { .. }))
            .count(),
        0
    );
    let loose: Vec<i64> = items
        .iter()
        .filter_map(|i| match i {
            BoardItem::Task { task, .. } => Some(task.id),
            _ => None,
        })
        .collect();
    assert_eq!(loose, vec![4104]);
}

#[test]
fn no_run_leaves_the_board_exactly_as_it_was() {
    let board = board(vec![(STAGE, 1, tasks())]);
    let plain = build_board_tree(&board, &expanded());
    assert!(!plain
        .iter()
        .any(|i| matches!(i, BoardItem::QaRun { .. } | BoardItem::QaRunTask { .. })));
    let ids: Vec<i64> = plain
        .iter()
        .filter_map(|i| match i {
            BoardItem::Task { task, .. } => Some(task.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![4101, 4102, 4103, 4104]);
}

#[test]
fn a_run_row_survives_its_task_leaving_the_board() {
    // A task that fails QA moves out of the stage. The run still owns it, so
    // the row has to render without a task record.
    let (board, _, ask) = fixture();
    let runs = [run(vec![4101, 999_999])];
    let mut ctx = run_ctx(&ask);
    ctx.run_states.insert(999_999, state(0, 0));

    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    let orphan = items
        .iter()
        .find(|i| matches!(i, BoardItem::QaRunTask { entry, .. } if entry.task_id == 999_999))
        .expect("the orphaned task lost its row");
    assert!(text(&format_board_item(orphan, &ctx_at(133))).contains("#999999"));
}

#[test]
fn row_keys_are_unique_so_selection_cannot_jump() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    let keys: Vec<String> = items.iter().map(board_item_key).collect();
    let unique: HashSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "duplicate row keys: {keys:?}");
}

// ----------------------------------------------------------------- width ----

#[test]
fn nothing_overflows_at_any_pane_width() {
    // 133 is a 208-column terminal at the default conversation width; 50 is an
    // 80-column one. The rest are where the breakpoint has to behave.
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    for tree_cols in [133_u16, 120, 102, 90, 89, 76, 63, 56, 50] {
        for item in &items {
            if !matches!(item, BoardItem::QaRun { .. } | BoardItem::QaRunTask { .. }) {
                continue;
            }
            let row = text(&format_board_item(item, &ctx_at(tree_cols)));
            let width = unicode_width::UnicodeWidthStr::width(row.as_str());
            assert!(
                width <= tree_cols as usize,
                "row was {width} wide in a {tree_cols}-column pane: {row}"
            );
        }
    }
}

#[test]
fn a_very_long_name_is_truncated_rather_than_wrapped() {
    let long = board(vec![(STAGE, 1, vec![task(4101, &"x".repeat(300))])]);
    let runs = [run(vec![4101])];
    let ask = question();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&long, &expanded(), &runs, Some(&ctx));
    let row = items
        .iter()
        .find(|i| matches!(i, BoardItem::QaRunTask { .. }))
        .unwrap();
    let width =
        unicode_width::UnicodeWidthStr::width(text(&format_board_item(row, &ctx_at(133))).as_str());
    assert!(width <= 133);
}

#[test]
fn the_wide_form_appears_above_the_breakpoint_and_not_below() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    let row = items
        .iter()
        .find(|i| matches!(i, BoardItem::QaRunTask { entry, .. } if entry.task_id == 4102))
        .unwrap();

    assert!(text(&format_board_item(row, &ctx_at(QA_WIDE_MIN_COLS))).contains("⠿ testing 3/9"));
    assert!(!text(&format_board_item(row, &ctx_at(QA_WIDE_MIN_COLS - 1))).contains("testing"));
}

// ------------------------------------------------------------ what it says --

fn row_for(task_id: i64, tree_cols: u16) -> String {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    let row = items
        .iter()
        .find(|i| matches!(i, BoardItem::QaRunTask { entry, .. } if entry.task_id == task_id))
        .unwrap();
    text(&format_board_item(row, &ctx_at(tree_cols)))
}

#[test]
fn an_asking_task_says_so_and_names_its_round() {
    let row = row_for(4101, 133);
    assert!(row.contains("#4101"), "{row}");
    assert!(row.contains("⏸ asks you r2"), "{row}");
}

#[test]
fn a_testing_task_shows_gap_progress() {
    assert!(row_for(4102, 133).contains("⠿ testing 3/9"));
}

#[test]
fn a_passed_task_shows_the_recorded_verdict() {
    assert!(row_for(4103, 133).contains("✓ PASS"));
}

#[test]
fn a_run_row_carries_no_leading_status_marker() {
    // The row read "○ … ✓ PASS" before this was removed — two markers
    // disagreeing about one task.
    assert!(!row_for(4103, 133).contains('○'));
}

#[test]
fn the_header_carries_the_fixed_denominator() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    let header = items
        .iter()
        .find(|i| matches!(i, BoardItem::QaRun { .. }))
        .unwrap();

    let wide = text(&format_board_item(header, &ctx_at(133)));
    assert!(wide.contains(&format!("QA RUN · {STAGE}")), "{wide}");
    assert!(wide.contains("3 tasks"), "{wide}");
    assert!(wide.contains("1 ask"), "{wide}");

    // On a narrow pane the stage name goes: it is one row above already.
    let narrow = text(&format_board_item(header, &ctx_at(76)));
    assert!(narrow.contains("QA RUN"), "{narrow}");
    assert!(!narrow.contains(STAGE), "{narrow}");
}

#[test]
fn the_status_cell_carries_its_own_colour() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    for (task_id, want) in [(4101, QaStatus::Asks), (4103, QaStatus::Pass)] {
        let row = items
            .iter()
            .find(|i| matches!(i, BoardItem::QaRunTask { entry, .. } if entry.task_id == task_id))
            .unwrap();
        let formatted = format_board_item(row, &ctx_at(133));
        let role = super::role_of(&formatted, want.glyph());
        assert!(role.is_some(), "the status cell lost its style");
    }
}

#[test]
fn a_run_row_shows_the_task_name() {
    // The regression this exists for: the row pushed the PADDING computed from
    // the name and never pushed the name itself. Every other assertion still
    // passed — the id was there, the status cell was there, and the row was
    // comfortably inside the pane because it was missing its widest column. On
    // screen it rendered as a column of bare ids at ragged indentation, since
    // the "indent" was really the leftover padding and so varied with the
    // length of the name nobody could see.
    let row = row_for(4102, 133);
    assert!(
        row.contains("Description field ignores its length cap"),
        "the task name is missing from the row: {row:?}"
    );
}

#[test]
fn every_run_row_shows_its_name_at_every_width() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    for tree_cols in [133_u16, 102, 76, 50] {
        for item in &items {
            let BoardItem::QaRunTask {
                task: Some(task), ..
            } = item
            else {
                continue;
            };
            let row = text(&format_board_item(item, &ctx_at(tree_cols)));
            // The name is truncated at narrow widths, so assert on a prefix
            // rather than the whole thing.
            let head: String = task.name.chars().take(8).collect();
            assert!(
                row.contains(&head),
                "at {tree_cols} cols the row lost its name: {row:?}"
            );
        }
    }
}

#[test]
fn the_id_column_lands_in_the_same_place_on_every_row() {
    // What made the bug obvious on screen: the ids stepped left and right by
    // the length of the missing name. They must line up.
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));

    let columns: Vec<usize> = items
        .iter()
        .filter(|i| matches!(i, BoardItem::QaRunTask { .. }))
        .map(|item| {
            let row = text(&format_board_item(item, &ctx_at(133)));
            row.find('#').expect("every run row carries an id")
        })
        .collect();

    assert!(columns.len() > 1, "need several rows to compare");
    assert!(
        columns.iter().all(|at| *at == columns[0]),
        "the id column is ragged across rows: {columns:?}"
    );
}

#[test]
#[ignore = "prints the rows for eyeballing: cargo test -- --ignored --nocapture"]
fn print_the_rows() {
    let (board, runs, ask) = fixture();
    let ctx = run_ctx(&ask);
    let items = build_board_tree_with_runs(&board, &expanded(), &runs, Some(&ctx));
    for cols in [133_u16, 76] {
        println!("\n{} cols {}", cols, "-".repeat(cols as usize - 10));
        for item in &items {
            println!("{}", text(&format_board_item(item, &ctx_at(cols))));
        }
    }
}
