//! The incremental reader. The point of these tests is the cost model: a poll
//! must read the appended bytes and not one more.

use serde_json::json;

use super::{assistant_text, assistant_text_with_usage, line, user_prompt, Transcript};
use crate::transcript::{parse_session_file, Collect, TranscriptCursor};

#[test]
fn a_poll_reads_only_what_was_appended() {
    let t = Transcript::new(&(user_prompt("first") + &assistant_text("a reply")));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    let after_cold_start = cursor.total_bytes_read();
    assert_eq!(
        after_cold_start,
        std::fs::metadata(&t.path).unwrap().len(),
        "the cold start reads the file once"
    );

    let appended = user_prompt("second");
    t.append(&appended);
    let poll = cursor.poll().expect("poll");

    assert_eq!(poll.bytes_read, appended.len() as u64);
    assert_eq!(poll.lines, 1);
    assert!(poll.changed());
    assert!(!poll.reloaded);
    assert_eq!(
        cursor.total_bytes_read(),
        after_cold_start + appended.len() as u64,
        "a poll must not re-read the file"
    );
    assert_eq!(
        cursor
            .session()
            .prompts
            .iter()
            .map(|p| p.text.clone())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn an_idle_poll_reads_nothing_at_all() {
    let t = Transcript::new(&user_prompt("only"));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    let before = cursor.total_bytes_read();
    for _ in 0..5 {
        let poll = cursor.poll().expect("poll");
        assert_eq!(poll.bytes_read, 0);
        assert!(!poll.changed());
    }
    assert_eq!(cursor.total_bytes_read(), before);
}

#[test]
fn a_line_split_across_two_polls_is_reassembled() {
    let t = Transcript::new(&user_prompt("first"));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");

    let whole = user_prompt("split across writes");
    let (head, tail) = whole.split_at(whole.len() / 2);
    t.append(head);
    let poll = cursor.poll().expect("poll");
    assert_eq!(poll.lines, 0, "half a line is not an entry yet");
    assert_eq!(cursor.session().prompts.len(), 1);

    t.append(tail);
    let poll = cursor.poll().expect("poll");
    assert_eq!(poll.lines, 1);
    assert_eq!(
        cursor.session().prompts.last().expect("prompt").text,
        "split across writes"
    );
}

#[test]
fn truncation_forces_a_fresh_read() {
    let t = Transcript::new(&(user_prompt("before") + &assistant_text("reply")));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    assert_eq!(cursor.session().prompts.len(), 1);

    // `> file` in place: same path, same inode, fewer bytes. Continuing from the
    // old offset would read garbage or nothing.
    t.overwrite(&user_prompt("after truncation"));
    let poll = cursor.poll().expect("poll");
    assert!(poll.reloaded);
    assert!(poll.changed());
    assert_eq!(
        cursor
            .session()
            .prompts
            .iter()
            .map(|p| p.text.clone())
            .collect::<Vec<_>>(),
        ["after truncation"]
    );
}

/// Unix only: it is the inode that gives this away, and nothing stable hands
/// one over elsewhere — see `file_id` in the parser. A file that shrank or grew
/// is still noticed on every platform, which the tests either side of this one
/// cover.
#[cfg(unix)]
#[test]
fn a_replaced_file_of_the_same_length_is_noticed() {
    // An archived transcript restored over a live one keeps the path and can
    // keep the length; only the inode gives it away.
    let t = Transcript::new(&user_prompt("aaaaa"));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");

    let replacement = t.path.with_extension("new");
    std::fs::write(&replacement, user_prompt("bbbbb")).expect("write");
    std::fs::rename(&replacement, &t.path).expect("rename over");

    let poll = cursor.poll().expect("poll");
    assert!(poll.reloaded, "a replaced file must be re-read");
    assert_eq!(cursor.session().prompts[0].text, "bbbbb");
}

#[test]
fn a_cursor_ends_up_where_a_cold_parse_would() {
    // The two paths share one folding core; this is the test that says so.
    let mut body = user_prompt("kick-off");
    for i in 0..20 {
        body.push_str(&assistant_text_with_usage(
            &format!("turn {i}"),
            json!({ "input_tokens": i, "output_tokens": 2, "cache_read_input_tokens": 10 }),
        ));
        body.push_str(&user_prompt(&format!("prompt {i}")));
    }
    let t = Transcript::new("");
    let mut cursor =
        TranscriptCursor::open(&t.path, Collect::SessionAndConversation).expect("open");

    // Dribble it in at arbitrary boundaries, mid-line included.
    let bytes = body.as_bytes();
    let mut at = 0;
    for chunk in [7usize, 500, 3, 1200, 91].iter().cycle() {
        if at >= bytes.len() {
            break;
        }
        let end = (at + chunk).min(bytes.len());
        t.append(std::str::from_utf8(&bytes[at..end]).expect("utf8 boundary"));
        cursor.poll().expect("poll");
        at = end;
    }

    let cold = parse_session_file(&t.path).expect("cold parse");
    let incremental = cursor.session();
    assert_eq!(incremental, cold);
    assert_eq!(
        cursor.messages(),
        crate::transcript::parse_conversation(&t.path)
            .expect("cold parse")
            .as_slice()
    );
    assert_eq!(
        cursor.total_bytes_read(),
        body.len() as u64,
        "every byte read exactly once"
    );
}

#[test]
fn a_session_only_cursor_holds_no_conversation() {
    let t = Transcript::new(&(user_prompt("hi") + &assistant_text("hello")));
    let cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    assert!(
        cursor.messages().is_empty(),
        "the sessions list must not pin every message of every session"
    );
    assert_eq!(cursor.session().prompts.len(), 1);
    assert_eq!(cursor.meta().session_id, "sess-1");
}

#[test]
fn the_trailing_entry_tracks_the_newest_line() {
    let t = Transcript::new(&user_prompt("hi"));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    assert_eq!(
        cursor.last_entry().expect("last entry").kind.as_str(),
        "user"
    );

    t.append(&line(
        json!({ "type": "system", "subtype": "turn_duration" }),
    ));
    cursor.poll().expect("poll");
    let last = cursor.last_entry().expect("last entry");
    assert_eq!(last.kind.as_str(), "system");
    assert_eq!(last.subtype.as_deref(), Some("turn_duration"));
    assert_eq!(cursor.path(), t.path);
}

#[test]
fn a_missing_file_is_an_error_not_a_panic() {
    assert!(TranscriptCursor::open("/nonexistent/session.jsonl", Collect::Session).is_err());
    let t = Transcript::new(&user_prompt("hi"));
    let mut cursor = TranscriptCursor::open(&t.path, Collect::Session).expect("open");
    std::fs::remove_file(&t.path).expect("remove");
    assert!(cursor.poll().is_err());
}
