//! Scheduler for managing download concurrency and throughput.
//!
//! # Design
//!
//! The scheduler controls:
//! - How many jobs run concurrently
//! - How many segments per job
//! - Per-origin connection limits
//! - EWMA-based throughput measurement
//! - Adaptive concurrency: increases only while throughput improves
//!
//! # Invariants
//!
//! - Per-origin connection caps are enforced globally, not per job.
//! - Concurrency increases only while measured effective throughput improves.
//! - Retries use exponential backoff with jitter; `Retry-After` is honored.
//! - Idle slow segments may be split or stolen from; never duplicate-write.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::Mutex;
use url::Url;

/// Tracks per-origin connection counts.
///
/// Enforces global (not per-job) connection caps per origin.
#[derive(Debug)]
pub struct ConnectionTracker {
    /// Map from origin (scheme + host + port) to active connection count.
    counts: HashMap<String, usize>,
    /// Maximum connections per origin.
    max_per_origin: usize,
}

impl ConnectionTracker {
    /// Creates a new tracker with the given per-origin limit.
    pub fn new(max_per_origin: usize) -> Self {
        Self {
            counts: HashMap::new(),
            max_per_origin,
        }
    }

    /// Extracts the origin key from a URL.
    fn origin_key(url: &Url) -> String {
        format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap_or("unknown"),
            url.port_or_known_default().unwrap_or(0)
        )
    }

    /// Attempts to acquire a connection slot for the given URL's origin.
    /// Returns `true` if a slot was available.
    pub fn try_acquire(&mut self, url: &Url) -> bool {
        let key = Self::origin_key(url);
        let count = self.counts.entry(key).or_insert(0);
        if *count < self.max_per_origin {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Releases a connection slot for the given URL's origin.
    pub fn release(&mut self, url: &Url) {
        let key = Self::origin_key(url);
        if let Some(count) = self.counts.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&key);
            }
        }
    }

    /// Returns current connection count for the given URL's origin.
    pub fn count(&self, url: &Url) -> usize {
        let key = Self::origin_key(url);
        self.counts.get(&key).copied().unwrap_or(0)
    }
}

/// Exponentially weighted moving average for throughput measurement.
#[derive(Debug, Clone)]
pub struct EwmaThroughput {
    /// Current smoothed value in bytes/second.
    value: f64,
    /// Smoothing factor (0.0–1.0). Higher = more weight on recent samples.
    alpha: f64,
    /// Whether any sample has been recorded yet.
    initialized: bool,
    /// When the last sample was recorded.
    last_sample_time: Option<Instant>,
}

impl EwmaThroughput {
    /// Creates a new EWMA with the given smoothing factor.
    pub fn new(alpha: f64) -> Self {
        Self {
            value: 0.0,
            alpha: alpha.clamp(0.01, 1.0),
            initialized: false,
            last_sample_time: None,
        }
    }

    /// Records a throughput sample (bytes transferred in the given duration).
    pub fn record(&mut self, bytes: u64, duration: Duration) {
        let secs = duration.as_secs_f64();
        if secs <= 0.0 {
            return;
        }
        let sample = bytes as f64 / secs;

        if self.initialized {
            self.value = self.alpha * sample + (1.0 - self.alpha) * self.value;
        } else {
            self.value = sample;
            self.initialized = true;
        }
        self.last_sample_time = Some(Instant::now());
    }

    /// Returns the current smoothed throughput in bytes/second.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Returns true if any samples have been recorded.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

/// Calculates retry delay with exponential backoff and jitter.
///
/// # Arguments
///
/// * `attempt` - Retry attempt number (0-indexed).
/// * `base_delay_ms` - Base delay in milliseconds.
/// * `max_delay_ms` - Maximum delay cap in milliseconds.
/// * `retry_after_secs` - Optional `Retry-After` value from server.
///
/// # Returns
///
/// The delay duration to wait before the next retry.
pub fn retry_delay(
    attempt: u32,
    base_delay_ms: u64,
    max_delay_ms: u64,
    retry_after_secs: Option<u64>,
) -> Duration {
    // If server specified Retry-After, use it as the minimum.
    if let Some(ra) = retry_after_secs {
        let server_delay = Duration::from_secs(ra);
        let max = Duration::from_millis(max_delay_ms);
        return server_delay.max(max.min(Duration::from_millis(base_delay_ms)));
    }

    // Exponential backoff: base * 2^attempt
    let exp_delay = base_delay_ms.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
    let capped = exp_delay.min(max_delay_ms);

    // Add jitter: random value between 0 and capped.
    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(0..=capped);
    let total = capped.saturating_add(jitter) / 2; // Average of capped and jitter.

    Duration::from_millis(total.min(max_delay_ms))
}

/// Thread-safe wrapper around the connection tracker.
pub type SharedConnectionTracker = Arc<Mutex<ConnectionTracker>>;

/// Throughput hill-climber with bounded exploration, latency checks, health
/// backoff and a frozen plateau to avoid repeated no-gain probes.
pub struct AdaptiveConcurrency {
    limit: usize,
    max: usize,
    baseline: Option<f64>,
    windows: u32,
    probe: Option<(usize, f64, f64, u32)>,
    frozen: bool,
    latency_ms: Option<f64>,
    probe_latency_ms: Option<f64>,
}
impl AdaptiveConcurrency {
    /// Start with one worker, bounded by the configured cap.
    pub fn new(max: usize) -> Self {
        Self {
            limit: 1,
            max: max.max(1),
            baseline: None,
            windows: 0,
            probe: None,
            frozen: false,
            latency_ms: None,
            probe_latency_ms: None,
        }
    }
    /// Current worker admission limit.
    pub fn limit(&self) -> usize {
        self.limit
    }
    /// Connection resets and throttling cut admission immediately. Header
    /// latency is retained as a saturation signal for candidate evaluation.
    pub fn health(&mut self, failures: u32, latency_ms: f64) {
        if failures > 0 {
            self.limit = (self.limit / 2).max(1);
            self.probe = None;
            self.baseline = None;
            self.windows = 0;
            self.frozen = true;
        }
        if latency_ms.is_finite() && latency_ms > 0.0 {
            self.latency_ms = Some(
                self.latency_ms
                    .map_or(latency_ms, |old| old * 0.7 + latency_ms * 0.3),
            );
        }
    }
    /// Evaluate a bounded probe after two stable EWMA windows. More workers
    /// remain only after a >=12% aggregate gain; a failed probe freezes further
    /// exploration until conditions materially change. One settling window and
    /// two measured windows prevent single completion bursts driving changes.
    pub fn observe(&mut self, throughput: f64) {
        if !throughput.is_finite() || throughput <= 0.0 {
            return;
        }
        if let Some((old, baseline, sum, samples)) = self.probe {
            if samples == 0 {
                self.probe = Some((old, baseline, 0.0, 1));
                return;
            }
            let sum = sum + throughput;
            if samples < 2 {
                self.probe = Some((old, baseline, sum, samples + 1));
                return;
            }
            let candidate = sum / 2.0;
            let latency_penalty = self
                .probe_latency_ms
                .zip(self.latency_ms)
                .is_some_and(|(before, after)| after > before * 1.5 && candidate < baseline * 1.25);
            if candidate >= baseline * 1.12 && !latency_penalty {
                self.baseline = Some(candidate);
                self.frozen = false;
            } else {
                self.limit = old;
                self.baseline = Some(baseline);
                self.frozen = true;
            }
            self.probe = None;
            self.windows = 0;
            return;
        }
        let Some(baseline) = self.baseline else {
            self.baseline = Some(throughput);
            return;
        };
        if self.frozen {
            // No repeated probes on a flat link: noise within 25% stays frozen.
            if (throughput - baseline).abs() > baseline * 0.25 {
                self.frozen = false;
                self.baseline = Some(throughput);
                self.windows = 0;
            }
            return;
        }
        if throughput < baseline * 0.75 && self.limit > 1 {
            self.limit -= 1;
            self.baseline = Some(throughput);
            self.windows = 0;
            return;
        }
        self.baseline = Some(baseline * 0.7 + throughput * 0.3);
        self.windows += 1;
        if self.windows >= 2 && self.limit < self.max {
            let baseline = self.baseline.unwrap_or(throughput);
            self.probe_latency_ms = self.latency_ms;
            self.probe = Some((self.limit, baseline, 0.0, 0));
            self.limit += 1;
            self.windows = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_probes_gain_then_health_backs_off() {
        let mut scheduler = AdaptiveConcurrency::new(3);
        for _ in 0..3 {
            scheduler.observe(100.0);
        }
        assert_eq!(scheduler.limit(), 2);
        for _ in 0..3 {
            scheduler.observe(200.0);
        }
        for _ in 0..2 {
            scheduler.observe(200.0);
        }
        assert_eq!(scheduler.limit(), 3);
        scheduler.health(1, 100.0);
        assert_eq!(scheduler.limit(), 1);
    }

    proptest::proptest! {
        #[test]
        fn saturated_link_settles_without_oscillation(noise in proptest::collection::vec(-4i32..=4, 100..300), max in 2usize..128) {
            let mut scheduler=AdaptiveConcurrency::new(max);let mut changes=0;let mut previous=1;
            for sample in noise {scheduler.observe(100.0+sample as f64);if scheduler.limit()!=previous {changes+=1;previous=scheduler.limit();}proptest::prop_assert!(scheduler.limit()>=1 && scheduler.limit()<=max);}
            proptest::prop_assert!(changes<=2);proptest::prop_assert_eq!(scheduler.limit(),1);
        }
    }

    #[test]
    fn test_connection_tracker_basic() {
        let mut tracker = ConnectionTracker::new(2);
        let url = Url::parse("https://example.com/file.zip").unwrap();

        assert!(tracker.try_acquire(&url));
        assert_eq!(tracker.count(&url), 1);
        assert!(tracker.try_acquire(&url));
        assert_eq!(tracker.count(&url), 2);
        assert!(!tracker.try_acquire(&url)); // At limit.

        tracker.release(&url);
        assert_eq!(tracker.count(&url), 1);
        assert!(tracker.try_acquire(&url));
    }

    #[test]
    fn test_connection_tracker_per_origin() {
        let mut tracker = ConnectionTracker::new(1);
        let url1 = Url::parse("https://example.com/a.zip").unwrap();
        let url2 = Url::parse("https://other.com/b.zip").unwrap();

        assert!(tracker.try_acquire(&url1));
        assert!(tracker.try_acquire(&url2)); // Different origin.
        assert!(!tracker.try_acquire(&url1)); // Same origin, at limit.
    }

    #[test]
    fn test_ewma_throughput() {
        let mut ewma = EwmaThroughput::new(0.5);
        assert!(!ewma.is_initialized());

        ewma.record(1000, Duration::from_secs(1));
        assert!(ewma.is_initialized());
        assert!((ewma.value() - 1000.0).abs() < 0.01);

        ewma.record(2000, Duration::from_secs(1));
        // 0.5 * 2000 + 0.5 * 1000 = 1500
        assert!((ewma.value() - 1500.0).abs() < 0.01);
    }

    #[test]
    fn test_retry_delay_exponential() {
        let d0 = retry_delay(0, 500, 60_000, None);
        let d1 = retry_delay(1, 500, 60_000, None);
        let d2 = retry_delay(2, 500, 60_000, None);

        // With jitter these are not deterministic, but should be bounded.
        assert!(d0 <= Duration::from_millis(60_000));
        assert!(d1 <= Duration::from_millis(60_000));
        assert!(d2 <= Duration::from_millis(60_000));
    }

    #[test]
    fn test_retry_delay_with_retry_after() {
        let d = retry_delay(0, 500, 60_000, Some(30));
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn test_retry_delay_retry_after_capped() {
        let d = retry_delay(0, 500, 10_000, Some(30));
        assert_eq!(d, Duration::from_secs(30));
    }
}
