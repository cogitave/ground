use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::json;

use crate::adapters::{SystemClock, SystemExec, token_from_gh};
use crate::daemon::{db_path, load_repos, socket_path, socket_request};
use crate::state::StateStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub ok: bool,
    pub detail: String,
}

impl CheckResult {
    pub fn ok(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: detail.into(),
        }
    }
    pub fn fail(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

type CheckFuture = Pin<Box<dyn Future<Output = Result<CheckResult, String>> + Send>>;

pub struct Check {
    pub name: String,
    /// A failing warn check is reported but does not fail doctor: ground fails open.
    pub warn: bool,
    run: Box<dyn Fn() -> CheckFuture + Send + Sync>,
}

impl Check {
    /// `run` returning `Err` is a check that blew up; it fails with that message.
    pub fn new<F, Fut>(name: &str, run: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CheckResult, String>> + Send + 'static,
    {
        Self {
            name: name.to_owned(),
            warn: false,
            run: Box::new(move || Box::pin(run())),
        }
    }

    pub fn warn_only(mut self) -> Self {
        self.warn = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub name: String,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub rows: Vec<DoctorRow>,
    pub exit_code: i32,
}

pub async fn run_checks(checks: &[Check]) -> DoctorReport {
    let mut rows = Vec::new();
    for c in checks {
        let r = (c.run)().await.unwrap_or_else(CheckResult::fail);
        let status = if r.ok {
            DoctorStatus::Ok
        } else if c.warn {
            DoctorStatus::Warn
        } else {
            DoctorStatus::Fail
        };
        rows.push(DoctorRow {
            name: c.name.clone(),
            status,
            detail: r.detail,
        });
    }
    let exit_code = i32::from(rows.iter().any(|r| r.status == DoctorStatus::Fail));
    DoctorReport { rows, exit_code }
}

pub fn format_report(report: &DoctorReport) -> String {
    let w = report
        .rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(5);
    report
        .rows
        .iter()
        .map(|r| format!("{:<4}  {:<w$}  {}", r.status.label(), r.name, r.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn default_checks(home: &Path) -> Vec<Check> {
    let home: PathBuf = home.to_owned();
    let (h1, h2, h3) = (home.clone(), home.clone(), home);
    vec![
        // The reference checked the Node version; the Rust binary has no runtime to check.
        Check::new("ground", || async {
            Ok(CheckResult::ok(format!(
                "ground {}",
                env!("CARGO_PKG_VERSION")
            )))
        }),
        Check::new("gh token", || async {
            token_from_gh(&SystemExec)
                .await
                .map_err(|e| e.to_string())?;
            Ok(CheckResult::ok("gh auth token works"))
        }),
        Check::new("repo configs", move || {
            let home = h1.clone();
            async move {
                let loaded = load_repos(&home);
                if !loaded.errors.is_empty() {
                    return Ok(CheckResult::fail(loaded.errors.join("; ")));
                }
                if loaded.configs.is_empty() {
                    return Ok(CheckResult::fail(format!(
                        "no configs in {}/repos",
                        home.display()
                    )));
                }
                Ok(CheckResult::ok(
                    loaded
                        .configs
                        .iter()
                        .map(|c| c.repo.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ))
            }
        }),
        Check::new("state db", move || {
            let home = h2.clone();
            async move {
                StateStore::new(SystemClock, db_path(&home)).map_err(|e| e.to_string())?;
                Ok(CheckResult::ok(db_path(&home).display().to_string()))
            }
        }),
        Check::new("daemon socket", move || {
            let home = h3.clone();
            async move {
                let path = socket_path(&home);
                Ok(
                    match socket_request(&path, &json!({"cmd": "status"})).await {
                        Ok(_) => CheckResult::ok(path.display().to_string()),
                        Err(_) => CheckResult::fail(
                            "groundd is not running (ground still works without it)",
                        ),
                    },
                )
            }
        })
        .warn_only(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str) -> Check {
        Check::new(name, || async { Ok(CheckResult::ok("fine")) })
    }

    #[tokio::test]
    async fn exits_0_when_every_check_passes() {
        let r = run_checks(&[ok("a"), ok("b")]).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.rows.iter().all(|x| x.status == DoctorStatus::Ok));
    }

    #[tokio::test]
    async fn exits_1_on_a_failing_or_throwing_check_and_keeps_running_the_rest() {
        let r = run_checks(&[
            Check::new("bad", || async { Ok(CheckResult::fail("broken")) }),
            Check::new("boom", || async { Err("kaput".to_owned()) }),
            ok("last"),
        ])
        .await;
        assert_eq!(r.exit_code, 1);
        let details: Vec<_> = r.rows.iter().map(|x| x.detail.as_str()).collect();
        assert_eq!(details, ["broken", "kaput", "fine"]);
    }

    #[tokio::test]
    async fn a_failing_warn_check_is_non_fatal() {
        let r = run_checks(&[
            Check::new("socket", || async { Ok(CheckResult::fail("down")) }).warn_only(),
        ])
        .await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.rows[0].status, DoctorStatus::Warn);
    }

    #[tokio::test]
    async fn prints_a_table_with_one_line_per_check() {
        let text = format_report(&run_checks(&[ok("node"), ok("gh token")]).await);
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("gh token"));
        assert!(text.starts_with("ok    node      fine"));
    }
}
