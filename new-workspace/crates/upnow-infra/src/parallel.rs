use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
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
    let queue = Arc::new(Mutex::new(
        jobs.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let (result_tx, result_rx) = mpsc::channel::<(usize, R)>();
    let worker_count = threads.max(1).min(job_count);

    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let result_tx = result_tx.clone();
            let worker = &worker;
            let should_stop_after_result = &should_stop_after_result;
            handles.push(scope.spawn(move || {
                loop {
                    if stop_requested.load(Ordering::Relaxed) {
                        break;
                    }

                    let job = queue
                        .lock()
                        .map_or_else(|_| None, |mut jobs| jobs.pop_front());
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
    indexed.sort_by_key(|(index, _)| *index);
    Ok(indexed.into_iter().map(|(_, result)| result).collect())
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::{effective_parallelism, run_ordered_parallel, run_ordered_parallel_stoppable};
    use crate::InfraError;

    #[test]
    fn effective_parallelism_clamps_to_valid_range() {
        assert_eq!(effective_parallelism(0, 4), 1);
        assert_eq!(effective_parallelism(2, 4), 2);
        assert_eq!(effective_parallelism(8, 4), 4);
        assert_eq!(effective_parallelism(8, 0), 1);
    }

    #[test]
    fn parallel_execution_preserves_input_order() {
        let jobs: Vec<usize> = (0..32).collect();

        let result = run_ordered_parallel(jobs.clone(), 4, "test", |job| {
            thread::sleep(Duration::from_millis(((31 - job) % 5) as u64));
            job * 2
        })
        .expect("parallel work should succeed");

        let expected: Vec<usize> = jobs.into_iter().map(|job| job * 2).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn stoppable_parallel_execution_stops_queued_work_and_preserves_completed_order() {
        let stop_requested = AtomicBool::new(false);
        let started = Arc::new(AtomicUsize::new(0));
        let result = run_ordered_parallel_stoppable(
            (0..8).collect(),
            1,
            "test",
            &stop_requested,
            {
                let started = Arc::clone(&started);
                move |job| {
                    started.fetch_add(1, Ordering::SeqCst);
                    job
                }
            },
            |result| *result == 2,
        )
        .expect("parallel work should succeed");

        assert_eq!(result, vec![0, 1, 2]);
        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert!(stop_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn stoppable_parallel_execution_reports_worker_panic() {
        let stop_requested = AtomicBool::new(false);
        let result = run_ordered_parallel_stoppable(
            vec![0],
            1,
            "panic test",
            &stop_requested,
            |_| -> usize { panic!("worker panic") },
            |_| false,
        );

        assert!(matches!(
            result,
            Err(InfraError::ParallelWorkerPanic { label }) if label == "panic test"
        ));
    }
}
