use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use std::cell::UnsafeCell;
use crossbeam_utils::CachePadded;
use crate::parser::FhirParser;

pub struct PipelineMetrics {
    pub total_bytes_processed: CachePadded<AtomicU64>,
    pub total_records_processed: CachePadded<AtomicU64>,
    pub total_latency_us: CachePadded<AtomicU64>,
    pub total_errors: CachePadded<AtomicU64>,
    pub corrupt_bytes: CachePadded<AtomicU64>,
}

impl PipelineMetrics {
    pub fn new() -> Self {
        Self {
            total_bytes_processed: CachePadded::new(AtomicU64::new(0)),
            total_records_processed: CachePadded::new(AtomicU64::new(0)),
            total_latency_us: CachePadded::new(AtomicU64::new(0)),
            total_errors: CachePadded::new(AtomicU64::new(0)),
            corrupt_bytes: CachePadded::new(AtomicU64::new(0)),
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

pub struct Slot<T> {
    pub sequence: AtomicU64,
    pub value: UnsafeCell<Option<T>>,
}

// SAFETY: UnsafeCell access is guarded via sequence CAS logic ensuring exclusive ownership.
unsafe impl<T: Send> Sync for Slot<T> {}

pub struct RingBuffer<T, const N: usize> {
    buffer: Vec<Slot<T>>,
    write_idx: AtomicU64,
    read_idx: AtomicU64,
}

// SAFETY: Send and Sync implementations are safe due to CAS sequencing invariants.
unsafe impl<T: Send, const N: usize> Send for RingBuffer<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for RingBuffer<T, N> {}

impl<T, const N: usize> RingBuffer<T, N> {
    pub fn new() -> Self {
        assert!(N.is_power_of_two(), "Buffer size N must be a power of two");
        let mut buffer = Vec::with_capacity(N);
        for i in 0..N {
            buffer.push(Slot {
                sequence: AtomicU64::new(i as u64),
                value: UnsafeCell::new(None),
            });
        }
        Self {
            buffer,
            write_idx: AtomicU64::new(0),
            read_idx: AtomicU64::new(0),
        }
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        let mut head = self.write_idx.load(Ordering::Relaxed);
        loop {
            let slot = &self.buffer[(head & (N as u64 - 1)) as usize];
            let seq = slot.sequence.load(Ordering::Acquire);
            let diff = seq as i64 - head as i64;
            
            if diff == 0 {
                match self.write_idx.compare_exchange_weak(head, head + 1, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => {
                        // SAFETY: Exclusively acquired slot index via CAS write_idx update.
                        unsafe { *slot.value.get() = Some(value); }
                        slot.sequence.store(head + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(h) => head = h,
                }
            } else if diff < 0 {
                return Err(value);
            } else {
                head = self.write_idx.load(Ordering::Relaxed);
            }
        }
    }

    pub fn push_blocking(&self, mut value: T) {
        loop {
            match self.push(value) {
                Ok(_) => return,
                Err(val) => {
                    value = val;
                    std::thread::yield_now();
                }
            }
        }
    }

    pub fn pop(&self) -> Option<T> {
        let mut tail = self.read_idx.load(Ordering::Relaxed);
        loop {
            let slot = &self.buffer[(tail & (N as u64 - 1)) as usize];
            let seq = slot.sequence.load(Ordering::Acquire);
            let diff = seq as i64 - (tail + 1) as i64;
            
            if diff == 0 {
                match self.read_idx.compare_exchange_weak(tail, tail + 1, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => {
                        // SAFETY: Exclusively acquired slot index via CAS read_idx update.
                        let val = unsafe { (*slot.value.get()).take() };
                        slot.sequence.store(tail + N as u64, Ordering::Release);
                        return val;
                    }
                    Err(t) => tail = t,
                }
            } else if diff < 0 {
                return None;
            } else {
                tail = self.read_idx.load(Ordering::Relaxed);
            }
        }
    }

    pub fn pop_blocking(&self) -> T {
        loop {
            if let Some(val) = self.pop() {
                return val;
            }
            std::thread::yield_now();
        }
    }
}

pub struct IngestionPipeline {
    queues: Vec<Arc<RingBuffer<String, 2048>>>,
    write_selector: AtomicU64,
    _metrics: Arc<PipelineMetrics>,
}

impl IngestionPipeline {
    pub fn new(metrics: Arc<PipelineMetrics>) -> Self {
        let num_workers = num_cpus::get();
        let num_queues = (num_workers / 8).clamp(4, 8);
        
        let mut queues = Vec::with_capacity(num_queues);
        for _ in 0..num_queues {
            queues.push(Arc::new(RingBuffer::<String, 2048>::new()));
        }

        let core_ids = core_affinity::get_core_ids().unwrap_or_default();

        for i in 0..num_workers {
            let q_clone = Arc::clone(&queues[i % num_queues]);
            let m = Arc::clone(&metrics);
            let core_id = core_ids.get(i % core_ids.len()).copied();
            
            thread::spawn(move || {
                if let Some(id) = core_id {
                    core_affinity::set_for_current(id);
                }
                
                let mut bump = bumpalo::Bump::with_capacity(512 * 1024);
                let mut local_bytes = 0u64;
                let mut local_records = 0u64;
                let mut local_latency = 0u64;
                let mut local_errors = 0u64;
                let mut local_corrupt = 0u64;

                loop {
                    if let Some(record) = q_clone.pop() {
                        let start = Instant::now();
                        let record_len = record.len() as u64;

                        let (duration, num_errors, corrupt) = {
                            let mut parser = FhirParser::new(&record, &bump);
                            let _parsed = parser.parse_patient();
                            
                            let duration = start.elapsed().as_micros() as u64;
                            let num_errors = parser.get_errors().len() as u64;
                            let corrupt = parser.get_corrupt_bytes() as u64;
                            (duration, num_errors, corrupt)
                        };

                        local_bytes += record_len;
                        local_records += 1;
                        local_latency += duration;
                        local_errors += num_errors;
                        local_corrupt += corrupt;

                        if local_records >= 256 {
                            m.total_bytes_processed.fetch_add(local_bytes, Ordering::Relaxed);
                            m.total_records_processed.fetch_add(local_records, Ordering::Relaxed);
                            m.total_latency_us.fetch_add(local_latency, Ordering::Relaxed);
                            m.total_errors.fetch_add(local_errors, Ordering::Relaxed);
                            m.corrupt_bytes.fetch_add(local_corrupt, Ordering::Relaxed);
                            
                            local_bytes = 0;
                            local_records = 0;
                            local_latency = 0;
                            local_errors = 0;
                            local_corrupt = 0;
                        }

                        bump.reset();
                    } else {
                        if local_records > 0 {
                            m.total_bytes_processed.fetch_add(local_bytes, Ordering::Relaxed);
                            m.total_records_processed.fetch_add(local_records, Ordering::Relaxed);
                            m.total_latency_us.fetch_add(local_latency, Ordering::Relaxed);
                            m.total_errors.fetch_add(local_errors, Ordering::Relaxed);
                            m.corrupt_bytes.fetch_add(local_corrupt, Ordering::Relaxed);
                            
                            local_bytes = 0;
                            local_records = 0;
                            local_latency = 0;
                            local_errors = 0;
                            local_corrupt = 0;
                        }
                        std::thread::yield_now();
                    }
                }
            });
        }

        Self {
            queues,
            write_selector: AtomicU64::new(0),
            _metrics: metrics,
        }
    }

    pub fn submit(&self, record: String) {
        let idx = self.write_selector.fetch_add(1, Ordering::Relaxed) as usize;
        let queue_idx = idx % self.queues.len();
        self.queues[queue_idx].push_blocking(record);
    }
}
