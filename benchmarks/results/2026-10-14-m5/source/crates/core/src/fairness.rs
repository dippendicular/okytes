//! Bounded job-aware quantum admission. Least-active jobs receive the next slot;
//! equal contenders rotate FIFO. A permit owns accounting until its body drops.
use crate::{
    error::{EngineError, EngineResult},
    types::JobId,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

struct Waiter {
    ticket: u64,
    job: JobId,
    origin: String,
    reply: oneshot::Sender<Permit>,
}
struct State {
    next: u64,
    active: usize,
    jobs: HashMap<JobId, usize>,
    origins: HashMap<String, usize>,
    queue: VecDeque<Waiter>,
}
/// Shared global and origin cap with per-job fair admission at request boundaries.
pub struct FairGate {
    state: Mutex<State>,
    global: usize,
    origin: usize,
}
/// RAII quantum slot. Cancellation releases both caps and wakes queued jobs.
pub struct Permit {
    gate: Arc<FairGate>,
    job: JobId,
    origin: String,
    armed: bool,
}
struct Pending {
    gate: Arc<FairGate>,
    ticket: u64,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.queue.retain(|item| item.ticket != self.ticket);
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut state) = self.gate.state.lock() {
            state.active = state.active.saturating_sub(1);
            if let Some(count) = state.jobs.get_mut(&self.job) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.jobs.remove(&self.job);
                }
            }
            if let Some(count) = state.origins.get_mut(&self.origin) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.origins.remove(&self.origin);
                }
            }
            self.gate.dispatch(&mut state);
        }
    }
}
fn invalid() -> EngineError {
    EngineError::Scheduler("fair admission unavailable".into())
}
impl FairGate {
    /// Construct bounded admission; engine configuration validates these caps.
    pub fn new(global: usize, origin: usize) -> Arc<Self> {
        Arc::new(Self {
            global: global.max(1),
            origin: origin.max(1),
            state: Mutex::new(State {
                next: 0,
                active: 0,
                jobs: HashMap::new(),
                origins: HashMap::new(),
                queue: VecDeque::new(),
            }),
        })
    }
    fn dispatch(self: &Arc<Self>, state: &mut State) {
        state.queue.retain(|waiter| !waiter.reply.is_closed());
        while state.active < self.global {
            let Some(index) = state
                .queue
                .iter()
                .enumerate()
                .filter(|(_, w)| state.origins.get(&w.origin).copied().unwrap_or(0) < self.origin)
                .min_by_key(|(_, w)| state.jobs.get(&w.job).copied().unwrap_or(0))
                .map(|(index, _)| index)
            else {
                break;
            };
            let Some(waiter) = state.queue.remove(index) else {
                break;
            };
            state.active += 1;
            *state.jobs.entry(waiter.job).or_default() += 1;
            *state.origins.entry(waiter.origin.clone()).or_default() += 1;
            let permit = Permit {
                gate: self.clone(),
                job: waiter.job,
                origin: waiter.origin,
                armed: true,
            };
            // A canceled receiver can race dispatch. Undo under this lock rather
            // than dropping a permit that would recursively acquire the lock.
            if let Err(mut permit) = waiter.reply.send(permit) {
                state.active -= 1;
                if let Some(count) = state.jobs.get_mut(&permit.job) {
                    *count -= 1;
                }
                if let Some(count) = state.origins.get_mut(&permit.origin) {
                    *count -= 1;
                }
                permit.armed = false;
            }
        }
        state.jobs.retain(|_, count| *count > 0);
        state.origins.retain(|_, count| *count > 0);
    }
    /// Wait for one slot; dropping this future removes its bounded wait ticket.
    pub async fn acquire(self: &Arc<Self>, job: JobId, origin: String) -> EngineResult<Permit> {
        let (reply, receive) = oneshot::channel();
        let ticket = {
            let mut state = self.state.lock().map_err(|_| invalid())?;
            if state.queue.len() >= 4096 {
                return Err(invalid());
            }
            state.next = state.next.wrapping_add(1);
            let ticket = state.next;
            state.queue.push_back(Waiter {
                ticket,
                job,
                origin,
                reply,
            });
            self.dispatch(&mut state);
            ticket
        };
        let _pending = Pending {
            gate: self.clone(),
            ticket,
        };
        receive.await.map_err(|_| invalid())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_does_not_leak_global_or_origin_slots() {
        let gate = FairGate::new(1, 1);
        let job = JobId::new_v4();
        let first = gate.acquire(job, "a".into()).await.unwrap();
        let task = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(job, "a".into()).await })
        };
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;
        drop(first);
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gate.acquire(job, "a".into()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permit);
        assert_eq!(gate.state.lock().unwrap().active, 0);
    }
    #[tokio::test]
    async fn saturated_equal_jobs_have_no_starvation_and_bounded_skew() {
        let gate = FairGate::new(4, 2);
        let mut tasks = tokio::task::JoinSet::new();
        let counts = Arc::new(Mutex::new(vec![0usize; 4]));
        let skew = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..4 {
            let gate = gate.clone();
            let counts = counts.clone();
            let skew = skew.clone();
            tasks.spawn(async move {
                let id = JobId::new_v4();
                for _ in 0..100 {
                    let permit = gate.acquire(id, "shared".into()).await.unwrap();
                    {
                        let mut counts = counts.lock().unwrap();
                        counts[index] += 1;
                        let difference =
                            counts.iter().max().unwrap() - counts.iter().min().unwrap();
                        skew.fetch_max(difference, std::sync::atomic::Ordering::SeqCst);
                    }
                    tokio::task::yield_now().await;
                    drop(permit);
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        assert_eq!(*counts.lock().unwrap(), vec![100; 4]);
        assert!(
            skew.load(std::sync::atomic::Ordering::SeqCst) <= 4,
            "equal-sized backlogged jobs differed by more than four quanta"
        );
        let state = gate.state.lock().unwrap();
        assert_eq!(state.active, 0);
        assert!(state.queue.is_empty());
    }
}
