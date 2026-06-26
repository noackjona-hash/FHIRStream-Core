use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sysinfo::System;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;
use std::borrow::Cow;
use rand::Rng;
use fhirstream_core::parser::{FhirParser, ParseError, FieldMetadata};
use fhirstream_core::pipeline::PipelineMetrics;
use fhirstream_core::mask::mask_phi_name;

#[derive(Deserialize)]
pub struct ParseRequest {
    pub raw_json: String,
}

#[derive(Serialize)]
pub struct ErrorMatrixResponse {
    pub errors: Vec<ParseError>,
    pub success_percentage: f64,
}

#[derive(Serialize)]
pub struct MaskedZeroCopyField<T> {
    pub value: T,
    pub metadata: FieldMetadata,
}

#[derive(Serialize)]
pub struct MaskedFhirHumanName<'a> {
    pub family: Option<MaskedZeroCopyField<Cow<'a, str>>>,
    pub given: Vec<MaskedZeroCopyField<Cow<'a, str>>>,
    pub metadata: FieldMetadata,
}

#[derive(Serialize)]
pub struct MaskedFhirPatient<'a> {
    pub resource_type: MaskedZeroCopyField<&'a str>,
    pub id: MaskedZeroCopyField<&'a str>,
    pub active: Option<MaskedZeroCopyField<bool>>,
    pub gender: Option<MaskedZeroCopyField<&'a str>>,
    pub birth_date: Option<MaskedZeroCopyField<&'a str>>,
    pub names: Vec<MaskedFhirHumanName<'a>>,
}

#[derive(Serialize)]
pub struct ParseResponse<'a> {
    pub patient: Option<MaskedFhirPatient<'a>>,
    pub error_matrix: ErrorMatrixResponse,
}

#[derive(Serialize)]
pub struct StressTestResponse {
    pub duration_ms: u64,
    pub records_processed: u64,
    pub bytes_processed: u64,
    pub errors_encountered: u64,
    pub throughput_mb_s: f64,
    pub average_latency_us: f64,
    pub success_recovered_pct: f64,
}

pub struct ApiState {
    pub metrics: Arc<PipelineMetrics>,
    pub mask_phi: bool,
}

pub fn create_router(metrics: Arc<PipelineMetrics>, mask_phi: bool) -> Router {
    let state = Arc::new(ApiState { metrics, mask_phi });
    Router::new()
        .route("/api/v1/parse", post(parse_single_handler))
        .route("/api/v1/stresstest", get(stresstest_handler))
        .route("/api/v1/metrics", get(ws_handler))
        .route("/metrics", get(prometheus_metrics_handler))
        .with_state(state)
}

pub async fn parse_single_handler(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<ParseRequest>,
) -> impl IntoResponse {
    let raw = &payload.raw_json;
    let bump = bumpalo::Bump::new();
    let mut parser = FhirParser::new(raw, &bump);
    let patient = parser.parse_patient().ok();
    
    let errors = parser.get_errors().to_vec();
    let corrupt = parser.get_corrupt_bytes();
    let total = raw.len();
    
    let success_percentage = if total > 0 {
        (((total - corrupt) as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        100.0
    };

    let masked_patient = patient.map(|p| {
        let masked_names = p.names.iter().map(|name| {
            let family = name.family.as_ref().map(|f| {
                let value = if state.mask_phi {
                    mask_phi_name(f.value)
                } else {
                    Cow::Borrowed(f.value)
                };
                MaskedZeroCopyField {
                    value,
                    metadata: f.metadata,
                }
            });

            let given = name.given.iter().map(|g| {
                let value = if state.mask_phi {
                    mask_phi_name(g.value)
                } else {
                    Cow::Borrowed(g.value)
                };
                MaskedZeroCopyField {
                    value,
                    metadata: g.metadata,
                }
            }).collect();

            MaskedFhirHumanName {
                family,
                given,
                metadata: name.metadata,
            }
        }).collect();

        MaskedFhirPatient {
            resource_type: MaskedZeroCopyField {
                value: p.resource_type.value,
                metadata: p.resource_type.metadata,
            },
            id: MaskedZeroCopyField {
                value: p.id.value,
                metadata: p.id.metadata,
            },
            active: p.active.map(|a| MaskedZeroCopyField {
                value: a.value,
                metadata: a.metadata,
            }),
            gender: p.gender.map(|g| MaskedZeroCopyField {
                value: g.value,
                metadata: g.metadata,
            }),
            birth_date: p.birth_date.map(|b| MaskedZeroCopyField {
                value: b.value,
                metadata: b.metadata,
            }),
            names: masked_names,
        }
    });

    let response_data = ParseResponse {
        patient: masked_patient,
        error_matrix: ErrorMatrixResponse {
            errors,
            success_percentage,
        },
    };

    let serialized = match serde_json::to_string(&response_data) {
        Ok(s) => s,
        Err(e) => return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        ).into_response(),
    };

    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serialized,
    ).into_response()
}

pub async fn prometheus_metrics_handler(
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    let bytes = state.metrics.total_bytes_processed.load(Ordering::Relaxed);
    let records = state.metrics.total_records_processed.load(Ordering::Relaxed);
    let latency = state.metrics.total_latency_us.load(Ordering::Relaxed);
    let errors = state.metrics.total_errors.load(Ordering::Relaxed);
    let corrupt = state.metrics.corrupt_bytes.load(Ordering::Relaxed);

    let output = format!(
        r#"# HELP fhirstream_bytes_processed_total Total bytes processed by the ingestion engine
# TYPE fhirstream_bytes_processed_total counter
fhirstream_bytes_processed_total {}
# HELP fhirstream_records_processed_total Total FHIR patient records processed
# TYPE fhirstream_records_processed_total counter
fhirstream_records_processed_total {}
# HELP fhirstream_latency_us_total Cumulative processing latency in microseconds
# TYPE fhirstream_latency_us_total counter
fhirstream_latency_us_total {}
# HELP fhirstream_errors_total Total parsing/validation errors encountered
# TYPE fhirstream_errors_total counter
fhirstream_errors_total {}
# HELP fhirstream_corrupt_bytes_total Total corrupt bytes isolated and recovered
# TYPE fhirstream_corrupt_bytes_total counter
fhirstream_corrupt_bytes_total {}
"#,
        bytes, records, latency, errors, corrupt
    );

    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        output,
    )
}

pub async fn stresstest_handler(
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    let start_time = Instant::now();
    let (tx, rx) = crossbeam_channel::bounded::<String>(1000);
    let num_workers = num_cpus::get();
    let total_records: u64 = 100_000;

    let success_count = Arc::new(AtomicU64::new(0));
    let error_count = Arc::new(AtomicU64::new(0));
    let bytes_processed = Arc::new(AtomicU64::new(0));
    let corrupt_bytes = Arc::new(AtomicU64::new(0));
    let total_latency = Arc::new(AtomicU64::new(0));

    let mut workers = Vec::new();
    for _ in 0..num_workers {
        let rx_clone = rx.clone();
        let success_clone = Arc::clone(&success_count);
        let error_clone = Arc::clone(&error_count);
        let bytes_clone = Arc::clone(&bytes_processed);
        let corrupt_clone = Arc::clone(&corrupt_bytes);
        let latency_clone = Arc::clone(&total_latency);
        let global_metrics = Arc::clone(&state.metrics);

        let handle = thread::spawn(move || {
            let mut bump = bumpalo::Bump::with_capacity(1024 * 1024);
            while let Ok(record) = rx_clone.recv() {
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

                success_clone.fetch_add(1, Ordering::Relaxed);
                error_clone.fetch_add(num_errors, Ordering::Relaxed);
                bytes_clone.fetch_add(record_len, Ordering::Relaxed);
                corrupt_clone.fetch_add(corrupt, Ordering::Relaxed);
                latency_clone.fetch_add(duration, Ordering::Relaxed);

                global_metrics.total_bytes_processed.fetch_add(record_len, Ordering::Relaxed);
                global_metrics.total_records_processed.fetch_add(1, Ordering::Relaxed);
                global_metrics.total_latency_us.fetch_add(duration, Ordering::Relaxed);
                global_metrics.total_errors.fetch_add(num_errors, Ordering::Relaxed);
                global_metrics.corrupt_bytes.fetch_add(corrupt, Ordering::Relaxed);
                
                bump.reset();
            }
        });
        workers.push(handle);
    }

    thread::spawn(move || {
        for i in 0..total_records {
            let inject_chaos = i % 10 == 0;
            let patient_json = generate_mock_patient(i as u32, inject_chaos);
            let _ = tx.send(patient_json);
        }
    });

    for worker in workers {
        let _ = worker.join();
    }

    let elapsed = start_time.elapsed();
    let duration_ms = elapsed.as_millis() as u64;
    let bytes = bytes_processed.load(Ordering::Relaxed);
    let errors = error_count.load(Ordering::Relaxed);
    let latency = total_latency.load(Ordering::Relaxed);
    let corrupt = corrupt_bytes.load(Ordering::Relaxed);

    let throughput_mb_s = if elapsed.as_secs_f64() > 0.0 {
        (bytes as f64 / 1024.0 / 1024.0) / elapsed.as_secs_f64()
    } else {
        0.0
    };

    let average_latency_us = if total_records > 0 {
        latency as f64 / total_records as f64
    } else {
        0.0
    };

    let success_recovered_pct = if bytes > 0 {
        (((bytes - corrupt) as f64 / bytes as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        100.0
    };

    Json(StressTestResponse {
        duration_ms,
        records_processed: total_records,
        bytes_processed: bytes,
        errors_encountered: errors,
        throughput_mb_s,
        average_latency_us,
        success_recovered_pct,
    })
}

pub fn generate_mock_patient(id_val: u32, inject_chaos: bool) -> String {
    let mut rng = rand::thread_rng();
    if inject_chaos {
        let chaos_type = rng.gen_range(0..3);
        match chaos_type {
            0 => {
                format!(
                    r#"{{"resourceType":"Patient","id":"pat-{}","active":true,"gender":"male","birthDate":"invalid-iso-date","name":[{{"family":"Smith","given":["John"]}}]}}"#,
                    id_val
                )
            }
            1 => {
                format!(
                    r#"{{"resourceType":"Patient","id":"pat-{}","active":"yes","gender":"female","birthDate":"1990-05-20","name":[{{"family":"Davis","given":["Alice"]}}]}}"#,
                    id_val
                )
            }
            _ => {
                format!(
                    r#"{{"resourceType":"Patient","id":"pat-{}","active":true,"gender":"other","birthDate":"2001-11-30","name":[{{"family":"Taylor","given": "not-an-array"}}]}}"#,
                    id_val
                )
            }
        }
    } else {
        let genders = ["male", "female", "other", "unknown"];
        let families = ["Smith", "Doe", "Johnson", "Williams", "Brown", "Jones", "Miller", "Davis"];
        let givens = ["John", "Jane", "William", "James", "Mary", "Patricia", "Robert", "Jennifer"];
        let gender = genders[rng.gen_range(0..genders.len())];
        let family = families[rng.gen_range(0..families.len())];
        let given = givens[rng.gen_range(0..givens.len())];
        let active = rng.gen_bool(0.9);

        format!(
            r#"{{"resourceType":"Patient","id":"pat-{}","active":{},"gender":"{}","birthDate":"1995-10-15","name":[{{"family":"{}","given":["{}"]}}]}}"#,
            id_val, active, gender, family, given
        )
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Serialize)]
pub struct RealTimeMetrics {
    pub throughput_mb_s: f64,
    pub avg_latency_us: f64,
    pub total_records: u64,
    pub total_errors: u64,
    pub cpu_cores: Vec<f32>,
}

async fn handle_socket(mut socket: WebSocket, state: Arc<ApiState>) {
    let mut sys = System::new_all();
    sys.refresh_cpu();

    let mut last_bytes = state.metrics.total_bytes_processed.load(Ordering::Relaxed);
    let mut last_records = state.metrics.total_records_processed.load(Ordering::Relaxed);
    let mut last_latency = state.metrics.total_latency_us.load(Ordering::Relaxed);
    let mut last_instant = Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;

        let now = Instant::now();
        let elapsed_secs = now.duration_since(last_instant).as_secs_f64();
        last_instant = now;

        let current_bytes = state.metrics.total_bytes_processed.load(Ordering::Relaxed);
        let current_records = state.metrics.total_records_processed.load(Ordering::Relaxed);
        let current_latency = state.metrics.total_latency_us.load(Ordering::Relaxed);
        let current_errors = state.metrics.total_errors.load(Ordering::Relaxed);

        let delta_bytes = current_bytes.saturating_sub(last_bytes);
        let delta_records = current_records.saturating_sub(last_records);
        let delta_latency = current_latency.saturating_sub(last_latency);

        last_bytes = current_bytes;
        last_records = current_records;
        last_latency = current_latency;

        let throughput_mb_s = if elapsed_secs > 0.0 {
            (delta_bytes as f64 / 1024.0 / 1024.0) / elapsed_secs
        } else {
            0.0
        };

        let avg_latency_us = if delta_records > 0 {
            delta_latency as f64 / delta_records as f64
        } else {
            0.0
        };

        sys.refresh_cpu();
        let cpu_cores: Vec<f32> = sys.cpus().iter().map(|cpu| cpu.cpu_usage()).collect();

        let payload = RealTimeMetrics {
            throughput_mb_s,
            avg_latency_us,
            total_records: current_records,
            total_errors: current_errors,
            cpu_cores,
        };

        if let Ok(json_str) = serde_json::to_string(&payload) {
            if socket.send(Message::Text(json_str)).await.is_err() {
                break;
            }
        } else {
            break;
        }
    }
}
