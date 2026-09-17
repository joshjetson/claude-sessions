//! Grouping (`buildGroupedTree`) and row formatting (`treefmt.js`).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::config::Group;
use crate::types::SessionStatus;
use crate::ui::tests::{session, temp_config, usage};
use crate::ui::tree::{
    build_grouped_tree, build_grouped_tree_with, format_tree_item, SessionsByProject, TreeItem,
};

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
    let config = crate::config::ConfigHandle::load(&paths, crate::config::EnvOverrides::default());
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
fn a_session_row_carries_its_status_dot_label_and_token_count() {
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
    // No percentage: the row carries the task number in that space now.
    assert!(!rendered.contains('%'), "{rendered}");
}

#[test]
fn a_session_with_no_process_still_renders_its_row() {
    // The transcript-only case: everything the row shows comes from the
    // transcript, and the columns a process would have fill are simply absent
    // rather than blanking the line.
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.pids.clear();
    s.tty = None;
    s.lstart = None;
    s.last_usage = Some(usage(72_000));
    let item = TreeItem::Session {
        project_name: "x/alpha",
        session: &s,
    };
    let rendered = line_text(&item, &config);
    assert!(rendered.contains("abcd"), "{rendered}");
    assert!(rendered.contains("idle"), "{rendered}");
    assert!(rendered.contains("72K"), "{rendered}");
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
fn no_session_row_draws_a_git_branch() {
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
    // The row used to end with the branch in magenta, and this test guarded the
    // one case that read badly: a detached HEAD showing as the word "HEAD".
    //
    // The column is gone entirely. The task number says the same thing in less
    // space, and every QA branch for one task looks like every other — so the
    // guard is now the stronger one: no branch at all, named or detached.
    assert!(!of(&head).contains("HEAD"), "{}", of(&head));
    assert!(!of(&named).contains("feat/wide"), "{}", of(&named));
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

// --- the context meter (Node's `treefmt.js` lines 42-49) --------------------

/// A session whose last message is far smaller than everything it has ever
/// sent: the two numbers must never be confused for one another.
fn long_running() -> crate::types::Session {
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.last_usage = Some(crate::types::Usage {
        input_tokens: Some(2),
        cache_creation_input_tokens: Some(3_000),
        cache_read_input_tokens: Some(146_000),
        output_tokens: Some(998),
    });
    s.cumulative_usage = Some(crate::types::CumulativeUsage {
        input_tokens: 196,
        cache_creation_input_tokens: 187_203,
        cache_read_input_tokens: 87_450_669,
        output_tokens: 123_415,
    });
    s
}

#[test]
fn the_context_meter_reads_the_last_message_not_the_running_total() {
    // GOLDEN: the row shows what the session is holding (input + both cache
    // counters + output of the LAST assistant message), never the cumulative
    // sum — 87M of lifetime cache reads is not one session's context.
    //
    // The percentage that made the original bug visible is gone from the row,
    // so the count itself is now the whole guard.
    let (_dir, config) = temp_config();
    let s = long_running();
    let rendered = line_text(
        &TreeItem::Session {
            project_name: "x/alpha",
            session: &s,
        },
        &config,
    );
    assert!(rendered.contains("150K"), "last-message tokens: {rendered}");
    assert!(
        !rendered.contains('%'),
        "the row carries no percentage: {rendered}"
    );
}

#[test]
fn a_session_row_shows_its_token_count_and_no_percentage() {
    // This began as the `887K (444%)` row: the token count was right and the
    // denominator was not, and the fix measured against the real window.
    //
    // The percentage is now gone from the row altogether — it was a share of a
    // fixed window, which stopped meaning anything once the header gained a real
    // usage readout, and the space carries the task number instead. The token
    // count still has to be right, so that half of the original guard stays.
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.last_usage = Some(crate::types::Usage {
        input_tokens: Some(2),
        cache_creation_input_tokens: Some(306),
        cache_read_input_tokens: Some(885_713),
        output_tokens: Some(1_013),
    });
    let rendered = line_text(
        &TreeItem::Session {
            project_name: "x/alpha",
            session: &s,
        },
        &config,
    );
    assert!(rendered.contains("887K"), "{rendered}");
    assert!(
        !rendered.contains('%'),
        "the row should carry no percentage: {rendered}"
    );
}

#[test]
fn the_token_count_is_the_same_over_the_wire_as_it_is_in_process() {
    // Both transports, one answer: the embedded struct and the one that came
    // back off the daemon's JSON must render the identical row. `wire_session`
    // strips `cumulativeUsage`, so a client that measured the wrong field would
    // read zero here and differ.
    let (_dir, config) = temp_config();
    let embedded = long_running();
    let json = serde_json::to_string(&crate::daemon::wire_session(&embedded)).unwrap();
    let wired: crate::types::Session = serde_json::from_str(&json).unwrap();
    let row = |s: &crate::types::Session| {
        line_text(
            &TreeItem::Session {
                project_name: "x/alpha",
                session: s,
            },
            &config,
        )
    };
    assert_eq!(row(&embedded), row(&wired));
    // The percentage this used to check is gone from the row, so the token
    // count carries it: a client that measured the wrong field reads zero.
    assert!(row(&wired).contains("150K"), "{}", row(&wired));
}

#[test]
fn a_session_row_shows_the_task_it_is_working() {
    // The column that replaced the context percentage, and the answer to the
    // question people actually ask of this list: "which task is that?".
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.task_id = Some(6688);
    let rendered = line_text(
        &TreeItem::Session {
            project_name: "x/alpha",
            session: &s,
        },
        &config,
    );
    assert!(rendered.contains("#6688"), "{rendered}");
}

#[test]
fn a_session_with_no_task_shows_no_task_column() {
    // Plenty legitimately have none: a hand-started session, a merge-conflict
    // run, the dashboard's own. The column is blank, not "#null".
    let (_dir, config) = temp_config();
    let s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    let rendered = line_text(
        &TreeItem::Session {
            project_name: "x/alpha",
            session: &s,
        },
        &config,
    );
    assert!(!rendered.contains('#'), "{rendered}");
}

#[test]
fn the_status_is_the_last_thing_on_the_row() {
    // Status is the only field whose text changes length constantly. With it at
    // the end, only its own tail moves and the columns you read stay put.
    let (_dir, config) = temp_config();
    let mut s = session("abcd1234", "/Users/x/dev/alpha", SessionStatus::Idle);
    s.task_id = Some(6688);
    s.last_usage = Some(crate::types::Usage {
        input_tokens: Some(72_000),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        output_tokens: None,
    });
    let rendered = line_text(
        &TreeItem::Session {
            project_name: "x/alpha",
            session: &s,
        },
        &config,
    );
    let at = |needle: &str| {
        rendered
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from {rendered}"))
    };
    assert!(at("abcd") < at("#6688"), "{rendered}");
    assert!(at("#6688") < at("72K"), "{rendered}");
    assert!(at("72K") < at("idle"), "{rendered}");
    assert!(rendered.trim_end().ends_with("idle"), "{rendered}");
}

#[test]
#[ignore = "prints the rows: cargo test --lib -- --ignored --nocapture ui::tests::tree::print_session_rows"]
fn print_session_rows() {
    let (_dir, config) = temp_config();
    let mut rows = Vec::new();
    for (id, task, tokens, status) in [
        ("cbc3", Some(6688_i64), 133_000_u64, SessionStatus::Working),
        ("9280", Some(6685), 118_000, SessionStatus::Working),
        ("6c92", None, 116_000, SessionStatus::Idle),
        ("98f4", Some(6673), 532_000, SessionStatus::Awaiting),
    ] {
        let mut s = session(id, "/Users/x/dev/alpha", status);
        s.task_id = task;
        s.last_usage = Some(crate::types::Usage {
            input_tokens: Some(tokens),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            output_tokens: None,
        });
        rows.push(s);
    }
    for s in &rows {
        println!(
            "{}",
            line_text(
                &TreeItem::Session {
                    project_name: "x/alpha",
                    session: s
                },
                &config
            )
        );
    }
}

#[test]
fn quiet_folders_are_hidden_by_default() {
    // A group of eighteen checkouts drew eighteen grey rows and buried the two
    // projects actually running.
    let mut sessions: SessionsByProject = BTreeMap::new();
    sessions.insert(
        "x/alpha".to_string(),
        vec![session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle)],
    );
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/Users/x/dev".to_string(),
        vec![
            "alpha".to_string(),
            "bravo".to_string(),
            "charlie".to_string(),
        ],
    );
    let groups = vec![Group::new("Dev", "/Users/x/dev")];

    let hidden = build_grouped_tree_with(&sessions, &HashSet::new(), &groups, &dirs, false);
    assert!(
        !hidden
            .iter()
            .any(|item| matches!(item, TreeItem::Inactive { .. })),
        "quiet folders should be hidden by default"
    );
    // …and the project that IS running is still there.
    assert!(hidden
        .iter()
        .any(|item| matches!(item, TreeItem::Project { name, .. } if name == &"x/alpha")));
}

#[test]
fn quiet_folders_come_back_when_asked_for() {
    // They are the only way to launch into a repo that is currently quiet, so
    // the toggle has to actually restore them.
    let mut sessions: SessionsByProject = BTreeMap::new();
    sessions.insert(
        "x/alpha".to_string(),
        vec![session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle)],
    );
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "/Users/x/dev".to_string(),
        vec![
            "alpha".to_string(),
            "bravo".to_string(),
            "charlie".to_string(),
        ],
    );
    let groups = vec![Group::new("Dev", "/Users/x/dev")];

    let shown = build_grouped_tree_with(&sessions, &HashSet::new(), &groups, &dirs, true);
    let quiet: Vec<&str> = shown
        .iter()
        .filter_map(|item| match item {
            TreeItem::Inactive { name, .. } => Some(*name),
            _ => None,
        })
        .collect();
    // `alpha` has a live session, so it is a project row and not a quiet folder.
    assert_eq!(quiet, ["bravo", "charlie"]);
}
