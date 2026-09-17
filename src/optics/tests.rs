//! The Node app had no test file for `optics.js`. These pin the three things
//! the board relies on: one query per project, a name that resolves the way the
//! team's convention expects, and — most importantly — that nothing here can
//! turn into a board error.

use std::sync::Mutex;

use super::*;

/// A transport that answers from a canned table and records what was asked.
#[derive(Default)]
struct StubHttp {
    answers: Vec<(String, String)>,
    calls: Mutex<Vec<String>>,
    tokens: Mutex<Vec<String>>,
    fail: Option<String>,
}

impl StubHttp {
    fn with(answers: &[(&str, &str)]) -> Self {
        StubHttp {
            answers: answers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            ..StubHttp::default()
        }
    }

    fn failing(error: &str) -> Self {
        StubHttp {
            fail: Some(error.to_string()),
            ..StubHttp::default()
        }
    }
}

impl OpticsHttp for StubHttp {
    fn get(&self, url: &str, token: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push(url.to_string());
        self.tokens.lock().unwrap().push(token.to_string());
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        self.answers
            .iter()
            .find(|(fragment, _)| url.contains(fragment.as_str()))
            .map(|(_, body)| body.clone())
            .ok_or_else(|| format!("stub has no answer for {url}"))
    }
}

const PROJECTS: &str = r#"[
  {"id": 1, "name": "Aurora Platform", "sdk_key": "aurora"},
  {"id": 2, "name": "NovaLink", "sdk_key": "novalink"},
  {"id": 3, "name": "Orbit Media Group", "sdk_key": "orbit"}
]"#;

fn client(http: StubHttp, mapping: &[(&str, &str)]) -> OpticsClient {
    OpticsClient::new(
        Box::new(http),
        "https://optics.example.com/".to_string(),
        "test-token".to_string(),
        mapping
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    )
}

fn projects() -> Vec<OpticsProject> {
    serde_json::from_str(PROJECTS).unwrap()
}

// --- name resolution --------------------------------------------------------

#[test]
fn an_exact_sdk_key_or_name_resolves() {
    let list = projects();
    let empty = HashMap::new();
    assert_eq!(
        match_project(&list, &empty, "novalink").map(|p| p.id),
        Some(2)
    );
    assert_eq!(
        match_project(&list, &empty, "Aurora Platform").map(|p| p.id),
        Some(1)
    );
}

#[test]
fn a_longer_odoo_name_containing_the_key_still_resolves() {
    // The whole reason the fuzzy pass exists: Odoo carries the full company
    // name, Optics carries a short key.
    let list = projects();
    assert_eq!(
        match_project(&list, &HashMap::new(), "Orbit Media Group Ltd").map(|p| p.sdk_key),
        Some("orbit".to_string())
    );
}

#[test]
fn an_explicit_mapping_wins_over_the_fuzzy_match() {
    let list = projects();
    let mapping: HashMap<String, String> =
        [("Aurora Platform".to_string(), "novalink".to_string())]
            .into_iter()
            .collect();
    assert_eq!(
        match_project(&list, &mapping, "Aurora Platform").map(|p| p.id),
        Some(2),
        "the configured key must beat the name that matches itself"
    );
}

#[test]
fn a_mapping_key_is_matched_case_and_punctuation_insensitively() {
    let list = projects();
    let mapping: HashMap<String, String> = [("aurora-platform".to_string(), "orbit".to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        match_project(&list, &mapping, "Aurora Platform").map(|p| p.id),
        Some(3)
    );
}

#[test]
fn a_project_nobody_records_resolves_to_nothing() {
    assert!(match_project(&projects(), &HashMap::new(), "Unrelated Thing").is_none());
    assert!(match_project(&projects(), &HashMap::new(), "").is_none());
}

// --- coverage ---------------------------------------------------------------

#[test]
fn board_coverage_is_one_query_per_project_and_counts_processes() {
    let http = StubHttp::with(&[
        ("/projects?", PROJECTS),
        (
            "/process_categories?",
            r#"[
              {"name": "Task-5944", "processes": [{"id": 1}, {"id": 2}]},
              {"name": "Task-6117", "processes": []},
              {"name": "Task-6200", "processes": [{"id": 3}]}
            ]"#,
        ),
    ]);
    let client = client(http, &[]);
    let coverage = client.project_coverage("NovaLink", &[5944, 6117, 6200]);

    assert_eq!(coverage.get(&5944), Some(&2));
    assert_eq!(coverage.get(&6200), Some(&1));
    assert_eq!(
        coverage.get(&6117),
        None,
        "a category with no processes is not coverage"
    );
}

#[test]
fn the_token_travels_as_a_header_and_the_filter_names_every_task() {
    let recorder = std::sync::Arc::new(Recorder::default());
    let client = OpticsClient::new(
        Box::new(RecordingHttp {
            inner: StubHttp::with(&[("/projects?", PROJECTS), ("/process_categories?", "[]")]),
            recorder: std::sync::Arc::clone(&recorder),
        }),
        "https://optics.example.com".into(),
        "secret-token".into(),
        HashMap::new(),
    );
    client.project_coverage("NovaLink", &[5944, 6117]);

    let calls = recorder.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "one project lookup, one category query");
    assert!(calls[1].0.contains("project_id=eq.2"), "{}", calls[1].0);
    // `name=in.("Task-5944","Task-6117")`, percent-encoded.
    assert!(calls[1].0.contains("name=in."), "{}", calls[1].0);
    assert!(calls[1].0.contains("Task-5944"), "{}", calls[1].0);
    assert!(calls[1].0.contains("Task-6117"), "{}", calls[1].0);
    assert_eq!(
        calls[1].1, "secret-token",
        "the token is a header, not a query"
    );
}

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, String)>>,
}

struct RecordingHttp {
    inner: StubHttp,
    recorder: std::sync::Arc<Recorder>,
}

impl OpticsHttp for RecordingHttp {
    fn get(&self, url: &str, token: &str) -> Result<String, String> {
        self.recorder
            .calls
            .lock()
            .unwrap()
            .push((url.to_string(), token.to_string()));
        self.inner.get(url, token)
    }
}

#[test]
fn the_project_list_is_fetched_once_and_reused() {
    let recorder = std::sync::Arc::new(Recorder::default());
    let client = OpticsClient::new(
        Box::new(RecordingHttp {
            inner: StubHttp::with(&[("/projects?", PROJECTS), ("/process_categories?", "[]")]),
            recorder: std::sync::Arc::clone(&recorder),
        }),
        "https://optics.example.com".into(),
        "t".into(),
        HashMap::new(),
    );
    client.project_coverage("NovaLink", &[1]);
    client.project_coverage("Aurora Platform", &[2]);
    let projects_queries = recorder
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(url, _)| url.contains("/projects?"))
        .count();
    assert_eq!(projects_queries, 1);
}

#[test]
fn coverage_never_surfaces_an_error_to_the_board() {
    // Every failure mode resolves to "no coverage": the badge is a nice-to-have
    // and a dashboard that will not render because Optics is down is a fault.
    let down = client(StubHttp::failing("connection refused"), &[]);
    assert!(down.project_coverage("NovaLink", &[1, 2]).is_empty());

    let nonsense = client(StubHttp::with(&[("/projects?", "not json")]), &[]);
    assert!(nonsense.project_coverage("NovaLink", &[1]).is_empty());

    let unmapped = client(StubHttp::with(&[("/projects?", PROJECTS)]), &[]);
    assert!(unmapped
        .project_coverage("Unknown Project", &[1])
        .is_empty());

    let empty = client(StubHttp::with(&[("/projects?", PROJECTS)]), &[]);
    assert!(empty.project_coverage("NovaLink", &[]).is_empty());
}

#[test]
fn task_detail_returns_the_process_list() {
    let client = client(
        StubHttp::with(&[
            ("/projects?", PROJECTS),
            (
                "/process_categories?",
                r#"[{"id": 9, "name": "Task-5944", "processes": [
                     {"id": 1, "name": "Log in", "actor": "customer", "description": "happy path"},
                     {"id": 2, "name": "Reset password", "actor": null, "description": null}
                   ]}]"#,
            ),
        ]),
        &[],
    );
    let detail = client.task_optics(5944, "NovaLink").unwrap().unwrap();
    assert_eq!(detail.project_sdk_key, "novalink");
    assert_eq!(detail.category, "Task-5944");
    assert_eq!(detail.category_id, 9);
    assert_eq!(detail.count(), 2);
    assert_eq!(detail.processes[0].name, "Log in");
    assert_eq!(detail.processes[0].actor.as_deref(), Some("customer"));
}

#[test]
fn a_category_with_no_processes_is_no_coverage_at_all() {
    let client = client(
        StubHttp::with(&[
            ("/projects?", PROJECTS),
            (
                "/process_categories?",
                r#"[{"id": 9, "name": "Task-5944", "processes": []}]"#,
            ),
        ]),
        &[],
    );
    assert!(client.task_optics(5944, "NovaLink").unwrap().is_none());
}

// --- naming -----------------------------------------------------------------

#[test]
fn the_category_convention_round_trips() {
    assert_eq!(category_name(5944), "Task-5944");
    assert_eq!(task_id_of("Task-5944"), Some(5944));
    assert_eq!(task_id_of("Task-"), None);
    assert_eq!(task_id_of("Regression sweep"), None);
    assert_eq!(task_id_of("Task-5944-extra"), None);
}

#[test]
fn query_values_are_percent_encoded() {
    assert_eq!(
        encode("\"Task-1\",\"Task-2\""),
        "%22Task-1%22%2C%22Task-2%22"
    );
}
