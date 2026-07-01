use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct StreamIdAllocator {
    next: AtomicU64,
}

impl StreamIdAllocator {
    pub fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
        }
    }

    pub fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}
