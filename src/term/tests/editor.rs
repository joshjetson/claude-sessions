//! Handing the terminal to `$EDITOR` and getting it back.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::term::{
    build_editor_argv, line_args, resolve_editor, Editor, EditorSource, SpawnPolicy,
};

/// Running a script this test just wrote into its own temp directory is the one
/// case where allowing a spawn is the point of the test. Nothing outside the
/// directory is reachable from it.
const ALLOWED: SpawnPolicy = SpawnPolicy::Allow;

fn fake_editor(script: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let bin = dir.path().join("fake-editor");
    fs::write(&bin, format!("#!/bin/sh\n{script}\n")).expect("write");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, bin)
}

fn editor_at(bin: &Path, args: &[&str]) -> Editor {
    Editor {
        cmd: bin.display().to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        source: EditorSource::Default,
    }
}

#[test]
fn visual_wins_over_editor_which_is_the_convention() {
    let editor = resolve_editor(Some("nvim"), Some("nano"));
    assert_eq!(editor.cmd, "nvim");
    assert_eq!(editor.source, EditorSource::Visual);
}

#[test]
fn falls_back_to_editor_then_to_vi() {
    assert_eq!(resolve_editor(None, Some("nano")).cmd, "nano");
    let fallback = resolve_editor(None, None);
    assert_eq!(fallback.cmd, "vi");
    assert_eq!(fallback.source, EditorSource::Default);
}

#[test]
fn keeps_arguments_so_code_w_actually_waits() {
    // Without this a GUI editor returns instantly and the dashboard repaints
    // over a window the user is still typing in.
    let editor = resolve_editor(None, Some("code -w"));
    assert_eq!(editor.cmd, "code");
    assert_eq!(editor.args, ["-w"]);
}

#[test]
fn ignores_blank_and_whitespace_only_values() {
    assert_eq!(resolve_editor(None, Some("   ")).cmd, "vi");
    assert_eq!(resolve_editor(Some(""), Some("nano")).cmd, "nano");
    assert_eq!(
        resolve_editor(Some("  "), Some("nano")).source,
        EditorSource::Editor,
        "a blank VISUAL is not the source of the editor that ran"
    );
}

#[test]
fn the_vim_family_opens_at_a_line_with_a_plus_flag() {
    assert_eq!(line_args("vim", 12), ["+12"]);
    assert_eq!(line_args("/usr/bin/nvim", 3), ["+3"]);
    assert_eq!(line_args("nano", 7), ["+7"]);
}

#[test]
fn the_vs_code_family_opens_at_a_line_with_goto() {
    assert_eq!(line_args("code", 12), ["--goto"]);
    assert_eq!(line_args("cursor", 12), ["--goto"]);
}

#[test]
fn an_unknown_editor_gets_no_flag_it_might_not_understand() {
    // Opening at the top is fine; being handed a flag it does not know is not.
    assert!(line_args("my-editor", 12).is_empty());
    assert!(line_args("subl", 12).is_empty());
}

#[test]
fn no_line_means_no_line_argument() {
    assert!(line_args("vim", 0).is_empty());
    assert!(line_args("code", 0).is_empty());
}

#[test]
fn argv_puts_editor_arguments_first_then_the_line_then_the_file() {
    let editor = Editor {
        cmd: "vim".to_string(),
        args: vec!["-R".to_string()],
        source: EditorSource::Editor,
    };
    assert_eq!(
        build_editor_argv(&editor, "/tmp/p.json", 12),
        ["-R", "+12", "/tmp/p.json"]
    );
}

#[test]
fn the_vs_code_family_takes_the_line_on_the_filename() {
    assert_eq!(
        build_editor_argv(&Editor::command("code"), "/tmp/p.json", 12),
        ["--goto", "/tmp/p.json:12"]
    );
    // Sublime takes the suffix with no flag at all.
    assert_eq!(
        build_editor_argv(&Editor::command("subl"), "/tmp/p.json", 12),
        ["/tmp/p.json:12"]
    );
}

#[test]
fn without_a_line_every_editor_just_gets_the_filename() {
    for cmd in ["vim", "code", "subl", "my-editor"] {
        assert_eq!(
            build_editor_argv(&Editor::command(cmd), "/tmp/p.json", 0),
            ["/tmp/p.json"],
            "{cmd}"
        );
    }
}

#[test]
fn runs_the_editor_against_the_file_and_reports_success() {
    let (dir, bin) = fake_editor("echo \"edited by the editor\" >> \"$1\"");
    let file = dir.path().join("pipeline.json");
    fs::write(&file, "original\n").expect("write");

    let result = crate::term::run_editor(
        &editor_at(&bin, &[]),
        &file.display().to_string(),
        0,
        ALLOWED,
    );
    assert!(result.ok, "{result:?}");
    assert!(fs::read_to_string(&file)
        .expect("read")
        .contains("edited by the editor"));
}

#[test]
fn passes_editor_arguments_before_the_filename() {
    let (dir, bin) = fake_editor("printf \"%s\" \"$*\" > \"$2\"");
    let file = dir.path().join("out.txt");
    fs::write(&file, "").expect("write");
    let path = file.display().to_string();

    crate::term::run_editor(&editor_at(&bin, &["--flag"]), &path, 0, ALLOWED);
    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        format!("--flag {path}")
    );
}

#[test]
fn a_non_zero_exit_is_reported_not_thrown() {
    let (dir, bin) = fake_editor("exit 3");
    let result = crate::term::run_editor(
        &editor_at(&bin, &[]),
        &dir.path().join("x").display().to_string(),
        0,
        ALLOWED,
    );
    assert!(!result.ok);
    assert_eq!(result.code, Some(3));
    assert!(result.error.unwrap().contains("exited with 3"));
}

#[test]
fn a_missing_editor_is_reported_so_the_dashboard_can_come_back() {
    // The important property: this returns rather than unwinding. A panic here
    // would leave the UI unmounted and the terminal half-restored.
    let result = crate::term::run_editor(
        &Editor::command("definitely-not-a-real-editor-xyz"),
        "/tmp/x",
        0,
        ALLOWED,
    );
    assert!(!result.ok);
    assert!(result.error.unwrap().contains("could not start"));
}

#[test]
fn the_spawn_policy_covers_the_editor_too() {
    let result = crate::term::run_editor(&Editor::command("vi"), "/tmp/x", 0, SpawnPolicy::Refuse);
    assert!(!result.ok);
    assert!(result
        .error
        .unwrap()
        .starts_with("Refusing to open an editor"));
}
