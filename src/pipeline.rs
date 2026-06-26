use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use crossbeam_channel::{Sender, unbounded};
use crate::parser::FhirParser;

pub struct PipelineMetrics {
    pub total_bytes_processed: AtomicU64,
    pub total_records_processed: AtomicU64,
    pub total_latency_us: AtomicU64,
    pub total_errors: AtomicU64,
    pub corrupt_bytes: AtomicU64,
}

impl PipelineMetrics {
    pub fn new() -> Self {
        Self {
            total_bytes_processed: AtomicU64::new(0),
            total_records_processed: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            corrupt_bytes: AtomicU64::new(0),
        }
    }

    #[allow(dead_code)]
    pub fn reset(&self) {
        self.total_bytes_processed.store(0, Ordering::Relaxed);
        self.total_records_processed.store(0, Ordering::Relaxed);
        self.total_latency_us.store(0, Ordering::Relaxed);
        self.total_errors.store(0, Ordering::Relaxed);
        self.corrupt_bytes.store(0, Ordering::Relaxed);
    }
}

pub struct IngestionPipeline {
    sender: Sender<String>,
    _metrics: Arc<PipelineMetrics>,
}

impl IngestionPipeline {
    pub fn new(metrics: Arc<PipelineMetrics>) -> Self {
        let (sender, receiver) = unbounded::<String>();
        let num_workers = num_cpus::get();

        for _ in 0..num_workers {
            let rx = receiver.clone();
            let m = Arc::clone(&metrics);
            thread::spawn(move || {
                while let Ok(record) = rx.recv() {
                    let start = Instant::now();
                    let record_len = record.len() as u64;

                    let mut parser = FhirParser::new(&record);
                    let _parsed = parser.parse_patient();
                    
                    let duration = start.elapsed().as_micros() as u64;
                    let num_errors = parser.get_errors().len() as u64;
                    let corrupt = parser.get_corrupt_bytes() as u64;

                    m.total_bytes_processed.fetch_add(record_len, Ordering::Relaxed);
                    m.total_records_processed.fetch_add(1, Ordering::Relaxed);
                    m.total_latency_us.fetch_add(duration, Ordering::Relaxed);
                    m.total_errors.fetch_add(num_errors, Ordering::Relaxed);
                    m.corrupt_bytes.fetch_add(corrupt, Ordering::Relaxed);
                }
            });
        }

        Self { sender, _metrics: metrics }
    }

    pub fn submit(&self, record: String) {
        let _ = self.sender.send(record);
    }
}
