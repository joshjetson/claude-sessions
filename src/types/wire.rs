//! Serde shims for the two session fields whose JSON shape is the protocol's,
//! not Rust's.
//!
//! The daemon wire is the Node app's: a client written against either
//! implementation has to read the other's payloads. Two fields do not survive
//! the derived representation —
//!
//! * `sessionMtime` — `SystemTime`'s serde form is
//!   `{"secs_since_epoch":…,"nanos_since_epoch":…}`, where Node sends
//!   `stat.mtimeMs`, a number. A Node client sorted rows by it (`bt - at`) and
//!   got `NaN`; a Rust client reading a Node payload failed to deserialise the
//!   session at all, which dropped the whole tick's sessions list.
//! * `pids` — Node's scanner pulls pids out of a `ps` line with a regex, so
//!   they cross the wire as strings. Numbers are what we send (they are what
//!   they are, and JS reads either the same way); strings are what we accept.
//!
//! Both readers take either form, so a 1.0 daemon's payloads keep working.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserializer, Serializer};

/// Milliseconds since the epoch, as Node's `stat.mtimeMs` and `Date.now()`
/// report them.
pub(crate) mod epoch_ms {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(time: &SystemTime, out: S) -> Result<S::Ok, S::Error> {
        let millis = time
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or(0);
        out.serialize_u64(millis.min(u128::from(u64::MAX)) as u64)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<SystemTime, D::Error> {
        input.deserialize_any(TimeVisitor)
    }
}

/// Integer arithmetic on purpose: `Duration::from_secs_f64` on a number this
/// large loses the millisecond it was asked to preserve, and this value is
/// compared against a file's mtime by the status machine.
fn from_millis(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::new(millis / 1000, (millis % 1000) as u32 * 1_000_000)
}

struct TimeVisitor;

impl<'de> Visitor<'de> for TimeVisitor {
    type Value = SystemTime;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("epoch milliseconds, or a SystemTime object")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<SystemTime, E> {
        Ok(from_millis(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<SystemTime, E> {
        Ok(from_millis(value.max(0) as u64))
    }

    /// `stat.mtimeMs` is fractional; the sub-millisecond part is noise the
    /// whole protocol rounds away.
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<SystemTime, E> {
        Ok(from_millis(if value <= 0.0 { 0 } else { value as u64 }))
    }

    fn visit_unit<E: de::Error>(self) -> Result<SystemTime, E> {
        Ok(UNIX_EPOCH)
    }

    fn visit_none<E: de::Error>(self) -> Result<SystemTime, E> {
        Ok(UNIX_EPOCH)
    }

    /// The 1.0 daemons' form: `SystemTime`'s own serde representation.
    fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<SystemTime, M::Error> {
        let mut secs: u64 = 0;
        let mut nanos: u32 = 0;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "secs_since_epoch" => secs = map.next_value()?,
                "nanos_since_epoch" => nanos = map.next_value()?,
                _ => {
                    let _ = map.next_value::<de::IgnoredAny>()?;
                }
            }
        }
        Ok(UNIX_EPOCH + Duration::new(secs, nanos))
    }
}

/// Process ids, as numbers or as the strings Node's `ps` parser produces.
pub(crate) fn pids<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<u32>, D::Error> {
    input.deserialize_seq(PidsVisitor)
}

struct PidsVisitor;

impl<'de> Visitor<'de> for PidsVisitor {
    type Value = Vec<u32>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of process ids")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u32>, A::Error> {
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(1));
        while let Some(pid) = seq.next_element::<Pid>()? {
            match pid {
                Pid::Number(n) => out.push(n),
                // A pid that is not a number is not a pid; skipping it keeps
                // the rest of the session, which is the part worth drawing.
                Pid::Text(text) => {
                    if let Ok(n) = text.trim().parse() {
                        out.push(n);
                    }
                }
            }
        }
        Ok(out)
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Pid {
    Number(u32),
    Text(String),
}
