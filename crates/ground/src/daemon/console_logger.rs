use chrono::SecondsFormat;
use serde_json::Value;

use crate::ports::{Clock, LogLevel, Logger};

/// One line per entry on stderr, so stdout stays clean for command output and journald picks it up.
pub struct ConsoleLogger<C: Clock> {
    clock: C,
    write: Box<dyn Fn(&str) + Send + Sync>,
    verbose: bool,
}

impl<C: Clock> ConsoleLogger<C> {
    pub fn new(clock: C) -> Self {
        Self::with_writer(clock, |l| eprintln!("{l}"), false)
    }

    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn with_writer(
        clock: C,
        write: impl Fn(&str) + Send + Sync + 'static,
        verbose: bool,
    ) -> Self {
        Self {
            clock,
            write: Box::new(write),
            verbose,
        }
    }
}

impl<C: Clock> Logger for ConsoleLogger<C> {
    fn log(&self, level: LogLevel, msg: &str, data: Option<Value>) {
        if level == LogLevel::Debug && !self.verbose {
            return;
        }
        let tail = data.map(|d| format!(" {d}")).unwrap_or_default();
        (self.write)(&format!(
            "{} {} {msg}{tail}",
            self.clock
                .now()
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            level.as_str().to_uppercase()
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::testing::FakeClock;

    #[test]
    fn writes_one_line_per_entry_and_hides_debug_unless_verbose() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let log = ConsoleLogger::with_writer(
            FakeClock::default(),
            move |l| sink.lock().unwrap().push(l.to_owned()),
            false,
        );
        log.debug("hidden", None);
        log.info("hello", Some(json!({"a": 1})));
        log.error("bad", None);
        assert_eq!(
            *lines.lock().unwrap(),
            [
                "2026-01-01T00:00:00.000Z INFO hello {\"a\":1}",
                "2026-01-01T00:00:00.000Z ERROR bad"
            ]
        );
    }
}
