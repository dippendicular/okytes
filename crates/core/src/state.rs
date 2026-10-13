//! Download job and segment state machines.
//!
//! # Invariant
//!
//! Every state transition is **explicit** and **idempotent**: replaying the
//! same transition from the same state produces the same result. This ensures
//! crash recovery can safely replay the log without side effects.
//!
//! Invalid transitions return an error rather than panicking.

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use uuid::Uuid;

/// State of a download job.
///
/// Transitions are validated by [`JobState::transition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobState {
    /// Job is queued, waiting to start.
    Queued,
    /// Job is probing the remote server (HEAD request).
    Probing,
    /// Job is actively downloading.
    Downloading,
    /// Job is paused by the user.
    Paused,
    /// Job is completed successfully.
    Completed,
    /// Job failed with an error.
    Failed,
    /// Job was cancelled by the user.
    Cancelled,
    /// Job is verifying integrity (hash check).
    Verifying,
}

impl JobState {
    /// Attempt a state transition. Returns `Ok(new_state)` if the transition
    /// is valid, or `Err` if it is not.
    ///
    /// # Idempotency
    ///
    /// Transitioning from a state to itself is always allowed (no-op).
    pub fn transition(self, to: JobState, job_id: Uuid) -> Result<JobState, EngineError> {
        // Idempotent: same state → same state is always ok.
        if self == to {
            return Ok(to);
        }

        let valid = matches!(
            (self, to),
            // From Queued
            (JobState::Queued, JobState::Probing)
                | (JobState::Queued, JobState::Paused)
                | (JobState::Probing, JobState::Paused)
                | (JobState::Probing, JobState::Queued)
                | (JobState::Downloading, JobState::Queued)
                | (JobState::Verifying, JobState::Queued)
                | (JobState::Verifying, JobState::Paused)
                | (JobState::Verifying, JobState::Cancelled)
                | (JobState::Queued, JobState::Cancelled)
                // From Probing
                | (JobState::Probing, JobState::Downloading)
                | (JobState::Probing, JobState::Failed)
                | (JobState::Probing, JobState::Cancelled)
                // From Downloading
                | (JobState::Downloading, JobState::Verifying)
                | (JobState::Downloading, JobState::Paused)
                | (JobState::Downloading, JobState::Failed)
                | (JobState::Downloading, JobState::Cancelled)
                // From Verifying
                | (JobState::Verifying, JobState::Completed)
                | (JobState::Verifying, JobState::Failed)
                // From Paused
                | (JobState::Paused, JobState::Queued)
                | (JobState::Paused, JobState::Cancelled)
                // From Failed (retry)
                | (JobState::Failed, JobState::Queued)
                | (JobState::Failed, JobState::Cancelled)
        );

        if valid {
            Ok(to)
        } else {
            Err(EngineError::InvalidTransition {
                job_id,
                detail: format!("{:?} -> {:?}", self, to),
            })
        }
    }

    /// Returns true if the job is in a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Completed | JobState::Cancelled)
    }

    /// Returns true if the job is currently active (probing or downloading).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            JobState::Probing | JobState::Downloading | JobState::Verifying
        )
    }

    /// Returns true if the job can be resumed.
    pub fn is_resumable(self) -> bool {
        matches!(self, JobState::Paused | JobState::Failed)
    }
}

impl std::fmt::Display for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobState::Queued => write!(f, "queued"),
            JobState::Probing => write!(f, "probing"),
            JobState::Downloading => write!(f, "downloading"),
            JobState::Paused => write!(f, "paused"),
            JobState::Completed => write!(f, "completed"),
            JobState::Failed => write!(f, "failed"),
            JobState::Cancelled => write!(f, "cancelled"),
            JobState::Verifying => write!(f, "verifying"),
        }
    }
}

/// State of a download segment (a byte-range worker).
///
/// Transitions are validated by [`SegmentState::transition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SegmentState {
    /// Segment is pending, waiting to be assigned a worker.
    Pending,
    /// Segment is actively downloading.
    Active,
    /// Segment download is complete.
    Complete,
    /// Segment failed with an error.
    Failed,
    /// Segment was cancelled.
    Cancelled,
}

impl SegmentState {
    /// Attempt a state transition. Returns `Ok(new_state)` if the transition
    /// is valid, or `Err` if it is not.
    ///
    /// # Idempotency
    ///
    /// Transitioning from a state to itself is always allowed (no-op).
    pub fn transition(self, to: SegmentState, job_id: Uuid) -> Result<SegmentState, EngineError> {
        // Idempotent: same state → same state is always ok.
        if self == to {
            return Ok(to);
        }

        let valid = matches!(
            (self, to),
            // From Pending
            (SegmentState::Pending, SegmentState::Active)
                | (SegmentState::Pending, SegmentState::Cancelled)
                // From Active
                | (SegmentState::Active, SegmentState::Complete)
                | (SegmentState::Active, SegmentState::Failed)
                | (SegmentState::Active, SegmentState::Cancelled)
                // From Failed (retry)
                | (SegmentState::Failed, SegmentState::Pending)
                | (SegmentState::Failed, SegmentState::Cancelled)
        );

        if valid {
            Ok(to)
        } else {
            Err(EngineError::InvalidTransition {
                job_id,
                detail: format!("segment {:?} -> {:?}", self, to),
            })
        }
    }

    /// Returns true if the segment is in a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, SegmentState::Complete | SegmentState::Cancelled)
    }
}

impl std::fmt::Display for SegmentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SegmentState::Pending => write!(f, "pending"),
            SegmentState::Active => write!(f, "active"),
            SegmentState::Complete => write!(f, "complete"),
            SegmentState::Failed => write!(f, "failed"),
            SegmentState::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn transition_replay_is_idempotent(from in 0usize..8,to in 0usize..8) {
            let states=[JobState::Queued,JobState::Probing,JobState::Downloading,JobState::Paused,JobState::Completed,JobState::Failed,JobState::Cancelled,JobState::Verifying];
            let id=Uuid::nil();if let Ok(next)=states[from].transition(states[to],id) {prop_assert_eq!(next.transition(next,id).unwrap(),next);}
        }
        #[test]
        fn segment_transition_replay_is_idempotent(from in 0usize..5,to in 0usize..5) {
            let states=[SegmentState::Pending,SegmentState::Active,SegmentState::Complete,SegmentState::Failed,SegmentState::Cancelled];
            let id=Uuid::nil();if let Ok(next)=states[from].transition(states[to],id) {prop_assert_eq!(next.transition(next,id).unwrap(),next);}
        }
    }

    #[test]
    fn test_job_state_idempotent() {
        let id = Uuid::new_v4();
        for state in [
            JobState::Queued,
            JobState::Probing,
            JobState::Downloading,
            JobState::Paused,
            JobState::Completed,
            JobState::Failed,
            JobState::Cancelled,
            JobState::Verifying,
        ] {
            assert_eq!(state.transition(state, id).unwrap(), state);
        }
    }

    #[test]
    fn test_job_state_valid_transitions() {
        let id = Uuid::new_v4();
        assert_eq!(
            JobState::Queued.transition(JobState::Probing, id).unwrap(),
            JobState::Probing
        );
        assert_eq!(
            JobState::Probing
                .transition(JobState::Downloading, id)
                .unwrap(),
            JobState::Downloading
        );
        assert_eq!(
            JobState::Downloading
                .transition(JobState::Verifying, id)
                .unwrap(),
            JobState::Verifying
        );
        assert_eq!(
            JobState::Verifying
                .transition(JobState::Completed, id)
                .unwrap(),
            JobState::Completed
        );
        assert_eq!(
            JobState::Downloading
                .transition(JobState::Paused, id)
                .unwrap(),
            JobState::Paused
        );
        assert_eq!(
            JobState::Paused.transition(JobState::Queued, id).unwrap(),
            JobState::Queued
        );
        assert_eq!(
            JobState::Failed.transition(JobState::Queued, id).unwrap(),
            JobState::Queued
        );
    }

    #[test]
    fn test_job_state_invalid_transitions() {
        let id = Uuid::new_v4();
        assert!(JobState::Completed
            .transition(JobState::Downloading, id)
            .is_err());
        assert!(JobState::Queued
            .transition(JobState::Downloading, id)
            .is_err());
        assert!(JobState::Cancelled
            .transition(JobState::Queued, id)
            .is_err());
        assert!(JobState::Paused
            .transition(JobState::Downloading, id)
            .is_err());
    }

    #[test]
    fn test_segment_state_idempotent() {
        let id = Uuid::new_v4();
        for state in [
            SegmentState::Pending,
            SegmentState::Active,
            SegmentState::Complete,
            SegmentState::Failed,
            SegmentState::Cancelled,
        ] {
            assert_eq!(state.transition(state, id).unwrap(), state);
        }
    }

    #[test]
    fn test_segment_state_valid_transitions() {
        let id = Uuid::new_v4();
        assert_eq!(
            SegmentState::Pending
                .transition(SegmentState::Active, id)
                .unwrap(),
            SegmentState::Active
        );
        assert_eq!(
            SegmentState::Active
                .transition(SegmentState::Complete, id)
                .unwrap(),
            SegmentState::Complete
        );
        assert_eq!(
            SegmentState::Active
                .transition(SegmentState::Failed, id)
                .unwrap(),
            SegmentState::Failed
        );
        assert_eq!(
            SegmentState::Failed
                .transition(SegmentState::Pending, id)
                .unwrap(),
            SegmentState::Pending
        );
    }

    #[test]
    fn test_segment_state_invalid_transitions() {
        let id = Uuid::new_v4();
        assert!(SegmentState::Complete
            .transition(SegmentState::Active, id)
            .is_err());
        assert!(SegmentState::Cancelled
            .transition(SegmentState::Pending, id)
            .is_err());
    }

    #[test]
    fn test_job_terminal_states() {
        assert!(JobState::Completed.is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(!JobState::Failed.is_terminal());
        assert!(!JobState::Downloading.is_terminal());
    }
}
