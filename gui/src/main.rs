#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use sysinfo::System;
use rand::Rng;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::VecDeque;
use std::thread;
use fhirstream_core::parser::{FhirParser, ParseError};
use fhirstream_core::pipeline::{PipelineMetrics, IngestionPipeline};

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1150.0, 780.0])
            .with_title("FHIRStream Core - Enterprise Ingestion Monitor"),
        ..Default::default()
    };
    eframe::run_native(
        "FHIRStream Monitor",
        options,
        Box::new(|_cc| Box::new(FhirApp::new())),
    )
}

struct HoverableField {
    name: String,
    offset: usize,
    len: usize,
    address: usize,
}

struct FhirApp {
    raw_json: String,
    fields: Vec<HoverableField>,
    errors: Vec<ParseError>,
    recovery_percentage: f64,
    metrics: Arc<PipelineMetrics>,
    _pipeline: Arc<IngestionPipeline>,
    sys_info: System,
    cpu_usages: Vec<f32>,
    throughput_history: VecDeque<f64>,
    last_refresh: Instant,
    last_bytes: u64,
    stresstest_active: Arc<std::sync::atomic::AtomicBool>,
    stresstest_progress: f32,
    _stresstest_duration_ms: u64,
    _stresstest_throughput: f64,
}

impl FhirApp {
    fn new() -> Self {
        let metrics = Arc::new(PipelineMetrics::new());
        let pipeline = Arc::new(IngestionPipeline::new(Arc::clone(&metrics)));
        let mut sys_info = System::new_all();
        sys_info.refresh_cpu();
        
        let initial_json = r#"{
    "resourceType": "Patient",
    "id": "pat-00123",
    "active": true,
    "gender": "female",
    "birthDate": "1994-08-24",
    "name": [
        {
            "family": "Mustermann",
            "given": [
                "Clara",
                "Maria"
            ]
        }
    ]
}"#;

        let mut app = Self {
            raw_json: initial_json.to_string(),
            fields: Vec::new(),
            errors: Vec::new(),
            recovery_percentage: 100.0,
            metrics,
            _pipeline: pipeline,
            sys_info,
            cpu_usages: Vec::new(),
            throughput_history: VecDeque::from(vec![0.0; 40]),
            last_refresh: Instant::now(),
            last_bytes: 0,
            stresstest_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stresstest_progress: 0.0,
            _stresstest_duration_ms: 0,
            _stresstest_throughput: 0.0,
        };
        app.reparse_current_json();
        app
    }

    fn reparse_current_json(&mut self) {
        self.fields.clear();
        self.errors.clear();
        self.recovery_percentage = 100.0;
        
        if self.raw_json.is_empty() {
            return;
        }

        let mut parser = FhirParser::new(&self.raw_json);
        let patient = parser.parse_patient().ok();
        self.errors = parser.get_errors().to_vec();
        
        let corrupt = parser.get_corrupt_bytes();
        let total = self.raw_json.len();
        self.recovery_percentage = if total > 0 {
            (((total - corrupt) as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };

        if let Some(p) = patient {
            self.fields.push(HoverableField {
                name: "resourceType".to_string(),
                offset: p.resource_type.metadata.offset,
                len: p.resource_type.value.len(),
                address: p.resource_type.metadata.address,
            });

            self.fields.push(HoverableField {
                name: "id".to_string(),
                offset: p.id.metadata.offset,
                len: p.id.value.len(),
                address: p.id.metadata.address,
            });

            if let Some(act) = p.active {
                self.fields.push(HoverableField {
                    name: "active".to_string(),
                    offset: act.metadata.offset,
                    len: if act.value { 4 } else { 5 },
                    address: act.metadata.address,
                });
            }

            if let Some(gender_val) = p.gender {
                self.fields.push(HoverableField {
                    name: "gender".to_string(),
                    offset: gender_val.metadata.offset,
                    len: gender_val.value.len(),
                    address: gender_val.metadata.address,
                });
            }

            if let Some(bd) = p.birth_date {
                self.fields.push(HoverableField {
                    name: "birthDate".to_string(),
                    offset: bd.metadata.offset,
                    len: bd.value.len(),
                    address: bd.metadata.address,
                });
            }

            for (i, name) in p.names.iter().enumerate() {
                if let Some(fam) = &name.family {
                    self.fields.push(HoverableField {
                        name: format!("name[{}].family", i),
                        offset: fam.metadata.offset,
                        len: fam.value.len(),
                        address: fam.metadata.address,
                    });
                }
                for (j, giv) in name.given.iter().enumerate() {
                    self.fields.push(HoverableField {
                        name: format!("name[{}].given[{}]", i, j),
                        offset: giv.metadata.offset,
                        len: giv.value.len(),
                        address: giv.metadata.address,
                    });
                }
            }
        }
        
        self.fields.sort_by_key(|f| f.offset);
    }
}

impl eframe::App for FhirApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Dynamic stats refresh loop (100ms)
        let now = Instant::now();
        if now.duration_since(self.last_refresh) >= Duration::from_millis(100) {
            self.sys_info.refresh_cpu();
            self.cpu_usages = self.sys_info.cpus().iter().map(|cpu| cpu.cpu_usage()).collect();

            let current_bytes = self.metrics.total_bytes_processed.load(Ordering::Relaxed);
            let delta = current_bytes.saturating_sub(self.last_bytes);
            self.last_bytes = current_bytes;

            let elapsed = now.duration_since(self.last_refresh).as_secs_f64();
            let throughput = if elapsed > 0.0 {
                (delta as f64 / 1024.0 / 1024.0) / elapsed
            } else {
                0.0
            };
            self.throughput_history.push_back(throughput);
            if self.throughput_history.len() > 40 {
                self.throughput_history.pop_front();
            }

            self.last_refresh = now;
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("header_panel").show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(egui::RichText::new("FHIRStream Enterprise Ingestion Core").size(24.0).strong().color(egui::Color32::from_rgb(100, 180, 255)));
                ui.label("Real-time zero-copy parsing, core utilization & validator analytics dashboard.");
                ui.add_space(8.0);
            });
        });

        egui::SidePanel::left("metrics_panel").width_range(280.0..=350.0).show(ctx, |ui| {
            ui.add_space(10.0);
            ui.heading("Telemetry Indicators");
            ui.separator();
            ui.add_space(5.0);

            let records = self.metrics.total_records_processed.load(Ordering::Relaxed);
            let errors = self.metrics.total_errors.load(Ordering::Relaxed);
            let total_bytes = self.metrics.total_bytes_processed.load(Ordering::Relaxed);
            let latency_sum = self.metrics.total_latency_us.load(Ordering::Relaxed);
            let avg_latency = if records > 0 { latency_sum as f64 / records as f64 } else { 0.0 };

            ui.columns(2, |cols| {
                cols[0].label("Processed Records:");
                cols[1].label(egui::RichText::new(records.to_string()).strong());
                cols[0].label("Isolated Errors:");
                cols[1].label(egui::RichText::new(errors.to_string()).strong().color(if errors > 0 { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN }));
                cols[0].label("Volume Processed:");
                cols[1].label(egui::RichText::new(format!("{:.2} MB", total_bytes as f64 / 1024.0 / 1024.0)).strong());
                cols[0].label("Avg Latency/File:");
                cols[1].label(egui::RichText::new(format!("{:.3} μs", avg_latency)).strong().color(egui::Color32::LIGHT_BLUE));
            });

            ui.add_space(15.0);
            ui.heading("Live Throughput (MB/s)");
            ui.separator();
            ui.add_space(5.0);

            let current_throughput = *self.throughput_history.back().unwrap_or(&0.0);
            ui.label(egui::RichText::new(format!("{:.2} MB/s", current_throughput)).size(28.0).strong().color(egui::Color32::from_rgb(50, 220, 120)));

            // Visual bar chart for throughput
            ui.add_space(5.0);
            let max_val = self.throughput_history.iter().copied().fold(1.0, f64::max);
            ui.horizontal(|ui| {
                for &val in &self.throughput_history {
                    let pct = (val / max_val) as f32;
                    let color = egui::Color32::from_rgb(
                        (50.0 + (pct * 150.0)) as u8,
                        (220.0 - (pct * 50.0)) as u8,
                        (120.0 + (pct * 50.0)) as u8,
                    );
                    ui.colored_label(color, "┃");
                }
            });

            ui.add_space(15.0);
            ui.heading("Core Utilization");
            ui.separator();
            ui.add_space(5.0);

            for (i, &usage) in self.cpu_usages.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("Core {}:", i));
                    let progress = usage / 100.0;
                    let color = egui::Color32::from_rgb(
                        (progress * 255.0) as u8,
                        ((1.0 - progress) * 200.0) as u8,
                        ((1.0 - progress) * 50.0 + 100.0) as u8,
                    );
                    let bar = egui::ProgressBar::new(progress)
                        .text(format!("{:.1}%", usage))
                        .fill(color);
                    ui.add(bar);
                });
            }

            ui.add_space(20.0);
            ui.heading("Stress Testing");
            ui.separator();
            ui.add_space(10.0);

            let is_stresstest_running = self.stresstest_active.load(Ordering::Relaxed);
            if is_stresstest_running {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Stress test executing...");
                ui.add(egui::ProgressBar::new(self.stresstest_progress));
            } else {
                let btn = egui::Button::new(
                    egui::RichText::new("Simulate 100k Records")
                        .size(16.0)
                        .strong()
                ).fill(egui::Color32::from_rgb(180, 50, 50));
                
                if ui.add(btn).clicked() {
                    self.stresstest_progress = 0.0;
                    
                    let metrics_clone = Arc::clone(&self.metrics);
                    let active_clone = Arc::clone(&self.stresstest_active);
                    active_clone.store(true, Ordering::Relaxed);
                    
                    thread::spawn(move || {
                        let _start = Instant::now();
                        let (tx, rx) = crossbeam_channel::bounded::<String>(1000);
                        let num_workers = num_cpus::get();
                        let total_records = 100_000;

                        let mut workers = Vec::new();
                        for _ in 0..num_workers {
                            let rx_clone = rx.clone();
                            let global_m = Arc::clone(&metrics_clone);
                            let handle = thread::spawn(move || {
                                while let Ok(record) = rx_clone.recv() {
                                    let start = Instant::now();
                                    let record_len = record.len() as u64;
                                    let mut parser = FhirParser::new(&record);
                                    let _parsed = parser.parse_patient();
                                    let duration = start.elapsed().as_micros() as u64;
                                    let num_errors = parser.get_errors().len() as u64;
                                    let corrupt = parser.get_corrupt_bytes() as u64;

                                    global_m.total_bytes_processed.fetch_add(record_len, Ordering::Relaxed);
                                    global_m.total_records_processed.fetch_add(1, Ordering::Relaxed);
                                    global_m.total_latency_us.fetch_add(duration, Ordering::Relaxed);
                                    global_m.total_errors.fetch_add(num_errors, Ordering::Relaxed);
                                    global_m.corrupt_bytes.fetch_add(corrupt, Ordering::Relaxed);
                                }
                            });
                            workers.push(handle);
                        }

                        for i in 0..total_records {
                            let inject_chaos = i % 10 == 0;
                            let patient_json = crate::generate_mock_patient(i, inject_chaos);
                            let _ = tx.send(patient_json);
                        }
                        drop(tx);

                        for worker in workers {
                            let _ = worker.join();
                        }
                        active_clone.store(false, Ordering::Relaxed);
                    });
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(5.0);
            ui.heading("Interactive Hex/RAM Pointer Highlighting");
            ui.separator();
            ui.add_space(5.0);
            ui.label("Edit or paste JSON below. Hover over highlighted zero-copy fields to inspect real-time memory addresses.");

            // JSON Edit input area
            let prev_json = self.raw_json.clone();
            ui.horizontal(|ui| {
                ui.label("Input Payload:");
                if ui.button("Reset Default Payload").clicked() {
                    self.raw_json = r#"{
    "resourceType": "Patient",
    "id": "pat-00123",
    "active": true,
    "gender": "female",
    "birthDate": "1994-08-24",
    "name": [
        {
            "family": "Mustermann",
            "given": [
                "Clara",
                "Maria"
            ]
        }
    ]
}"#.to_string();
                }
            });
            
            let text_edit = egui::TextEdit::multiline(&mut self.raw_json)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(12);

            ui.add(text_edit);

            if self.raw_json != prev_json {
                self.reparse_current_json();
            }

            ui.add_space(10.0);
            ui.heading("Live Highlight & Pointer Mapping");
            ui.separator();
            ui.add_space(5.0);

            // Render interactive offsets highlights
            ui.horizontal_wrapped(|ui| {
                let mut current_idx = 0;
                for field in &self.fields {
                    if field.offset > current_idx {
                        let text = &self.raw_json[current_idx..field.offset];
                        ui.monospace(text);
                    }
                    if field.offset + field.len <= self.raw_json.len() {
                        let text = &self.raw_json[field.offset..field.offset + field.len];
                        let colored_text = egui::RichText::new(text)
                            .monospace()
                            .strong()
                            .color(egui::Color32::from_rgb(10, 10, 10));
                        let label = egui::Label::new(colored_text)
                            .sense(egui::Sense::hover());
                        
                        let response = ui.add(label);
                        // Draw highlight block
                        let rect = response.rect.expand(2.0);
                        let bg_color = egui::Color32::from_rgba_unmultiplied(100, 200, 255, 120);
                        ui.painter().rect_filled(rect, 4.0, bg_color);

                        response.on_hover_ui(|ui| {
                            ui.heading(format!("Field: {}", field.name));
                            ui.separator();
                            ui.label(format!("Byte Offset: {} - {}", field.offset, field.offset + field.len));
                            ui.label(format!("RAM Pointer: 0x{:X}", field.address));
                        });
                    }
                    current_idx = field.offset + field.len;
                }
                if current_idx < self.raw_json.len() {
                    let text = &self.raw_json[current_idx..];
                    ui.monospace(text);
                }
            });

            ui.add_space(15.0);
            ui.horizontal(|ui| {
                ui.heading("Chaos Clinic recovery matrix");
                ui.add_space(20.0);
                ui.label(egui::RichText::new(format!("Data Recovery Rate: {:.2}%", self.recovery_percentage))
                    .strong()
                    .size(16.0)
                    .color(if self.recovery_percentage > 90.0 { egui::Color32::LIGHT_GREEN } else { egui::Color32::LIGHT_RED }));
            });
            ui.separator();
            ui.add_space(5.0);

            if self.errors.is_empty() {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "✔ Clinical Validator: No structural or logical anomalies detected.");
            } else {
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for err in &self.errors {
                        ui.horizontal(|ui| {
                            let (badge, color) = if err.code == 401 {
                                ("STRUCTURAL ERROR (401)", egui::Color32::LIGHT_RED)
                            } else {
                                ("VALIDATION ANOMALY (402)", egui::Color32::GOLD)
                            };
                            ui.colored_label(color, format!("[{}]", badge));
                            ui.label(format!("Offset {}: {}", err.offset, err.message));
                        });
                    }
                });
            }
        });
    }
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
