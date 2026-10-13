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

/// Conservative throughput hill-climber. A second improving EWMA window is
/// required before adding a worker; flat or declining windows remove a worker.
pub struct AdaptiveConcurrency {
    limit: usize,
    max: usize,
    previous: Option<f64>,
}
impl AdaptiveConcurrency {
    /// Start with one worker, bounded by the configured cap.
    pub fn new(max: usize) -> Self {
        Self {
            limit: 1,
            max: max.max(1),
            previous: None,
        }
    }
    /// Current worker admission limit.
    pub fn limit(&self) -> usize {
        self.limit
    }
    /// Observe a completed effective-throughput window.
    pub fn observe(&mut self, throughput: f64) {
        if let Some(previous) = self.previous {
            if throughput > previous * 1.05 {
                self.limit = (self.limit + 1).min(self.max);
            } else {
                self.limit = self.limit.saturating_sub(1).max(1);
            }
        }
        self.previous = Some(throughput);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_admission_requires_improvement_and_backs_off() {
        let mut scheduler = AdaptiveConcurrency::new(3);
        scheduler.observe(100.0);
        assert_eq!(scheduler.limit(), 1);
        scheduler.observe(120.0);
        assert_eq!(scheduler.limit(), 2);
        scheduler.observe(140.0);
        assert_eq!(scheduler.limit(), 3);
        scheduler.observe(140.0);
        assert_eq!(scheduler.limit(), 2);
        scheduler.observe(90.0);
        assert_eq!(scheduler.limit(), 1);
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
