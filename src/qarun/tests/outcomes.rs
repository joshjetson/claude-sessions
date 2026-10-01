//! What a QA pass usually takes, from the passes before it.

use crate::qarun::outcomes::{estimate_for, estimates, Estimate, OutcomeLog, QaOutcome};

fn pass(project: &str, minutes: f64, tokens: u64) -> QaOutcome {
    QaOutcome {
        task_id: 1,
        project: project.to_string(),
        round: 1,
        verdict: "pass".to_string(),
        minutes,
        tokens,
        ..QaOutcome::default()
    }
}

#[test]
fn a_project_with_enough_passes_gets_its_own_median() {
    let outcomes = vec![
        pass("Aurora", 30.0, 100_000),
        pass("Aurora", 50.0, 300_000),
        pass("Aurora", 40.0, 160_000),
        pass("Borealis", 90.0, 900_000),
    ];
    let all = estimates(&outcomes);
    let aurora = estimate_for(&all, " aurora ").expect("an estimate");
    assert_eq!(aurora.minutes, 40.0);
    assert_eq!(aurora.tokens, 160_000);
    assert_eq!(aurora.passes, 3);
    assert!(!aurora.everywhere);
}

/// Too few passes of its own: every project's median, said to be so.
#[test]
fn a_project_with_too_few_passes_falls_back_to_every_project() {
    let outcomes = vec![
        pass("Aurora", 30.0, 100_000),
        pass("Aurora", 50.0, 300_000),
        pass("Borealis", 40.0, 160_000),
    ];
    let borealis = estimate_for(&estimates(&outcomes), "Borealis").expect("an estimate");
    assert!(borealis.everywhere);
    assert_eq!(borealis.minutes, 40.0);
    assert!(estimate_for(&estimates(&outcomes[..2]), "Aurora").is_none());
}

/// Only the recent passes count, so an estimate follows a project that got
/// faster or slower. Recent by when each finished, not by where it sits in
/// the file: a backfill writes them in folder order.
#[test]
fn only_the_recent_passes_count() {
    let at = |day: u32| format!("2026-09-{day:02}T12:00:00Z");
    let mut outcomes: Vec<QaOutcome> = (0..20)
        .map(|_| QaOutcome {
            finished_at: at(29),
            ..pass("Aurora", 20.0, 1)
        })
        .collect();
    outcomes.extend((0..30).map(|_| QaOutcome {
        finished_at: at(1),
        ..pass("Aurora", 90.0, 1)
    }));
    let aurora = estimate_for(&estimates(&outcomes), "Aurora").expect("an estimate");
    assert_eq!(aurora.minutes, 20.0);
    assert_eq!(aurora.passes, 20);
}

#[test]
fn the_text_rounds_and_says_where_it_comes_from() {
    let own = Estimate {
        minutes: 41.6,
        tokens: 162_400,
        passes: 5,
        everywhere: false,
    };
    assert_eq!(own.text(), "≈42 min, ≈162K tokens a pass");
    let big = Estimate {
        tokens: 1_480_000,
        everywhere: true,
        ..own
    };
    assert_eq!(big.text(), "≈42 min, ≈1.5M tokens a pass (all projects)");
}

#[test]
fn the_log_appends_and_reads_back_skipping_what_does_not_parse() {
    let dir = tempfile::tempdir().unwrap();
    let log = OutcomeLog::new(dir.path());
    log.append(&pass("Aurora", 30.0, 1));
    std::fs::OpenOptions::new()
        .append(true)
        .open(log.path())
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"not json\n"))
        .unwrap();
    log.append(&pass("Aurora", 40.0, 2));
    let read = log.read();
    assert_eq!(read.len(), 2);
    assert_eq!(read[1].minutes, 40.0);
}
