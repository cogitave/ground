use std::future::Future;

use crate::ports::{PortError, PortResult};

/// Runs an external command and returns its stdout. A seam so tests never spawn `gh`.
pub trait Exec: Send + Sync {
    fn run(&self, cmd: &str, args: &[String]) -> impl Future<Output = PortResult<String>> + Send;
}

/// A hung `gh` must not hang a poll or a ship.
const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Spawns the process for real.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemExec;

impl Exec for SystemExec {
    async fn run(&self, cmd: &str, args: &[String]) -> PortResult<String> {
        let child = tokio::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(EXEC_TIMEOUT, child)
            .await
            .map_err(|_| PortError::msg(format!("`{cmd}` did not finish within {EXEC_TIMEOUT:?}")))?
            .map_err(|e| PortError::msg(format!("cannot run `{cmd}`: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(PortError::msg(format!(
                "`{cmd} {}` failed ({}): {}",
                args.join(" "),
                out.status,
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The token of the logged-in `gh`.
pub async fn token_from_gh(exec: &impl Exec) -> PortResult<String> {
    let out = exec.run("gh", &["auth".into(), "token".into()]).await?;
    let token = out.trim();
    if token.is_empty() {
        return Err(PortError::msg(
            "`gh auth token` returned nothing; run `gh auth login`",
        ));
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RecordingExec;

    #[tokio::test]
    async fn reads_the_token_through_the_injected_exec() {
        let exec = RecordingExec::new("tok\n");
        assert_eq!(token_from_gh(&exec).await.unwrap(), "tok");
        assert_eq!(exec.calls(), [["gh", "auth", "token"]]);
        let err = token_from_gh(&RecordingExec::new("\n")).await.unwrap_err();
        assert!(err.to_string().contains("gh auth login"));
    }
}
