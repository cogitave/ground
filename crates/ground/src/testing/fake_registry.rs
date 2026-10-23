use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ports::{PortResult, RegistryPort};

#[derive(Debug, Default)]
struct Inner {
    queries: Vec<String>,
    versions: HashMap<String, String>,
    scheduled: HashMap<String, (u32, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl FakeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publish immediately.
    pub fn set_version(&self, pkg: &str, version: &str) -> &Self {
        self.lock().versions.insert(pkg.into(), version.into());
        self
    }

    /// Simulate propagation lag: `version` becomes visible on the `(after + 1)`th query of `pkg`.
    pub fn publish_after_queries(&self, pkg: &str, version: &str, after: u32) -> &Self {
        self.lock()
            .scheduled
            .insert(pkg.into(), (after, version.into()));
        self
    }

    /// Every package asked about, in order.
    pub fn queries(&self) -> Vec<String> {
        self.lock().queries.clone()
    }
}

impl RegistryPort for FakeRegistry {
    async fn latest_version(&self, pkg: &str) -> PortResult<Option<String>> {
        let mut inner = self.lock();
        inner.queries.push(pkg.to_owned());
        if let Some((after, version)) = inner.scheduled.get_mut(pkg) {
            if *after == 0 {
                let version = version.clone();
                inner.scheduled.remove(pkg);
                inner.versions.insert(pkg.to_owned(), version);
            } else {
                *after -= 1;
            }
        }
        Ok(inner.versions.get(pkg).cloned())
    }
}
