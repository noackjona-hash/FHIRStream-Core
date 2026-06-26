mod api;
mod config;

use std::sync::Arc;
use std::time::Duration;
use std::thread;
use tracing_subscriber::filter::EnvFilter;
use fhirstream_core::pipeline::{PipelineMetrics, IngestionPipeline};
use api::{create_router, generate_mock_patient};
use config::AppConfig;
use axum_server::Handle;

#[tokio::main]
async fn main() {
    // Configure structured JSON logging
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // Load configurations
    let cfg = AppConfig::load().unwrap_or_else(|e| {
        tracing::error!("Failed to load configuration: {}", e);
        std::process::exit(1);
    });

    tracing::info!("Initializing FHIRStream Ingestion Engine. Mask PHI: {}", cfg.mask_phi);

    let metrics = Arc::new(PipelineMetrics::new());
    let pipeline = Arc::new(IngestionPipeline::new(Arc::clone(&metrics)));

    // Spawn network load simulator
    let pipeline_clone = Arc::clone(&pipeline);
    thread::spawn(move || {
        let mut id = 0;
        loop {
            thread::sleep(Duration::from_millis(20));
            let inject_chaos = id % 25 == 0;
            let patient_json = generate_mock_patient(id, inject_chaos);
            pipeline_clone.submit(patient_json);
            id += 1;
        }
    });

    let app = create_router(metrics, cfg.mask_phi);

    // Setup TLS 1.3 configuration
    let tls_config = if let (Some(c_path), Some(k_path)) = (&cfg.cert_path, &cfg.key_path) {
        tracing::info!("Loading TLS certificates from files: {} / {}", c_path, k_path);
        axum_server::tls_rustls::RustlsConfig::from_pem_file(c_path, k_path)
            .await
            .expect("Failed to load TLS cert/key files")
    } else {
        tracing::warn!("No TLS cert/key paths provided. Generating self-signed in-memory certificate for TLS 1.3...");
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let cert = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();
        let cert_pem = cert.cert.pem().into_bytes();
        let key_pem = cert.key_pair.serialize_pem().into_bytes();
        axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem, key_pem)
            .await
            .expect("Failed to load generated self-signed TLS cert/key")
    };

    let addr = format!("{}:{}", cfg.host, cfg.port).parse().unwrap();
    tracing::info!("Server listening securely on https://{}", addr);

    // Graceful shutdown handle
    let handle = Handle::new();
    let handle_clone = handle.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }

        tracing::info!("Received shutdown signal. Flashing pipeline workers and stopping API...");
        handle_clone.graceful_shutdown(Some(Duration::from_secs(10)));
    });

    axum_server::bind_rustls(addr, tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .unwrap();

    tracing::info!("Secure server shut down successfully.");
}
