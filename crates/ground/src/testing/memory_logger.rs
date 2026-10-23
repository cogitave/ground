use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::ports::{LogLevel, Logger};

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub level: LogLevel,
    pub msg: String,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryLogger {
    entries: Arc<Mutex<Vec<LogEntry>>>,
}

impl MemoryLogger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Messages, optionally only those of one level.
    pub fn messages(&self, level: Option<LogLevel>) -> Vec<String> {
        self.entries()
            .into_iter()
            .filter(|e| level.is_none_or(|l| e.level == l))
            .map(|e| e.msg)
            .collect()
    }
}

impl Logger for MemoryLogger {
    fn log(&self, level: LogLevel, msg: &str, data: Option<Value>) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LogEntry {
                level,
                msg: msg.to_owned(),
                data,
            });
    }
}
