mod channels;
mod commands;
mod mock;
mod nix;
mod nix_edit;
mod options;
mod runner;
mod settings;
mod workspace;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::options::HmOption;
use crate::runner::RunLock;
use crate::settings::Settings;
use crate::workspace::WorkingCopy;

pub struct AppState {
    settings_path: PathBuf,
    mock_file: PathBuf,
    settings: Mutex<Settings>,
    working: Mutex<Option<WorkingCopy>>,
    /// Parsed options, keyed by where they came from.
    options: Mutex<Option<(String, Vec<HmOption>)>>,
    mock: Mutex<mock::MockState>,
    run_lock: Arc<RunLock>,
}

impl AppState {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Runs `f` on the working copy of the configured file, loading it first if
    /// needed. A clean copy is reloaded when the file changes on disk.
    fn with_working<T>(
        &self,
        f: impl FnOnce(&mut WorkingCopy) -> Result<T, String>,
    ) -> Result<T, String> {
        let path = self.settings().config_path();
        let mut working = self.working.lock().unwrap();
        let stale = match working.as_ref() {
            None => true,
            Some(wc) => wc.path != path || (!wc.dirty() && wc.changed_on_disk()),
        };
        if stale {
            *working = Some(WorkingCopy::load(&path).map_err(|e| format!("{e:#}"))?);
        }
        f(working.as_mut().unwrap())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let mock_file = app.path().app_data_dir()?.join("mock").join("home.nix");
            if !mock_file.exists() {
                std::fs::create_dir_all(mock_file.parent().unwrap())?;
                std::fs::write(&mock_file, mock::SAMPLE_HOME)?;
            }
            let settings_path = config_dir.join("settings.json");
            let settings = Settings::load(&settings_path, &mock_file);
            app.manage(AppState {
                settings_path,
                mock_file,
                settings: Mutex::new(settings),
                working: Mutex::new(None),
                options: Mutex::new(None),
                mock: Mutex::new(mock::MockState::default()),
                run_lock: Arc::new(RunLock::default()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_settings,
            commands::save_settings,
            commands::default_config_file,
            commands::load_options,
            commands::get_file_overview,
            commands::get_option_in_file,
            commands::stage_option,
            commands::stage_remove_option,
            commands::get_packages,
            commands::stage_add_package,
            commands::stage_remove_package,
            commands::search_packages,
            commands::get_changes,
            commands::save_changes,
            commands::discard_changes,
            commands::eval_option,
            commands::start_run,
            commands::list_generations,
            commands::activate_generation,
            commands::list_channels,
            commands::add_channel,
            commands::remove_channel,
            commands::start_channel_update,
            commands::rollback_channels,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
