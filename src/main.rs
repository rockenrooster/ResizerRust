#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod image_processor;
mod format_handlers;
mod file_handler;
mod settings;

slint::include_modules!();

use anyhow::{Context, Result};
use app::App;
use tokio::runtime::Runtime;

fn main() -> Result<()> {
    let rt = Runtime::new()?;
    
    rt.block_on(async {
        let app = App::new().await?;
        
        app.run().await.context("Application error")
    })
}
