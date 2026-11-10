// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;
mod channels;
mod editor;
mod mock;
mod nix;
mod nix_edit;
mod nixos_search;
mod options;
mod runner;
mod search;
mod settings;
mod snapshot;
mod text;
mod workspace;

use std::sync::Arc;

slint::include_modules!();

/// Settings and the mock sample file live here; it's the identifier the Tauri
/// build used, so existing settings carry over.
const APP_ID: &str = "dev.adham.hmgui";

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let config_dir = dirs::config_dir().unwrap_or_default().join(APP_ID);
    let data_dir = dirs::data_dir().unwrap_or_default().join(APP_ID);
    let state = Arc::new(backend::AppState::new(
        config_dir,
        data_dir,
        runtime.handle().clone(),
    )?);
    app::run(state, runtime.handle().clone())?;
    Ok(())
}
