use std::path::Path;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("bad reply: {0}")]
    Json(#[from] serde_json::Error),
    #[error("socket closed without a reply")]
    Closed,
}

/// Sends one JSON-lines request and returns the one-line reply. Errors if nobody listens.
pub async fn socket_request(path: &Path, request: &Value) -> Result<Value, ClientError> {
    tokio::time::timeout(REQUEST_TIMEOUT, exchange(path, request))
        .await
        .map_err(|_| ClientError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)))?
}

/// A socket that accepts but never answers must not hang the liveness probe.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn exchange(path: &Path, request: &Value) -> Result<Value, ClientError> {
    let stream = UnixStream::connect(path).await?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;
    let mut reader = BufReader::new(rd);
    let mut reply = String::new();
    if reader.read_line(&mut reply).await? == 0 || !reply.ends_with('\n') {
        return Err(ClientError::Closed);
    }
    Ok(serde_json::from_str(reply.trim_end())?)
}
