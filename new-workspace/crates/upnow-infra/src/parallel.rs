use rayon::prelude::*;

use crate::InfraError;

#[must_use]
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

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::{effective_parallelism, run_ordered_parallel};

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
}
