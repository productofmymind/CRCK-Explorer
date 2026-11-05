//! Handle for a background job: the worker reports progress and checks for cancellation, the window reads one and sets the other.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    Arc,
};

#[derive(Clone, Default)]
pub struct Job {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
}

impl Job {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    pub fn set_total(&self, n: usize) {
        self.total.store(n, Relaxed);
    }

    /// one more unit of work finished
    pub fn step(&self) {
        self.done.fetch_add(1, Relaxed);
    }

    /// (finished, total); total is 0 until the worker knows it
    pub fn progress(&self) -> (usize, usize) {
        (self.done.load(Relaxed), self.total.load(Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_state() {
        let a = Job::new();
        let b = a.clone();
        b.set_total(3);
        b.step();
        a.cancel();
        assert_eq!(a.progress(), (1, 3));
        assert!(b.is_cancelled());
    }
}
