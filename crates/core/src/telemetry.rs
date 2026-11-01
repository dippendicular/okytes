//! Display-only transfer telemetry. Workers update atomics on the data path;
//! the registry mutex is taken only when a connection opens, closes or is
//! sampled. Nothing here is persisted or used for resume decisions.
use crate::types::JobId;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// A connection without received bytes for this long is reported as stalled.
const STALL_AFTER: Duration = Duration::from_secs(5);

/// Transient network request state. Persisted segments have their own states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Waiting for an origin permit or response headers.
    Connecting,
    /// Receiving body bytes.
    Downloading,
    /// Waiting out retry backoff after a retryable failure.
    Retrying,
}

struct Link {
    id: u32,
    segment_start: u64,
    start: u64,
    end: Option<u64>,
    opened: Instant,
    received: AtomicU64,
    progressed_ms: AtomicU64,
    state: AtomicU8,
    attempt: AtomicU32,
    // Only touched while sampling; never on the transfer path.
    sample: Mutex<(Instant, u64, f64)>,
}

/// Handle owned by one bounded request. Dropping it removes the connection.
pub struct LinkGuard<'a> {
    registry: &'a Telemetry,
    job: JobId,
    link: Arc<Link>,
}

impl LinkGuard<'_> {
    /// Record a state change for this connection.
    pub fn set_state(&self, state: LinkState) {
        self.link.state.store(state as u8, Ordering::Relaxed);
        if state == LinkState::Downloading {
            self.touch();
        }
    }
    /// Record a retry attempt and enter backoff.
    pub fn retrying(&self, attempt: u32) {
        self.link.attempt.store(attempt, Ordering::Relaxed);
        self.set_state(LinkState::Retrying);
    }
    /// Count bytes accepted from the network. Two relaxed atomic stores.
    pub fn received(&self, bytes: usize) {
        self.link
            .received
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }
    fn touch(&self) {
        self.link.progressed_ms.store(
            self.link.opened.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );
    }
}

impl Drop for LinkGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut links) = self.registry.links.lock() {
            if let Some(entries) = links.get_mut(&self.job) {
                entries.retain(|link| !Arc::ptr_eq(link, &self.link));
                if entries.is_empty() {
                    links.remove(&self.job);
                }
            }
        }
    }
}

/// Live connection snapshot.
#[derive(Debug, Clone)]
pub struct LinkSnapshot {
    /// Per-job connection number, unique for the engine lifetime.
    pub id: u32,
    /// First byte of the persisted segment this request serves.
    pub segment_start: u64,
    /// First requested byte.
    pub start: u64,
    /// Last requested byte, when the size is known.
    pub end: Option<u64>,
    /// Bytes received on this connection.
    pub received: u64,
    /// Sampled throughput in bytes per second.
    pub speed: f64,
    /// Retry attempts made by this request.
    pub attempt: u32,
    /// connecting, downloading, stalled or retrying.
    pub state: &'static str,
    /// Time since the request started.
    pub age: Duration,
}

/// Session statistics for one job, retained after it stops.
#[derive(Debug, Clone, Default)]
pub struct JobStats {
    /// Server honours byte ranges for this resource (known after probing).
    pub range_support: Option<bool>,
    /// Current adaptive or fixed connection limit.
    pub concurrency_limit: Option<u32>,
    /// Highest sampled aggregate throughput this session.
    pub peak_speed: f64,
    /// Durable bytes and instant when this session started transferring.
    started: Option<(Instant, u64)>,
    /// Instant the session stopped, freezing the average.
    stopped: Option<Instant>,
}

impl JobStats {
    /// Average throughput since this session started transferring.
    pub fn average_speed(&self, bytes: u64) -> Option<f64> {
        let (at, base) = self.started?;
        let elapsed = self.stopped.unwrap_or_else(Instant::now).duration_since(at);
        (elapsed >= Duration::from_millis(500))
            .then(|| bytes.saturating_sub(base) as f64 / elapsed.as_secs_f64())
    }
}

/// Bounded registry shared by the engine's workers.
#[derive(Default)]
pub struct Telemetry {
    links: Mutex<HashMap<JobId, Vec<Arc<Link>>>>,
    stats: Mutex<HashMap<JobId, JobStats>>,
    next: AtomicU32,
}

impl Telemetry {
    /// Register a request; the guard removes it when the request ends.
    pub fn open(
        &self,
        job: JobId,
        segment_start: u64,
        start: u64,
        end: Option<u64>,
    ) -> LinkGuard<'_> {
        let link = Arc::new(Link {
            id: self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1),
            segment_start,
            start,
            end,
            opened: Instant::now(),
            received: AtomicU64::new(0),
            progressed_ms: AtomicU64::new(0),
            state: AtomicU8::new(LinkState::Connecting as u8),
            attempt: AtomicU32::new(0),
            sample: Mutex::new((Instant::now(), 0, 0.0)),
        });
        if let Ok(mut links) = self.links.lock() {
            links.entry(job).or_default().push(link.clone());
        }
        LinkGuard {
            registry: self,
            job,
            link,
        }
    }

    fn with_stats(&self, job: JobId, update: impl FnOnce(&mut JobStats)) {
        if let Ok(mut stats) = self.stats.lock() {
            if stats.len() >= 256 && !stats.contains_key(&job) {
                if let Some(old) = stats.keys().next().copied() {
                    stats.remove(&old);
                }
            }
            update(stats.entry(job).or_default());
        }
    }

    /// Start a transfer session after a successful probe.
    pub fn started(&self, job: JobId, range_support: bool, durable: u64) {
        self.with_stats(job, |stats| {
            *stats = JobStats {
                range_support: Some(range_support),
                started: Some((Instant::now(), durable)),
                ..Default::default()
            };
        });
    }

    /// Record the current admission limit and smoothed throughput.
    pub fn observe(&self, job: JobId, limit: usize, speed: f64) {
        self.with_stats(job, |stats| {
            stats.concurrency_limit = Some(limit as u32);
            stats.peak_speed = stats.peak_speed.max(speed);
        });
    }

    /// Freeze the average when the job's runner releases ownership.
    pub fn stopped(&self, job: JobId) {
        if let Ok(mut stats) = self.stats.lock() {
            if let Some(stats) = stats.get_mut(&job) {
                stats.stopped.get_or_insert_with(Instant::now);
            }
        }
    }

    /// Sample live connections and session statistics for one job.
    pub fn snapshot(&self, job: JobId) -> (Vec<LinkSnapshot>, JobStats) {
        let links = self
            .links
            .lock()
            .ok()
            .and_then(|links| links.get(&job).cloned())
            .unwrap_or_default();
        let now = Instant::now();
        let mut total = 0.0;
        let connections: Vec<_> = links
            .iter()
            .map(|link| {
                let received = link.received.load(Ordering::Relaxed);
                let speed = match link.sample.lock() {
                    Ok(mut sample) => {
                        let elapsed = now.duration_since(sample.0);
                        if elapsed >= Duration::from_millis(200) {
                            sample.2 =
                                received.saturating_sub(sample.1) as f64 / elapsed.as_secs_f64();
                            sample.0 = now;
                            sample.1 = received;
                        }
                        sample.2
                    }
                    Err(_) => 0.0,
                };
                let idle = link.opened.elapsed().saturating_sub(Duration::from_millis(
                    link.progressed_ms.load(Ordering::Relaxed),
                ));
                let state = match link.state.load(Ordering::Relaxed) {
                    s if s == LinkState::Retrying as u8 => "retrying",
                    s if s == LinkState::Downloading as u8 && idle >= STALL_AFTER => "stalled",
                    s if s == LinkState::Downloading as u8 => "downloading",
                    _ => "connecting",
                };
                total += speed;
                LinkSnapshot {
                    id: link.id,
                    segment_start: link.segment_start,
                    start: link.start,
                    end: link.end,
                    received,
                    speed,
                    attempt: link.attempt.load(Ordering::Relaxed),
                    state,
                    age: link.opened.elapsed(),
                }
            })
            .collect();
        let stats = self
            .stats
            .lock()
            .ok()
            .and_then(|mut stats| {
                let current = stats.get_mut(&job)?;
                current.peak_speed = current.peak_speed.max(total);
                Some(current.clone())
            })
            .unwrap_or_default();
        (connections, stats)
    }
}
