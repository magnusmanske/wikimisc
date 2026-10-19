//! Pushes a service's health and metrics to a dashboard (by default the
//! [Buggregator service dashboard](https://magnustools.toolforge.org/buggregator/#/dashboard)).
//!
//! Once installed, the service records metrics through the regular
//! [`metrics`] macros; a background task sends them, together with memory,
//! CPU, thread and file-descriptor readings, every minute. On SIGTERM/SIGINT,
//! a fatal panic, or a [`crate::seppuku`] watchdog kill, a final push tells
//! the dashboard why the process ended, so restarts can be told apart from
//! crashes.
//!
//! ```no_run
//! # async fn example() -> Result<(), wikimisc::telemetry::TelemetryError> {
//! use wikimisc::telemetry::{Level, Telemetry};
//!
//! let telemetry = Telemetry::builder("web")   // token from $DASHBOARD_TOKEN
//!     .version(env!("CARGO_PKG_VERSION"))
//!     .install()?;
//! wikimisc::metrics::counter!("jobs_finished_total", "result" => "done").increment(1);
//! telemetry.event(Level::Warn, "maxlag, backing off");
//! # Ok(()) }
//! ```
//!
//! Pushing never blocks or fails the service: it runs in its own task with a
//! timeout, a failed push is retried with the next one (counters are totals,
//! so no counts are lost), and failures are only logged to stderr when pushes
//! start or stop failing.

use crate::process_stats::ProcessStats;
use crate::telemetry_recorder::{MetricKind, MetricValue};
use metrics::{Key, Recorder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use crate::telemetry_recorder::TelemetryRecorder;

/// Where pushes go unless configured otherwise.
pub const DEFAULT_URL: &str = "https://magnustools.toolforge.org/buggregator/ingest.php";
/// Environment variable with the service's token, used when none is configured.
pub const TOKEN_ENV: &str = "DASHBOARD_TOKEN";
/// Environment variable overriding the push URL.
pub const URL_ENV: &str = "DASHBOARD_URL";

const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);
/// The dashboard refuses regular pushes from one instance more often than every 15 s.
const MIN_INTERVAL: Duration = Duration::from_secs(20);
const PUSH_TIMEOUT: Duration = Duration::from_secs(10);
const FINAL_PUSH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the final push waits for a regular push in flight before it is
/// sent anyway. The regular push may never finish: after a panic on the main
/// thread of a `current_thread` runtime, its task is stuck on the runtime the
/// panicking thread drives, and that thread waits for the final push.
///
/// Sending both at once is harmless. The final push carries everything the
/// regular one did (counters are totals, unacknowledged events are resent),
/// so the worst case is an event shown twice; a regular push arriving after
/// the final one is rejected and acknowledges nothing; and no regular push
/// follows the final one.
const FINAL_PUSH_LOCK_WAIT: Duration = Duration::from_secs(1);
/// The dashboard's per-push limits.
const MAX_METRICS: usize = 500;
const MAX_EVENTS: usize = 50;
/// Events kept while pushes fail; older ones are dropped first.
const MAX_QUEUED_EVENTS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("no telemetry token configured (set {TOKEN_ENV})")]
    NoToken,
    #[error("invalid component name '{0}': use 1-32 of a-z, 0-9, '-', '_'")]
    InvalidComponent(String),
    #[error("invalid dashboard URL '{0}'")]
    InvalidUrl(String),
    /// The token must not travel in clear text; plain http is only allowed to localhost.
    #[error("dashboard URL '{0}' must use https (plain http only to localhost)")]
    InsecureUrl(String),
    #[error("a global metrics recorder is already installed")]
    RecorderAlreadyInstalled,
    #[error("telemetry must be started from within a tokio runtime")]
    NoRuntime,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("push rejected with HTTP {status}: {message}")]
    Rejected { status: u16, message: String },
}

/// The `telemetry` block of a service's config file. Both fields are
/// optional: the token falls back to `$DASHBOARD_TOKEN`, the URL to
/// `$DASHBOARD_URL` and then [`DEFAULT_URL`].
#[derive(Clone, Default, Deserialize)]
pub struct TelemetryConfig {
    pub url: Option<String>,
    pub token: Option<String>,
}

impl std::fmt::Debug for TelemetryConfig {
    // Hand-written so the token never ends up in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryConfig")
            .field("url", &self.url)
            .field("token", &redacted(&self.token))
            .finish()
    }
}

/// Shows whether a token is set, but not the token.
fn redacted(token: &Option<String>) -> Option<&'static str> {
    token.as_ref().map(|_| "<redacted>")
}

/// Why a process is ending, as reported in the final push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// Asked to stop, e.g. by a signal; the detail names it (`"SIGTERM"`).
    Shutdown(String),
    /// The work is done (one-off runs).
    Exit,
    /// A fatal panic; the detail is its location (the hook leaves the message out).
    Panic(String),
    /// Killed itself because it seemed stuck or idle.
    Watchdog(String),
    /// Restarting itself because it is running out of memory.
    Memory(String),
    Other(String),
}

impl ExitReason {
    fn wire(&self) -> ExitWire<'_> {
        let (reason, detail) = match self {
            Self::Shutdown(d) => ("shutdown", d.as_str()),
            Self::Exit => ("exit", ""),
            Self::Panic(d) => ("panic", d.as_str()),
            Self::Watchdog(d) => ("watchdog", d.as_str()),
            Self::Memory(d) => ("memory", d.as_str()),
            Self::Other(d) => ("other", d.as_str()),
        };
        ExitWire { reason, detail }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
struct Event {
    /// Position in the queue's history, for acknowledging exactly the events
    /// a push carried: the queue may drop its oldest entries meanwhile.
    #[serde(skip)]
    seq: u64,
    ts: u64,
    level: Level,
    message: String,
}

#[derive(Debug, Serialize)]
struct ExitWire<'a> {
    reason: &'static str,
    detail: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct InstanceInfo {
    uid: String,
    component: String,
    host: String,
    version: String,
    started: u64,
}

#[derive(Debug, Serialize)]
struct Payload<'a> {
    v: u8,
    instance: &'a InstanceInfo,
    metrics: Vec<MetricValue>,
    events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit: Option<ExitWire<'a>>,
}

/// Configures a [`Telemetry`]; see [`Telemetry::builder`].
#[derive(Clone)]
pub struct TelemetryBuilder {
    component: String,
    url: Option<String>,
    token: Option<String>,
    version: String,
    interval: Duration,
    ignored_labels: HashSet<String>,
    handle_signals: bool,
}

impl std::fmt::Debug for TelemetryBuilder {
    // Hand-written so the token never ends up in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryBuilder")
            .field("component", &self.component)
            .field("url", &self.url)
            .field("token", &redacted(&self.token))
            .field("version", &self.version)
            .field("interval", &self.interval)
            .field("ignored_labels", &self.ignored_labels)
            .field("handle_signals", &self.handle_signals)
            .finish()
    }
}

impl TelemetryBuilder {
    /// Takes URL and token from a config block; unset fields keep their defaults.
    pub fn config(mut self, config: &TelemetryConfig) -> Self {
        if config.url.is_some() {
            self.url.clone_from(&config.url);
        }
        if config.token.is_some() {
            self.token.clone_from(&config.token);
        }
        self
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The running version, e.g. `env!("CARGO_PKG_VERSION")` or a commit
    /// hash. A short hash of the executable is appended (`1.2.3+9f86d081`),
    /// so a rebuild is a new version even if this one stays the same; the
    /// dashboard marks a restart that changes it as a deploy.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Time between pushes; at least 20 s. Default 60 s.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval.max(MIN_INTERVAL);
        self
    }

    /// Drops this label from all metrics, merging the series it would split
    /// (see [`TelemetryRecorder`]). Use for labels with many values.
    pub fn ignore_label(mut self, label: impl Into<String>) -> Self {
        self.ignored_labels.insert(label.into());
        self
    }

    /// On by default: on SIGTERM or SIGINT, send a final push and exit with
    /// the conventional status (143/130). Turn it off for a service that shuts
    /// down gracefully on its own; it should then call
    /// [`Telemetry::shutdown`] itself once it is done.
    pub fn handle_signals(mut self, handle_signals: bool) -> Self {
        self.handle_signals = handle_signals;
        self
    }

    /// Creates the telemetry without installing its recorder or starting it.
    /// For combining with another recorder; otherwise use [`Self::install`].
    /// The URL must be https, except to localhost.
    pub fn build(self) -> Result<Telemetry, TelemetryError> {
        if !is_valid_component(&self.component) {
            return Err(TelemetryError::InvalidComponent(self.component));
        }
        let token = setting(self.token, TOKEN_ENV).ok_or(TelemetryError::NoToken)?;
        let url = setting(self.url, URL_ENV).unwrap_or_else(|| DEFAULT_URL.to_string());
        check_url(&url)?;
        let host = host_name();
        let started = unix_now();
        let instance = InstanceInfo {
            uid: instance_uid(&host, std::process::id(), started_millis()),
            component: self.component,
            host,
            version: with_binary_hash(&self.version, binary_hash()),
            started,
        };
        Ok(Telemetry {
            inner: Arc::new(Inner {
                recorder: TelemetryRecorder::new(self.ignored_labels),
                client: http_client()?,
                url,
                token,
                instance,
                interval: self.interval,
                handle_signals: self.handle_signals,
                panics: AtomicU64::new(0),
                failing: AtomicBool::new(false),
                ended: AtomicBool::new(false),
                push_lock: tokio::sync::Mutex::new(()),
                state: Mutex::new(PushState::default()),
            }),
        })
    }

    /// Builds, installs the recorder as the global `metrics` recorder, and starts.
    pub fn install(self) -> Result<Telemetry, TelemetryError> {
        let telemetry = self.build()?;
        // Checked before installing the recorder, which cannot be undone: a
        // retry from within a runtime would find the place taken.
        current_runtime()?;
        metrics::set_global_recorder(telemetry.recorder())
            .map_err(|_| TelemetryError::RecorderAlreadyInstalled)?;
        telemetry.start()?;
        Ok(telemetry)
    }

    /// Like [`Self::install`], but every metric also goes to `other`, e.g. a
    /// Prometheus recorder serving `/metrics`. Ignored labels only apply to
    /// what is pushed; `other` sees all of them.
    pub fn install_alongside<R>(self, other: R) -> Result<Telemetry, TelemetryError>
    where
        R: Recorder + Send + Sync + 'static,
    {
        let telemetry = self.build()?;
        current_runtime()?;
        let fanout = metrics_util::layers::FanoutBuilder::default()
            .add_recorder(other)
            .add_recorder(telemetry.recorder())
            .build();
        metrics::set_global_recorder(fanout)
            .map_err(|_| TelemetryError::RecorderAlreadyInstalled)?;
        telemetry.start()?;
        Ok(telemetry)
    }
}

/// Handle to the running telemetry; cheap to clone.
#[derive(Debug, Clone)]
pub struct Telemetry {
    inner: Arc<Inner>,
}

struct Inner {
    recorder: TelemetryRecorder,
    client: reqwest::Client,
    url: String,
    token: String,
    instance: InstanceInfo,
    interval: Duration,
    handle_signals: bool,
    panics: AtomicU64,
    failing: AtomicBool,
    ended: AtomicBool,
    /// One push at a time, so the final push does not normally overtake a
    /// regular one; it waits only briefly, though (see [`FINAL_PUSH_LOCK_WAIT`]).
    push_lock: tokio::sync::Mutex<()>,
    state: Mutex<PushState>,
}

impl std::fmt::Debug for Inner {
    // Hand-written so the token never ends up in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("url", &self.url)
            .field("instance", &self.instance)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// Metric name and labels.
type CounterKey = (String, Vec<(String, String)>);

#[derive(Default)]
struct PushState {
    /// Counter values the dashboard has acknowledged; unchanged ones are not resent.
    acked: HashMap<CounterKey, f64>,
    histogram_totals: HashMap<Key, (f64, f64)>,
    events: VecDeque<Event>,
    next_event_seq: u64,
}

/// What a push carried, to be acknowledged once the dashboard accepted it.
#[derive(Debug, Default)]
struct Sent {
    counters: Vec<(CounterKey, f64)>,
    /// Sequence number of the last event sent, if any.
    last_event: Option<u64>,
}

impl PushState {
    fn queue_event(&mut self, level: Level, message: String) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.pop_front();
        }
        let seq = self.next_event_seq;
        // Cannot overflow in practice; saturating so that it can never panic.
        self.next_event_seq = seq.saturating_add(1);
        self.events.push_back(Event {
            seq,
            ts: unix_now(),
            level,
            message,
        });
    }

    /// Goes by sequence number, not position: an event queued while the push
    /// was in flight may have dropped sent events from the front, and
    /// counting off the front would then remove unsent ones.
    fn acknowledge(&mut self, sent: Sent) {
        self.acked.extend(sent.counters);
        if let Some(last) = sent.last_event {
            self.events.retain(|e| e.seq > last);
        }
    }
}

static GLOBAL: OnceLock<Telemetry> = OnceLock::new();

/// The telemetry started in this process, if any.
pub fn global() -> Option<&'static Telemetry> {
    GLOBAL.get()
}

/// Records an event if telemetry is running; a no-op otherwise. See
/// [`Telemetry::event`] for what must not go into the message.
pub fn event(level: Level, message: impl Into<String>) {
    if let Some(telemetry) = global() {
        telemetry.event(level, message);
    }
}

/// Sends the final push if telemetry is running; a no-op otherwise.
pub async fn shutdown(reason: ExitReason) {
    if let Some(telemetry) = global() {
        telemetry.shutdown(reason).await;
    }
}

/// Records one served HTTP request as `http_requests_total{status}` and
/// `http_request_duration_seconds`. `status` is the status class (`2xx`, …),
/// except for 429, 503 and 504, which are kept as is to show load shedding
/// and timeouts. Call it from the web framework's middleware.
pub fn record_http_request(status: u16, duration: Duration) {
    let status = match status {
        429 | 503 | 504 => status.to_string(),
        _ => format!("{}xx", status / 100),
    };
    metrics::counter!("http_requests_total", "status" => status).increment(1);
    metrics::histogram!("http_request_duration_seconds").record(duration.as_secs_f64());
}

impl Telemetry {
    /// Starts configuring telemetry for one component of a service (e.g.
    /// `"web"`, `"bot"`); the token decides which service it belongs to.
    pub fn builder(component: impl Into<String>) -> TelemetryBuilder {
        TelemetryBuilder {
            component: component.into(),
            url: None,
            token: None,
            version: String::new(),
            interval: DEFAULT_INTERVAL,
            ignored_labels: HashSet::new(),
            handle_signals: true,
        }
    }

    /// The recorder, for installing it yourself (after [`TelemetryBuilder::build`]).
    pub fn recorder(&self) -> TelemetryRecorder {
        self.inner.recorder.clone()
    }

    /// Starts pushing, and installs the panic hook and (if enabled) the
    /// signal handlers. Only the first started telemetry of a process does
    /// anything; later calls are no-ops.
    pub fn start(&self) -> Result<(), TelemetryError> {
        let runtime = current_runtime()?;
        if GLOBAL.set(self.clone()).is_err() {
            return Ok(());
        }
        let me = self.clone();
        runtime.spawn(async move { me.push_forever().await });
        self.install_panic_hook();
        if self.inner.handle_signals {
            self.handle_signals(&runtime);
        }
        Ok(())
    }

    /// Queues an event (shown in the dashboard's event log) for the next push.
    ///
    /// Telemetry leaves the service, so the message must not identify anyone:
    /// no user names or ids, page titles, query strings, IP addresses, or raw
    /// error text that may contain them. The same goes for [`ExitReason`]
    /// details, which the dashboard shows publicly. Log the specifics locally.
    pub fn event(&self, level: Level, message: impl Into<String>) {
        lock(&self.inner.state).queue_event(level, message.into());
    }

    /// Sends the final push, telling the dashboard why the process is
    /// ending, and stops regular pushes. Waits at most a few seconds. Call it
    /// right before the process exits.
    pub async fn shutdown(&self, reason: ExitReason) {
        if self.inner.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        let push = self.push(&self.inner.client, Some(&reason));
        if let Ok(Err(e)) = tokio::time::timeout(FINAL_PUSH_TIMEOUT, push).await {
            eprintln!("telemetry: final push failed: {e}");
        }
    }

    async fn push_forever(self) {
        let mut ticker = tokio::time::interval(self.inner.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await; // The first tick is immediate, so the instance shows up at once
            if self.inner.ended.load(Ordering::SeqCst) {
                return;
            }
            let result = self.push(&self.inner.client, None).await;
            self.log_push_result(&result);
        }
    }

    /// Logs only when pushes start or stop failing, not on every attempt.
    fn log_push_result(&self, result: &Result<(), TelemetryError>) {
        let was_failing = self.inner.failing.swap(result.is_err(), Ordering::Relaxed);
        match result {
            Err(e) if !was_failing => eprintln!("telemetry: push failed: {e}"),
            Ok(()) if was_failing => eprintln!("telemetry: pushing again"),
            _ => {}
        }
    }

    async fn push(
        &self,
        client: &reqwest::Client,
        exit: Option<&ExitReason>,
    ) -> Result<(), TelemetryError> {
        let _push_lock = if exit.is_some() {
            // Sent without the lock if need be; see FINAL_PUSH_LOCK_WAIT.
            tokio::time::timeout(FINAL_PUSH_LOCK_WAIT, self.inner.push_lock.lock())
                .await
                .ok()
        } else {
            let guard = self.inner.push_lock.lock().await;
            if self.inner.ended.load(Ordering::SeqCst) {
                // The dashboard would reject it after the final push anyway.
                return Ok(());
            }
            Some(guard)
        };
        let (payload, sent) = self.payload(exit);
        let response = client
            .post(&self.inner.url)
            .bearer_auth(&self.inner.token)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(TelemetryError::Rejected {
                status: status.as_u16(),
                message: message.chars().take(200).collect(),
            });
        }
        lock(&self.inner.state).acknowledge(sent);
        Ok(())
    }

    /// The payload, and what it carries that needs acknowledging.
    fn payload<'a>(&'a self, exit: Option<&'a ExitReason>) -> (Payload<'a>, Sent) {
        let mut state = lock(&self.inner.state);
        let mut metrics = self.system_metrics(exit.is_none());
        let recorded = self.inner.recorder.snapshot(&mut state.histogram_totals);
        metrics.extend(recorded);
        // Counters that have not changed since the last acknowledged push are left out.
        metrics.retain(|m| {
            m.kind == MetricKind::Gauge
                || state.acked.get(&(m.name.clone(), m.labels.clone())) != Some(&m.value)
        });
        metrics.truncate(MAX_METRICS);
        let events: Vec<Event> = state.events.iter().take(MAX_EVENTS).cloned().collect();
        let sent = Sent {
            counters: metrics
                .iter()
                .filter(|m| m.kind == MetricKind::Counter)
                .map(|m| ((m.name.clone(), m.labels.clone()), m.value))
                .collect(),
            last_event: events.last().map(|e| e.seq),
        };
        let payload = Payload {
            v: 1,
            instance: &self.inner.instance,
            metrics,
            events,
            exit: exit.map(ExitReason::wire),
        };
        (payload, sent)
    }

    /// Process readings, first in the list so the per-push limit never cuts them.
    fn system_metrics(&self, with_runtime: bool) -> Vec<MetricValue> {
        let stats = ProcessStats::read();
        let gauge = |name: &str, value: Option<f64>| {
            value.map(|v| system_metric(name, MetricKind::Gauge, v))
        };
        let counter = |name: &str, value: Option<f64>| {
            value.map(|v| system_metric(name, MetricKind::Counter, v))
        };
        // Measured in whichever runtime pushes, so left out of the final push,
        // which may come from a temporary one.
        let alive_tasks = with_runtime
            .then(tokio::runtime::Handle::try_current)
            .and_then(Result::ok)
            .map(|h| h.metrics().num_alive_tasks() as f64);
        [
            // Named for the dashboard, which compares it to the limit to infer
            // OOM kills: page cache of mmap'd files in a full RSS would make
            // such services look permanently close to it.
            gauge(
                "process_resident_memory_bytes",
                stats.anon_rss_bytes.or(stats.rss_bytes).map(|v| v as f64),
            ),
            gauge(
                "process_memory_limit_bytes",
                stats.memory_limit_bytes.map(|v| v as f64),
            ),
            counter("process_cpu_seconds_total", stats.cpu_seconds),
            gauge("process_cpu_limit_cores", stats.cpu_limit_cores),
            gauge("process_threads", stats.threads.map(|v| v as f64)),
            gauge("process_open_fds", stats.open_fds.map(|v| v as f64)),
            counter("process_oom_kills_total", stats.oom_kills.map(|v| v as f64)),
            counter(
                "process_panics_total",
                Some(self.inner.panics.load(Ordering::Relaxed) as f64),
            ),
            gauge("tokio_alive_tasks", alive_tasks),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Records every panic as an event. A panic that ends the process also
    /// gets a final push, sent from a separate thread and runtime: the
    /// panicking thread may be the one driving the service's runtime.
    fn install_panic_hook(&self) {
        let previous = std::panic::take_hook();
        let me = self.clone();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            let location = panic_location(info);
            me.inner.panics.fetch_add(1, Ordering::Relaxed);
            me.event(Level::Error, format!("panic at {location}"));
            if this_panic_ends_process() {
                me.shutdown_blocking(ExitReason::Panic(format!("at {location}")));
            }
        }));
    }

    fn shutdown_blocking(&self, reason: ExitReason) {
        let me = self.clone();
        let send = move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                if me.inner.ended.swap(true, Ordering::SeqCst) {
                    return;
                }
                // A fresh client: the service's pooled connections belong to its runtime.
                let Ok(client) = http_client() else { return };
                let _ =
                    tokio::time::timeout(FINAL_PUSH_TIMEOUT, me.push(&client, Some(&reason))).await;
            });
        };
        // Not `std::thread::spawn`, which panics if no thread can be created:
        // a panic inside the panic hook aborts the process.
        match std::thread::Builder::new().spawn(send) {
            Ok(thread) => {
                let _ = thread.join();
            }
            Err(e) => eprintln!("telemetry: cannot send the final push: {e}"),
        }
    }

    fn handle_signals(&self, runtime: &tokio::runtime::Handle) {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            for (kind, name, code) in [
                (SignalKind::terminate(), "SIGTERM", 143),
                (SignalKind::interrupt(), "SIGINT", 130),
            ] {
                let me = self.clone();
                runtime.spawn(async move {
                    let Ok(mut stream) = signal(kind) else {
                        eprintln!("telemetry: cannot listen for {name}");
                        return;
                    };
                    stream.recv().await;
                    me.shutdown(ExitReason::Shutdown(name.to_string())).await;
                    std::process::exit(code);
                });
            }
        }
        #[cfg(not(unix))]
        {
            let me = self.clone();
            runtime.spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    me.shutdown(ExitReason::Shutdown("Ctrl-C".to_string()))
                        .await;
                    std::process::exit(130);
                }
            });
        }
    }
}

fn system_metric(name: &str, kind: MetricKind, value: f64) -> MetricValue {
    MetricValue {
        name: name.to_string(),
        kind,
        labels: vec![],
        value,
    }
}

/// A configured value, else the environment variable; blank counts as unset
/// either way, so an empty config entry does not hide the variable.
fn setting(configured: Option<String>, env: &str) -> Option<String> {
    let non_blank = |v: String| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    configured
        .and_then(non_blank)
        .or_else(|| std::env::var(env).ok().and_then(non_blank))
}

/// Redirects are not followed: reqwest would turn the POST into a GET, which
/// can succeed without anything being ingested. A redirect is a failed push.
fn http_client() -> Result<reqwest::Client, TelemetryError> {
    Ok(reqwest::Client::builder()
        .timeout(PUSH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("wikimisc-telemetry/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Every push carries the token, so it must not go over plain http, except
/// to this machine (e.g. a local test server).
fn check_url(url: &str) -> Result<(), TelemetryError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| TelemetryError::InvalidUrl(url.to_string()))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if parsed.host_str().is_some_and(is_loopback) => Ok(()),
        _ => Err(TelemetryError::InsecureUrl(url.to_string())),
    }
}

/// `localhost` or a loopback address; IPv6 hosts come in brackets (`[::1]`).
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn current_runtime() -> Result<tokio::runtime::Handle, TelemetryError> {
    tokio::runtime::Handle::try_current().map_err(|_| TelemetryError::NoRuntime)
}

/// Lock a mutex, recovering from poisoning: a panic elsewhere (which the
/// panic hook reports through this very state) must not disable telemetry.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a panic ends the process: always when built with `panic = "abort"`
/// (the abort follows right after the hook), otherwise only on the main
/// thread — elsewhere it ends just that thread.
///
/// Not inside a tokio task, though, even on the main thread: tokio catches
/// the panic and only that task ends. Tasks run on the main thread on a
/// `current_thread` runtime. The main future of `#[tokio::main]` is not a
/// task, so a panic there does end the process, whatever the flavour.
///
/// Getting this right matters: the dashboard rejects every push after an
/// exit push, so a panic wrongly taken as fatal shows a running instance as
/// dead for good. A panic the service catches itself on the main thread,
/// outside a task, still counts as fatal; there is no way to tell.
fn panic_ends_process(thread_name: Option<&str>, in_tokio_task: bool) -> bool {
    cfg!(panic = "abort") || (thread_name == Some("main") && !in_tokio_task)
}

/// [`panic_ends_process`] for a panic on the calling thread.
fn this_panic_ends_process() -> bool {
    panic_ends_process(
        std::thread::current().name(),
        tokio::task::try_id().is_some(),
    )
}

/// Where a panic happened, e.g. `src/main.rs:12:5`. The message is left out on
/// purpose: it can contain whatever the code was handling (titles, user names,
/// query strings), and the location is enough to find the bug; the full panic
/// still goes to stderr through the previous hook.
fn panic_location(info: &std::panic::PanicHookInfo<'_>) -> String {
    info.location()
        .map_or_else(|| "an unknown location".to_string(), ToString::to_string)
}

fn is_valid_component(component: &str) -> bool {
    let mut chars = component.chars();
    component.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// On Kubernetes the host name is the pod name, which a container restarted
/// in place keeps; the dashboard uses that to tell crashes from redeploys.
/// `version+hash`, so that every rebuild counts as a new version even when
/// the crate version is never bumped; just the hash without a version.
fn with_binary_hash(version: &str, hash: Option<String>) -> String {
    match (version.is_empty(), hash) {
        (_, None) => version.to_string(),
        (true, Some(hash)) => hash,
        (false, Some(hash)) => format!("{version}+{hash}"),
    }
}

/// A short hash of the running executable. File times are no use for this:
/// buildpack images (Toolforge's build service) reset them all to 1980.
/// `/proc/self/exe` still works after a deploy has replaced the file.
fn binary_hash() -> Option<String> {
    use std::hash::Hasher;
    use std::io::Read;
    let path = Some(std::path::PathBuf::from("/proc/self/exe"))
        .filter(|p| p.exists())
        .or_else(|| std::env::current_exe().ok())?;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.write(buffer.get(..read)?);
    }
    Some(format!("{:08x}", hasher.finish() >> 32))
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

/// Unique per process run; limited to what the dashboard accepts (64 of `A-Za-z0-9._:-`).
fn instance_uid(host: &str, pid: u32, started_millis: u128) -> String {
    let host: String = host
        .chars()
        .take(32) // 32 + pid (≤ 10) + millis (≤ 20) + 2 dashes = 64
        .map(|c| {
            if c.is_ascii_alphanumeric() || ".:-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{host}-{pid}-{started_millis}")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn started_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn telemetry(url: &str) -> Telemetry {
        Telemetry::builder("web")
            .url(url)
            .token("secret")
            .version("1.2.3")
            .build()
            .unwrap()
    }

    async fn received(server: &MockServer) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    fn metric<'a>(payload: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
        payload["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
    }

    #[test]
    fn test_builder_validation() {
        assert!(matches!(
            Telemetry::builder("Web Server").token("x").build(),
            Err(TelemetryError::InvalidComponent(_))
        ));
        assert!(matches!(
            Telemetry::builder("web").token("  ").build(),
            Err(TelemetryError::NoToken)
        ));
        let config = TelemetryConfig {
            url: None,
            token: Some("from-config".to_string()),
        };
        let t = Telemetry::builder("web").config(&config).build().unwrap();
        assert_eq!(t.inner.token, "from-config");
        assert_eq!(t.inner.url, DEFAULT_URL);
        assert!(
            !format!("{t:?}").contains("from-config"),
            "Debug must not show the token"
        );
    }

    #[test]
    fn test_blank_settings_count_as_unset() {
        // An env var that is surely unset, so the fallback yields nothing.
        assert_eq!(
            setting(Some(" x ".to_string()), "WIKIMISC_TEST_UNSET"),
            Some("x".to_string())
        );
        assert_eq!(setting(Some("  ".to_string()), "WIKIMISC_TEST_UNSET"), None);
        assert_eq!(setting(None, "WIKIMISC_TEST_UNSET"), None);
        // A blank configured value falls through to a set variable.
        assert_eq!(
            setting(Some(String::new()), "PATH"),
            std::env::var("PATH").ok()
        );
    }

    #[test]
    fn test_interval_has_a_floor() {
        let b = Telemetry::builder("web").interval(Duration::from_secs(1));
        assert_eq!(b.interval, MIN_INTERVAL);
    }

    #[test]
    fn test_version_gets_the_binary_hash() {
        assert_eq!(
            with_binary_hash("1.2.3", Some("abcd".to_string())),
            "1.2.3+abcd"
        );
        assert_eq!(with_binary_hash("", Some("abcd".to_string())), "abcd");
        assert_eq!(with_binary_hash("1.2.3", None), "1.2.3");
        let hash = binary_hash().unwrap();
        assert_eq!(hash.len(), 8);
        assert_eq!(binary_hash().unwrap(), hash, "must be stable");
    }

    #[test]
    fn test_instance_uid() {
        assert_eq!(instance_uid("pod-1.x", 7, 1234), "pod-1.x-7-1234");
        assert_eq!(instance_uid("a b/c", 7, 1), "a_b_c-7-1");
        assert!(instance_uid(&"h".repeat(100), u32::MAX, u128::from(u64::MAX)).len() <= 64);
    }

    #[test]
    fn test_component_names() {
        assert!(is_valid_component("web"));
        assert!(is_valid_component("bot-2_x"));
        assert!(!is_valid_component(""));
        assert!(!is_valid_component("-web"));
        assert!(!is_valid_component("Web"));
        assert!(!is_valid_component(&"a".repeat(33)));
    }

    #[tokio::test]
    async fn test_push_sends_payload_and_skips_unchanged_counters() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        metrics::with_local_recorder(&t.recorder(), || {
            metrics::counter!("jobs_total").increment(2);
            metrics::gauge!("queue").set(5.0);
        });
        t.event(Level::Warn, "careful");

        t.push(&t.inner.client, None).await.unwrap();
        t.push(&t.inner.client, None).await.unwrap();

        let pushes = received(&server).await;
        assert_eq!(pushes.len(), 2);
        let first = &pushes[0];
        assert_eq!(first["v"], 1);
        assert_eq!(first["instance"]["component"], "web");
        assert!(first["instance"]["version"]
            .as_str()
            .unwrap()
            .starts_with("1.2.3+"));
        assert_eq!(metric(first, "jobs_total").unwrap()["value"], 2.0);
        assert_eq!(metric(first, "queue").unwrap()["kind"], "gauge");
        assert_eq!(first["events"][0]["message"], "careful");
        assert!(first.get("exit").is_none());
        // Second push: the unchanged counter and the delivered event are not repeated.
        assert!(metric(&pushes[1], "jobs_total").is_none());
        assert!(metric(&pushes[1], "queue").is_some());
        assert_eq!(pushes[1]["events"].as_array().unwrap().len(), 0);
    }

    /// Waits until the server has received `count` requests.
    async fn wait_for_requests(server: &MockServer, count: usize) {
        for _ in 0..500 {
            if server.received_requests().await.unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the server never received {count} requests");
    }

    #[tokio::test]
    async fn test_event_queued_during_a_push_is_not_lost() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        for i in 0..MAX_QUEUED_EVENTS {
            t.event(Level::Info, format!("event {i}"));
        }

        let push = tokio::spawn({
            let t = t.clone();
            async move { t.push(&t.inner.client, None).await }
        });
        wait_for_requests(&server, 1).await;
        // The queue is full, so this drops "event 0", which is in flight.
        t.event(Level::Warn, "queued during the push");
        push.await.unwrap().unwrap();

        // The push carried events 0..MAX_EVENTS; all others must remain.
        let state = lock(&t.inner.state);
        let messages: Vec<&str> = state.events.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages.len(), MAX_QUEUED_EVENTS - MAX_EVENTS + 1);
        assert_eq!(
            messages.first(),
            Some(&format!("event {MAX_EVENTS}").as_str())
        );
        assert_eq!(messages.last(), Some(&"queued during the push"));
    }

    #[tokio::test]
    async fn test_failed_push_is_retried_with_the_next_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        metrics::with_local_recorder(&t.recorder(), || {
            metrics::counter!("jobs_total").increment(1)
        });
        t.event(Level::Error, "boom");

        let err = t.push(&t.inner.client, None).await.unwrap_err();
        assert!(matches!(err, TelemetryError::Rejected { status: 503, .. }));
        t.push(&t.inner.client, None).await.unwrap();

        let pushes = received(&server).await;
        assert!(
            metric(&pushes[1], "jobs_total").is_some(),
            "counter was never acknowledged"
        );
        assert_eq!(pushes[1]["events"][0]["message"], "boom");
    }

    #[tokio::test]
    async fn test_shutdown_sends_exit_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        t.shutdown(ExitReason::Shutdown("SIGTERM".to_string()))
            .await;
        t.shutdown(ExitReason::Exit).await;

        let pushes = received(&server).await;
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0]["exit"]["reason"], "shutdown");
        assert_eq!(pushes[0]["exit"]["detail"], "SIGTERM");
    }

    #[tokio::test]
    async fn test_shutdown_blocking_from_a_runtime_thread() {
        // The panic path sends from a separate thread with its own runtime,
        // which must work even when called from inside a runtime.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        tokio::task::spawn_blocking(move || {
            t.shutdown_blocking(ExitReason::Panic("oops".to_string()))
        })
        .await
        .unwrap();
        let pushes = received(&server).await;
        assert_eq!(pushes[0]["exit"]["reason"], "panic");
    }

    const SLOW_PUSH: Duration = Duration::from_secs(2);

    /// A server that takes `SLOW_PUSH` to answer regular pushes, but answers
    /// final pushes at once.
    async fn slow_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"exit\""))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(SLOW_PUSH))
            .mount(&server)
            .await;
        server
    }

    /// Starts a regular push and waits until the server has it, so it holds the push lock.
    async fn start_slow_push(
        t: &Telemetry,
        server: &MockServer,
    ) -> tokio::task::JoinHandle<Result<(), TelemetryError>> {
        let push = tokio::spawn({
            let t = t.clone();
            async move { t.push(&t.inner.client, None).await }
        });
        wait_for_requests(server, 1).await;
        assert!(t.inner.push_lock.try_lock().is_err());
        push
    }

    async fn assert_final_push_arrived(
        server: &MockServer,
        regular: &tokio::task::JoinHandle<Result<(), TelemetryError>>,
    ) {
        assert!(
            !regular.is_finished(),
            "the final push waited for the regular one"
        );
        let pushes = received(server).await;
        assert!(pushes.iter().any(|p| p["exit"]["reason"] == "panic"));
    }

    #[test]
    fn test_final_push_from_panic_hook_does_not_wait_for_a_stuck_push() {
        // A panic in the main future of #[tokio::main(flavor = "current_thread")]:
        // the hook blocks the thread that drives the runtime, so a regular
        // push in flight on it cannot finish and release the lock.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server = slow_server().await;
            let t = telemetry(&server.uri());
            let regular = start_slow_push(&t, &server).await;

            t.shutdown_blocking(ExitReason::Panic("at src/main.rs:1:1".to_string()));
            assert_final_push_arrived(&server, &regular).await;

            // The regular push can still finish; nothing follows the final push.
            regular.await.unwrap().unwrap();
            t.push(&t.inner.client, None).await.unwrap();
            assert_eq!(received(&server).await.len(), 2);
        });
    }

    #[tokio::test]
    async fn test_shutdown_does_not_wait_for_a_slow_push() {
        let server = slow_server().await;
        let t = telemetry(&server.uri());
        let regular = start_slow_push(&t, &server).await;

        t.shutdown(ExitReason::Panic("at src/main.rs:1:1".to_string()))
            .await;
        assert_final_push_arrived(&server, &regular).await;
    }

    #[test]
    fn test_which_panics_end_the_process() {
        assert!(panic_ends_process(Some("main"), false));
        // Tests are built with panic = "unwind": a worker panic is survivable.
        assert!(!panic_ends_process(Some("tokio-runtime-worker"), false));
        assert!(!panic_ends_process(Some("tokio-runtime-worker"), true));
        assert!(!panic_ends_process(None, false));
        // A task on a current_thread runtime: tokio catches the panic.
        assert!(!panic_ends_process(Some("main"), true));
    }

    #[test]
    fn test_main_thread_panics_in_and_outside_tasks() {
        // What #[tokio::main(flavor = "current_thread")] does: the runtime is
        // driven from the main thread, and spawned tasks run there too.
        let verdicts = std::thread::Builder::new()
            .name("main".to_string())
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let in_task = tokio::spawn(async { this_panic_ends_process() })
                        .await
                        .unwrap();
                    (this_panic_ends_process(), in_task)
                })
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(
            verdicts,
            (true, false),
            "the main future's panics are fatal, a task's are not"
        );
    }

    #[test]
    fn test_debug_does_not_show_the_token() {
        let config = TelemetryConfig {
            url: None,
            token: Some("s3cret".to_string()),
        };
        let builder = Telemetry::builder("web").token("s3cret");
        for debug in [format!("{config:?}"), format!("{builder:?}")] {
            assert!(!debug.contains("s3cret"), "token in {debug}");
            assert!(debug.contains("<redacted>"), "{debug}");
        }
        assert!(format!("{:?}", TelemetryConfig::default()).contains("token: None"));
    }

    #[test]
    fn test_url_must_be_https_except_to_localhost() {
        for url in [
            DEFAULT_URL,
            "http://localhost:8080/ingest.php",
            "http://127.0.0.1:1234",
            "http://[::1]/x",
        ] {
            assert!(check_url(url).is_ok(), "{url}");
        }
        for url in ["http://example.org/ingest.php", "ftp://localhost/x"] {
            assert!(
                matches!(check_url(url), Err(TelemetryError::InsecureUrl(_))),
                "{url}"
            );
        }
        assert!(matches!(
            check_url("not a url"),
            Err(TelemetryError::InvalidUrl(_))
        ));
        assert!(matches!(
            Telemetry::builder("web")
                .token("x")
                .url("http://example.org")
                .build(),
            Err(TelemetryError::InsecureUrl(_))
        ));
    }

    #[test]
    fn test_install_without_runtime_leaves_the_recorder_slot_free() {
        let result = Telemetry::builder("web")
            .token("x")
            .handle_signals(false)
            .install();
        assert!(matches!(result, Err(TelemetryError::NoRuntime)));
        // The only test that installs a global recorder, so this proves the
        // failed install did not take the slot.
        assert!(metrics::set_global_recorder(metrics::NoopRecorder).is_ok());
    }

    #[tokio::test]
    async fn test_redirect_is_a_failed_push() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/elsewhere"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let t = telemetry(&server.uri());
        metrics::with_local_recorder(&t.recorder(), || {
            metrics::counter!("jobs_total").increment(1)
        });

        let err = t.push(&t.inner.client, None).await.unwrap_err();
        assert!(matches!(err, TelemetryError::Rejected { status: 302, .. }));
        assert_eq!(received(&server).await.len(), 1, "redirect not followed");
        assert!(lock(&t.inner.state).acked.is_empty());
    }

    #[test]
    fn test_exit_reasons_on_the_wire() {
        assert_eq!(ExitReason::Exit.wire().reason, "exit");
        let reason = ExitReason::Memory("RSS 95%".to_string());
        let wire = reason.wire();
        assert_eq!((wire.reason, wire.detail), ("memory", "RSS 95%"));
    }
}
