//! Grouping (`buildGroupedTree`) and row formatting (`treefmt.js`).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::config::Group;
use crate::types::SessionStatus;
use crate::ui::tests::{session, temp_config, usage};
use crate::ui::tree::{build_grouped_tree, format_tree_item, SessionsByProject, TreeItem};

fn by_project(sessions: Vec<crate::types::Session>) -> SessionsByProject {
    crate::ui::feed::group_sessions(sessions)
}

fn no_dirs() -> BTreeMap<String, Vec<String>> {
    BTreeMap::new()
}

fn names(items: &[TreeItem<'_>]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            TreeItem::Separator { name, .. } => format!("sep:{name}"),
            TreeItem::Project { name, .. } => format!("proj:{name}"),
            TreeItem::Session { session, .. } => format!("sess:{}", session.session_id),
            TreeItem::Inactive { name, .. } => format!("dir:{name}"),
        })
        .collect()
}

fn line_text(item: &TreeItem<'_>, config: &crate::config::ConfigHandle) -> String {
    format_tree_item(item, config)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

#[test]
fn with_no_groups_the_tree_is_a_flat_project_list() {
    let sessions = by_project(vec![
        session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle),
        session("bbb", "/Users/x/dev/beta", SessionStatus::Working),
    ]);
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &[], &dirs);
    assert_eq!(names(&items), vec!["proj:x/alpha", "proj:x/beta"]);
}

#[test]
fn an_expanded_project_emits_its_sessions() {
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/dev/alpha",
        SessionStatus::Idle,
    )]);
    let expanded: HashSet<String> = ["x/alpha".to_string()].into_iter().collect();
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &expanded, &[], &dirs);
    assert_eq!(names(&items), vec!["proj:x/alpha", "sess:aaa"]);
    assert!(matches!(items[0], TreeItem::Project { expanded: true, .. }));
}

#[test]
fn a_group_claims_the_projects_underneath_its_path() {
    let sessions = by_project(vec![
        session("aaa", "/Users/x/work/alpha", SessionStatus::Idle),
        session("bbb", "/Users/x/other/beta", SessionStatus::Idle),
    ]);
    let groups = vec![Group::new("Work", "/Users/x/work")];
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
    assert_eq!(
        names(&items),
        vec![
            "sep:Work",
            "proj:work/alpha",
            "sep:Other sessions",
            "proj:other/beta",
        ]
    );
}

#[test]
fn a_trailing_slash_on_a_group_path_still_matches() {
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work/alpha",
        SessionStatus::Idle,
    )]);
    let groups = vec![Group::new("Work", "/Users/x/work///")];
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
    assert_eq!(names(&items), vec!["sep:Work", "proj:work/alpha"]);
}

/// A config file written by hand, so the group path is read exactly as typed
/// rather than through the dialog, which expands `~` on the way in.
fn config_with_group(path: &str) -> (tempfile::TempDir, crate::config::ConfigHandle, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(dir.path());
    let json = serde_json::json!({ "groups": [{ "name": "Dev", "path": path }] });
    std::fs::write(&paths.config_path, json.to_string()).expect("write config");
    let home = paths.home.clone();
    let config =
        crate::config::ConfigHandle::load(&paths, crate::config::EnvOverrides::default());
    (dir, config, home)
}

#[test]
fn a_hand_written_tilde_group_claims_the_sessions_under_it() {
    // The live bug: `"groups": [{"path": "~/dev"}]` compared the literal `~/dev`
    // against absolute working directories, matched nothing, and rendered an
    // empty group with every session dumped into "Other sessions". Expansion
    // belongs to the config accessor, so the tree sees an absolute path.
    let (_dir, config, home) = config_with_group("~/dev");
    let native = home.join("dev").join("repo").to_string_lossy().into_owned();
    // The same directory spelled with the other separator, which is what a
    // Windows machine writes into a transcript when the config says `~/dev`.
    let slashed = format!("{}/dev/repo", home.display());

    let dirs = no_dirs();
    for cwd in [native, slashed] {
        let sessions = by_project(vec![session("aaa", &cwd, SessionStatus::Idle)]);
        let groups = config.groups();
        let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
        assert_eq!(
            names(&items).first().map(String::as_str),
            Some("sep:Dev"),
            "group did not claim {cwd}"
        );
        assert!(
            !names(&items).iter().any(|row| row == "sep:Other sessions"),
            "{cwd} fell through to the ungrouped list: {:?}",
            names(&items)
        );
    }
}

#[test]
fn a_group_is_matched_by_segment_not_by_prefix() {
    // `/Users/x/work-notes` starts with `/Users/x/work`, and a plain
    // `starts_with` claimed it for the group.
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work-notes/alpha",
        SessionStatus::Idle,
    )]);
    let groups = vec![Group::new("Work", "/Users/x/work")];
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
    assert_eq!(
        names(&items),
        vec!["sep:Work", "sep:Other sessions", "proj:work-notes/alpha"]
    );
}

#[test]
fn live_folders_are_listed_before_quiet_ones() {
    // Otherwise a running session is buried in an alphabetical list of idle
    // checkouts and scrolls out of view, which reads as a missed detection.
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work/zeta",
        SessionStatus::Working,
    )]);
    let groups = vec![Group::new("Work", "/Users/x/work")];
    let discovered: BTreeMap<String, Vec<String>> = [(
        "/Users/x/work".to_string(),
        vec!["alpha".to_string(), "zeta".to_string()],
    )]
    .into_iter()
    .collect();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &discovered);
    assert_eq!(
        names(&items),
        vec!["sep:Work", "proj:work/zeta", "dir:alpha"]
    );
}

#[test]
fn a_project_is_claimed_by_the_first_group_that_matches() {
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work/alpha",
        SessionStatus::Idle,
    )]);
    let groups = vec![
        Group::new("First", "/Users/x/work"),
        Group::new("Second", "/Users/x/work"),
    ];
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
    assert_eq!(
        names(&items),
        vec!["sep:First", "proj:work/alpha", "sep:Second"]
    );
}

#[test]
fn there_is_no_other_sessions_divider_when_every_project_is_claimed() {
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work/alpha",
        SessionStatus::Idle,
    )]);
    let groups = vec![Group::new("Work", "/Users/x/work")];
    let dirs = no_dirs();
    let items = build_grouped_tree(&sessions, &HashSet::new(), &groups, &dirs);
    assert!(!names(&items).iter().any(|n| n == "sep:Other sessions"));
}

#[test]
fn item_keys_are_stable_and_distinct_per_kind() {
    let sessions = by_project(vec![session(
        "aaa",
        "/Users/x/work/alpha",
        SessionStatus::Idle,
    )]);
    let groups = vec![Group::new("Work", "/Users/x/work")];
    let expanded: HashSet<String> = ["work/alpha".to_string()].into_iter().collect();
    let discovered: BTreeMap<String, Vec<String>> =
        [("/Users/x/work".to_string(), vec!["beta".to_string()])]
            .into_iter()
            .collect();
    let items = build_grouped_tree(&sessions, &expanded, &groups, &discovered);
    let keys: Vec<String> = items.iter().map(TreeItem::key).collect();
    assert_eq!(
        keys,
        vec!["g:Work", "p:work/alpha", "s:aaa", "i:/Users/x/work/beta"]
    );
}

#[test]
fn a_session_row_carries_its_status_dot_label_and_context_meter() {
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.last_usage = Some(usage(72_000));
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    let rendered = line_text(&item, &config);
    assert!(rendered.contains('●'), "{rendered}");
    assert!(rendered.contains("abcd"), "short id: {rendered}");
    assert!(rendered.contains("idle"), "{rendered}");
    assert!(rendered.contains("72K"), "{rendered}");
    assert!(rendered.contains("(36%)"), "context meter: {rendered}");
}

#[test]
fn a_working_session_shows_what_it_is_doing() {
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Working);
    s.activity_detail = "reading".to_string();
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    assert!(line_text(&item, &config).contains("reading..."));
}

#[test]
fn a_compacting_session_says_so() {
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Compacting);
    s.activity_detail = "compacting".to_string();
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    assert!(line_text(&item, &config).contains("compacting..."));
}

#[test]
fn a_nickname_replaces_the_short_id() {
    let (_dir, mut config) = temp_config();
    config
        .save_session_nickname("abcd1234", Some("the migration notes"))
        .expect("save nickname");
    let s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    let rendered = line_text(&item, &config);
    assert!(rendered.contains("the migration notes"), "{rendered}");
    assert!(!rendered.contains("abcd"), "{rendered}");
}

#[test]
fn a_starting_session_reads_as_new() {
    let (_dir, config) = temp_config();
    let mut s = session(
        "starting-991",
        "/Users/x/dev/alpha",
        SessionStatus::Starting,
    );
    s.starting = true;
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    let rendered = line_text(&item, &config);
    assert!(rendered.contains("new"), "{rendered}");
    assert!(rendered.contains("starting"), "{rendered}");
}

#[test]
fn a_detached_head_is_not_shown_as_a_branch() {
    let (_dir, config) = temp_config();
    let mut head = session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle);
    head.git_branch = Some("HEAD".to_string());
    let mut named = head.clone();
    named.git_branch = Some("feat/wide".to_string());
    let of = |s: &crate::types::Session| {
        line_text(
            &TreeItem::Session {
                project_name: "x/alpha",
                session: s,
            },
            &config,
        )
    };
    assert!(!of(&head).contains("HEAD"));
    assert!(of(&named).contains("feat/wide"));
}

#[test]
fn a_project_row_counts_its_sessions_and_tokens() {
    let (_dir, config) = temp_config();
    let one = TreeItem::Project {
        name: "x/alpha",
        session_count: 1,
        total_tokens: 0,
        expanded: false,
    };
    let many = TreeItem::Project {
        name: "x/alpha",
        session_count: 3,
        total_tokens: 12_400,
        expanded: true,
    };
    assert!(line_text(&one, &config).contains("1 session"));
    assert!(line_text(&one, &config).contains('▶'));
    let rendered = line_text(&many, &config);
    assert!(rendered.contains("3 sessions"), "{rendered}");
    assert!(rendered.contains("12K tokens"), "{rendered}");
    assert!(rendered.contains('▼'), "{rendered}");
}

#[test]
fn sessions_inside_a_project_are_newest_first() {
    let mut older = session("old", "/Users/x/dev/alpha", SessionStatus::Idle);
    older.last_timestamp = Some("2026-09-16T10:00:00.000Z".into());
    let mut newer = session("new", "/Users/x/dev/alpha", SessionStatus::Idle);
    newer.last_timestamp = Some("2026-09-16T18:00:00.000Z".into());
    let grouped = by_project(vec![older, newer]);
    let ids: Vec<&str> = grouped["x/alpha"]
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, vec!["new", "old"]);
}
