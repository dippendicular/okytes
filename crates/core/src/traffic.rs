//! Payload token buckets and recurring wall-clock policies. Token refill uses
//! monotonic time; window selection converts each UTC observation to IANA local
//! time. Clock jumps never mint tokens or replay queue commands.
use crate::{
    error::{EngineError, EngineResult},
    types::JobId,
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};
use tokio::time::Instant;
use ts_rs::TS;

/// Persistent payload policy shared by every backend. Null rates are unlimited.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
#[ts(export)]
pub struct TrafficPolicy {
    /// Aggregate upload bytes/second for protocols that share files.
    #[ts(type = "number | null")]
    pub upload_bps: Option<u64>,
    /// Independent default upload cap for each sharing job.
    #[ts(type = "number | null")]
    pub per_job_upload_bps: Option<u64>,
    /// Aggregate bytes/second across all jobs.
    #[ts(type = "number | null")]
    pub global_bps: Option<u64>,
    /// Default bytes/second for each job independently.
    #[ts(type = "number | null")]
    pub per_job_bps: Option<u64>,
    /// Explicit overrides, at most 128 unique job UUIDs.
    pub job_limits: Vec<JobLimit>,
    /// IANA timezone, independent of the host's local zone.
    pub timezone: String,
    /// Whether the queue runs outside matching queue windows.
    pub queue_allowed: bool,
    /// At most 32 recurring local-time windows.
    pub windows: Vec<ScheduleWindow>,
}
impl Default for TrafficPolicy {
    fn default() -> Self {
        Self {
            upload_bps: None,
            per_job_upload_bps: None,
            global_bps: None,
            per_job_bps: None,
            job_limits: vec![],
            timezone: "UTC".into(),
            queue_allowed: true,
            windows: vec![],
        }
    }
}
/// Per-job payload override. Removing an override restores the default rate.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct JobLimit {
    /// UUID of the job to limit.
    pub id: String,
    /// Positive bytes/second, or null for an unlimited job.
    #[ts(type = "number | null")]
    pub bps: Option<u64>,
}
/// Recurring window. Start is inclusive, end exclusive; overnight windows
/// belong to their starting weekday. Overlapping rules use the stricter limit.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ScheduleWindow {
    /// Monday=0 through Sunday=6; at least one distinct day.
    pub days: Vec<u8>,
    /// Local start minute, 0..1439.
    pub start_minute: u16,
    /// Local end minute, 0..1440; must differ from start.
    pub end_minute: u16,
    /// Optional aggregate cap applied in addition to the base cap.
    #[ts(type = "number | null")]
    pub global_bps: Option<u64>,
    /// Null leaves queue behavior unchanged; false closes the queue.
    pub queue_allowed: Option<bool>,
}
/// Evaluated policy for one job at a specific wall-clock instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// Aggregate payload cap.
    pub global_bps: Option<u64>,
    /// Individual payload cap.
    pub job_bps: Option<u64>,
    /// Current queue gate.
    pub queue_allowed: bool,
}
fn invalid() -> EngineError {
    EngineError::Config("invalid bandwidth or schedule policy".into())
}
fn rate_valid(rate: Option<u64>) -> bool {
    rate.is_none_or(|n| n > 0 && n <= 1 << 40)
}
fn stricter(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}
impl TrafficPolicy {
    /// Validate bounds, UUID uniqueness, timezone and local windows before save.
    pub fn validate(&self) -> EngineResult<()> {
        if !rate_valid(self.upload_bps)
            || !rate_valid(self.per_job_upload_bps)
            || !rate_valid(self.global_bps)
            || !rate_valid(self.per_job_bps)
            || self.windows.len() > 32
            || self.job_limits.len() > 128
            || self.timezone.parse::<chrono_tz::Tz>().is_err()
        {
            return Err(invalid());
        }
        let mut ids = std::collections::HashSet::new();
        for limit in &self.job_limits {
            if !rate_valid(limit.bps)
                || uuid::Uuid::parse_str(&limit.id).is_err()
                || !ids.insert(&limit.id)
            {
                return Err(invalid());
            }
        }
        for window in &self.windows {
            let unique: std::collections::HashSet<_> = window.days.iter().collect();
            if window.days.is_empty()
                || unique.len() != window.days.len()
                || window.days.iter().any(|day| *day > 6)
                || window.start_minute > 1439
                || window.end_minute > 1440
                || window.start_minute == window.end_minute
                || !rate_valid(window.global_bps)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Build the separate upload direction while retaining queue-window gates.
    pub fn upload_policy(&self) -> Self {
        let mut upload = self.clone();
        upload.global_bps = self.upload_bps;
        upload.per_job_bps = self.per_job_upload_bps;
        upload.job_limits.clear();
        for window in &mut upload.windows {
            window.global_bps = None;
        }
        upload
    }
    /// Evaluate from UTC rather than constructing ambiguous/nonexistent local
    /// timestamps. Fall-back repeated minutes both match; spring gaps do not.
    pub fn effective(&self, at: DateTime<Utc>, job: JobId) -> EffectivePolicy {
        let Ok(zone) = self.timezone.parse::<chrono_tz::Tz>() else {
            return EffectivePolicy {
                global_bps: None,
                job_bps: None,
                queue_allowed: false,
            };
        };
        let local = at.with_timezone(&zone);
        let day = local.weekday().num_days_from_monday() as u8;
        let minute = (local.hour() * 60 + local.minute()) as u16;
        let mut global = self.global_bps;
        let mut queue = None;
        for window in &self.windows {
            let matches = if window.start_minute < window.end_minute {
                window.days.contains(&day)
                    && minute >= window.start_minute
                    && minute < window.end_minute
            } else {
                (window.days.contains(&day) && minute >= window.start_minute)
                    || (window.days.contains(&((day + 6) % 7)) && minute < window.end_minute)
            };
            if matches {
                global = stricter(global, window.global_bps);
                if let Some(allowed) = window.queue_allowed {
                    queue = Some(queue.unwrap_or(true) && allowed);
                }
            }
        }
        EffectivePolicy {
            global_bps: global,
            job_bps: self
                .job_limits
                .iter()
                .find(|limit| limit.id == job.to_string())
                .map(|limit| limit.bps)
                .unwrap_or(self.per_job_bps),
            queue_allowed: queue.unwrap_or(self.queue_allowed),
        }
    }
}

/// Maximum burst: 100 ms of the configured rate, capped at 64 KiB. At very low
/// rates the minimum one-byte burst makes forward progress possible.
pub fn burst(rate: u64) -> u64 {
    (rate / 10).clamp(1, 65536)
}
struct Bucket {
    rate: Option<u64>,
    tokens: f64,
    last: Instant,
}
impl Bucket {
    fn new(now: Instant) -> Self {
        Self {
            rate: None,
            tokens: 0.0,
            last: now,
        }
    }
    fn refill(&mut self, rate: Option<u64>, now: Instant) {
        if rate != self.rate {
            self.rate = rate;
            self.tokens = 0.0;
            self.last = now;
        }
        if let Some(rate) = rate {
            self.tokens = (self.tokens
                + now.saturating_duration_since(self.last).as_secs_f64() * rate as f64)
                .min(burst(rate) as f64);
        }
        self.last = now;
    }
    fn available(&self) -> usize {
        if self.rate.is_none() {
            usize::MAX
        } else {
            self.tokens.floor() as usize
        }
    }
    fn debit(&mut self, amount: usize) {
        if self.rate.is_some() {
            self.tokens -= amount as f64;
        }
    }
}
struct State {
    policy: TrafficPolicy,
    global: Bucket,
    jobs: HashMap<JobId, Bucket>,
}
/// A single atomic debit covers aggregate and per-job buckets, preventing many
/// workers from multiplying a cap. Buckets are removed when a job releases its
/// engine permit. Policy changes reset accumulated credit rather than mint it.
pub struct TrafficController {
    state: Mutex<State>,
}
impl TrafficController {
    /// Create validated runtime control state.
    pub fn new(policy: TrafficPolicy) -> EngineResult<Self> {
        policy.validate()?;
        Ok(Self {
            state: Mutex::new(State {
                policy,
                global: Bucket::new(Instant::now()),
                jobs: HashMap::new(),
            }),
        })
    }
    /// Apply already persisted preferences to active workers.
    pub fn set(&self, policy: TrafficPolicy) -> EngineResult<()> {
        policy.validate()?;
        let mut state = self.state.lock().map_err(|_| invalid())?;
        state.policy = policy;
        state.global = Bucket::new(Instant::now());
        state.jobs.clear();
        Ok(())
    }
    /// Current wall-clock queue gate; evaluated every time, including after jumps.
    pub fn queue_allowed(&self, at: DateTime<Utc>) -> EngineResult<bool> {
        Ok(self
            .state
            .lock()
            .map_err(|_| invalid())?
            .policy
            .effective(at, JobId::nil())
            .queue_allowed)
    }
    /// Atomically claim up to `wanted` bytes. Zero means wait and re-evaluate;
    /// callers poll at most every 100 ms and remain cancellation-aware.
    pub fn take(
        &self,
        job: JobId,
        wanted: usize,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> EngineResult<usize> {
        self.take_limited(job, wanted, None, now, wall)
    }
    /// Additional protocol cap participates in the same atomic shared debit.
    pub fn take_limited(
        &self,
        job: JobId,
        wanted: usize,
        cap: Option<u64>,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> EngineResult<usize> {
        if !rate_valid(cap) {
            return Err(invalid());
        }
        let mut state = self.state.lock().map_err(|_| invalid())?;
        let policy = state.policy.effective(wall, job);
        if !policy.queue_allowed {
            return Ok(0);
        }
        state.global.refill(policy.global_bps, now);
        let global = state.global.available();
        if !state.jobs.contains_key(&job) && state.jobs.len() >= 32 {
            return Err(invalid());
        }
        let individual = state.jobs.entry(job).or_insert_with(|| Bucket::new(now));
        individual.refill(stricter(policy.job_bps, cap), now);
        let amount = wanted.min(global).min(individual.available());
        individual.debit(amount);
        state.global.debit(amount);
        Ok(amount)
    }
    /// Forget an inactive job's credit; resume cannot accumulate a larger burst.
    pub fn release(&self, job: JobId) {
        if let Ok(mut state) = self.state.lock() {
            state.jobs.remove(&job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }
    #[test]
    fn overnight_windows_end_exclusive_and_wall_clock_jumps() {
        let policy = TrafficPolicy {
            timezone: "Asia/Dhaka".into(),
            queue_allowed: false,
            windows: vec![ScheduleWindow {
                days: vec![0],
                start_minute: 1320,
                end_minute: 120,
                global_bps: Some(500),
                queue_allowed: Some(true),
            }],
            ..Default::default()
        };
        policy.validate().unwrap();
        for (at, allowed) in [
            ("2026-10-12T15:59:00Z", false),
            ("2026-10-12T16:00:00Z", true),
            ("2026-10-12T19:59:00Z", true),
            ("2026-10-12T20:00:00Z", false),
            ("2026-10-12T17:00:00Z", true),
        ] {
            assert_eq!(
                policy.effective(utc(at), JobId::nil()).queue_allowed,
                allowed
            );
        }
    }
    #[test]
    fn dst_fall_back_both_occurrences_and_spring_gap() {
        let mut policy = TrafficPolicy {
            timezone: "America/New_York".into(),
            queue_allowed: false,
            windows: vec![ScheduleWindow {
                days: vec![6],
                start_minute: 60,
                end_minute: 120,
                global_bps: None,
                queue_allowed: Some(true),
            }],
            ..Default::default()
        };
        for at in ["2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z"] {
            assert!(policy.effective(utc(at), JobId::nil()).queue_allowed);
        }
        policy.windows[0].start_minute = 120;
        policy.windows[0].end_minute = 180;
        for at in ["2026-03-08T06:59:00Z", "2026-03-08T07:00:00Z"] {
            assert!(!policy.effective(utc(at), JobId::nil()).queue_allowed);
        }
    }
    #[test]
    fn many_workers_share_one_cap_and_pause_credit_is_bounded() {
        let limiter = TrafficController::new(TrafficPolicy {
            global_bps: Some(10000),
            per_job_bps: Some(4000),
            ..Default::default()
        })
        .unwrap();
        let now = Instant::now();
        let jobs = [JobId::new_v4(), JobId::new_v4(), JobId::new_v4()];
        let wall = utc("2026-10-14T00:00:00Z");
        let mut total = 0;
        let mut each = [0; 3];
        for tick in 0..1000 {
            for (index, job) in jobs.iter().enumerate() {
                for _ in 0..8 {
                    let amount = limiter
                        .take(*job, 1024, now + Duration::from_millis(tick * 10), wall)
                        .unwrap();
                    total += amount;
                    each[index] += amount;
                }
            }
        }
        assert!((98500..=100000).contains(&total));
        assert!(each.iter().all(|bytes| *bytes <= 40000));
        limiter.release(jobs[0]);
        let after = now + Duration::from_secs(3600);
        assert_eq!(limiter.take(jobs[0], 100000, after, wall).unwrap(), 0);
        assert!(limiter.take(jobs[1], 100000, after, wall).unwrap() <= burst(4000) as usize);
    }
    proptest::proptest! {
        #[test]
        fn per_job_rate_accuracy_with_many_workers(rate in 100u64..100000, workers in 1usize..16) {
            let limiter=TrafficController::new(TrafficPolicy {per_job_bps:Some(rate),..Default::default()}).unwrap();let job=JobId::new_v4();let now=Instant::now();let wall=Utc::now();let mut bytes=0;
            for tick in 0..1000 {for _ in 0..workers {bytes+=limiter.take(job,65536,now+Duration::from_millis(tick*10),wall).unwrap();}}
            proptest::prop_assert!(bytes as f64 >= rate as f64*9.85-1.0);
            proptest::prop_assert!(bytes as u64 <= rate*10+burst(rate));
        }
        #[test]
        fn aggregate_never_exceeds_rate_times_monotonic_duration(rate in 1u64..100000, workers in 1usize..32) {
            let limiter=TrafficController::new(TrafficPolicy {global_bps:Some(rate),..Default::default()}).unwrap();let job=JobId::new_v4();let now=Instant::now();let mut bytes=0;
            for tick in 0..100 {for _ in 0..workers {bytes+=limiter.take(job,65536,now+Duration::from_millis(tick*10),Utc::now()).unwrap();}}
            proptest::prop_assert!(bytes as u64 <= rate+burst(rate));
        }
    }
}
