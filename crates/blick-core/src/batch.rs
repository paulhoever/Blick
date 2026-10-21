//! Batch runs over many files: parallel, cancellable, with progress.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::error::{CoreError, Result};

#[derive(Debug, Default)]
pub struct Report {
    pub done: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, CoreError)>,
    pub cancelled: usize,
}

/// Runs `op` for every file. `progress(done, total)` is called from
/// worker threads; the app forwards it to the UI thread itself.
pub fn run<T, F, P>(paths: &[PathBuf], cancel: &AtomicBool, progress: P, op: F) -> Report
where
    F: Fn(&Path) -> Result<T> + Sync,
    P: Fn(usize, usize) + Sync,
{
    let total = paths.len();
    let finished = AtomicUsize::new(0);
    let results: Vec<_> = paths
        .par_iter()
        .map(|p| {
            if cancel.load(Ordering::Relaxed) {
                return (p.clone(), Err(CoreError::Cancelled));
            }
            let r = op(p).map(|_| ());
            progress(finished.fetch_add(1, Ordering::Relaxed) + 1, total);
            (p.clone(), r)
        })
        .collect();

    let mut report = Report::default();
    for (path, r) in results {
        match r {
            Ok(()) => report.done.push(path),
            Err(CoreError::Cancelled) => report.cancelled += 1,
            Err(e) => report.failed.push((path, e)),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_errors_and_respects_cancel() {
        let paths: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("{i}"))).collect();
        let cancel = AtomicBool::new(false);
        let report = run(
            &paths,
            &cancel,
            |_, _| {},
            |p| {
                if p.as_os_str() == "3" {
                    Err(CoreError::Unsupported(p.to_owned()))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(report.done.len(), 19);
        assert_eq!(report.failed.len(), 1);

        cancel.store(true, Ordering::Relaxed);
        let report = run(&paths, &cancel, |_, _| {}, |_| Ok(()));
        assert_eq!(report.cancelled, 20);
    }
}
