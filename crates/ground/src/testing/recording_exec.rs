use std::sync::{Arc, Mutex};

use crate::adapters::Exec;
use crate::ports::{PortError, PortResult};

/// An [`Exec`] that runs nothing: it records each call and answers with a fixed stdout
/// (or a failure, when built with [`RecordingExec::failing`]).
#[derive(Debug, Clone)]
pub struct RecordingExec {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    result: Result<String, String>,
}

impl RecordingExec {
    pub fn new(stdout: &str) -> Self {
        Self {
            calls: Arc::default(),
            result: Ok(stdout.into()),
        }
    }

    pub fn failing(message: &str) -> Self {
        Self {
            calls: Arc::default(),
            result: Err(message.into()),
        }
    }

    /// Each call as `[cmd, arg, ...]`.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Exec for RecordingExec {
    async fn run(&self, cmd: &str, args: &[String]) -> PortResult<String> {
        let mut call = vec![cmd.to_owned()];
        call.extend(args.iter().cloned());
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
        self.result.clone().map_err(PortError::Message)
    }
}
