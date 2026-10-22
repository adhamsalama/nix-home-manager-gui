//! Tauri commands: the API the React frontend calls with `invoke`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::channels::{self, Channels};
use crate::nix::{self, Generation, PackageInfo};
use crate::nix_edit::{self, FileValue, Packages};
use crate::options::HmOption;
use crate::runner;
use crate::settings::{Mode, Settings};
use crate::workspace::DiffLine;
use crate::{AppState, mock};

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    mode: Mode,
    config_file: String,
    nix_available: bool,
    home_manager_available: bool,
    dirty: bool,
    /// Why the config file can't be loaded or edited, if it can't.
    file_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOverview {
    path: String,
    dirty: bool,
    defined_paths: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    path: String,
    dirty: bool,
    diff: Vec<DiffLine>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StagedValue {
    /// A plain value from a form widget, rendered to Nix by the backend.
    Json { value: serde_json::Value },
    /// A Nix expression typed by the user.
    Raw { expr: String },
}

#[tauri::command]
pub fn get_status(state: State<AppState>) -> Status {
    let settings = state.settings();
    let file = state.with_working(|wc| Ok((wc.dirty(), nix_edit::parse(&wc.current).err())));
    let (dirty, file_error) = match file {
        Ok((dirty, parse_error)) => (dirty, parse_error.map(err)),
        Err(e) => (false, Some(e)),
    };
    Status {
        mode: settings.mode,
        config_file: settings.config_path().display().to_string(),
        nix_available: nix::nix_available(),
        home_manager_available: nix::home_manager_available(),
        dirty,
        file_error,
    }
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.settings()
}

#[tauri::command]
pub fn save_settings(state: State<AppState>, settings: Settings) -> CmdResult<()> {
    let old = state.settings();
    if old.config_path() != settings.config_path() {
        let mut working = state.working.lock().unwrap();
        if working.as_ref().is_some_and(|wc| wc.dirty()) {
            return Err("save or discard your pending changes before switching files".into());
        }
        *working = None;
    }
    settings.save(&state.settings_path).map_err(err)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

/// The suggested config file for a mode, used when the user switches modes.
#[tauri::command]
pub fn default_config_file(state: State<AppState>, mode: Mode) -> String {
    match mode {
        Mode::Mock => state.mock_file.display().to_string(),
        Mode::Real => crate::settings::default_real_config_file(),
    }
}

#[tauri::command]
pub async fn load_options(state: State<'_, AppState>, refresh: bool) -> CmdResult<Vec<HmOption>> {
    let settings = state.settings();
    let key = match settings.mode {
        Mode::Mock => "mock".to_string(),
        Mode::Real => format!(
            "{}|{}|{}",
            settings.options_json, settings.flake_ref, settings.home_manager_flake
        ),
    };
    if !refresh
        && let Some((cached_key, options)) = state.options.lock().unwrap().as_ref()
        && *cached_key == key
    {
        return Ok(options.clone());
    }
    let options = match settings.mode {
        Mode::Mock => mock::options(),
        Mode::Real => nix::load_options(&settings).await,
    }
    .map_err(err)?;
    *state.options.lock().unwrap() = Some((key, options.clone()));
    Ok(options)
}

#[tauri::command]
pub fn get_file_overview(state: State<AppState>) -> CmdResult<FileOverview> {
    state.with_working(|wc| {
        Ok(FileOverview {
            path: wc.path.display().to_string(),
            dirty: wc.dirty(),
            defined_paths: nix_edit::defined_paths(&wc.current).map_err(err)?,
        })
    })
}

#[tauri::command]
pub fn get_option_in_file(state: State<AppState>, loc: Vec<String>) -> CmdResult<FileValue> {
    state.with_working(|wc| nix_edit::get_option(&wc.current, &loc).map_err(err))
}

#[tauri::command]
pub fn stage_option(state: State<AppState>, loc: Vec<String>, value: StagedValue) -> CmdResult<()> {
    let expr = match value {
        StagedValue::Json { value } => nix_edit::to_nix(&value),
        StagedValue::Raw { expr } => expr.trim().to_string(),
    };
    state.with_working(|wc| {
        wc.stage(|t| nix_edit::set_option(t, &loc, &expr))
            .map_err(err)
    })
}

#[tauri::command]
pub fn stage_remove_option(state: State<AppState>, loc: Vec<String>) -> CmdResult<()> {
    state.with_working(|wc| wc.stage(|t| nix_edit::remove_option(t, &loc)).map_err(err))
}

#[tauri::command]
pub fn get_packages(state: State<AppState>) -> CmdResult<Packages> {
    state.with_working(|wc| nix_edit::get_packages(&wc.current).map_err(err))
}

#[tauri::command]
pub fn stage_add_package(state: State<AppState>, name: String) -> CmdResult<()> {
    state.with_working(|wc| wc.stage(|t| nix_edit::add_package(t, &name)).map_err(err))
}

#[tauri::command]
pub fn stage_remove_package(state: State<AppState>, name: String) -> CmdResult<()> {
    state.with_working(|wc| {
        wc.stage(|t| nix_edit::remove_package(t, &name))
            .map_err(err)
    })
}

#[tauri::command]
pub async fn search_packages(
    state: State<'_, AppState>,
    query: String,
) -> CmdResult<Vec<PackageInfo>> {
    let query = query.trim().to_string();
    if query.len() < 2 {
        return Err("type at least two characters".into());
    }
    if query.starts_with('-') {
        return Err("search terms can't start with '-'".into());
    }
    match state.settings().mode {
        Mode::Mock => mock::search_packages(&query),
        Mode::Real => nix::search_packages(&query).await,
    }
    .map_err(err)
}

#[tauri::command]
pub fn get_changes(state: State<AppState>) -> CmdResult<Changes> {
    state.with_working(|wc| {
        Ok(Changes {
            path: wc.path.display().to_string(),
            dirty: wc.dirty(),
            diff: wc.diff(),
        })
    })
}

#[tauri::command]
pub fn save_changes(state: State<AppState>) -> CmdResult<()> {
    state.with_working(|wc| wc.save().map_err(err))
}

#[tauri::command]
pub fn discard_changes(state: State<AppState>) -> CmdResult<()> {
    state.with_working(|wc| {
        wc.discard();
        Ok(())
    })?;
    // Drop the working copy so the next access re-reads the file from disk.
    *state.working.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub async fn eval_option(
    state: State<'_, AppState>,
    loc: Vec<String>,
    default: Option<String>,
) -> CmdResult<String> {
    let settings = state.settings();
    match settings.mode {
        Mode::Mock => {
            let text = state.with_working(|wc| Ok(wc.current.clone()))?;
            mock::eval_option(&text, &loc, default.as_deref()).map_err(err)
        }
        Mode::Real => nix::eval_option(&settings, &loc).await.map_err(err),
    }
}

/// Starts `home-manager build` or `switch`; output arrives as events.
#[tauri::command]
pub fn start_run(app: AppHandle, state: State<AppState>, action: String) -> CmdResult<String> {
    if action != "build" && action != "switch" {
        return Err(format!("unknown action {action}"));
    }
    let (dirty, text) = state.with_working(|wc| Ok((wc.dirty(), wc.current.clone())))?;
    if dirty {
        return Err("save your changes before running home-manager".into());
    }
    let settings = state.settings();
    let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
    if !state.run_lock.try_acquire() {
        return Err("home-manager is already running".into());
    }
    let lock = state.run_lock.clone();
    let id = run_id.clone();
    match settings.mode {
        Mode::Mock => {
            let file = settings.config_path().display().to_string();
            let (lines, ok) = state
                .mock
                .lock()
                .unwrap()
                .run_home_manager(&action, &file, &text);
            tauri::async_runtime::spawn(async move {
                runner::run_simulated(app, id, lines, ok).await;
                lock.release();
            });
        }
        Mode::Real => {
            let args = nix::home_manager_args(&settings, &action);
            tauri::async_runtime::spawn(async move {
                runner::run_process(app, id, "home-manager".into(), args).await;
                lock.release();
            });
        }
    }
    Ok(run_id)
}

#[tauri::command]
pub async fn list_generations(state: State<'_, AppState>) -> CmdResult<Vec<Generation>> {
    match state.settings().mode {
        Mode::Mock => Ok(state.mock.lock().unwrap().generations.clone()),
        Mode::Real => nix::list_generations().await.map_err(err),
    }
}

#[tauri::command]
pub async fn activate_generation(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
) -> CmdResult<String> {
    let mode = state.settings().mode;
    // Look the path up ourselves rather than trusting one from the frontend.
    let path = match mode {
        Mode::Mock => None,
        Mode::Real => {
            let gens = nix::list_generations().await.map_err(err)?;
            let g = gens
                .into_iter()
                .find(|g| g.id == id)
                .ok_or(format!("no generation {id}"))?;
            Some(nix::activation_script(&g.path).map_err(err)?)
        }
    };
    if !state.run_lock.try_acquire() {
        return Err("home-manager is already running".into());
    }
    let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
    let lock = state.run_lock.clone();
    let rid = run_id.clone();
    match path {
        None => {
            let (lines, ok) = state.mock.lock().unwrap().activate(id);
            tauri::async_runtime::spawn(async move {
                runner::run_simulated(app, rid, lines, ok).await;
                lock.release();
            });
        }
        Some(script) => {
            tauri::async_runtime::spawn(async move {
                runner::run_process(app, rid, script, vec![]).await;
                lock.release();
            });
        }
    }
    Ok(run_id)
}

#[tauri::command]
pub async fn list_channels(state: State<'_, AppState>) -> CmdResult<Channels> {
    match state.settings().mode {
        Mode::Mock => Ok(state.mock.lock().unwrap().list_channels()),
        Mode::Real => channels::list().await.map_err(err),
    }
}

/// Adds a channel, or points an existing one at a new URL.
#[tauri::command]
pub async fn add_channel(state: State<'_, AppState>, name: String, url: String) -> CmdResult<()> {
    let (name, url) = (name.trim(), url.trim());
    match state.settings().mode {
        Mode::Mock => state.mock.lock().unwrap().add_channel(name, url),
        Mode::Real => channels::add(name, url).await,
    }
    .map_err(err)
}

#[tauri::command]
pub async fn remove_channel(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    match state.settings().mode {
        Mode::Mock => state.mock.lock().unwrap().remove_channel(&name),
        Mode::Real => channels::remove(&name).await,
    }
    .map_err(err)
}

/// Starts `nix-channel --update`, for one channel or all of them.
#[tauri::command]
pub fn start_channel_update(
    app: AppHandle,
    state: State<AppState>,
    name: Option<String>,
) -> CmdResult<String> {
    let args = channels::update_args(name.as_deref()).map_err(err)?;
    if !state.run_lock.try_acquire() {
        return Err("another command is already running".into());
    }
    let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
    let lock = state.run_lock.clone();
    let id = run_id.clone();
    match state.settings().mode {
        Mode::Mock => {
            let (lines, ok) = state.mock.lock().unwrap().update_channels(name.as_deref());
            tauri::async_runtime::spawn(async move {
                runner::run_simulated(app, id, lines, ok).await;
                lock.release();
            });
        }
        Mode::Real => {
            tauri::async_runtime::spawn(async move {
                runner::run_process(app, id, "nix-channel".into(), args).await;
                lock.release();
            });
        }
    }
    Ok(run_id)
}

/// Undoes the last channel update or change.
#[tauri::command]
pub async fn rollback_channels(state: State<'_, AppState>) -> CmdResult<String> {
    match state.settings().mode {
        Mode::Mock => state.mock.lock().unwrap().rollback_channels(),
        Mode::Real => channels::rollback().await,
    }
    .map_err(err)
}
