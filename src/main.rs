#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod file_handler;
mod format_handlers;
mod image_processor;
mod settings;
mod updater;

slint::include_modules!();

use anyhow::{Context, Result};
use app::App;
use tokio::runtime::Runtime;

fn main() -> Result<()> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        if let Err(err) = updater::update_if_available().await {
            eprintln!("Update check failed: {err:#}");
        } else if updater::restart_started() {
            return Ok(());
        }

        let app = App::new().await?;

        app.run().await.context("Application error")
    })
}
