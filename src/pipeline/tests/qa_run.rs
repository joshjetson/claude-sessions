//! The QA-run coordinator's prompt.
//!
//! Not a golden master: this pipeline has no Node original to be pinned
//! against. What is asserted is the four rules that make a coordinator safe,
//! because each of them is a sentence a future edit could soften without
//! anything failing.

use crate::pipeline::definitions::{QA_ROOT_VAR, RUN_ID_VAR, TASK_IDS_VAR, TRIAGE_VAR};
use crate::pipeline::resolve_pipeline;
use crate::pipeline::vars::PromptVars;

fn prompt(triage: bool) -> String {
    let mut vars = PromptVars::new(4101, "https://odoo.example/web#id=4101");
    vars.extras
        .insert(RUN_ID_VAR.to_string(), "Aurora::Quality Assurance".into());
    vars.extras
        .insert(TASK_IDS_VAR.to_string(), "4101, 4102, 4103".into());
    vars.extras.insert(QA_ROOT_VAR.to_string(), "/qa".into());
    vars.extras
        .insert(TRIAGE_VAR.to_string(), triage.to_string());
    resolve_pipeline("qa-run", None)
        .expect("qa-run pipeline")
        .build_prompt(&vars)
}

#[test]
fn the_coordinator_is_told_not_to_run_qa_itself() {
    // A session that reviews one task in depth loses the ability to watch the
    // other six.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(prompt.contains("Do NOT run /qa"), "triage={triage}");
        assert!(
            prompt.contains("do not open the application"),
            "triage={triage}"
        );
    }
}

#[test]
fn a_verdict_is_never_answered_in_either_mode() {
    // Not a shadow-mode restriction. QAden stops /qa at a human checkpoint by
    // design, and a coordinator answering it removes that checkpoint without
    // anyone deciding to.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(
            prompt.contains("you must NOT answer it"),
            "the verdict rule went missing at triage={triage}"
        );
        assert!(
            prompt.contains("not even to agree"),
            "the 'even to agree' case went missing at triage={triage}"
        );
        assert!(
            prompt.contains("--kind verdict"),
            "the verdict escalation lost its kind at triage={triage}"
        );
    }
}

#[test]
fn nothing_is_ever_posted_to_odoo() {
    for triage in [false, true] {
        assert!(prompt(triage).contains("Do NOT post anything to Odoo"));
    }
}

#[test]
fn the_verdict_is_read_from_the_record_not_from_prose() {
    let prompt = prompt(false);
    assert!(prompt.contains("run.json"));
    assert!(prompt.contains("Never infer a verdict from an agent's prose"));
}

#[test]
fn shadow_mode_records_and_answers_nothing() {
    let prompt = prompt(false);
    assert!(prompt.contains("you do NOT answer it"));
    assert!(prompt.contains("qa-shadow"));
    // The delivery command must not appear at all: its presence is the whole
    // difference between the two modes.
    assert!(
        !prompt.contains("qa-answer"),
        "shadow mode was handed the answer command"
    );
}

#[test]
fn triage_mode_answers_facts_and_escalates_judgment() {
    let prompt = prompt(true);
    assert!(
        prompt.contains("qa-answer"),
        "triage cannot answer anything"
    );
    assert!(prompt.contains("question of FACT"));
    assert!(prompt.contains("JUDGMENT CALL"));
    assert!(
        prompt.contains("When you are between the two, escalate"),
        "the tie-break went missing"
    );
}

#[test]
fn both_modes_record_before_acting() {
    // The ordering IS the measurement. An answer recorded after the real one is
    // known measures nothing, and the store refuses to overwrite precisely so
    // that this ordering cannot be quietly dropped.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(
            prompt.contains("FIRST record what you would answer"),
            "triage={triage}"
        );
        assert!(
            prompt.contains("Record BEFORE you act, never after"),
            "triage={triage}"
        );
    }
}

#[test]
fn a_refusal_is_escalated_rather_than_rephrased() {
    // The failure mode this exists to prevent: a coordinator meeting the
    // answer policy's refusal and trying different words until one gets past.
    let prompt = prompt(true);
    assert!(prompt.contains("do not rephrase it to get past the refusal"));
}

#[test]
fn the_run_is_told_its_own_task_list() {
    // A run owns its list from launch. Re-reading the stage would drop a task
    // that failed QA and moved out of it.
    let prompt = prompt(false);
    assert!(prompt.contains("4101, 4102, 4103"));
    assert!(prompt.contains("Aurora::Quality Assurance"));
}

#[test]
fn the_coordinator_is_never_told_to_spawn() {
    // Admission lives outside any model: a backstop a model can raise is not a
    // backstop.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(!prompt.contains("start the QA sessions"), "triage={triage}");
        assert!(!prompt.contains("lane"), "triage={triage}");
    }
}

// --- what the run taught us --------------------------------------------------

#[test]
fn the_coordinator_is_told_to_look_before_reporting_a_gap() {
    // Observed: it reported `note_written: null` for three tasks, three times,
    // while all three notes sat finished on disk. /qa had rewritten them
    // without updating run.json. Reading the field and stopping there cost the
    // reviewer the whole check.
    let prompt = prompt(true);
    assert!(
        prompt.contains("authority on a VERDICT and on nothing else"),
        "run.json is still treated as authoritative for everything: {prompt}"
    );
    assert!(
        prompt.contains("Never report a gap you have not looked for twice"),
        "nothing tells it to check the directory: {prompt}"
    );
}

#[test]
fn a_triage_coordinator_is_told_to_chase_what_is_missing() {
    // Reporting a malformed note leaves the work with the reviewer, which is
    // the work this session exists to take on.
    let prompt = prompt(true);
    assert!(
        prompt.contains("/rev-req"),
        "it is not told it can ask for a note to be re-run: {prompt}"
    );
    assert!(
        prompt.contains("check the file on disk to confirm it arrived"),
        "it is not told to verify the chase landed: {prompt}"
    );
}

#[test]
fn a_shadow_coordinator_chases_by_escalating_not_by_typing() {
    // Chasing is a message typed into a session. Letting shadow do it would be
    // a hole in the one guarantee shadow makes.
    let prompt = prompt(false);
    assert!(
        prompt.contains("you do not type into any session"),
        "shadow mode was told to chase directly: {prompt}"
    );
    assert!(
        !prompt.contains("qa-answer"),
        "shadow mode was handed the answer command by the chase step: {prompt}"
    );
}

#[test]
fn the_coordinator_is_forbidden_from_killing_by_pattern() {
    // `pkill -f "claude-sessions notify"` appeared in four agents' sessions in
    // one run. It matches every session's processes, so each cleanup killed the
    // others' in-flight calls and the run went quiet with nothing in any log.
    let prompt = prompt(true);
    assert!(
        prompt.contains("Never kill a process by PATTERN"),
        "nothing forbids a pattern kill: {prompt}"
    );
    for forbidden in ["pkill", "killall"] {
        assert!(
            prompt.contains(forbidden),
            "{forbidden} is not named as forbidden: {prompt}"
        );
    }
}

#[test]
fn the_chase_step_does_not_reach_for_a_command_that_refuses_it() {
    // The first version told the coordinator to chase with `qa-answer`. That
    // command refuses whenever the task has no open question — deliberately —
    // so every chase came back `Refusing to answer something of kind "info"`.
    // An instruction the code rejects by construction.
    let prompt = prompt(true);
    let chase_start = prompt
        .find("When a task owes something")
        .expect("the chase step");
    let chase_end = prompt[chase_start..]
        .find("Never kill a process")
        .map(|i| chase_start + i)
        .unwrap_or(prompt.len());
    let chase = &prompt[chase_start..chase_end];

    assert!(
        chase.contains("will REFUSE this"),
        "the chase step does not warn that qa-answer refuses: {chase}"
    );
    assert!(
        chase.contains("Do not retry it and do not reword it"),
        "nothing stops it retrying the refusal: {chase}"
    );
}

#[test]
fn the_coordinator_is_told_to_count_blockers() {
    // Nineteen blocked checks across seven tasks in one run, exactly one
    // dispositioned. The prompt did not mention blockers at all, so a task with
    // eight unaccepted ones read as finished because its `verdict` was set.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(
            prompt.contains("A recorded verdict does NOT mean a task is finished"),
            "a verdict is still treated as done (triage={triage}): {prompt}"
        );
        assert!(
            prompt.contains("accepted_blocked"),
            "it is not told where acceptance is recorded (triage={triage})"
        );
    }
}

#[test]
fn accepting_a_blocker_stays_with_the_reviewer() {
    // Same class of decision as a verdict: it closes a gap nobody proved.
    let prompt = prompt(true);
    assert!(
        prompt.contains("Do not accept a blocker yourself"),
        "nothing stops it dispositioning a blocker: {prompt}"
    );
}

#[test]
fn a_permission_refusal_blocker_is_escalated_once_and_never_retried() {
    // "Third attempt, after the reviewer's permission was relayed by a peer.
    // Still blocked." A permission prompt is a modal in this tool's own UI —
    // invisible to the coordinator and unanswerable by it, so retrying it
    // burns turns and changes nothing.
    let prompt = prompt(true);
    assert!(
        prompt.contains("never retry them"),
        "it is still free to retry a permission block: {prompt}"
    );
    assert!(
        prompt.contains("preview environment and seeding is allowed"),
        "it is not told that 'I could not create the data' is usually recoverable: {prompt}"
    );
}

#[test]
fn the_coordinator_may_not_speak_for_the_reviewer() {
    // One escalation opened "the reviewer wants this finished without waiting
    // on them" when they had said no such thing. The session refused it and was
    // right to — and then nothing could reach that session at all, because it
    // could not tell an inference from a decision.
    for triage in [false, true] {
        let prompt = prompt(triage);
        assert!(
            prompt.contains("NEVER say what the reviewer wants unless they have said it to you"),
            "it may still speak for the reviewer (triage={triage})"
        );
        assert!(
            prompt.contains("You cannot deliver a reviewer's decision"),
            "it is not told the dashboard carries decisions (triage={triage})"
        );
    }
}
