//! The daemon's machine-health reading, behind `qa.healthGate`.

use serde_json::json;

use super::*;
use crate::daemon::EngineEvent;
use crate::health::{HealthLevel, Vitals};

fn scripted(level: HealthLevel) -> super::super::engine::HealthHook {
    Box::new(move || Vitals {
        level,
        sampled_at: "2026-09-30T12:00:00Z".to_string(),
        ..Vitals::default()
    })
}

#[test]
fn with_the_gate_on_a_reading_is_kept_published_and_in_the_snapshot() {
    let harness = engine_with(Setup {
        config: Some(json!({ "qa": { "healthGate": true } })),
        health: Some(scripted(HealthLevel::Amber)),
        ..Setup::default()
    });
    let events = harness.engine.subscribe();
    harness.inner().read_health();

    assert_eq!(
        harness.state().health.as_ref().map(|v| v.level),
        Some(HealthLevel::Amber)
    );
    assert!(events
        .try_iter()
        .any(|event| matches!(event, EngineEvent::Health(v) if v.level == HealthLevel::Amber)));
    assert_eq!(
        harness.engine.snapshot().health.map(|v| v.level),
        Some(HealthLevel::Amber)
    );
}

/// Off by default: nothing is read, and a reading from when it was on goes.
#[test]
fn with_the_gate_off_nothing_is_read_and_an_old_reading_goes() {
    let harness = engine_with(Setup {
        health: Some(scripted(HealthLevel::Red)),
        ..Setup::default()
    });
    harness.state().health = Some(Vitals::default());
    let events = harness.engine.subscribe();
    harness.inner().read_health();

    assert!(harness.state().health.is_none());
    assert_eq!(events.try_iter().count(), 0);
}
