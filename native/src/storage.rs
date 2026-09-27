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

    pub fn record_error(&self, record: Value) {
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
#[path = "../tests/unit/storage.rs"]
mod tests;
