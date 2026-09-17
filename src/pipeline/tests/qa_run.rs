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
    let mut vars = PromptVars::new(6688, "https://odoo.example/web#id=6688");
    vars.extras
        .insert(RUN_ID_VAR.to_string(), "Aurora::Quality Assurance".into());
    vars.extras
        .insert(TASK_IDS_VAR.to_string(), "6688, 6685, 6681".into());
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
    assert!(prompt.contains("6688, 6685, 6681"));
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
