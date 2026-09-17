//! Shift-Q, the shutdown confirmation. Ports `test/shutdown.test.js`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::types::SessionStatus;
use crate::ui::dialogs::{Dialog, DialogCtx, DialogOutcome, ShutdownConfirm};
use crate::ui::state::Quit;
use crate::ui::tests::{session, temp_config};

fn press(
    dialog: &mut Dialog,
    code: KeyCode,
    config: &mut crate::config::ConfigHandle,
) -> DialogOutcome {
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut ctx = DialogCtx { config };
    dialog.handle_key(KeyEvent::new(code, KeyModifiers::NONE), area, &mut ctx)
}

// --- shutdown ---------------------------------------------------------------

fn shutdown_frame(dialog: &ShutdownConfirm) -> String {
    let lines = dialog.lines();
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn shutdown_says_it_is_safe_when_nothing_is_running() {
    let frame = shutdown_frame(&ShutdownConfirm::default());
    assert!(frame.contains("Stop the background daemon and quit?"));
    assert!(frame.contains("Nothing is running"));
}

#[test]
fn shutdown_warns_loudly_about_a_running_deploy_and_pluralises() {
    let one = ShutdownConfirm {
        running_deploys: vec!["alpha".into()],
        ..ShutdownConfirm::default()
    };
    let two = ShutdownConfirm {
        running_deploys: vec!["alpha".into(), "beta".into()],
        ..ShutdownConfirm::default()
    };
    assert!(shutdown_frame(&one).contains("1 deploy still running"));
    assert!(shutdown_frame(&one).contains("kills it"));
    assert!(shutdown_frame(&two).contains("2 deploys still running"));
    assert!(shutdown_frame(&two).contains("kills them"));
}

#[test]
fn shutdown_says_task_sessions_survive_because_they_do() {
    let dialog = ShutdownConfirm {
        task_sessions: vec![5944],
        ..ShutdownConfirm::default()
    };
    let frame = shutdown_frame(&dialog);
    assert!(frame.contains("1 task session being tracked"));
    assert!(frame.contains("#5944"));
    assert!(frame.contains("keep running"));
}

#[test]
fn shutdown_lists_the_running_deploys_and_never_the_finished_ones() {
    // Stopping the daemon kills its children. Warning about a deploy that
    // already exited would make the warning noise nobody reads.
    let mut deploy = crate::ui::deploy::DeploySlice::default();
    deploy.set_run(crate::types::DeployRun::started("Aurora", "./go.sh", "now"));
    deploy.set_run(crate::types::DeployRun {
        status: crate::types::DeployRunStatus::Ok,
        exit_code: Some(0),
        ..crate::types::DeployRun::started("Atlas", "./go.sh", "now")
    });
    deploy.set_run(crate::types::DeployRun {
        status: crate::types::DeployRunStatus::Fail,
        exit_code: Some(1),
        ..crate::types::DeployRun::started("NovaLink", "./go.sh", "now")
    });

    let dialog = ShutdownConfirm::default().with_deploys(deploy.running_deploys());
    let frame = shutdown_frame(&dialog);
    assert!(frame.contains("1 deploy still running"), "{frame}");
    assert!(frame.contains("Aurora"), "{frame}");
    assert!(
        !frame.contains("Atlas"),
        "a finished deploy was listed: {frame}"
    );
    assert!(
        !frame.contains("NovaLink"),
        "a failed deploy was listed: {frame}"
    );
}

#[test]
fn shift_q_asks_the_deploy_tab_what_is_running() {
    let (_dir, mut state) = crate::ui::tests::temp_state();
    state
        .deploy
        .set_run(crate::types::DeployRun::started("Aurora", "./go.sh", "now"));
    crate::ui::keys::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT),
        ratatui::layout::Rect::new(0, 0, 100, 30),
    );
    let Some(crate::ui::dialogs::Dialog::Shutdown(dialog)) = state.dialog.as_ref() else {
        panic!("no shutdown dialog");
    };
    assert_eq!(dialog.running_deploys, ["Aurora"]);
}

#[test]
fn shutdown_derives_its_task_list_from_the_live_sessions() {
    let mut linked = session("aaa", "/Users/x/dev/alpha", SessionStatus::Working);
    linked.task_id = Some(5944);
    let mut duplicate = session("bbb", "/Users/x/dev/alpha", SessionStatus::Working);
    duplicate.task_id = Some(5944);
    let plain = session("ccc", "/Users/x/dev/beta", SessionStatus::Idle);
    let sessions = [linked, duplicate, plain];
    let dialog = ShutdownConfirm::from_sessions(sessions.iter());
    assert_eq!(dialog.task_sessions, vec![5944], "deduplicated");
}

#[test]
fn shutdown_explains_what_plain_q_does_instead() {
    assert!(shutdown_frame(&ShutdownConfirm::default())
        .contains("q on its own just closes the dashboard"));
}

#[test]
fn shutdown_confirms_on_y_and_cancels_on_n_or_escape() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Shutdown(ShutdownConfirm::default());
    assert_eq!(
        press(&mut dialog, KeyCode::Char('y'), &mut config),
        DialogOutcome::Quit(Quit::ShutdownAll)
    );
    for code in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('q')] {
        let mut dialog = Dialog::Shutdown(ShutdownConfirm::default());
        assert_eq!(press(&mut dialog, code, &mut config), DialogOutcome::Close);
    }
}
