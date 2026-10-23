//! The push format between [`crate::telemetry`] and the dashboard's ingest
//! endpoint (wire format v1). Both sides use these types, so they cannot
//! drift apart.
//!
//! ```json
//! {
//!   "v": 1,
//!   "instance": { "uid": "…", "component": "web", "host": "…", "version": "…", "started": 1760000000 },
//!   "metrics":  [ { "name": "http_requests_total", "kind": "counter", "labels": { "status": "2xx" }, "value": 12 } ],
//!   "events":   [ { "ts": 1760000000, "level": "warn", "message": "…" } ],
//!   "exit":     { "reason": "shutdown", "detail": "SIGTERM" }
//! }
//! ```
//!
//! Counters are totals since process start and are only sent when they
//! changed; gauges are sent every time. `exit` is only in the final push.
//! The token travels as `Authorization: Bearer <token>`.
//!
//! The `is_valid_*` functions and limits are the dashboard's rules. A broken
//! instance block rejects a whole push; a single metric or event breaking
//! them is skipped, so one bad metric does not cost the rest.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The wire format version, sent as `v`.
pub const VERSION: u8 = 1;
/// Metrics per push; clients put their process readings first, so cutting off the end keeps those.
pub const MAX_METRICS: usize = 500;
/// Events per push.
pub const MAX_EVENTS: usize = 50;
/// Labels per metric.
pub const MAX_LABELS: usize = 4;
/// Characters per label value.
pub const MAX_LABEL_VALUE: usize = 64;
/// Characters of an event message or exit detail the dashboard keeps.
pub const MAX_TEXT: usize = 1000;
/// Characters of `instance.host` the dashboard keeps.
pub const MAX_HOST: usize = 128;
/// Characters of `instance.version` the dashboard keeps.
pub const MAX_VERSION: usize = 64;
/// Largest request body the dashboard accepts.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// The dashboard refuses regular (non-final) pushes from one instance closer together than this.
pub const MIN_PUSH_GAP_SECS: u64 = 15;

/// Label name to value; sorted, so equal label sets compare and serialize equal.
pub type Labels = BTreeMap<String, String>;

/// One push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u8,
    pub instance: Instance,
    #[serde(default)]
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub events: Vec<Event>,
    /// Only in the final push of a process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Exit>,
}

/// The process a push comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    /// Unique per process; see [`is_valid_uid`].
    pub uid: String,
    /// Separates the processes of one service; see [`is_valid_component`].
    pub component: String,
    /// On Kubernetes the pod name, which a container restarted in place keeps.
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub version: String,
    /// Process start, Unix seconds.
    #[serde(default)]
    pub started: u64,
}

/// One metric value. Histograms arrive expanded into `<name>_count` /
/// `<name>_sum` counters and per-interval `<name>_p50` / `_p95` / `_max` gauges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub kind: MetricKind,
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    Counter,
    Gauge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds, by the client's clock.
    pub ts: u64,
    pub level: Level,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// How a process ended, as reported in its final push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub reason: ExitKind,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitKind {
    Shutdown,
    Exit,
    Panic,
    Watchdog,
    Memory,
    /// Also what an unknown reason from a newer client reads as.
    #[serde(other)]
    Other,
}

impl ExitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Exit => "exit",
            Self::Panic => "panic",
            Self::Watchdog => "watchdog",
            Self::Memory => "memory",
            Self::Other => "other",
        }
    }
}

/// 1-32 of `a-z`, `0-9`, `-`, `_`, starting with a letter or digit.
pub fn is_valid_component(component: &str) -> bool {
    let mut chars = component.chars();
    component.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 1-64 of ASCII letters, digits, `.`, `_`, `:`, `-`.
pub fn is_valid_uid(uid: &str) -> bool {
    (1..=64).contains(&uid.len())
        && uid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// 1-100 of ASCII letters, digits, `_`, `.`, `:`, not starting with a digit, `.` or `:`.
pub fn is_valid_metric_name(name: &str) -> bool {
    starts_like_identifier(name, 100, |c| matches!(c, '.' | ':'))
}

/// 1-32 of ASCII letters, digits, `_`, not starting with a digit.
pub fn is_valid_label_name(name: &str) -> bool {
    starts_like_identifier(name, 32, |_| false)
}

/// Whether a metric follows the rules: valid name and labels, a finite
/// value, and no negative counter.
pub fn is_valid_metric(metric: &Metric) -> bool {
    is_valid_metric_name(&metric.name)
        && metric.value.is_finite()
        && !(metric.kind == MetricKind::Counter && metric.value < 0.0)
        && metric.labels.len() <= MAX_LABELS
        && metric.labels.iter().all(|(name, value)| {
            is_valid_label_name(name) && value.chars().count() <= MAX_LABEL_VALUE
        })
}

fn starts_like_identifier(s: &str, max_len: usize, also: impl Fn(char) -> bool) -> bool {
    let mut chars = s.chars();
    s.len() <= max_len
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || also(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(name: &str, kind: MetricKind, value: f64) -> Metric {
        Metric {
            name: name.to_string(),
            kind,
            labels: Labels::new(),
            value,
        }
    }

    #[test]
    fn test_payload_round_trip_and_shape() {
        let payload = Payload {
            v: VERSION,
            instance: Instance {
                uid: "host-1-2".to_string(),
                component: "web".to_string(),
                host: "host".to_string(),
                version: "1.0".to_string(),
                started: 1_760_000_000,
            },
            metrics: vec![Metric {
                labels: Labels::from([("status".to_string(), "2xx".to_string())]),
                ..metric("http_requests_total", MetricKind::Counter, 12.0)
            }],
            events: vec![Event {
                ts: 1_760_000_001,
                level: Level::Warn,
                message: "careful".to_string(),
            }],
            exit: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["metrics"][0]["kind"], "counter");
        assert_eq!(json["metrics"][0]["labels"]["status"], "2xx");
        assert_eq!(json["events"][0]["level"], "warn");
        assert!(json.get("exit").is_none());
        let back: Payload = serde_json::from_value(json).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn test_minimal_payload_and_unlabelled_metric() {
        let json = r#"{"v":1,"instance":{"uid":"u","component":"web"},
            "metrics":[{"name":"x","kind":"gauge","value":1.5}]}"#;
        let p: Payload = serde_json::from_str(json).unwrap();
        assert!(p.events.is_empty());
        assert!(p.metrics[0].labels.is_empty());
        let out = serde_json::to_value(&p.metrics[0]).unwrap();
        assert!(out.get("labels").is_none());
    }

    #[test]
    fn test_unknown_exit_reason_is_other() {
        let exit: Exit = serde_json::from_str(r#"{"reason":"meteor"}"#).unwrap();
        assert_eq!(exit.reason, ExitKind::Other);
        assert_eq!(exit.detail, "");
        let exit: Exit = serde_json::from_str(r#"{"reason":"panic","detail":"x"}"#).unwrap();
        assert_eq!(exit.reason.as_str(), "panic");
    }

    #[test]
    fn test_component() {
        assert!(is_valid_component("web"));
        assert!(is_valid_component("bot-2_x"));
        assert!(!is_valid_component(""));
        assert!(!is_valid_component("-web"));
        assert!(!is_valid_component("Web"));
        assert!(!is_valid_component(&"a".repeat(33)));
    }

    #[test]
    fn test_uid() {
        assert!(is_valid_uid("pod-abc-123-1760000000000"));
        assert!(is_valid_uid("a.b:c_d"));
        assert!(!is_valid_uid(""));
        assert!(!is_valid_uid("a b"));
        assert!(!is_valid_uid(&"a".repeat(65)));
    }

    #[test]
    fn test_metric_and_label_names() {
        assert!(is_valid_metric_name("http_requests_total"));
        assert!(is_valid_metric_name("_x.y:z"));
        assert!(!is_valid_metric_name("1x"));
        assert!(!is_valid_metric_name(".x"));
        assert!(!is_valid_metric_name("foo-bar"));
        assert!(!is_valid_metric_name(&"a".repeat(101)));
        assert!(is_valid_label_name("status"));
        assert!(!is_valid_label_name("a.b"));
        assert!(!is_valid_label_name(&"a".repeat(33)));
    }

    #[test]
    fn test_metric_rules() {
        assert!(is_valid_metric(&metric("x", MetricKind::Counter, 0.0)));
        assert!(is_valid_metric(&metric("x", MetricKind::Gauge, -1.0)));
        assert!(!is_valid_metric(&metric("x", MetricKind::Counter, -1.0)));
        assert!(!is_valid_metric(&metric("x", MetricKind::Gauge, f64::NAN)));
        let too_many = Metric {
            labels: (0..5).map(|i| (format!("l{i}"), String::new())).collect(),
            ..metric("x", MetricKind::Gauge, 1.0)
        };
        assert!(!is_valid_metric(&too_many));
        let long_value = Metric {
            labels: Labels::from([("l".to_string(), "v".repeat(65))]),
            ..metric("x", MetricKind::Gauge, 1.0)
        };
        assert!(!is_valid_metric(&long_value));
    }
}
