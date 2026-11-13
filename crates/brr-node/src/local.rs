use crate::{events::EventHub, instance::InstanceLock};
use brr_remote_protocol::{ApiError, Event, EventCursor, EventReplay, Reply, Request};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

pub type Dispatch =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Reply> + Send>> + Send + Sync>;

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Frame {
    Request { request: Request },
    Stop,
    Stopped,
    Subscribe { cursor: Option<EventCursor> },
    Reply { reply: Reply },
    Event { event: Event },
    Replay { replay: EventReplay },
}

pub struct LocalServer {
    listener: UnixListener,
    path: PathBuf,
    stop: watch::Sender<bool>,
}
impl LocalServer {
    pub fn bind(lock: &InstanceLock) -> Result<Self, ApiError> {
        let path = lock.directory().join("brrd.sock");
        if path.exists() {
            std::fs::remove_file(&path).map_err(transport)?;
        }
        let listener = UnixListener::bind(&path).map_err(transport)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(transport)?;
        let (stop, _) = watch::channel(false);
        Ok(Self {
            listener,
            path,
            stop,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub async fn run(
        self,
        dispatch: Dispatch,
        events: EventHub,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut clients = JoinSet::new();
        let mut local_stop = self.stop.subscribe();
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = local_stop.changed() => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        if stream.peer_cred().map(|peer| peer.uid()).ok() != Some(unsafe { libc::geteuid() }) { continue; }
                        let dispatch = dispatch.clone();
                        let events = events.clone();
                        let stop = self.stop.clone();
                        clients.spawn(async move { let _ = connection(stream, dispatch, events, stop).await; });
                    }
                    Err(_) => break,
                },
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone)]
pub struct LocalClient {
    path: PathBuf,
}
impl LocalClient {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub async fn request(&self, request: Request) -> Result<Reply, ApiError> {
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(|_| ApiError::new("unavailable", "The local Brr service is unavailable"))?;
        write_frame(&mut stream, &Frame::Request { request }).await?;
        let mut stream = BufReader::new(stream);
        match tokio::time::timeout(std::time::Duration::from_secs(180), read_frame(&mut stream))
            .await
        {
            Ok(Ok(Some(Frame::Reply { reply }))) => Ok(reply),
            _ => Err(ApiError::new(
                "uncertain",
                "The service reply was not received. Check the operation status before retrying.",
            )),
        }
    }

    pub async fn stop(&self) -> Result<(), ApiError> {
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(|_| ApiError::new("unavailable", "The local Brr service is unavailable"))?;
        write_frame(&mut stream, &Frame::Stop).await?;
        let mut stream = BufReader::new(stream);
        match tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(&mut stream)).await
        {
            Ok(Ok(Some(Frame::Stopped))) => Ok(()),
            _ => Err(ApiError::new(
                "uncertain",
                "The local service did not acknowledge its stop request",
            )),
        }
    }

    pub async fn subscribe(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<LocalSubscription, ApiError> {
        let mut stream = UnixStream::connect(&self.path).await.map_err(transport)?;
        write_frame(&mut stream, &Frame::Subscribe { cursor }).await?;
        Ok(LocalSubscription {
            stream: BufReader::new(stream),
        })
    }
}

pub struct LocalSubscription {
    stream: BufReader<UnixStream>,
}
impl LocalSubscription {
    pub async fn next(&mut self) -> Result<Option<Frame>, ApiError> {
        read_frame(&mut self.stream).await
    }
}

async fn connection(
    stream: UnixStream,
    dispatch: Dispatch,
    events: EventHub,
    shutdown: watch::Sender<bool>,
) -> Result<(), ApiError> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    while let Some(frame) = read_frame(&mut reader).await? {
        match frame {
            Frame::Stop => {
                write_frame(&mut writer, &Frame::Stopped).await?;
                let _ = shutdown.send(true);
                return Ok(());
            }
            Frame::Request { request } => {
                let reply = dispatch(request).await;
                write_frame(&mut writer, &Frame::Reply { reply }).await?;
            }
            Frame::Subscribe { cursor } => {
                let mut subscription = events.subscribe();
                let mut shutdown_rx = shutdown.subscribe();
                let replay = events.replay(cursor.as_ref());
                let mut sent = EventCursor {
                    epoch: replay.epoch.clone(),
                    sequence: replay.latest,
                };
                write_frame(&mut writer, &Frame::Replay { replay }).await?;
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => return Ok(()),
                        next = subscription.recv() => match next {
                            Ok(event) => {
                                if event.epoch == sent.epoch && event.sequence <= sent.sequence { continue; }
                                sent = EventCursor { epoch: event.epoch.clone(), sequence: event.sequence };
                                write_frame(&mut writer, &Frame::Event { event }).await?;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                let replay = events.replay(Some(&sent));
                                sent = EventCursor { epoch: replay.epoch.clone(), sequence: replay.latest };
                                write_frame(&mut writer, &Frame::Replay { replay }).await?;
                            }
                            Err(_) => return Ok(()),
                        }
                    }
                }
            }
            _ => {
                return Err(ApiError::new(
                    "invalid_frame",
                    "Clients may send only requests or subscriptions",
                ))
            }
        }
    }
    Ok(())
}

async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: &Frame,
) -> Result<(), ApiError> {
    let mut bytes = serde_json::to_vec(frame).map_err(transport)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ApiError::new(
            "frame_too_large",
            "The service frame is too large",
        ));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await.map_err(transport)
}

async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Frame>, ApiError> {
    let mut bytes = Vec::new();
    (&mut *reader)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(transport)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(ApiError::new(
            "invalid_frame",
            "The service frame is incomplete or too large",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| ApiError::new("invalid_frame", "The service frame is invalid"))
}

fn transport(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("transport_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brr_remote_protocol::Channel;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn status(id: &str) -> Request {
        Request {
            id: id.into(),
            channel: Channel::System,
            method: "status".into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        }
    }

    #[tokio::test]
    async fn disconnecting_a_client_does_not_replace_the_service() {
        let data = tempfile::tempdir().unwrap();
        let _lock = InstanceLock::acquire(data.path()).unwrap();
        let server = LocalServer::bind(&_lock).unwrap();
        let path = server.path().to_owned();
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatch: Dispatch = Arc::new(move |request| {
            let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move { Reply::from_result(request.id, Ok(json!({"calls":count}))) })
        });
        let (stop, shutdown) = watch::channel(false);
        let task = tokio::spawn(server.run(
            dispatch,
            EventHub::new("node".into(), "epoch".into(), 10),
            shutdown,
        ));
        let first = LocalClient::new(path.clone())
            .request(status("one"))
            .await
            .unwrap();
        assert_eq!(first.result.unwrap()["calls"], 1);
        let second = LocalClient::new(path).request(status("two")).await.unwrap();
        assert_eq!(second.result.unwrap()["calls"], 2);
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn explicit_local_stop_exits_only_the_target_daemon_and_preserves_its_store() {
        let local_data = tempfile::tempdir().unwrap();
        let local_store = local_data.path().join("store.sqlite");
        std::fs::write(&local_store, b"preserve local store").unwrap();
        let local_lock = InstanceLock::acquire(local_data.path()).unwrap();
        let local_server = LocalServer::bind(&local_lock).unwrap();
        let local_path = local_server.path().to_owned();
        let (_local_shutdown, local_signal) = watch::channel(false);
        let local_task = tokio::spawn(local_server.run(
            Arc::new(|request| {
                Box::pin(async move { Reply::from_result(request.id, Ok(json!({"node":"local"}))) })
            }),
            EventHub::new("local".into(), "local-epoch".into(), 10),
            local_signal,
        ));

        let remote_data = tempfile::tempdir().unwrap();
        let remote_lock = InstanceLock::acquire(remote_data.path()).unwrap();
        let remote_server = LocalServer::bind(&remote_lock).unwrap();
        let remote_path = remote_server.path().to_owned();
        let (remote_shutdown, remote_signal) = watch::channel(false);
        let remote_task = tokio::spawn(remote_server.run(
            Arc::new(|request| {
                Box::pin(
                    async move { Reply::from_result(request.id, Ok(json!({"node":"remote"}))) },
                )
            }),
            EventHub::new("remote".into(), "remote-epoch".into(), 10),
            remote_signal,
        ));

        LocalClient::new(local_path).stop().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), local_task)
            .await
            .expect("local daemon should stop after acknowledging")
            .unwrap();
        assert_eq!(std::fs::read(local_store).unwrap(), b"preserve local store");
        let still_running = LocalClient::new(remote_path)
            .request(status("remote-status"))
            .await
            .unwrap();
        assert_eq!(still_running.result.unwrap()["node"], "remote");

        remote_shutdown.send(true).unwrap();
        remote_task.await.unwrap();
    }
}
