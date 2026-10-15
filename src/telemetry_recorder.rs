//! The `metrics` recorder behind [`crate::telemetry`]: stores whatever the
//! service records through `metrics::counter!` / `gauge!` / `histogram!`, and
//! turns it into the metric list of a push.

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, Label, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::registry::{AtomicStorage, Registry};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// One metric value in a push. Histograms are already expanded into
/// `<name>_count` / `<name>_sum` counters and per-interval `<name>_p50` /
/// `_p95` / `_max` gauges.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MetricValue {
    pub name: String,
    pub kind: MetricKind,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "labels_as_map"
    )]
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    Counter,
    Gauge,
}

fn labels_as_map<S: serde::Serializer>(
    labels: &[(String, String)],
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(labels.iter().map(|(k, v)| (k, v)))
}

/// A `metrics::Recorder` that keeps everything in memory for the next push.
///
/// Labels named in `ignored_labels` are dropped when a metric is registered,
/// so every value of such a label shares one series: counters add up, and
/// histograms pool their observations. That keeps label sets that are fine
/// for Prometheus (eg one per API route) from flooding the dashboard.
#[derive(Clone)]
pub struct TelemetryRecorder {
    registry: Arc<Registry<Key, AtomicStorage>>,
    ignored_labels: Arc<HashSet<String>>,
}

impl std::fmt::Debug for TelemetryRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryRecorder")
            .field("ignored_labels", &self.ignored_labels)
            .finish_non_exhaustive()
    }
}

impl TelemetryRecorder {
    pub(crate) fn new(ignored_labels: HashSet<String>) -> Self {
        Self {
            registry: Arc::new(Registry::atomic()),
            ignored_labels: Arc::new(ignored_labels),
        }
    }

    fn reduced_key(&self, key: &Key) -> Key {
        if self.ignored_labels.is_empty() {
            return key.clone();
        }
        let labels: Vec<Label> = key
            .labels()
            .filter(|l| !self.ignored_labels.contains(l.key()))
            .cloned()
            .collect();
        Key::from_parts(key.name().to_string(), labels)
    }

    /// Current values. Histogram observations are drained, so each snapshot
    /// sees those made since the previous one; `histogram_totals` carries the
    /// running count and sum per histogram between snapshots.
    pub(crate) fn snapshot(
        &self,
        histogram_totals: &mut HashMap<Key, (f64, f64)>,
    ) -> Vec<MetricValue> {
        let mut ret = Vec::new();
        self.registry.visit_counters(|key, counter| {
            ret.push(metric(
                key.name(),
                key,
                MetricKind::Counter,
                counter.load(Ordering::Relaxed) as f64,
            ));
        });
        self.registry.visit_gauges(|key, gauge| {
            ret.push(metric(
                key.name(),
                key,
                MetricKind::Gauge,
                f64::from_bits(gauge.load(Ordering::Relaxed)),
            ));
        });
        self.registry.visit_histograms(|key, histogram| {
            let mut values = Vec::new();
            histogram.clear_with(|chunk| values.extend_from_slice(chunk));
            let totals = histogram_totals.entry(key.clone()).or_insert((0.0, 0.0));
            totals.0 += values.len() as f64;
            totals.1 += values.iter().sum::<f64>();
            ret.extend(expand_histogram(key, *totals, &mut values));
        });
        ret
    }
}

fn metric(name: &str, key: &Key, kind: MetricKind, value: f64) -> MetricValue {
    MetricValue {
        name: name.to_string(),
        kind,
        labels: key
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect(),
        value,
    }
}

/// `<name>_count` and `<name>_sum` (running totals), plus `<name>_p50`,
/// `_p95` and `_max` of this interval's observations when there were any.
fn expand_histogram(key: &Key, (count, sum): (f64, f64), values: &mut [f64]) -> Vec<MetricValue> {
    let name = key.name();
    let mut ret = vec![
        metric(&format!("{name}_count"), key, MetricKind::Counter, count),
        metric(&format!("{name}_sum"), key, MetricKind::Counter, sum),
    ];
    values.sort_by(f64::total_cmp);
    for (suffix, q) in [("p50", 0.5), ("p95", 0.95), ("max", 1.0)] {
        if let Some(v) = quantile(values, q) {
            ret.push(metric(
                &format!("{name}_{suffix}"),
                key,
                MetricKind::Gauge,
                v,
            ));
        }
    }
    ret
}

/// Nearest-rank quantile of sorted values.
fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.saturating_sub(1)).copied()
}

impl Recorder for TelemetryRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        self.registry
            .get_or_create_counter(&self.reduced_key(key), |c| Counter::from_arc(c.clone()))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        self.registry
            .get_or_create_gauge(&self.reduced_key(key), |g| Gauge::from_arc(g.clone()))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        self.registry
            .get_or_create_histogram(&self.reduced_key(key), |h| Histogram::from_arc(h.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(values: &'a [MetricValue], name: &str) -> Option<&'a MetricValue> {
        values.iter().find(|m| m.name == name)
    }

    #[test]
    fn test_counters_gauges_and_histograms() {
        let recorder = TelemetryRecorder::new(HashSet::new());
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("requests_total", "status" => "2xx").increment(3);
            metrics::gauge!("queue").set(7.5);
            for v in [1.0, 2.0, 3.0, 4.0] {
                metrics::histogram!("latency").record(v);
            }
        });
        let mut totals = HashMap::new();
        let snap = recorder.snapshot(&mut totals);

        let requests = find(&snap, "requests_total").unwrap();
        assert_eq!(requests.kind, MetricKind::Counter);
        assert_eq!(requests.value, 3.0);
        assert_eq!(
            requests.labels,
            vec![("status".to_string(), "2xx".to_string())]
        );
        assert_eq!(find(&snap, "queue").unwrap().value, 7.5);
        assert_eq!(find(&snap, "latency_count").unwrap().value, 4.0);
        assert_eq!(find(&snap, "latency_sum").unwrap().value, 10.0);
        assert_eq!(find(&snap, "latency_p50").unwrap().value, 2.0);
        assert_eq!(find(&snap, "latency_max").unwrap().value, 4.0);

        // Observations are drained: the next snapshot keeps the running
        // totals but has no quantiles for an interval without observations.
        let snap = recorder.snapshot(&mut totals);
        assert_eq!(find(&snap, "latency_count").unwrap().value, 4.0);
        assert!(find(&snap, "latency_p50").is_none());
    }

    #[test]
    fn test_ignored_labels_merge_series() {
        let recorder = TelemetryRecorder::new(HashSet::from(["route".to_string()]));
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("hits", "route" => "/a", "status" => "200").increment(1);
            metrics::counter!("hits", "route" => "/b", "status" => "200").increment(2);
        });
        let snap = recorder.snapshot(&mut HashMap::new());
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].value, 3.0);
        assert_eq!(
            snap[0].labels,
            vec![("status".to_string(), "200".to_string())]
        );
    }

    #[test]
    fn test_quantile() {
        assert_eq!(quantile(&[], 0.5), None);
        assert_eq!(quantile(&[5.0], 0.95), Some(5.0));
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(quantile(&values, 0.5), Some(50.0));
        assert_eq!(quantile(&values, 0.95), Some(95.0));
        assert_eq!(quantile(&values, 1.0), Some(100.0));
    }

    #[test]
    fn test_labels_serialize_as_object() {
        let m = metric(
            "x",
            &Key::from_parts("x", vec![Label::new("a", "b")]),
            MetricKind::Gauge,
            1.0,
        );
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["labels"]["a"], "b");
        assert_eq!(json["kind"], "gauge");
    }
}
