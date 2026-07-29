use eframe::egui;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 750.0])
            .with_title("VHS RF Signal Analyser v6.5"),
        ..Default::default()
    };
    eframe::run_native(
        "Signal Compare RF",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    original: String,
    decimated: String,
    baseline_adc: String,
    baseline_chain: String,
    standard: String,
    orig_bits: String,
    deci_bits: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            original: String::new(),
            decimated: String::new(),
            baseline_adc: String::new(),
            baseline_chain: String::new(),
            standard: "PAL".to_string(),
            orig_bits: "auto".to_string(),
            deci_bits: "auto".to_string(),
        }
    }
}

struct App {
    config: AppConfig,
    output: Arc<Mutex<String>>,
    running: Arc<Mutex<bool>>,
    config_path: PathBuf,
}

impl App {
    fn new() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("DecimateRF");
        std::fs::create_dir_all(&config_dir).ok();
        let config_path = config_dir.join("compare_config.json");

        let config = if config_path.exists() {
            std::fs::read_to_string(&config_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            AppConfig::default()
        };

        Self {
            config,
            output: Arc::new(Mutex::new("Ready. Select files and click Compare.".to_string())),
            running: Arc::new(Mutex::new(false)),
            config_path,
        }
    }

    fn save_config(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.config) {
            std::fs::write(&self.config_path, json).ok();
        }
    }

    fn browse_file(current: &str) -> Option<String> {
        let initial = if !current.is_empty() {
            PathBuf::from(current).parent().map(|p| p.to_path_buf())
        } else {
            None
        };

        let mut dialog = FileDialog::new()
            .add_filter("RF Signal Files", &["flac", "u16", "s16", "u8"])
            .add_filter("All Files", &["*"]);
        if let Some(dir) = initial {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_file().map(|p| p.display().to_string())
    }

    fn run_compare(&self) {
        let cfg = self.config.clone();
        let output = Arc::clone(&self.output);
        let running = Arc::clone(&self.running);

        // Find compare-rf binary next to this exe
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let backend = exe_dir.join("compare-rf.exe");

        *running.lock().unwrap() = true;
        *output.lock().unwrap() = "Analyzing... (streaming entire file with Welch averaging)".to_string();

        thread::spawn(move || {
            let mut cmd = Command::new(&backend);
            cmd.arg(&cfg.original).arg(&cfg.decimated);
            cmd.args(["--standard", &cfg.standard.to_lowercase()]);

            if cfg.orig_bits != "auto" {
                cmd.args(["--orig-bits", &cfg.orig_bits]);
            }
            if cfg.deci_bits != "auto" {
                cmd.args(["--deci-bits", &cfg.deci_bits]);
            }
            if !cfg.baseline_adc.is_empty() && !cfg.baseline_chain.is_empty() {
                cmd.args(["--baseline-adc", &cfg.baseline_adc]);
                cmd.args(["--baseline-chain", &cfg.baseline_chain]);
            }

            let result = match cmd.output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    if out.status.success() {
                        format!("{}\n\n--- Log ---\n{}", stdout, stderr)
                    } else {
                        format!("ERROR (exit {}):\n{}\n{}", out.status, stderr, stdout)
                    }
                }
                Err(e) => format!("Failed to run compare-rf.exe: {}\nExpected at: {}", e, backend.display()),
            };

            *output.lock().unwrap() = result;
            *running.lock().unwrap() = false;
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint while running so output updates
        if *self.running.lock().unwrap() {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VHS RF Signal Analyser v6.5");
            ui.separator();

            egui::Grid::new("file_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Original:");
                ui.add(egui::TextEdit::singleline(&mut self.config.original).desired_width(500.0));
                if ui.button("Browse...").clicked() {
                    if let Some(p) = Self::browse_file(&self.config.original) {
                        self.config.original = p;
                        self.save_config();
                    }
                }
                ui.end_row();

                ui.label("Comparison:");
                ui.add(egui::TextEdit::singleline(&mut self.config.decimated).desired_width(500.0));
                if ui.button("Browse...").clicked() {
                    if let Some(p) = Self::browse_file(&self.config.decimated) {
                        self.config.decimated = p;
                        self.save_config();
                    }
                }
                ui.end_row();

                ui.label("Baseline ADC Noise:");
                ui.add(egui::TextEdit::singleline(&mut self.config.baseline_adc).desired_width(500.0));
                if ui.button("Browse...").clicked() {
                    if let Some(p) = Self::browse_file(&self.config.baseline_adc) {
                        self.config.baseline_adc = p;
                        self.save_config();
                    }
                }
                ui.end_row();

                ui.label("Baseline Chain Noise:");
                ui.add(egui::TextEdit::singleline(&mut self.config.baseline_chain).desired_width(500.0));
                if ui.button("Browse...").clicked() {
                    if let Some(p) = Self::browse_file(&self.config.baseline_chain) {
                        self.config.baseline_chain = p;
                        self.save_config();
                    }
                }
                ui.end_row();
            });

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("RF Standard:");
                egui::ComboBox::from_id_salt("standard")
                    .selected_text(&self.config.standard)
                    .show_ui(ui, |ui| {
                        for s in &["NTSC", "PAL", "M-PAL", "N-PAL"] {
                            ui.selectable_value(&mut self.config.standard, s.to_string(), *s);
                        }
                    });

                ui.add_space(20.0);
                ui.label("Orig bits:");
                egui::ComboBox::from_id_salt("orig_bits")
                    .selected_text(&self.config.orig_bits)
                    .show_ui(ui, |ui| {
                        for s in &["auto", "8", "12", "16"] {
                            ui.selectable_value(&mut self.config.orig_bits, s.to_string(), *s);
                        }
                    });

                ui.add_space(20.0);
                ui.label("Cmp bits:");
                egui::ComboBox::from_id_salt("deci_bits")
                    .selected_text(&self.config.deci_bits)
                    .show_ui(ui, |ui| {
                        for s in &["auto", "8", "12", "16"] {
                            ui.selectable_value(&mut self.config.deci_bits, s.to_string(), *s);
                        }
                    });
            });

            ui.separator();

            let is_running = *self.running.lock().unwrap();
            ui.horizontal(|ui| {
                if ui.add_enabled(!is_running, egui::Button::new("COMPARE").min_size(egui::vec2(120.0, 35.0))).clicked() {
                    self.save_config();
                    self.run_compare();
                }
                if is_running {
                    ui.spinner();
                    ui.label("Analysing...");
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                let text = self.output.lock().unwrap().clone();
                ui.add(egui::TextEdit::multiline(&mut text.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY));
            });
        });
    }
}
