use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use slint::{ComponentHandle, ModelRc, StandardListViewItem, VecModel};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use crate::slint_generatedMainWindow::{MainWindow, AppGlobals};

use crate::image_processor::{process_file, ImageProcessorConfig};
use crate::file_handler::{QueuedFile, get_file_size, scan_directory, validate_file_format};
use crate::settings::{AppSettings, ResizeFilter, ResizeMode};
use crate::format_handlers::ImageFormat;

#[derive(Clone)]
struct FailedFile {
    input_path: PathBuf,
    error: String,
}

#[derive(Clone)]
pub struct AppStateInternal {
    pub config: ImageProcessorConfig,
    pub files: Arc<RwLock<Vec<QueuedFile>>>,
    pub failed_files: Arc<RwLock<Vec<FailedFile>>>,
    pub completed_files: Arc<RwLock<u64>>,
    pub total_bytes_before: Arc<RwLock<u64>>,
    pub total_bytes_after: Arc<RwLock<u64>>,
    pub processed_input_bytes: Arc<RwLock<u64>>,
    pub start_time: Arc<RwLock<Option<Instant>>>,
}

pub struct App {
    ui: MainWindow,
    app_state: AppStateInternal,
    cancellation_token: Arc<AtomicBool>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let ui = MainWindow::new()?;
        
        ui.global::<AppGlobals>().set_app_version(env!("CARGO_PKG_VERSION").into());

        let settings = AppSettings::load()
            .unwrap_or_else(|err| {
                eprintln!("Failed to load settings: {}, using defaults.", err); // Added logging
                AppSettings::default()
            });
        
        ui.global::<AppGlobals>().set_output_path(settings.save_location.to_string_lossy().to_string().into());
        ui.global::<AppGlobals>().set_quality(settings.quality as i32);
        ui.global::<AppGlobals>().set_resolution(settings.resolution as i32);
        let system_threads = num_cpus::get().max(1) as i32;
        let initial_threads = settings
            .threads_number
            .max(1)
            .min(system_threads as usize) as i32;
        ui.global::<AppGlobals>().set_max_threads(system_threads);
        ui.global::<AppGlobals>().set_threads(initial_threads);
        ui.global::<AppGlobals>().set_max_width(settings.max_width as i32);
        ui.global::<AppGlobals>().set_max_height(settings.max_height as i32);
        ui.global::<AppGlobals>().set_resize_mode(match settings.resize_mode {
            ResizeMode::Percent => "percent".into(),
            ResizeMode::Max => "max".into(),
        });
        ui.global::<AppGlobals>().set_max_res_preset(preset_from_dimensions(
            settings.max_width,
            settings.max_height,
        ));
        ui.global::<AppGlobals>().set_resize_filter(match settings.resize_filter {
            ResizeFilter::Nearest => "nearest".into(),
            ResizeFilter::Triangle => "triangle".into(),
            ResizeFilter::CatmullRom => "catmullrom".into(),
            ResizeFilter::Lanczos3 => "lanczos3".into(),
        });
        
        if let Some(fmt) = settings.format {
            ui.global::<AppGlobals>().set_format(fmt.to_string().into());
        }
        
        let ui_weak = ui.as_weak();
        let files_arc = Arc::new(RwLock::new(Vec::new()));
        let files_arc_clone = files_arc.clone();
        let failed_files = Arc::new(RwLock::new(Vec::new()));
        
        let app_state = AppStateInternal {
            config: ImageProcessorConfig {
                output_path: settings.save_location,
                quality: settings.quality,
                resolution_percent: settings.resolution,
                threads: initial_threads as usize,
                format: settings.format.unwrap_or(ImageFormat::Jpg),
                preserve_structure: false,
                resize_mode: settings.resize_mode,
                max_width: settings.max_width,
                max_height: settings.max_height,
                preserve_aspect: true,
                resize_filter: settings.resize_filter,
            },
            files: files_arc_clone,
            failed_files: failed_files.clone(),
            completed_files: Arc::new(RwLock::new(0)),
            total_bytes_before: Arc::new(RwLock::new(0)),
            total_bytes_after: Arc::new(RwLock::new(0)),
            processed_input_bytes: Arc::new(RwLock::new(0)),
            start_time: Arc::new(RwLock::new(None)),
        };
        
        let ui_weak_clone = ui_weak.clone();
        let app_state_clone = app_state.clone();
        
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_token_start = cancel_token.clone();
        ui.global::<AppGlobals>().on_start_conversion(move || {
            let ui = ui_weak_clone.unwrap();
            let app_state = app_state_clone.clone();
            let cancel_token = cancel_token_start.clone();
            
            if let Err(e) = start_conversion(ui, app_state, cancel_token) {
                eprintln!("Conversion error: {}", e);
            }
        });
        
        let cancel_token_clone = cancel_token.clone();
        
        ui.global::<AppGlobals>().on_cancel_conversion(move || {
            if let Err(e) = cancel_conversion(cancel_token_clone.clone()) {
                eprintln!("Cancel error: {}", e);
            }
        });
        
        ui.window().on_close_requested({
            let cancel_token_clone = cancel_token.clone();
            let ui_close = ui.as_weak();
            move || {
                // Signal cancellation to any running conversion task.
                cancel_token_clone.store(true, Ordering::Relaxed);
                if let Some(ui) = ui_close.upgrade() {
                    if let Err(err) = save_settings_from_ui(&ui) {
                        eprintln!("Failed to save settings: {}", err);
                    }
                }
                slint::CloseRequestResponse::HideWindow
            }
        });
        
        let ui_weak_browse = ui_weak.clone();
        
        ui.global::<AppGlobals>().on_browse_folder(move || {
            let ui = ui_weak_browse.unwrap();
            
            let folder = rfd::FileDialog::new()
                .pick_folder();
            
            if let Some(path) = folder {
                ui.global::<AppGlobals>().set_output_path(path.to_string_lossy().to_string().into());
            }
        });
        
        let files_clear = app_state.files.clone();
        let failed_files_clear = app_state.failed_files.clone();
        let ui_clear = ui_weak.clone();
        
        ui.global::<AppGlobals>().on_clear_files(move || {
            if let Ok(mut files) = files_clear.try_write() {
                files.clear();
            }
            if let Ok(mut failed) = failed_files_clear.try_write() {
                failed.clear();
            }
            
            let ui = ui_clear.unwrap();
            ui.global::<AppGlobals>().set_num_files_text("0".into());
            ui.global::<AppGlobals>().set_before_size_text("0.000".into());
            ui.global::<AppGlobals>().set_after_size_text("0.000".into());
            ui.global::<AppGlobals>().set_completed_files_text("0".into());
            ui.global::<AppGlobals>().set_mb_per_sec_text("0.00".into());
            ui.global::<AppGlobals>().set_percent_saved_text("0.00".into());
            ui.global::<AppGlobals>().set_elapsed_time_text("0.00".into());
            ui.global::<AppGlobals>().set_progress(0.0);
            ui.global::<AppGlobals>().set_file_rows(ModelRc::new(VecModel::from(vec![])));
            ui.global::<AppGlobals>().set_failed_file_rows(ModelRc::new(VecModel::from(vec![])));
        });

        let ui_optimal = ui_weak.clone();
        ui.global::<AppGlobals>().on_optimal_settings(move || {
            let ui = ui_optimal.unwrap();
            ui.global::<AppGlobals>().set_quality(85);
            ui.global::<AppGlobals>().set_format("webp".into());
            ui.global::<AppGlobals>().set_resize_mode("max".into());
            ui.global::<AppGlobals>().set_max_width(3840);
            ui.global::<AppGlobals>().set_max_height(2160);
            ui.global::<AppGlobals>().set_max_res_preset("4k".into());
            ui.global::<AppGlobals>().set_resize_filter("catmullrom".into());
            ui.global::<AppGlobals>().set_resolution(100);
            if let Err(err) = save_settings_from_ui(&ui) {
                eprintln!("Failed to save settings: {}", err);
            }
        });

        let ui_preset = ui_weak.clone();
        ui.global::<AppGlobals>().on_set_max_res_preset(move |preset| {
            let ui = ui_preset.unwrap();
            apply_preset_to_ui(&ui, &preset);
            if let Err(err) = save_settings_from_ui(&ui) {
                eprintln!("Failed to save settings: {}", err);
            }
        });

        let app_state_drop = app_state.clone();
        let ui_drop = ui.as_weak();
        ui.window().on_winit_window_event(move |_window, event| {
            match event {
                winit::event::WindowEvent::DroppedFile(path) => {
                    let app_state_drop = app_state_drop.clone();
                    let ui_drop = ui_drop.clone();
                    let path = path.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_dropped_path_async(app_state_drop, ui_drop, path).await {
                            eprintln!("Drop handling error: {}", err);
                        }
                    });
                    EventResult::PreventDefault
                }
                winit::event::WindowEvent::HoveredFile(_)
                | winit::event::WindowEvent::HoveredFileCancelled => EventResult::PreventDefault,
                _ => EventResult::Propagate,
            }
        });

        let files_open = app_state.files.clone();
        let failed_open = app_state.failed_files.clone();
        ui.global::<AppGlobals>().on_open_file(move |list, index| {
            if index < 0 {
                return;
            }
            let idx = index as usize;
            let list = list.to_string();

            let path = match list.as_str() {
                "files" => files_open
                    .try_read()
                    .ok()
                    .and_then(|files| files.get(idx).map(|f| f.input_path.clone())),
                "failed" => failed_open
                    .try_read()
                    .ok()
                    .and_then(|files| files.get(idx).map(|f| f.input_path.clone())),
                _ => None,
            };

            if let Some(path) = path {
                if let Err(err) = open_in_default_app(&path) {
                    eprintln!("Failed to open file {:?}: {}", path, err);
                }
            }
        });
        
        Ok(Self {
            ui,
            app_state,
            cancellation_token: cancel_token,
        })
    }
    
    pub async fn run(self) -> Result<()> {
        self.ui.run()?;
        Ok(())
    }
}

fn start_conversion(
    ui: MainWindow,
    app_state: AppStateInternal,
    cancel_token: Arc<AtomicBool>,
) -> Result<()> {
    if ui.global::<AppGlobals>().get_is_converting() {
        return Ok(());
    }
    let config = build_config_from_ui(&ui)?;
    save_settings_from_ui(&ui)?;
    // Use try_read to avoid blocking the UI thread
    let files = app_state.files.try_read().map(|f| f.clone()).context("Failed to read files list")?;
    
    if files.is_empty() {
        return Ok(());
    }
    cancel_token.store(false, Ordering::Relaxed);
    let num_files = files.len() as i32;
    ui.global::<AppGlobals>().set_num_files_text(num_files.to_string().into());
    ui.global::<AppGlobals>().set_completed_files_text("0".into());
    ui.global::<AppGlobals>().set_after_size_text("0.000".into());
    ui.global::<AppGlobals>().set_mb_per_sec_text("0.00".into());
    ui.global::<AppGlobals>().set_percent_saved_text("0.00".into());
    ui.global::<AppGlobals>().set_elapsed_time_text("0.00".into());
    ui.global::<AppGlobals>().set_progress(0.0);
    ui.global::<AppGlobals>().set_failed_file_rows(ModelRc::new(VecModel::from(vec![])));
    if let Ok(mut failed) = app_state.failed_files.try_write() {
        failed.clear();
    }
    ui.global::<AppGlobals>().set_is_converting(true);

    const UI_UPDATE_INTERVAL_MS: u64 = 250;
    let total_files = num_files as u64;
    let total_files_f = total_files as f64;
    let ui_tick = Instant::now();
    let last_ui_update = Arc::new(AtomicU64::new(0));
    
    let mut total_before = 0u64;
    for file in &files {
        if let Ok(size) = get_file_size(&file.input_path) {
            total_before += size;
        }
    }
    
    let before_mb = (total_before as f64) / (1024.0 * 1024.0);
    ui.global::<AppGlobals>().set_before_size_text(format!("{:.3}", before_mb).into());
    
    let threads = config.threads;
    let cancel_token_worker = cancel_token.clone();
    
    let config = config.clone();
    let ui_weak = ui.as_weak();
    
    let completed_files = app_state.completed_files.clone();
    let total_bytes_before = app_state.total_bytes_before.clone();
    let total_bytes_after = app_state.total_bytes_after.clone();
    let processed_input_bytes = app_state.processed_input_bytes.clone();
    let start_time = app_state.start_time.clone();
    let failed_files = app_state.failed_files.clone();
    let last_ui_update_worker = last_ui_update.clone();
    
    let rt = tokio::runtime::Handle::current();
    
    if let Ok(mut st) = start_time.try_write() {
        *st = Some(Instant::now());
    }

    // Initialize total_bytes_before
    if let Ok(mut before) = total_bytes_before.try_write() {
        *before = total_before;
    }
    if let Ok(mut after) = total_bytes_after.try_write() {
        *after = 0;
    }
    if let Ok(mut processed) = processed_input_bytes.try_write() {
        *processed = 0;
    }
    if let Ok(mut completed) = completed_files.try_write() {
        *completed = 0;
    }
    
    rt.spawn(async move {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(threads));
        let mut join_set = tokio::task::JoinSet::new();

        for file in files {
            if cancel_token_worker.load(Ordering::Relaxed) {
                break;
            }

            let permit = semaphore.clone().acquire_owned().await;
            if permit.is_err() {
                break;
            }
            let permit = permit.unwrap();

            let config = config.clone();
            let cancel_token = cancel_token_worker.clone();
            let ui_weak = ui_weak.clone();
            let completed_files = completed_files.clone();
            let total_bytes_before = total_bytes_before.clone();
            let total_bytes_after = total_bytes_after.clone();
            let processed_input_bytes = processed_input_bytes.clone();
            let start_time = start_time.clone();
            let failed_files = failed_files.clone();
            let total_files = total_files;
            let total_files_f = total_files_f;
            let ui_tick = ui_tick;
            let last_ui_update = last_ui_update_worker.clone();

            join_set.spawn(async move {
                let _permit = permit;
                if cancel_token.load(Ordering::Relaxed) {
                    return;
                }

                let file_for_error = file.clone();
                let result = tokio::task::spawn_blocking(move || process_file(&file, &config)).await;

                match result {
                    Ok(Ok(result)) => {
                        let mut completed = completed_files.write().await;
                        *completed += 1;

                        let mut after = total_bytes_after.write().await;
                        *after += result.output_size;

                        let mut processed = processed_input_bytes.write().await;
                        *processed += result.input_size;

                        let completed_count = *completed;
                        let total_after = *after;
                        let total_processed = *processed;
                        let elapsed = start_time.read().await.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                        let total_before = *total_bytes_before.read().await;

                        let now_ms = ui_tick.elapsed().as_millis() as u64;
                        let should_update = completed_count >= total_files
                            || should_update_ui(&last_ui_update, now_ms, UI_UPDATE_INTERVAL_MS);

                        if should_update {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                if total_files_f > 0.0 {
                                    let progress = (completed_count as f64 / total_files_f) * 100.0;
                                    ui.global::<AppGlobals>().set_progress(progress as f32);

                                    ui.global::<AppGlobals>().set_completed_files_text(completed_count.to_string().into());

                                let after_mb = (total_after as f64) / (1024.0 * 1024.0);
                                ui.global::<AppGlobals>().set_after_size_text(format!("{:.3}", after_mb).into());

                                ui.global::<AppGlobals>().set_elapsed_time_text(format!("{:.2}", elapsed).into());

                                if elapsed > 0.0 {
                                    let mb_per_sec = (total_processed as f64) / (1024.0 * 1024.0) / elapsed;
                                    ui.global::<AppGlobals>().set_mb_per_sec_text(format!("{:.2}", mb_per_sec).into());
                                }

                                    let before_bytes = total_before as f64;
                                    if before_bytes > 0.0 {
                                        let percent_saved = (1.0 - (total_after as f64) / before_bytes) * 100.0;
                                        ui.global::<AppGlobals>().set_percent_saved_text(format!("{:.2}", percent_saved).into());
                                    }
                                }
                            });
                        }
                    }
                    Ok(Err(e)) => {
                        let mut completed = completed_files.write().await;
                        *completed += 1;
                        let completed_count = *completed;

                        let total_after = *total_bytes_after.read().await;
                        let total_processed = *processed_input_bytes.read().await;
                        let elapsed = start_time.read().await.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                        let total_before = *total_bytes_before.read().await;

                        let failed_snapshot = {
                            let mut failed = failed_files.write().await;
                            failed.push(FailedFile {
                                input_path: file_for_error.input_path.clone(),
                                error: e.to_string(),
                            });
                            failed.clone()
                        };

                        let now_ms = ui_tick.elapsed().as_millis() as u64;
                        let should_update = completed_count >= total_files
                            || should_update_ui(&last_ui_update, now_ms, UI_UPDATE_INTERVAL_MS);

                        if should_update {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.global::<AppGlobals>().set_failed_file_rows(build_failed_rows(&failed_snapshot));

                                if total_files_f > 0.0 {
                                    let progress = (completed_count as f64 / total_files_f) * 100.0;
                                    ui.global::<AppGlobals>().set_progress(progress as f32);
                                    ui.global::<AppGlobals>().set_completed_files_text(completed_count.to_string().into());

                                let after_mb = (total_after as f64) / (1024.0 * 1024.0);
                                ui.global::<AppGlobals>().set_after_size_text(format!("{:.3}", after_mb).into());
                                ui.global::<AppGlobals>().set_elapsed_time_text(format!("{:.2}", elapsed).into());

                                if elapsed > 0.0 {
                                    let mb_per_sec = (total_processed as f64) / (1024.0 * 1024.0) / elapsed;
                                    ui.global::<AppGlobals>().set_mb_per_sec_text(format!("{:.2}", mb_per_sec).into());
                                }

                                    let before_bytes = total_before as f64;
                                    if before_bytes > 0.0 {
                                        let percent_saved = (1.0 - (total_after as f64) / before_bytes) * 100.0;
                                        ui.global::<AppGlobals>().set_percent_saved_text(format!("{:.2}", percent_saved).into());
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        let mut completed = completed_files.write().await;
                        *completed += 1;
                        let completed_count = *completed;

                        let total_after = *total_bytes_after.read().await;
                        let total_processed = *processed_input_bytes.read().await;
                        let elapsed = start_time.read().await.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                        let total_before = *total_bytes_before.read().await;

                        let failed_snapshot = {
                            let mut failed = failed_files.write().await;
                            failed.push(FailedFile {
                                input_path: file_for_error.input_path.clone(),
                                error: format!("Task failed: {}", e),
                            });
                            failed.clone()
                        };

                        let now_ms = ui_tick.elapsed().as_millis() as u64;
                        let should_update = completed_count >= total_files
                            || should_update_ui(&last_ui_update, now_ms, UI_UPDATE_INTERVAL_MS);

                        if should_update {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.global::<AppGlobals>().set_failed_file_rows(build_failed_rows(&failed_snapshot));

                                if total_files_f > 0.0 {
                                    let progress = (completed_count as f64 / total_files_f) * 100.0;
                                    ui.global::<AppGlobals>().set_progress(progress as f32);
                                    ui.global::<AppGlobals>().set_completed_files_text(completed_count.to_string().into());

                                let after_mb = (total_after as f64) / (1024.0 * 1024.0);
                                ui.global::<AppGlobals>().set_after_size_text(format!("{:.3}", after_mb).into());
                                ui.global::<AppGlobals>().set_elapsed_time_text(format!("{:.2}", elapsed).into());

                                if elapsed > 0.0 {
                                    let mb_per_sec = (total_processed as f64) / (1024.0 * 1024.0) / elapsed;
                                    ui.global::<AppGlobals>().set_mb_per_sec_text(format!("{:.2}", mb_per_sec).into());
                                }

                                    let before_bytes = total_before as f64;
                                    if before_bytes > 0.0 {
                                        let percent_saved = (1.0 - (total_after as f64) / before_bytes) * 100.0;
                                        ui.global::<AppGlobals>().set_percent_saved_text(format!("{:.2}", percent_saved).into());
                                    }
                                }
                            });
                        }
                    }
                }
            });
        }

        while let Some(res) = join_set.join_next().await {
            if let Err(e) = res {
                eprintln!("Worker task error: {}", e);
            }
        }
        
        let elapsed = {
            let st = start_time.read().await;
            st.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0)
        };

        let completed_count = *completed_files.read().await;
        let total_after = *total_bytes_after.read().await;
        let total_processed = *processed_input_bytes.read().await;
        let total_before = *total_bytes_before.read().await;
        let failed_snapshot = failed_files.read().await.clone();

        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            if total_files_f > 0.0 {
                let progress = (completed_count as f64 / total_files_f) * 100.0;
                ui.global::<AppGlobals>().set_progress(progress as f32);
                ui.global::<AppGlobals>().set_completed_files_text(completed_count.to_string().into());

                let after_mb = (total_after as f64) / (1024.0 * 1024.0);
                ui.global::<AppGlobals>().set_after_size_text(format!("{:.3}", after_mb).into());
                ui.global::<AppGlobals>().set_elapsed_time_text(format!("{:.2}", elapsed).into());

                if elapsed > 0.0 {
                    let mb_per_sec = (total_processed as f64) / (1024.0 * 1024.0) / elapsed;
                    ui.global::<AppGlobals>().set_mb_per_sec_text(format!("{:.2}", mb_per_sec).into());
                }

                let before_bytes = total_before as f64;
                if before_bytes > 0.0 {
                    let percent_saved = (1.0 - (total_after as f64) / before_bytes) * 100.0;
                    ui.global::<AppGlobals>().set_percent_saved_text(format!("{:.2}", percent_saved).into());
                }
            }

            ui.global::<AppGlobals>().set_failed_file_rows(build_failed_rows(&failed_snapshot));
            ui.global::<AppGlobals>().set_is_converting(false);
        });
    });
    
    Ok(())
}

fn cancel_conversion(cancel_token: Arc<AtomicBool>) -> Result<()> {
    cancel_token.store(true, Ordering::Relaxed);
    Ok(())
}

fn should_update_ui(last_update_ms: &AtomicU64, now_ms: u64, interval_ms: u64) -> bool {
    loop {
        let last = last_update_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < interval_ms {
            return false;
        }
        if last_update_ms
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

fn build_config_from_ui(ui: &MainWindow) -> Result<ImageProcessorConfig> {
    let globals = ui.global::<AppGlobals>();
    let output_path = PathBuf::from(globals.get_output_path().to_string());
    let quality = globals.get_quality().clamp(0, 100) as u8;
    let resolution_percent = globals.get_resolution().clamp(1, 100) as u32;
    let max_threads = globals.get_max_threads().max(1);
    let threads = globals.get_threads().clamp(1, max_threads) as usize;
    let format = ImageFormat::from_str(globals.get_format().as_str()).unwrap_or(ImageFormat::Jpg);
    let resize_mode = match globals.get_resize_mode().as_str() {
        "max" => ResizeMode::Max,
        _ => ResizeMode::Percent,
    };
    let max_width = globals.get_max_width().max(1) as u32;
    let max_height = globals.get_max_height().max(1) as u32;
    let preserve_aspect = true;
    let resize_filter = match globals.get_resize_filter().as_str() {
        "nearest" => ResizeFilter::Nearest,
        "triangle" => ResizeFilter::Triangle,
        "lanczos3" => ResizeFilter::Lanczos3,
        _ => ResizeFilter::CatmullRom,
    };

    Ok(ImageProcessorConfig {
        output_path,
        quality,
        resolution_percent,
        threads,
        format,
        preserve_structure: false,
        resize_mode,
        max_width,
        max_height,
        preserve_aspect,
        resize_filter,
    })
}

fn save_settings_from_ui(ui: &MainWindow) -> Result<()> {
    let globals = ui.global::<AppGlobals>();
    let settings = AppSettings {
        save_location: PathBuf::from(globals.get_output_path().to_string()),
        threads_number: globals
            .get_threads()
            .clamp(1, globals.get_max_threads().max(1)) as usize,
        resolution: globals.get_resolution().clamp(1, 100) as u32,
        quality: globals.get_quality().clamp(0, 100) as u8,
        format: ImageFormat::from_str(globals.get_format().as_str()),
        resize_mode: match globals.get_resize_mode().as_str() {
            "max" => ResizeMode::Max,
            _ => ResizeMode::Percent,
        },
        max_width: globals.get_max_width().max(1) as u32,
        max_height: globals.get_max_height().max(1) as u32,
        preserve_aspect: true,
        resize_filter: match globals.get_resize_filter().as_str() {
            "nearest" => ResizeFilter::Nearest,
            "triangle" => ResizeFilter::Triangle,
            "lanczos3" => ResizeFilter::Lanczos3,
            _ => ResizeFilter::CatmullRom,
        },
    };
    settings.save()
}

fn apply_preset_to_ui(ui: &MainWindow, preset: &str) {
    let (w, h) = match preset {
        "1440p" => (2560, 1440),
        "1080p" => (1920, 1080),
        _ => (3840, 2160),
    };
    ui.global::<AppGlobals>().set_max_width(w);
    ui.global::<AppGlobals>().set_max_height(h);
    ui.global::<AppGlobals>().set_max_res_preset(preset.into());
}

fn preset_from_dimensions(width: u32, height: u32) -> slint::SharedString {
    match (width, height) {
        (2560, 1440) => "1440p".into(),
        (1920, 1080) => "1080p".into(),
        _ => "4k".into(),
    }
}

async fn handle_dropped_path_async(
    app_state: AppStateInternal,
    ui_weak: slint::Weak<MainWindow>,
    path: PathBuf,
) -> Result<()> {
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        let globals = ui.global::<AppGlobals>();
        globals.set_is_scanning_drop(true);
        globals.set_drop_progress(0.0);
        globals.set_drop_status_text("Scanning files...".into());
    });

    let scanning_flag = Arc::new(AtomicBool::new(true));
    let scanning_flag_pulse = scanning_flag.clone();
    let ui_pulse = ui_weak.clone();
    tokio::spawn(async move {
        let mut value = 0.0f32;
        while scanning_flag_pulse.load(Ordering::Relaxed) {
            value = if value >= 95.0 { 5.0 } else { value + 5.0 };
            let _ = ui_pulse.upgrade_in_event_loop(move |ui| {
                ui.global::<AppGlobals>().set_drop_progress(value);
            });
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });

    let new_files = tokio::task::spawn_blocking(move || collect_dropped_files(path))
        .await
        .context("Failed to join drop scan task")??;

    scanning_flag.store(false, Ordering::Relaxed);

    if new_files.is_empty() {
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            let globals = ui.global::<AppGlobals>();
            globals.set_is_scanning_drop(false);
            globals.set_drop_progress(0.0);
            globals.set_drop_status_text("".into());
        });
        return Ok(());
    }

    let files_snapshot = {
        let mut files = app_state
            .files
            .write()
            .await;
        files.extend(new_files);
        files.clone()
    };

    let num_files = files_snapshot.len() as i32;
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        let globals = ui.global::<AppGlobals>();
        globals.set_num_files_text(num_files.to_string().into());
        globals.set_drop_status_text("Calculating sizes...".into());
        globals.set_drop_progress(0.0);
    });

    let total_files = files_snapshot.len().max(1);
    let processed = Arc::new(AtomicUsize::new(0));
    let processed_clone = processed.clone();
    let progress_flag = Arc::new(AtomicBool::new(true));
    let progress_flag_update = progress_flag.clone();
    let ui_progress = ui_weak.clone();
    tokio::spawn(async move {
        while progress_flag_update.load(Ordering::Relaxed) {
            let done = processed_clone.load(Ordering::Relaxed) as f32;
            let pct = (done / total_files as f32) * 100.0;
            let _ = ui_progress.upgrade_in_event_loop(move |ui| {
                ui.global::<AppGlobals>().set_drop_progress(pct.min(100.0));
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    let (rows_data, total_before) = tokio::task::spawn_blocking(move || {
        build_rows_data_with_progress(&files_snapshot, processed.as_ref())
    })
    .await
    .context("Failed to join file size task")?;

    progress_flag.store(false, Ordering::Relaxed);

    {
        let mut before_guard = app_state.total_bytes_before.write().await;
        *before_guard = total_before;
    }

    let before_mb = (total_before as f64) / (1024.0 * 1024.0);
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.global::<AppGlobals>().set_num_files_text(num_files.to_string().into());
        ui.global::<AppGlobals>().set_before_size_text(format!("{:.3}", before_mb).into());
        ui.global::<AppGlobals>().set_file_rows(build_file_rows_from_sizes(&rows_data));
        let globals = ui.global::<AppGlobals>();
        globals.set_drop_progress(100.0);
        globals.set_drop_status_text("".into());
        globals.set_is_scanning_drop(false);
    });

    Ok(())
}

fn collect_dropped_files(path: PathBuf) -> Result<Vec<QueuedFile>> {
    let mut new_files = Vec::new();

    if path.is_dir() {
        new_files.extend(scan_directory(&path)?);
    } else if path.is_file() && validate_file_format(&path) {
        let relative = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        new_files.push(QueuedFile {
            input_path: path,
            relative_path: relative,
        });
    }

    Ok(new_files)
}

fn build_rows_data_with_progress(files: &[QueuedFile], processed: &AtomicUsize) -> (Vec<(String, u64)>, u64) {
    let mut rows = Vec::with_capacity(files.len());
    let mut total_before = 0u64;
    for (idx, file) in files.iter().enumerate() {
        let size = get_file_size(&file.input_path).unwrap_or(0);
        total_before += size;
        rows.push((file.input_path.to_string_lossy().to_string(), size));
        processed.store(idx + 1, Ordering::Relaxed);
    }
    (rows, total_before)
}

fn build_file_rows_from_sizes(rows_data: &[(String, u64)]) -> ModelRc<ModelRc<StandardListViewItem>> {
    let mut rows: Vec<ModelRc<StandardListViewItem>> = Vec::with_capacity(rows_data.len());
    for (path, size) in rows_data {
        let row = VecModel::from(vec![
            StandardListViewItem::from(path.as_str()),
            StandardListViewItem::from(slint::SharedString::from(format_bytes(*size))),
        ]);
        rows.push(ModelRc::new(row));
    }
    ModelRc::new(VecModel::from(rows))
}

fn build_failed_rows(files: &[FailedFile]) -> ModelRc<ModelRc<StandardListViewItem>> {
    let mut rows: Vec<ModelRc<StandardListViewItem>> = Vec::new();
    for file in files {
        let row = VecModel::from(vec![
            StandardListViewItem::from(file.input_path.to_string_lossy().as_ref()),
            StandardListViewItem::from(file.error.as_str()),
        ]);
        rows.push(ModelRc::new(row));
    }
    ModelRc::new(VecModel::from(rows))
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn open_in_default_app(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
            .context("Failed to open file with default app")?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .context("Failed to open file with default app")?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .context("Failed to open file with default app")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Ok(())
}
