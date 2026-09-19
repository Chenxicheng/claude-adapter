use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use serde_json::Value;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncWriteExt, BufWriter},
    sync::mpsc,
    task::JoinHandle,
    time,
};

const QUEUE_CAPACITY: usize = 1024;
const FLUSH_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum RecordKind {
    Usage,
    Error,
}

struct WriteRecord {
    kind: RecordKind,
    line: Vec<u8>,
}

#[derive(Clone)]
pub struct Storage {
    sender: mpsc::Sender<WriteRecord>,
    dropped: Arc<AtomicU64>,
}

impl Storage {
    pub fn start(base_dir: PathBuf) -> (Self, JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let handle = tokio::spawn(writer_loop(base_dir, receiver));
        (Self { sender, dropped }, handle)
    }

    pub fn record_usage(&self, record: Value) {
        self.enqueue(RecordKind::Usage, record);
    }

    pub fn record_error(&self, status: u16, record: Value) {
        if matches!(status, 401 | 402 | 404 | 429) {
            return;
        }
        self.enqueue(RecordKind::Error, record);
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn enqueue(&self, kind: RecordKind, record: Value) {
        let mut line = match serde_json::to_vec(&record) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("[adapter] Failed to serialize JSONL record: {error}");
                return;
            }
        };
        line.push(b'\n');

        if self.sender.try_send(WriteRecord { kind, line }).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped.is_multiple_of(100) {
                eprintln!("[adapter] JSONL writer unavailable; dropped {dropped} records");
            }
        }
    }
}

struct DailyWriter {
    date: String,
    usage: BufWriter<File>,
    error: BufWriter<File>,
    buffered: usize,
}

impl DailyWriter {
    async fn open(base_dir: &Path, date: String) -> std::io::Result<Self> {
        let usage_dir = base_dir.join("token_usage");
        let error_dir = base_dir.join("error_logs");
        tokio::fs::create_dir_all(&usage_dir).await?;
        tokio::fs::create_dir_all(&error_dir).await?;

        Ok(Self {
            usage: BufWriter::new(open_append(usage_dir.join(format!("{date}.jsonl"))).await?),
            error: BufWriter::new(open_append(error_dir.join(format!("{date}.jsonl"))).await?),
            date,
            buffered: 0,
        })
    }

    async fn write(&mut self, record: WriteRecord) -> std::io::Result<()> {
        let length = record.line.len();
        match record.kind {
            RecordKind::Usage => self.usage.write_all(&record.line).await?,
            RecordKind::Error => self.error.write_all(&record.line).await?,
        }
        self.buffered += length;
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.usage.flush().await?;
        self.error.flush().await?;
        self.buffered = 0;
        Ok(())
    }
}

async fn open_append(path: PathBuf) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

async fn writer_loop(base_dir: PathBuf, receiver: mpsc::Receiver<WriteRecord>) {
    writer_loop_with_date(base_dir, receiver, today).await;
}

async fn writer_loop_with_date<F>(
    base_dir: PathBuf,
    mut receiver: mpsc::Receiver<WriteRecord>,
    mut current_date: F,
) where
    F: FnMut() -> String,
{
    let mut writer: Option<DailyWriter> = None;
    let mut interval = time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            record = receiver.recv() => {
                let Some(record) = record else { break };
                let date = current_date();
                if writer.as_ref().is_none_or(|current| current.date != date) {
                    if let Some(current) = writer.as_mut() {
                        log_io_error("flush JSONL files during rotation", current.flush().await);
                    }
                    writer = match DailyWriter::open(&base_dir, date).await {
                        Ok(next) => Some(next),
                        Err(error) => {
                            eprintln!("[adapter] Failed to open JSONL files: {error}");
                            None
                        }
                    };
                }
                if let Some(current) = writer.as_mut() {
                    if let Err(error) = current.write(record).await {
                        eprintln!("[adapter] Failed to write JSONL record: {error}");
                    } else if current.buffered >= FLUSH_BYTES {
                        log_io_error("flush JSONL files", current.flush().await);
                    }
                }
            }
            _ = interval.tick() => {
                if let Some(current) = writer.as_mut()
                    && current.buffered > 0
                {
                    log_io_error("flush JSONL files", current.flush().await);
                }
            }
        }
    }

    if let Some(current) = writer.as_mut() {
        log_io_error("flush JSONL files at shutdown", current.flush().await);
    }
}

fn log_io_error(operation: &str, result: std::io::Result<()>) {
    if let Err(error) = result {
        eprintln!("[adapter] Failed to {operation}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicU64};

    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn flushes_queued_records_on_shutdown() {
        let directory = tempdir().unwrap();
        let (storage, handle) = Storage::start(directory.path().to_path_buf());
        storage.record_usage(json!({"value": 1}));
        storage.record_error(500, json!({"message": "failed"}));
        drop(storage);
        handle.await.unwrap();

        let date = today();
        let usage = tokio::fs::read_to_string(
            directory
                .path()
                .join("token_usage")
                .join(format!("{date}.jsonl")),
        )
        .await
        .unwrap();
        let errors = tokio::fs::read_to_string(
            directory
                .path()
                .join("error_logs")
                .join(format!("{date}.jsonl")),
        )
        .await
        .unwrap();
        assert_eq!(usage, "{\"value\":1}\n");
        assert_eq!(errors, "{\"message\":\"failed\"}\n");
    }

    #[tokio::test]
    async fn writes_concurrent_records_as_complete_json_lines() {
        let directory = tempdir().unwrap();
        let (storage, handle) = Storage::start(directory.path().to_path_buf());
        let tasks = (0..100).map(|value| {
            let storage = storage.clone();
            tokio::spawn(async move { storage.record_usage(json!({"value": value})) })
        });
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(storage.dropped_count(), 0);
        drop(storage);
        handle.await.unwrap();

        let contents = tokio::fs::read_to_string(
            directory
                .path()
                .join("token_usage")
                .join(format!("{}.jsonl", today())),
        )
        .await
        .unwrap();
        let mut values = contents
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["value"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn counts_records_when_queue_is_closed() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        let storage = Storage {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        storage.record_usage(json!({"value": 1}));
        assert_eq!(storage.dropped_count(), 1);
    }

    #[test]
    fn drops_without_blocking_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        let storage = Storage {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        storage.record_usage(json!({"value": 1}));
        storage.record_usage(json!({"value": 2}));
        assert_eq!(storage.dropped_count(), 1);
    }

    #[tokio::test]
    async fn rotates_daily_files_in_writer_loop() {
        let directory = tempdir().unwrap();
        let (sender, receiver) = mpsc::channel(2);
        let dates =
            std::collections::VecDeque::from(["2026-09-18".to_owned(), "2026-09-19".to_owned()]);
        let handle = tokio::spawn(writer_loop_with_date(
            directory.path().to_path_buf(),
            receiver,
            {
                let mut dates = dates;
                move || dates.pop_front().unwrap()
            },
        ));
        sender
            .send(WriteRecord {
                kind: RecordKind::Usage,
                line: b"{\"day\":1}\n".to_vec(),
            })
            .await
            .unwrap();
        sender
            .send(WriteRecord {
                kind: RecordKind::Usage,
                line: b"{\"day\":2}\n".to_vec(),
            })
            .await
            .unwrap();
        drop(sender);
        handle.await.unwrap();

        assert_eq!(
            tokio::fs::read_to_string(directory.path().join("token_usage/2026-09-18.jsonl"))
                .await
                .unwrap(),
            "{\"day\":1}\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(directory.path().join("token_usage/2026-09-19.jsonl"))
                .await
                .unwrap(),
            "{\"day\":2}\n"
        );
    }

    #[tokio::test]
    async fn survives_jsonl_open_failure() {
        let directory = tempdir().unwrap();
        let blocked_path = directory.path().join("not-a-directory");
        tokio::fs::write(&blocked_path, b"file").await.unwrap();
        let (storage, handle) = Storage::start(blocked_path);
        storage.record_usage(json!({"value": 1}));
        drop(storage);
        handle.await.unwrap();
    }
}
