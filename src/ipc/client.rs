use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::core::paths;
use crate::core::state::{Event, Snapshot};
use crate::ipc::{encode, Request, Response};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    timeout: Duration,
}

impl Client {
    pub async fn connect() -> Result<Self> {
        Self::connect_to(&paths::socket_path()).await
    }

    pub async fn connect_to(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).await.map_err(|err| {
            anyhow!(
                "cannot reach the Duskr daemon at {} ({err}) - start it with \
                 `duskr daemon` or `systemctl --user start duskr`",
                path.display()
            )
        })?;
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer: write,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn send(&mut self, request: Request) -> Result<Response> {
        self.writer
            .write_all(encode(&request)?.as_bytes())
            .await
            .context("sending the request")?;

        let mut line = String::new();
        let read = tokio::time::timeout(self.timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| anyhow!("the daemon did not answer within {:?}", self.timeout))?
            .context("reading the response")?;
        if read == 0 {
            bail!("the daemon closed the connection");
        }
        serde_json::from_str(line.trim()).context("parsing the response")
    }

    pub async fn call(&mut self, request: Request) -> Result<Response> {
        match self.send(request).await? {
            Response::Error { message } => bail!(message),
            response => Ok(response),
        }
    }

    pub async fn status(&mut self) -> Result<Snapshot> {
        match self.call(Request::Status).await? {
            Response::Status(snapshot) => Ok(*snapshot),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub async fn subscribe<F>(&mut self, mut on_event: F) -> Result<()>
    where
        F: FnMut(Event) -> Result<bool>,
    {
        self.writer
            .write_all(encode(&Request::Subscribe)?.as_bytes())
            .await
            .context("subscribing")?;

        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Response>(trimmed) {
                Ok(Response::Event(event)) => {
                    if !on_event(event)? {
                        return Ok(());
                    }
                }
                Ok(Response::Error { message }) => bail!(message),
                Ok(_) => {}
                Err(err) => tracing::debug!("ignoring unparsable event: {err}"),
            }
        }
    }
}

pub async fn is_running() -> bool {
    daemon_socket_alive(&paths::socket_path()).await
}

pub async fn daemon_socket_alive(path: &PathBuf) -> bool {
    UnixStream::connect(path).await.is_ok()
}

pub async fn request(request: Request) -> Result<Response> {
    Client::connect().await?.call(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connecting_to_a_missing_socket_explains_how_to_start_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let err = Client::connect_to(&dir.path().join("nope.sock"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("duskr daemon"), "{err}");
    }

    #[tokio::test]
    async fn an_error_response_becomes_an_error_for_call_but_not_for_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while lines.next_line().await.unwrap().is_some() {
                let response = Response::error("nope");
                write
                    .write_all(encode(&response).unwrap().as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut client = Client::connect_to(&path).await.unwrap();
        assert!(client.send(Request::Ping).await.unwrap().is_error());
        assert!(client.call(Request::Ping).await.is_err());
    }

    #[tokio::test]
    async fn a_silent_daemon_is_reported_as_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let _held = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let mut client = Client::connect_to(&path)
            .await
            .unwrap()
            .with_timeout(Duration::from_millis(150));
        let err = client.send(Request::Ping).await.unwrap_err().to_string();
        assert!(err.contains("did not answer"), "{err}");
    }
}
