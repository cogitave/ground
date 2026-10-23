//! The `groundd` daemon: watches the configured repositories and serves the status socket.

#[tokio::main]
async fn main() {
    let env = std::env::vars().collect();
    std::process::exit(ground::daemon::run_daemon(&env).await);
}
