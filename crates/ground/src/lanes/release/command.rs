use super::ship::ship;
use super::types::{ShipDeps, ShipError, ShipOptions};
use crate::ports::{Clock, GitHubPort, Logger, RegistryPort};

/// `ground ship` without the argument parsing: runs the lane, writes the summary
/// (or the outcome as JSON), and returns the exit code. Exit 0 only when there is
/// nothing left for a human to look at.
pub async fn run_ship_command<G, R, C, L>(
    deps: &ShipDeps<'_, G, R, C, L>,
    confirm: bool,
    json: bool,
    mut write: impl FnMut(&str),
) -> Result<i32, ShipError>
where
    G: GitHubPort,
    R: RegistryPort,
    C: Clock,
    L: Logger,
{
    let options = ShipOptions {
        confirm,
        ..ShipOptions::default()
    };
    let outcome = ship(deps, &options).await?;
    if json {
        write(&serde_json::to_string_pretty(&outcome)?);
    } else {
        write(&outcome.summary);
    }
    Ok(if outcome.status.is_ok() { 0 } else { 1 })
}
