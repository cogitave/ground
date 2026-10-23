use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::ports::Clock;

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_real_date() {
        assert!(SystemClock.now().timestamp() > 1_700_000_000);
    }
}
