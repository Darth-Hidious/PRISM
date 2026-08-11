//! Synchronous bridge to the async Turso session-metadata mirror.
//!
//! Session JSONL writes are the source of truth and must never wait on, or
//! fail because of, the derived SQL index. One worker thread owns the async
//! store connection: writes are queued best-effort, while reads and explicit
//! rebuilds use a response channel so callers observe all earlier queued
//! updates in order.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::thread;

use anyhow::{Result, anyhow};
use prism_provenance::{ProvenanceStore, SessionIndexEntry, SessionIndexQuery};
use tracing::warn;

enum IndexCommand {
    Upsert(Box<SessionIndexEntry>),
    ReconciledAt {
        source_path: String,
        response: mpsc::SyncSender<Result<Option<f64>>>,
    },
    ReplaceSource {
        source_path: String,
        entries: Vec<SessionIndexEntry>,
        preserve_session_ids: Vec<String>,
        scan_started_at: f64,
        reconciled_at: f64,
        response: mpsc::SyncSender<Result<()>>,
    },
    List {
        query: SessionIndexQuery,
        response: mpsc::SyncSender<Result<Vec<SessionIndexEntry>>>,
    },
}

static INDEX_WORKERS: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionIndexWorkerInner>>>> =
    OnceLock::new();

/// A handle to the process-wide ordered command stream for one database path.
///
/// Sharing the stream across [`crate::session::SessionStore`] instances gives
/// a list started after a JSONL write read-after-write ordering, and prevents
/// a concurrent repair from overtaking an incremental update in this process.
pub(crate) struct SessionIndexWorker {
    inner: Arc<SessionIndexWorkerInner>,
}

struct SessionIndexWorkerInner {
    sender: Mutex<Option<mpsc::Sender<IndexCommand>>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SessionIndexWorker {
    pub(crate) fn start(database_path: PathBuf) -> Self {
        let registry = INDEX_WORKERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut workers = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        workers.retain(|_, worker| worker.strong_count() > 0);
        if let Some(inner) = workers.get(&database_path).and_then(Weak::upgrade) {
            return Self { inner };
        }

        let (sender, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("prism-session-index".to_string())
            .spawn({
                let database_path = database_path.clone();
                move || run_worker(database_path, receiver)
            })
            .ok();

        if thread.is_none() {
            warn!("failed to start session index worker; JSONL persistence remains active");
        }

        let inner = Arc::new(SessionIndexWorkerInner {
            sender: Mutex::new(thread.as_ref().map(|_| sender)),
            thread: Mutex::new(thread),
        });
        workers.insert(database_path, Arc::downgrade(&inner));
        Self { inner }
    }

    /// Queue an index update without affecting the authoritative JSONL write.
    pub(crate) fn upsert_best_effort(&self, entry: SessionIndexEntry) {
        if self.send(IndexCommand::Upsert(Box::new(entry))).is_err() {
            warn!("session metadata index update was dropped; JSONL remains authoritative");
        }
    }

    pub(crate) fn reconciled_at(&self, source_path: String) -> Result<Option<f64>> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.send(IndexCommand::ReconciledAt {
            source_path,
            response,
        })?;
        receive(receiver)
    }

    pub(crate) fn replace_source(
        &self,
        source_path: String,
        entries: Vec<SessionIndexEntry>,
        preserve_session_ids: Vec<String>,
        scan_started_at: f64,
        reconciled_at: f64,
    ) -> Result<()> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.send(IndexCommand::ReplaceSource {
            source_path,
            entries,
            preserve_session_ids,
            scan_started_at,
            reconciled_at,
            response,
        })?;
        receive(receiver)
    }

    pub(crate) fn list(&self, query: SessionIndexQuery) -> Result<Vec<SessionIndexEntry>> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.send(IndexCommand::List { query, response })?;
        receive(receiver)
    }

    fn send(&self, command: IndexCommand) -> Result<()> {
        self.inner
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or_else(|| anyhow!("session index worker is unavailable"))?
            .send(command)
            .map_err(|_| anyhow!("session index worker stopped unexpectedly"))
    }
}

impl Drop for SessionIndexWorkerInner {
    fn drop(&mut self) {
        self.sender
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && thread.join().is_err()
        {
            warn!("session index worker panicked while shutting down");
        }
    }
}

fn receive<T>(receiver: mpsc::Receiver<Result<T>>) -> Result<T> {
    receiver
        .recv()
        .map_err(|_| anyhow!("session index worker stopped before replying"))?
}

fn run_worker(database_path: PathBuf, receiver: mpsc::Receiver<IndexCommand>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            warn!(%error, "failed to create session index runtime");
            reject_commands(receiver, error.to_string());
            return;
        }
    };

    let store = match runtime.block_on(ProvenanceStore::open(&database_path)) {
        Ok(store) => store,
        Err(error) => {
            warn!(
                %error,
                path = %database_path.display(),
                "failed to open session metadata index; JSONL persistence remains active"
            );
            reject_commands(receiver, error.to_string());
            return;
        }
    };

    while let Ok(command) = receiver.recv() {
        match command {
            IndexCommand::Upsert(entry) => {
                if let Err(error) = runtime.block_on(store.upsert_session_metadata(&entry)) {
                    warn!(
                        %error,
                        session_id = %entry.session_id,
                        "session metadata index update failed; JSONL remains authoritative"
                    );
                }
            }
            IndexCommand::ReconciledAt {
                source_path,
                response,
            } => {
                let result = runtime.block_on(store.session_metadata_reconciled_at(&source_path));
                let _ = response.send(result);
            }
            IndexCommand::ReplaceSource {
                source_path,
                entries,
                preserve_session_ids,
                scan_started_at,
                reconciled_at,
                response,
            } => {
                let result = runtime.block_on(store.replace_session_metadata_source(
                    &source_path,
                    &entries,
                    &preserve_session_ids,
                    scan_started_at,
                    reconciled_at,
                ));
                let _ = response.send(result);
            }
            IndexCommand::List { query, response } => {
                let result = runtime.block_on(store.list_session_metadata(&query));
                let _ = response.send(result);
            }
        }
    }
}

fn reject_commands(receiver: mpsc::Receiver<IndexCommand>, reason: String) {
    while let Ok(command) = receiver.recv() {
        let error = || anyhow!("session index unavailable: {reason}");
        match command {
            IndexCommand::Upsert(_) => {}
            IndexCommand::ReconciledAt { response, .. } => {
                let _ = response.send(Err(error()));
            }
            IndexCommand::ReplaceSource { response, .. } => {
                let _ = response.send(Err(error()));
            }
            IndexCommand::List { response, .. } => {
                let _ = response.send(Err(error()));
            }
        }
    }
}
