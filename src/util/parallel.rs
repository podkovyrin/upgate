use anyhow::{Context, Result};
use rayon::prelude::*;

pub fn effective_parallelism(requested: usize, manager_cap: usize) -> usize {
    requested.clamp(1, manager_cap.max(1))
}

pub fn run_indexed_parallel<J, R, F>(
    jobs: Vec<J>,
    threads: usize,
    manager_id: &str,
    resolver: F,
) -> Result<Vec<R>>
where
    J: Send,
    R: Send,
    F: Fn(J) -> R + Sync + Send,
{
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .with_context(|| format!("failed to build {manager_id} planning thread pool"))?;

    Ok(pool.install(|| jobs.into_par_iter().map(resolver).collect()))
}

#[cfg(test)]
mod tests {
    use super::run_indexed_parallel;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn run_indexed_parallel_preserves_input_order() {
        let jobs: Vec<usize> = (0..32).collect();

        let result = run_indexed_parallel(jobs.clone(), 4, "test", |job| {
            // Force uneven completion times to exercise ordering guarantees.
            thread::sleep(Duration::from_millis(((31 - job) % 5) as u64));
            job * 2
        })
        .expect("parallel planning should succeed");

        let expected: Vec<usize> = jobs.into_iter().map(|job| job * 2).collect();
        assert_eq!(result, expected);
    }
}
