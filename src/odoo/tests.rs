//! Ported from the behaviours `src/odoo.ts` encoded, none of which the Node app
//! had tests for: the RPC envelope, the credential rules, stage resolution and
//! the board's subtask nesting. Nothing here touches the network — the client
//! is real and points at a loopback [`stub::StubServer`].

mod board;
mod queries;
mod rpc;
mod stages;
pub(crate) mod stub;

use std::time::Duration;

use serde_json::{json, Value};

use super::{HttpTransport, OdooClient};
use crate::types::OdooCreds;
use stub::{Reply, StubServer};

pub(crate) fn creds(url: &str) -> OdooCreds {
    OdooCreds {
        url: url.to_string(),
        db: "testdb".to_string(),
        user: "tester".to_string(),
        password: "secret".to_string(),
    }
}

/// A client wired to a stub whose first reply is a successful authentication.
pub(crate) fn client_with(replies: Vec<Reply>) -> (OdooClient, StubServer) {
    let mut all = vec![Reply::result(json!(7))];
    all.extend(replies);
    let server = StubServer::start(all);
    let client = OdooClient::with_transport(
        creds(&server.url()),
        Box::new(HttpTransport::new(Duration::from_secs(5))),
    );
    (client, server)
}

/// A task record shaped the way `search_read` returns one.
pub(crate) fn record(id: i64, name: &str, project: (i64, &str), stage: (i64, &str)) -> Value {
    json!({
        "id": id,
        "name": name,
        "project_id": [project.0, project.1],
        "stage_id": [stage.0, stage.1],
        "priority": "0",
        "date_deadline": false,
        "tag_ids": [],
        "x_studio_story_points_1": false,
        "x_studio_story_points": false,
        "parent_id": false,
        "child_ids": [],
        "depend_on_ids": [],
        "depend_on_count": 0,
        "closed_depend_on_count": 0,
    })
}

/// Stage metadata as `project.task.type.search_read` returns it.
pub(crate) fn stage_rows() -> Value {
    json!([
        { "id": 1, "name": "Approved to Start", "sequence": 1 },
        { "id": 2, "name": "In Progress", "sequence": 2 },
        { "id": 3, "name": "Quality Assurance", "sequence": 3 },
    ])
}
