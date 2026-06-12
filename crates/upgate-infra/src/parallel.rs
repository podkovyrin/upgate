use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;

use rayon::prelude::*;

use crate::InfraError;
pub fn effective_parallelism(requested: usize, manager_cap: usize) -> usize {
    requested.clamp(1, manager_cap.max(1))
}

/// Runs jobs in parallel and returns results in the original input order.
///
/// # Errors
///
/// Returns an error when the worker thread pool cannot be created.
pub fn run_ordered_parallel<J, R, F>(
    jobs: Vec<J>,
    threads: usize,
    label: &str,
    worker: F,
) -> Result<Vec<R>, InfraError>
where
    J: Send,
    R: Send,
    F: Fn(J) -> R + Sync + Send,
{
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build()
        .map_err(|err| InfraError::ParallelPoolBuild {
            label: label.to_owned(),
            detail: err.to_string(),
        })?;

    Ok(pool.install(|| jobs.into_par_iter().map(worker).collect()))
}

/// Runs jobs on a bounded worker set, stops pulling queued jobs when requested,
/// and returns completed results in original input order.
///
/// Already-running jobs are allowed to finish; the stop flag prevents starting
/// additional queued work.
///
/// # Errors
///
/// Returns an error when a worker thread panics.
pub fn run_ordered_parallel_stoppable<J, R, F, S>(
    jobs: Vec<J>,
    threads: usize,
    label: &str,
    stop_requested: &AtomicBool,
    worker: F,
    should_stop_after_result: S,
) -> Result<Vec<R>, InfraError>
where
    J: Send,
    R: Send,
    F: Fn(J) -> R + Sync,
    S: Fn(&R) -> bool + Sync,
{
    if jobs.is_empty() {
        return Ok(Vec::new());
    }

    let job_count = jobs.len();
    let queue = Mutex::new(jobs.into_iter().enumerate().collect::<VecDeque<_>>());
    let (result_tx, result_rx) = mpsc::channel::<(usize, R)>();
    let worker_count = threads.max(1).min(job_count);

    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let queue = &queue;
            let result_tx = result_tx.clone();
            let worker = &worker;
            let should_stop_after_result = &should_stop_after_result;
            handles.push(scope.spawn(move || {
                loop {
                    if stop_requested.load(Ordering::Relaxed) {
                        break;
                    }

                    let job = queue.lock().ok().and_then(|mut jobs| jobs.pop_front());
                    let Some((index, job)) = job else {
                        break;
                    };

                    let result = worker(job);
                    if should_stop_after_result(&result) {
                        stop_requested.store(true, Ordering::Relaxed);
                    }
                    if result_tx.send((index, result)).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(result_tx);

        for handle in handles {
            handle.join().map_err(|_| InfraError::ParallelWorkerPanic {
                label: label.to_owned(),
            })?;
        }

        Ok(())
    })?;

    let mut indexed = result_rx.into_iter().collect::<Vec<_>>();
    indexed.sort_unstable_by_key(|(index, _)| *index);
    Ok(indexed.into_iter().map(|(_, result)| result).collect())
}
