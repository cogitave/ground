use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::ports::Clock;

#[derive(Debug)]
struct Inner {
    now: DateTime<Utc>,
    sleeps: Vec<u64>,
}

/// Virtual time: `sleep` returns at once, advancing the clock and recording the request.
#[derive(Debug, Clone)]
pub struct FakeClock {
    inner: Arc<Mutex<Inner>>,
}

impl FakeClock {
    /// `start` is RFC 3339, e.g. `"2026-01-01T00:00:00Z"`. Panics when it does not parse.
    pub fn new(start: &str) -> Self {
        let now = DateTime::parse_from_rfc3339(start)
            .unwrap_or_else(|e| panic!("FakeClock: bad start {start:?}: {e}"))
            .with_timezone(&Utc);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                now,
                sleeps: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves time forward without recording a sleep.
    pub fn advance(&self, ms: u64) {
        self.lock().now += Duration::milliseconds(ms as i64);
    }

    /// Every `sleep` request so far, in order.
    pub fn sleeps(&self) -> Vec<u64> {
        self.lock().sleeps.clone()
    }

    pub fn total_slept(&self) -> u64 {
        self.lock().sleeps.iter().sum()
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new("2026-01-01T00:00:00.000Z")
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        self.lock().now
    }

    async fn sleep(&self, ms: u64) {
        let mut inner = self.lock();
        inner.sleeps.push(ms);
        inner.now += Duration::milliseconds(ms as i64);
    }
}
