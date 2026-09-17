//! Finding a task's transcript: one folder, its ancestors, or anywhere.

use super::*;

#[test]
fn the_folder_search_takes_the_transcript_that_names_the_task() {
    // Sibling tasks worked in the same repo folder are what the process scanner
    // cross-links; the transcript head is what tells them apart.
    let mut t = open();
    t.transcript(Some(5600), "/repo/portal");
    let ours = t.aged(Some(5599), "/repo/portal", HOUR);

    let found = t.find_in("/repo/portal", 5599).expect("nothing found");
    assert_eq!(found.path(), ours.path());
    assert_eq!(t.find_in("/repo/portal", 4033), None);
    assert_eq!(t.find_in("", 5599), None);
}

#[test]
fn the_folder_search_takes_the_newest_of_this_tasks_transcripts() {
    let mut t = open();
    t.aged(Some(6688), "/repo/portal", HOUR);
    let newest = t.transcript(Some(6688), "/repo/portal");
    assert_eq!(
        t.find_in("/repo/portal", 6688).map(|f| f.file.path),
        Some(newest.file.path)
    );
}

#[test]
fn the_ancestor_search_finds_the_repo_from_a_subfolder() {
    // An agent that ran `cd` into a subfolder before finishing reports a cwd no
    // session directory matches — Claude keys them by exact path.
    //
    // Rooted for the platform: the search absolutises before it encodes.
    let repo = rooted("/repo/portal");
    let mut t = open();
    let own = t.transcript(Some(4033), &repo);

    let subfolder = std::path::Path::new(&repo)
        .join("grails-app")
        .join("assets");
    let found = t
        .find_near(&subfolder.to_string_lossy(), 4033)
        .expect("nothing found");
    assert_eq!(found.path(), own.path());
    assert_eq!(
        found.cwd, repo,
        "reported the directory it was handed, not the one it found"
    );
}

#[test]
fn the_ancestor_search_stops_after_four_levels() {
    let mut t = open();
    t.transcript(Some(4033), "/repo/portal");
    assert_eq!(t.find_near("/repo/portal/a/b/c/d", 4033), None);
}

#[test]
fn the_ancestor_search_never_walks_past_home() {
    // Every task in every repo would otherwise share whatever transcripts were
    // found in the directory above home.
    let mut t = open();
    let above_home = t
        .paths
        .home
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    t.transcript(Some(4033), &above_home);
    let start = t.paths.home.join("a").to_string_lossy().into_owned();
    assert_eq!(t.find_near(&start, 4033), None);
}

#[test]
fn the_global_search_ignores_the_recorded_folder() {
    // A QA round relocates the work to its own folder, which Claude keys as its
    // own project directory and which is no parent of the repo. One task's
    // later rounds lived there and the folder searches could not see them, so
    // the archive stayed on round 1.
    let mut t = open();
    let moved = t.transcript(Some(970110), "/Users/nobody/Desktop/QAden/task-970110-qa");

    let found = t.find_anywhere(970110).expect("nothing found");
    assert_eq!(found.path(), moved.path());
    assert_eq!(
        found.cwd, "/Users/nobody/Desktop/QAden/task-970110-qa",
        "did not read the working directory out of the transcript"
    );
}

#[test]
fn the_global_search_takes_the_newest_when_a_task_has_several() {
    let mut t = open();
    t.aged(Some(970111), "/repo/commonwealth", HOUR);
    let newest = t.transcript(Some(970111), "/Users/nobody/Desktop/QAden/task-970111-qa");
    assert_eq!(
        t.find_anywhere(970111).map(|f| f.file.path),
        Some(newest.file.path)
    );
}

#[test]
fn the_global_search_never_returns_another_tasks_transcript() {
    let mut t = open();
    t.transcript(Some(970112), "/repo/commonwealth");
    assert_eq!(t.find_anywhere(970113), None);
}

#[test]
fn the_global_search_only_reads_the_transcript_store() {
    // A transcript outside ~/.claude/projects is not a session Claude can
    // resume, whoever put it there.
    let mut t = open();
    t.stray_transcript(Some(970115));
    assert_eq!(t.find_anywhere(970115), None);
}
