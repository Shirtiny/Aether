use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const LARGE_FRAME_CPU_THRESHOLD_BYTES: usize = 64 * 1024;
const LARGE_FRAME_CPU_WAIT_TIMEOUT: Duration = Duration::from_millis(250);
const LARGE_FRAME_CPU_RETRY_INTERVAL: Duration = Duration::from_millis(5);
const LARGE_FRAME_CPU_WORKERS_ENV: &str = "AETHER_CODEX_WS_LARGE_FRAME_CPU_WORKERS";
const LARGE_FRAME_CPU_ADMISSION_CAPACITY_ENV: &str =
    "AETHER_CODEX_WS_LARGE_FRAME_CPU_ADMISSION_CAPACITY";
const MAX_LARGE_FRAME_CPU_WORKERS: usize = 64;
const MAX_LARGE_FRAME_CPU_ADMISSION_CAPACITY: usize = 256;

struct LargeFrameCpuGate {
    workers: Arc<Semaphore>,
    admission: Arc<Semaphore>,
}

pub(crate) struct LargeFrameCpuPermit {
    _worker: OwnedSemaphorePermit,
    _admission: OwnedSemaphorePermit,
}

static LARGE_FRAME_CPU_GATE: OnceLock<LargeFrameCpuGate> = OnceLock::new();

pub(crate) fn requires_large_frame_cpu_budget(encoded_len: usize) -> bool {
    encoded_len > LARGE_FRAME_CPU_THRESHOLD_BYTES
}

pub(crate) async fn acquire_large_frame_cpu_budget(
    encoded_len: usize,
) -> Result<Option<LargeFrameCpuPermit>, ()> {
    if !requires_large_frame_cpu_budget(encoded_len) {
        return Ok(None);
    }

    let gate = large_frame_cpu_gate();
    // The admission permit bounds Tokio's otherwise unbounded semaphore
    // waiter list before this operation waits for a worker.
    let admission = Arc::clone(&gate.admission)
        .try_acquire_owned()
        .map_err(|_| ())?;
    let worker = tokio::time::timeout(
        LARGE_FRAME_CPU_WAIT_TIMEOUT,
        Arc::clone(&gate.workers).acquire_owned(),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    Ok(Some(LargeFrameCpuPermit {
        _worker: worker,
        _admission: admission,
    }))
}

pub(crate) fn try_acquire_large_frame_cpu_budget(
    encoded_len: usize,
) -> Result<Option<LargeFrameCpuPermit>, ()> {
    if !requires_large_frame_cpu_budget(encoded_len) {
        return Ok(None);
    }

    let gate = large_frame_cpu_gate();
    // Provider writes use this after their final async fence check. Both
    // permits must therefore be obtained without yielding.
    let admission = Arc::clone(&gate.admission)
        .try_acquire_owned()
        .map_err(|_| ())?;
    let worker = Arc::clone(&gate.workers)
        .try_acquire_owned()
        .map_err(|_| ())?;
    Ok(Some(LargeFrameCpuPermit {
        _worker: worker,
        _admission: admission,
    }))
}

pub(crate) async fn acquire_large_frame_cpu_budget_until(
    encoded_len: usize,
    deadline: tokio::time::Instant,
) -> Result<Option<LargeFrameCpuPermit>, ()> {
    if !requires_large_frame_cpu_budget(encoded_len) {
        return Ok(None);
    }

    let gate = large_frame_cpu_gate();
    // Completed terminals already released inference permits and have a
    // bounded delivery deadline. Poll outside the admission semaphore so a
    // saturated lane cannot add unbounded semaphore waiters or discard the
    // terminal while delivery budget remains.
    let admission = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(());
        }
        match Arc::clone(&gate.admission).try_acquire_owned() {
            Ok(permit) => break permit,
            Err(_) => {
                tokio::time::sleep_until(std::cmp::min(
                    deadline,
                    tokio::time::Instant::now() + LARGE_FRAME_CPU_RETRY_INTERVAL,
                ))
                .await;
            }
        }
    };
    let worker = tokio::time::timeout_at(deadline, Arc::clone(&gate.workers).acquire_owned())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    Ok(Some(LargeFrameCpuPermit {
        _worker: worker,
        _admission: admission,
    }))
}

fn large_frame_cpu_gate() -> &'static LargeFrameCpuGate {
    LARGE_FRAME_CPU_GATE.get_or_init(|| {
        let workers = configured_large_frame_cpu_workers();
        let admission = configured_large_frame_cpu_admission_capacity(workers);
        LargeFrameCpuGate {
            workers: Arc::new(Semaphore::new(workers)),
            admission: Arc::new(Semaphore::new(admission)),
        }
    })
}

fn configured_large_frame_cpu_workers() -> usize {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4);
    let default = (available / 4).max(1);
    normalize_large_frame_cpu_workers(
        std::env::var(LARGE_FRAME_CPU_WORKERS_ENV).ok().as_deref(),
        default,
    )
}

fn normalize_large_frame_cpu_workers(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .clamp(1, MAX_LARGE_FRAME_CPU_WORKERS)
}

fn configured_large_frame_cpu_admission_capacity(workers: usize) -> usize {
    normalize_large_frame_cpu_admission_capacity(
        std::env::var(LARGE_FRAME_CPU_ADMISSION_CAPACITY_ENV)
            .ok()
            .as_deref(),
        workers,
    )
}

fn normalize_large_frame_cpu_admission_capacity(raw: Option<&str>, workers: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| workers.saturating_mul(4))
        .clamp(workers, MAX_LARGE_FRAME_CPU_ADMISSION_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_large_frames_enter_the_cpu_lane() {
        assert!(!requires_large_frame_cpu_budget(
            LARGE_FRAME_CPU_THRESHOLD_BYTES
        ));
        assert!(requires_large_frame_cpu_budget(
            LARGE_FRAME_CPU_THRESHOLD_BYTES + 1
        ));
    }

    #[test]
    fn worker_configuration_is_strictly_bounded() {
        assert_eq!(normalize_large_frame_cpu_workers(None, 3), 3);
        assert_eq!(normalize_large_frame_cpu_workers(Some("0"), 3), 3);
        assert_eq!(normalize_large_frame_cpu_workers(Some("bad"), 3), 3);
        assert_eq!(normalize_large_frame_cpu_workers(Some("999"), 3), 64);
        assert_eq!(normalize_large_frame_cpu_workers(Some(" 2 "), 3), 2);
        assert_eq!(normalize_large_frame_cpu_admission_capacity(None, 2), 8);
        assert_eq!(
            normalize_large_frame_cpu_admission_capacity(Some("1"), 2),
            2
        );
        assert_eq!(
            normalize_large_frame_cpu_admission_capacity(Some("999"), 2),
            256
        );
    }
}
