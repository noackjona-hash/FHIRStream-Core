mod parser;
mod pipeline;
mod api;

use std::sync::Arc;
use std::time::Duration;
use std::thread;
use pipeline::{PipelineMetrics, IngestionPipeline};
use api::{create_router, generate_mock_patient};

#[tokio::main]
async fn main() {
    let metrics = Arc::new(PipelineMetrics::new());
    let pipeline = Arc::new(IngestionPipeline::new(Arc::clone(&metrics)));

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

    let app = create_router(metrics);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080")
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
}
