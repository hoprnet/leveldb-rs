use std::collections::hash_map::HashMap;
use std::path::Path;

use crate::{Options, Result, Status, StatusCode, WriteBatch, DB};

#[cfg(feature = "async_std")]
use async_std::channel::{bounded, Receiver, Sender};
#[cfg(feature = "async_std")]
use async_std::task::{JoinHandle, spawn_blocking};

#[cfg(feature = "async_std")]
type OneshotSender<T> = async_oneshot::Sender<T>;
#[cfg(feature = "async_std")]
type OneshotReceiver<T> = async_oneshot::Receiver<T>;

#[cfg(feature = "async_std")]
fn bounded_channel<T>(size: usize) -> (Sender<T>, Receiver<T>) {
    bounded(size)
}

#[cfg(feature = "async_std")]
fn oneshot<T>() -> (OneshotSender<T>, OneshotReceiver<T>) {
    async_oneshot::oneshot()
}

#[cfg(feature = "async_tokio")]
use tokio::sync::{oneshot, mpsc, mpsc::Receiver, mpsc::Sender};
#[cfg(feature = "async_tokio")]
use tokio::task::{spawn_blocking, JoinHandle};

#[cfg(feature = "async_tokio")]
type OneshotSender<T> = oneshot::Sender<T>;
#[cfg(feature = "async_tokio")]
type OneshotReceiver<T> = oneshot::Receiver<T>;

#[cfg(feature = "async_tokio")]
fn bounded_channel<T>(size: usize) -> (Sender<T>, Receiver<T>) {
    mpsc::channel(size)
}

#[cfg(feature = "async_tokio")]
fn oneshot<T>() -> (OneshotSender<T>, OneshotReceiver<T>) {
    oneshot::channel()
}

const CHANNEL_BUFFER_SIZE: usize = 32;


#[derive(Clone, Copy)]
pub struct SnapshotRef(usize);

/// A request sent to the database thread.
enum Request {
    Close,
    Put { key: Vec<u8>, val: Vec<u8> },
    Delete { key: Vec<u8> },
    Write { batch: WriteBatch, sync: bool },
    Flush,
    GetAt { snapshot: SnapshotRef, key: Vec<u8> },
    Get { key: Vec<u8> },
    GetSnapshot,
    DropSnapshot { snapshot: SnapshotRef },
    CompactRange { from: Vec<u8>, to: Vec<u8> },
}

/// A response received from the database thread.
enum Response {
    OK,
    Error(Status),
    Value(Option<Vec<u8>>),
    Snapshot(SnapshotRef),
}

/// Contains both a request and a back-channel for the reply.
struct Message {
    req: Request,
    resp_channel: OneshotSender<Response>,
}

/// `AsyncDB` makes it easy to use LevelDB in a tokio runtime.
/// The methods follow very closely the main API (see `DB` type). Iteration is not yet implemented.
///
/// TODO: Make it work in other runtimes as well. This is a matter of adapting the blocking thread
/// mechanism as well as the channel types.
pub struct AsyncDB {
    jh: JoinHandle<()>,
    send: Sender<Message>,
}

impl AsyncDB {
    /// Create a new or open an existing database.
    pub fn new<P: AsRef<Path>>(name: P, opts: Options) -> Result<AsyncDB> {
        let db = DB::open(name, opts)?;
        let (send, recv) = bounded_channel(CHANNEL_BUFFER_SIZE);
        let jh = spawn_blocking(move || AsyncDB::run_server(db, recv));
        Ok(AsyncDB { jh, send })
    }

    pub async fn close(&self) -> Result<()> {
        let r = self.process_request(Request::Close).await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }

    pub async fn put(&self, key: Vec<u8>, val: Vec<u8>) -> Result<()> {
        let r = self.process_request(Request::Put { key, val }).await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn delete(&self, key: Vec<u8>) -> Result<()> {
        let r = self.process_request(Request::Delete { key }).await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn write(&self, batch: WriteBatch, sync: bool) -> Result<()> {
        let r = self.process_request(Request::Write { batch, sync }).await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn flush(&self) -> Result<()> {
        let r = self.process_request(Request::Flush).await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        let r = self.process_request(Request::Get { key }).await?;
        match r {
            Response::Value(v) => Ok(v),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn get_at(&self, snapshot: SnapshotRef, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        let r = self
            .process_request(Request::GetAt { snapshot, key })
            .await?;
        match r {
            Response::Value(v) => Ok(v),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn get_snapshot(&self) -> Result<SnapshotRef> {
        let r = self.process_request(Request::GetSnapshot).await?;
        match r {
            Response::Snapshot(sr) => Ok(sr),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    /// As snapshots returned by `AsyncDB::get_snapshot()` are sort-of "weak references" to an
    /// actual snapshot, they need to be dropped explicitly.
    pub async fn drop_snapshot(&self, snapshot: SnapshotRef) -> Result<()> {
        let r = self
            .process_request(Request::DropSnapshot { snapshot })
            .await?;
        match r {
            Response::OK => Ok(()),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }
    pub async fn compact_range(&self, from: Vec<u8>, to: Vec<u8>) -> Result<()> {
        let r = self
            .process_request(Request::CompactRange { from, to })
            .await?;
        match r {
            Response::OK => Ok(()),
            Response::Error(s) => Err(s),
            _ => Err(Status {
                code: StatusCode::AsyncError,
                err: "Wrong response type in AsyncDB.".to_string(),
            }),
        }
    }

    async fn process_request(&self, req: Request) -> Result<Response> {
        let (tx, rx) = oneshot();
        let m = Message {
            req,
            resp_channel: tx,
        };
        if let Err(e) = self.send.send(m).await {
            return Err(Status {
                code: StatusCode::AsyncError,
                err: e.to_string(),
            });
        }
        let resp = rx.await;
        match resp {
            Err(_) => Err(Status {
                code: StatusCode::AsyncError,
                err: "channel closed".into(),
            }),
            Ok(r) => Ok(r),
        }
    }

    #[cfg(feature = "async_tokio")]
    fn blocking_recv(recv: &mut Receiver<Message>) -> Option<Message> {
        recv.blocking_recv()
    }

    #[cfg(feature = "async_std")]
    fn blocking_recv(recv: &mut Receiver<Message>) -> Option<Message> {
        recv.recv_blocking().ok()
    }

    fn run_server(mut db: DB, mut recv: Receiver<Message>) {
        let mut snapshots = HashMap::new();
        let mut snapshot_counter: usize = 0;

        while let Some(mut message) = Self::blocking_recv(&mut recv) {
            match message.req {
                Request::Close => {
                    message.resp_channel.send(Response::OK).ok();
                    recv.close();
                    return;
                }
                Request::Put { key, val } => {
                    let ok = db.put(&key, &val);
                    send_response(message.resp_channel, ok);
                }
                Request::Delete { key } => {
                    let ok = db.delete(&key);
                    send_response(message.resp_channel, ok);
                }
                Request::Write { batch, sync } => {
                    let ok = db.write(batch, sync);
                    send_response(message.resp_channel, ok);
                }
                Request::Flush => {
                    let ok = db.flush();
                    send_response(message.resp_channel, ok);
                }
                Request::GetAt { snapshot, key } => {
                    let snapshot_id = snapshot.0;
                    if let Some(snapshot) = snapshots.get(&snapshot_id) {
                        let ok = db.get_at(&snapshot, &key);
                        match ok {
                            Err(e) => {
                                message.resp_channel.send(Response::Error(e)).ok();
                            }
                            Ok(v) => {
                                message.resp_channel.send(Response::Value(v)).ok();
                            }
                        };
                    } else {
                        message
                            .resp_channel
                            .send(Response::Error(Status {
                                code: StatusCode::AsyncError,
                                err: "Unknown snapshot reference: this is a bug".to_string(),
                            }))
                            .ok();
                    }
                }
                Request::Get { key } => {
                    let r = db.get(&key);
                    message.resp_channel.send(Response::Value(r)).ok();
                }
                Request::GetSnapshot => {
                    snapshots.insert(snapshot_counter, db.get_snapshot());
                    let sref = SnapshotRef(snapshot_counter);
                    snapshot_counter += 1;
                    message.resp_channel.send(Response::Snapshot(sref)).ok();
                }
                Request::DropSnapshot { snapshot } => {
                    snapshots.remove(&snapshot.0);
                    send_response(message.resp_channel, Ok(()));
                }
                Request::CompactRange { from, to } => {
                    let ok = db.compact_range(&from, &to);
                    send_response(message.resp_channel, ok);
                }
            }
        }
    }
}

fn send_response(mut ch: OneshotSender<Response>, result: Result<()>) {
    if let Err(e) = result {
        ch.send(Response::Error(e)).ok();
    } else {
        ch.send(Response::OK).ok();
    }
}
