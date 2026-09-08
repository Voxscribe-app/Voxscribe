use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::core::paths;
use crate::core::state::{Event, StateHandle};
use crate::ipc::{encode, Request, Response};

pub struct Command {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}

#[derive(Debug)]
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    pub fn bind(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            paths::ensure_private_dir(parent)
                .with_context(|| format!("preparing {}", parent.display()))?;
        }

        if path.exists() {
            if is_live(path) {
                anyhow::bail!(
                    "another Duskr daemon is already listening on {}",
                    path.display()
                );
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing the stale socket {}", path.display()))?;
        }

        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;

        let mut perms = std::fs::metadata(path)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o600);
        std::fs::set_permissions(path, perms)?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn run(
        self,
        commands: mpsc::Sender<Command>,
        state: StateHandle,
        mut shutdown: broadcast::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.recv() => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let commands = commands.clone();
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(err) = serve(stream, commands, state).await {
                                tracing::debug!("IPC connection ended: {err}");
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!("IPC accept failed: {err}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                },
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

async fn serve(
    stream: UnixStream,
    commands: mpsc::Sender<Command>,
    state: StateHandle,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => {
                let response = Response::error(format!("malformed request: {err}"));
                write_half.write_all(encode(&response)?.as_bytes()).await?;
                continue;
            }
        };

        if matches!(request, Request::Subscribe) {
            let snapshot = Response::Event(Event::State(state.get()));
            write_half.write_all(encode(&snapshot)?.as_bytes()).await?;
            return stream_events(write_half, state).await;
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        if commands
            .send(Command {
                request,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            let response = Response::error("daemon is shutting down");
            let _ = write_half.write_all(encode(&response)?.as_bytes()).await;
            return Ok(());
        }

        let response = reply_rx
            .await
            .unwrap_or_else(|_| Response::error("daemon dropped the request"));
        write_half.write_all(encode(&response)?.as_bytes()).await?;
    }

    Ok(())
}

async fn stream_events(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    state: StateHandle,
) -> Result<()> {
    let mut events = state.subscribe_events();
    let _watcher = state.watcher();
    loop {
        match events.recv().await {
            Ok(event) => {
                let shutting_down = matches!(event, Event::Shutdown);
                if write_half
                    .write_all(encode(&Response::Event(event))?.as_bytes())
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                if shutting_down {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!("IPC subscriber lagged {skipped} events");
                let snapshot = Response::Event(Event::State(state.get()));
                if write_half
                    .write_all(encode(&snapshot)?.as_bytes())
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::Snapshot;
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;

    async fn spawn_server(
        dir: &tempfile::TempDir,
    ) -> (
        PathBuf,
        mpsc::Receiver<Command>,
        StateHandle,
        broadcast::Sender<()>,
    ) {
        let path = dir.path().join("duskr.sock");
        let server = Server::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let state = StateHandle::new(Snapshot::default());
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        tokio::spawn(server.run(tx, state.clone(), shutdown_rx));
        (path, rx, state, shutdown_tx)
    }

    #[tokio::test]
    async fn a_request_reaches_the_daemon_and_the_answer_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut commands, _state, _shutdown) = spawn_server(&dir).await;

        tokio::spawn(async move {
            let command = commands.recv().await.unwrap();
            assert_eq!(command.request, Request::Ping);
            let _ = command.reply.send(Response::Ok);
        });

        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(b"{\"cmd\":\"ping\"}\n").await.unwrap();
        let (read, _write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Ok
        );
    }

    #[tokio::test]
    async fn malformed_input_is_answered_without_closing_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut commands, _state, _shutdown) = spawn_server(&dir).await;
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                let _ = command.reply.send(Response::Ok);
            }
        });

        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(b"not json\n{\"cmd\":\"ping\"}\n")
            .await
            .unwrap();
        let (read, _write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let first: Response =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(first.is_error());
        let second: Response =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(second, Response::Ok);
    }

    #[tokio::test]
    async fn subscribers_receive_the_current_state_before_any_updates() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _commands, state, _shutdown) = spawn_server(&dir).await;

        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(b"{\"cmd\":\"subscribe\"}\n")
            .await
            .unwrap();
        let (read, _write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let first: Response =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(matches!(first, Response::Event(Event::State(_))));

        state.update(|s| s.phase = crate::core::state::Phase::Recording);
        let next = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match serde_json::from_str::<Response>(&next).unwrap() {
            Response::Event(Event::State(snapshot)) => {
                assert_eq!(snapshot.phase, crate::core::state::Phase::Recording);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn binding_over_a_live_socket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _commands, _state, _shutdown) = spawn_server(&dir).await;
        let err = Server::bind(&path).unwrap_err().to_string();
        assert!(err.contains("already listening"), "{err}");
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duskr.sock");
        std::fs::write(&path, b"").unwrap();
        assert!(Server::bind(&path).is_ok());
    }
}
