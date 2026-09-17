//! The three dialogs the extras phase adds: the daily-log browser (`l`), the
//! purge confirmation (`X`) and the auto-dev-daemon's run logs (`D`).
//!
//! The smoke matrix in [`super::dialogs`] already renders each of them; these
//! are the behaviours that matter — which day is shown, what a purge would
//! actually close, and that neither dialog signals a process itself.

use crossterm::event::KeyCode;

use crate::config::ConfigHandle;
use crate::paths::Paths;
use crate::ui::dialogs::{DaemonLogs, Dialog, DialogCtx, DialogOutcome, LogViewer, PurgeConfirm};
use crate::ui::state::Action;
use crate::ui::tests::dialogs::{key, press, temp_config_paths};
use crate::ui::tests::{render_area, temp_config, text};

/// A runtime tree with two days of standup log in it.
fn log_paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    std::fs::create_dir_all(&paths.logs_dir).unwrap();
    std::fs::write(
        paths.logs_dir.join("2026-09-15.md"),
        "# Daily log — 2026-09-15\n\n\
         - **#5944 Portal login** — Moved the guard after session hydration.  _(09:23 AM)_\n",
    )
    .unwrap();
    std::fs::write(
        paths.logs_dir.join("2026-09-16.md"),
        "# Daily log — 2026-09-16\n\n\
         - **#6117 Invoice totals** — Round once at the total. ([MR](https://example.com/g/r/-/merge_requests/12))  _(02:10 PM)_\n",
    )
    .unwrap();
    (dir, paths)
}

#[test]
fn the_log_viewer_opens_on_the_newest_day_and_arrows_walk_back() {
    let (_dir, paths) = log_paths();
    let mut config = ConfigHandle::load(&paths, crate::config::EnvOverrides::default());
    let mut dialog = Dialog::LogViewer(LogViewer::open(&paths, None, "2026-09-16"));

    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("2026-09-16"), "{painted}");
    assert!(painted.contains("#6117"), "{painted}");
    assert!(painted.contains("Round once at the total."), "{painted}");
    assert!(
        painted.contains("MR !12"),
        "the MR number is shown: {painted}"
    );

    press(&mut dialog, KeyCode::Left, &mut config);
    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("2026-09-15"), "{painted}");
    assert!(painted.contains("#5944"), "{painted}");
    assert!(painted.contains("(1/2)"), "the day counter: {painted}");
}

#[test]
fn the_log_viewer_reads_each_day_once_rather_than_per_keystroke() {
    // Brief §10 mandate #8: Node re-read and re-parsed the file inside the
    // render function, so holding `j` re-parsed it once per frame.
    let (_dir, paths) = log_paths();
    let mut viewer = LogViewer::open(&paths, None, "2026-09-16");
    let after_open = viewer.reads();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut ctx = DialogCtx {
        config: &mut ConfigHandle::load(&paths, crate::config::EnvOverrides::default()),
    };
    for _ in 0..10 {
        viewer.handle_key(key(KeyCode::Down), area, &mut ctx);
    }
    assert_eq!(viewer.reads(), after_open, "the day was re-parsed");

    viewer.handle_key(key(KeyCode::Left), area, &mut ctx);
    assert_eq!(viewer.reads(), after_open + 1, "a new day must be read");
}

#[test]
fn the_log_viewer_creates_todays_file_so_o_has_something_to_open() {
    let (_dir, paths) = log_paths();
    let mut config = ConfigHandle::load(&paths, crate::config::EnvOverrides::default());
    let mut dialog = Dialog::LogViewer(LogViewer::open(&paths, None, "2026-09-17"));
    assert!(paths.logs_dir.join("2026-09-17.md").exists());
    match press(&mut dialog, KeyCode::Char('o'), &mut config) {
        DialogOutcome::Act(Action::OpenPath(path)) => assert!(path.ends_with("2026-09-17.md")),
        other => panic!("expected an open action, got {other:?}"),
    }
}

#[test]
fn the_purge_dialog_lists_both_buckets_and_space_toggles_review() {
    let (_dir, mut config) = temp_config();
    let target = |task_id: i64| crate::purge::PurgeTarget {
        session_id: format!("s-{task_id}"),
        pids: vec![1],
        tty: Some("ttys001".into()),
        task_id: Some(task_id),
    };
    let stages = [
        (6440, "QA".to_string()),
        (9001, "Deployed".to_string()),
        (4033, "In Progress".to_string()),
    ]
    .into_iter()
    .collect();
    let mut dialog = Dialog::PurgeConfirm(PurgeConfirm::new(
        vec![target(6440), target(9001), target(4033)],
        stages,
    ));

    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("Close 1 finished session"), "{painted}");
    assert!(painted.contains("Deployed"), "{painted}");
    assert!(painted.contains("Keeping 2"), "{painted}");
    assert!(painted.contains("still working"), "{painted}");
    assert!(painted.contains("Space to purge those too"), "{painted}");

    press(&mut dialog, KeyCode::Char(' '), &mut config);
    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("Close 2 finished session"), "{painted}");
    assert!(painted.contains("INCLUDED"), "{painted}");
}

#[test]
fn the_purge_dialog_hands_back_a_plan_rather_than_killing_anything() {
    // Brief §10 mandate #9 again: the draw thread never signals a process.
    let (_dir, mut config) = temp_config();
    let stages = [(9001, "Deployed".to_string())].into_iter().collect();
    let mut dialog = Dialog::PurgeConfirm(PurgeConfirm::new(
        vec![crate::purge::PurgeTarget {
            session_id: "s-9001".into(),
            pids: vec![4242],
            tty: Some("ttys002".into()),
            task_id: Some(9001),
        }],
        stages,
    ));
    match press(&mut dialog, KeyCode::Enter, &mut config) {
        DialogOutcome::Act(Action::Purge(entries)) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].target.pids, vec![4242]);
        }
        other => panic!("expected a purge action, got {other:?}"),
    }
}

#[test]
fn the_purge_dialog_waits_for_the_stages_it_asked_for() {
    let (_dir, mut config) = temp_config();
    let mut dialog = PurgeConfirm::new(
        vec![crate::purge::PurgeTarget {
            session_id: "s-7777".into(),
            pids: vec![1],
            tty: None,
            task_id: Some(7777),
        }],
        Default::default(),
    );
    assert_eq!(dialog.missing_stages(), vec![7777]);
    // Enter while the lookup is outstanding closes rather than purging on half
    // an answer.
    let mut wrapped = Dialog::PurgeConfirm(dialog.clone());
    assert_eq!(
        press(&mut wrapped, KeyCode::Enter, &mut config),
        DialogOutcome::Close
    );

    assert!(dialog.accept(&crate::ui::actions::BoardData::TaskStages(
        [(7777, "Deployed".to_string())].into_iter().collect()
    )));
    assert_eq!(dialog.plan().purge.len(), 1);
}

#[test]
fn the_daemon_log_dialog_says_when_there_is_nothing_to_show() {
    let (_dir, config, paths) = temp_config_paths();
    let mut dialog = Dialog::DaemonLogs(DaemonLogs::open(&paths.auto_dev_runs_dir, 5944, &[]));
    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("No auto-dev-daemon run logs"), "{painted}");
}

#[test]
fn the_daemon_log_dialog_lists_runs_and_opens_one() {
    let (_dir, config, paths) = temp_config_paths();
    let mut config = config;
    std::fs::create_dir_all(&paths.auto_dev_runs_dir).unwrap();
    std::fs::write(
        paths.auto_dev_runs_dir.join("implement-5944-20260916.log"),
        "started\nfinished\n",
    )
    .unwrap();
    let tags = vec!["auto_implemented".to_string()];
    let mut dialog = Dialog::DaemonLogs(DaemonLogs::open(&paths.auto_dev_runs_dir, 5944, &tags));

    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("implement"), "{painted}");
    assert!(
        painted.contains("implemented (MR open)"),
        "the auto-dev state is in the title: {painted}"
    );

    press(&mut dialog, KeyCode::Enter, &mut config);
    let painted = text(&render_area(100, 30, |frame, area| {
        dialog.render(frame, area, &config)
    }));
    assert!(painted.contains("finished"), "the log body: {painted}");

    // Esc goes back to the list, not out of the dialog — you are usually
    // comparing two runs.
    assert_eq!(
        press(&mut dialog, KeyCode::Esc, &mut config),
        DialogOutcome::Stay
    );
    assert_eq!(
        press(&mut dialog, KeyCode::Esc, &mut config),
        DialogOutcome::Close
    );
}
