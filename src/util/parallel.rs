use anyhow::{Context, Result};
use rayon::prelude::*;

pub(crate) fn effective_parallelism(requested: usize, manager_cap: usize) -> usize {
    requested.clamp(1, manager_cap.max(1))
}

pub(crate) fn run_indexed_parallel<J, R, F>(
    jobs: Vec<J>,
    threads: usize,
    pool_error_context: &'static str,
    missing_slot_context: &'static str,
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
        .context(pool_error_context)?;

    let indexed: Vec<(usize, R)> = pool.install(|| {
        jobs.into_par_iter()
            .enumerate()
            .map(|(index, job)| (index, resolver(job)))
            .collect()
    });

    let mut slots: Vec<Option<R>> = (0..indexed.len()).map(|_| None).collect();
    for (index, item) in indexed {
        slots[index] = Some(item);
    }

    slots
        .into_iter()
        .map(|item| item.context(missing_slot_context))
        .collect()
}
