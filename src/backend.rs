//! The backend API the UI calls. Methods that touch the network or run nix are
//! async and run on the tokio runtime; the rest are quick and run inline.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::channels::{self, Channels};
use crate::mock;
use crate::nix::{self, Generation, PackageDetails, SearchResults};
use crate::nix_edit::{self, FileValue, Packages};
use crate::options::HmOption;
use crate::runner::{self, RunLock, Sink};
use crate::settings::{Mode, PackageSearch, Settings};
use crate::workspace::{DiffLine, WorkingCopy};

pub struct AppState {
    settings_path: PathBuf,
    mock_file: PathBuf,
    settings: Mutex<Settings>,
    working: Mutex<Option<WorkingCopy>>,
    /// Parsed options, keyed by where they came from.
    options: Mutex<Option<(String, Arc<Vec<HmOption>>)>>,
    mock: Mutex<mock::MockState>,
    run_lock: Arc<RunLock>,
    runtime: tokio::runtime::Handle,
}

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

pub struct Status {
    pub mode: Mode,
    pub config_file: String,
    pub nix_available: bool,
    pub home_manager_available: bool,
    pub dirty: bool,
    /// Why the config file can't be loaded or edited, if it can't.
    pub file_error: Option<String>,
}

pub struct FileOverview {
    pub defined_paths: Vec<String>,
}

pub struct Changes {
    pub path: String,
    pub dirty: bool,
    pub diff: Vec<DiffLine>,
}

pub enum StagedValue {
    /// A plain value from a form widget, rendered to Nix by the backend.
    Json { value: serde_json::Value },
    /// A Nix expression typed by the user.
    Raw { expr: String },
}

impl AppState {
    /// Loads settings from `config_dir` and makes sure the mock sample file exists.
    pub fn new(
        config_dir: PathBuf,
        data_dir: PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> anyhow::Result<Self> {
        let mock_file = data_dir.join("mock").join("home.nix");
        if !mock_file.exists() {
            std::fs::create_dir_all(mock_file.parent().unwrap())?;
            std::fs::write(&mock_file, mock::SAMPLE_HOME)?;
        }
        let settings_path = config_dir.join("settings.json");
        let settings = Settings::load(&settings_path, &mock_file);
        Ok(AppState {
            settings_path,
            mock_file,
            settings: Mutex::new(settings),
            working: Mutex::new(None),
            options: Mutex::new(None),
            mock: Mutex::new(mock::MockState::default()),
            run_lock: Arc::new(RunLock::default()),
            runtime,
        })
    }

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

    /// Whether the file was edited outside the app since it was loaded, while
    /// there are no staged changes. The UI polls this to refresh.
    pub fn file_changed_externally(&self) -> bool {
        let path = self.settings().config_path();
        self.working
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|wc| wc.path == path && !wc.dirty() && wc.changed_on_disk())
    }

    pub fn get_status(&self) -> Status {
        let settings = self.settings();
        let file = self.with_working(|wc| Ok((wc.dirty(), nix_edit::parse(&wc.current).err())));
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

    pub fn get_settings(&self) -> Settings {
        self.settings()
    }

    pub fn save_settings(&self, settings: Settings) -> CmdResult<()> {
        let old = self.settings();
        if old.config_path() != settings.config_path() {
            let mut working = self.working.lock().unwrap();
            if working.as_ref().is_some_and(|wc| wc.dirty()) {
                return Err("save or discard your pending changes before switching files".into());
            }
            *working = None;
        }
        settings.save(&self.settings_path).map_err(err)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// The suggested config file for a mode, used when the user switches modes.
    pub fn default_config_file(&self, mode: Mode) -> String {
        match mode {
            Mode::Mock => self.mock_file.display().to_string(),
            Mode::Real => crate::settings::default_real_config_file(),
        }
    }

    pub async fn load_options(&self, refresh: bool) -> CmdResult<Arc<Vec<HmOption>>> {
        let settings = self.settings();
        let key = match settings.mode {
            Mode::Mock => "mock".to_string(),
            Mode::Real => format!(
                "{}|{}|{}",
                settings.options_json, settings.flake_ref, settings.home_manager_flake
            ),
        };
        if !refresh
            && let Some((cached_key, options)) = self.options.lock().unwrap().as_ref()
            && *cached_key == key
        {
            return Ok(options.clone());
        }
        let options = match settings.mode {
            Mode::Mock => mock::options(),
            Mode::Real => nix::load_options(&settings).await,
        }
        .map_err(err)?;
        let options = Arc::new(options);
        *self.options.lock().unwrap() = Some((key, options.clone()));
        Ok(options)
    }

    pub fn get_file_overview(&self) -> CmdResult<FileOverview> {
        self.with_working(|wc| {
            Ok(FileOverview {
                defined_paths: nix_edit::defined_paths(&wc.current).map_err(err)?,
            })
        })
    }

    pub fn get_option_in_file(&self, loc: Vec<String>) -> CmdResult<FileValue> {
        self.with_working(|wc| nix_edit::get_option(&wc.current, &loc).map_err(err))
    }

    pub fn stage_option(&self, loc: Vec<String>, value: StagedValue) -> CmdResult<()> {
        let expr = match value {
            StagedValue::Json { value } => nix_edit::to_nix(&value),
            StagedValue::Raw { expr } => expr.trim().to_string(),
        };
        self.with_working(|wc| {
            wc.stage(|t| nix_edit::set_option(t, &loc, &expr))
                .map_err(err)
        })
    }

    pub fn stage_remove_option(&self, loc: Vec<String>) -> CmdResult<()> {
        self.with_working(|wc| wc.stage(|t| nix_edit::remove_option(t, &loc)).map_err(err))
    }

    pub fn get_packages(&self) -> CmdResult<Packages> {
        self.with_working(|wc| nix_edit::get_packages(&wc.current).map_err(err))
    }

    pub fn stage_add_package(&self, name: String) -> CmdResult<()> {
        self.with_working(|wc| wc.stage(|t| nix_edit::add_package(t, &name)).map_err(err))
    }

    pub fn stage_remove_package(&self, name: String) -> CmdResult<()> {
        self.with_working(|wc| {
            wc.stage(|t| nix_edit::remove_package(t, &name))
                .map_err(err)
        })
    }

    pub async fn search_packages(
        &self,
        query: String,
        platform: Option<String>,
        from: usize,
    ) -> CmdResult<SearchResults> {
        let query = query.trim().to_string();
        if query.len() < 2 {
            return Err("type at least two characters".into());
        }
        if query.starts_with('-') {
            return Err("search terms can't start with '-'".into());
        }
        let settings = self.settings();
        // search.nixos.org needs no local nix, so it works in mock mode too.
        if settings.package_search == PackageSearch::NixosOrg {
            return crate::nixos_search::search(
                &settings.nixos_search_channel,
                &query,
                platform.as_deref(),
                from,
            )
            .await
            .map_err(err);
        }
        let mut results = match settings.mode {
            Mode::Mock => mock::search_packages(&query),
            Mode::Real => nix::search_packages(&settings, &query).await,
        }
        .map_err(err)?;
        crate::search::rank_packages(&mut results.packages, &query);
        Ok(results)
    }

    /// `meta.platforms` of search results, which `nix search` doesn't report.
    pub async fn package_platforms(
        &self,
        attrs: Vec<String>,
    ) -> CmdResult<std::collections::HashMap<String, Option<Vec<String>>>> {
        match self.settings().mode {
            Mode::Mock => Ok(attrs
                .iter()
                .map(|a| (a.clone(), Some(mock::platforms(a))))
                .collect()),
            Mode::Real => nix::package_platforms(&self.settings(), &attrs)
                .await
                .map_err(err),
        }
    }

    /// Switches the Packages page between `nix search` and search.nixos.org.
    pub fn set_package_search(&self, source: PackageSearch) -> CmdResult<()> {
        let mut settings = self.settings();
        settings.package_search = source;
        self.save_settings(settings)
    }

    /// Version, description and license of a package in the user's nixpkgs.
    pub async fn package_details(&self, attr: String) -> CmdResult<PackageDetails> {
        match self.settings().mode {
            Mode::Mock => mock::package_details(&attr),
            Mode::Real => nix::package_details(&self.settings(), &attr).await,
        }
        .map_err(err)
    }

    pub fn get_changes(&self) -> CmdResult<Changes> {
        self.with_working(|wc| {
            Ok(Changes {
                path: wc.path.display().to_string(),
                dirty: wc.dirty(),
                diff: wc.diff(),
            })
        })
    }

    pub fn save_changes(&self) -> CmdResult<()> {
        self.with_working(|wc| wc.save().map_err(err))
    }

    pub fn discard_changes(&self) -> CmdResult<()> {
        self.with_working(|wc| {
            wc.discard();
            Ok(())
        })?;
        // Drop the working copy so the next access re-reads the file from disk.
        *self.working.lock().unwrap() = None;
        Ok(())
    }

    pub async fn eval_option(
        &self,
        loc: Vec<String>,
        default: Option<String>,
    ) -> CmdResult<String> {
        let settings = self.settings();
        match settings.mode {
            Mode::Mock => {
                let text = self.with_working(|wc| Ok(wc.current.clone()))?;
                mock::eval_option(&text, &loc, default.as_deref()).map_err(err)
            }
            Mode::Real => nix::eval_option(&settings, &loc).await.map_err(err),
        }
    }

    /// Starts `home-manager build` or `switch`; output arrives as events.
    pub fn start_run(&self, sink: Sink, action: String) -> CmdResult<String> {
        if action != "build" && action != "switch" {
            return Err(format!("unknown action {action}"));
        }
        let (dirty, text) = self.with_working(|wc| Ok((wc.dirty(), wc.current.clone())))?;
        if dirty {
            return Err("save your changes before running home-manager".into());
        }
        let settings = self.settings();
        let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
        if !self.run_lock.try_acquire() {
            return Err("home-manager is already running".into());
        }
        let lock = self.run_lock.clone();
        let id = run_id.clone();
        match settings.mode {
            Mode::Mock => {
                let file = settings.config_path().display().to_string();
                let (lines, ok) = self
                    .mock
                    .lock()
                    .unwrap()
                    .run_home_manager(&action, &file, &text);
                self.runtime.spawn(async move {
                    runner::run_simulated(sink, id, lines, ok).await;
                    lock.release();
                });
            }
            Mode::Real => {
                let args = nix::home_manager_args(&settings, &action);
                self.runtime.spawn(async move {
                    runner::run_process(sink, id, "home-manager".into(), args).await;
                    lock.release();
                });
            }
        }
        Ok(run_id)
    }

    pub async fn list_generations(&self) -> CmdResult<Vec<Generation>> {
        match self.settings().mode {
            Mode::Mock => Ok(self.mock.lock().unwrap().generations.clone()),
            Mode::Real => nix::list_generations().await.map_err(err),
        }
    }

    pub async fn activate_generation(&self, sink: Sink, id: u64) -> CmdResult<String> {
        let mode = self.settings().mode;
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
        if !self.run_lock.try_acquire() {
            return Err("home-manager is already running".into());
        }
        let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
        let lock = self.run_lock.clone();
        let rid = run_id.clone();
        match path {
            None => {
                let (lines, ok) = self.mock.lock().unwrap().activate(id);
                self.runtime.spawn(async move {
                    runner::run_simulated(sink, rid, lines, ok).await;
                    lock.release();
                });
            }
            Some(script) => {
                self.runtime.spawn(async move {
                    runner::run_process(sink, rid, script, vec![]).await;
                    lock.release();
                });
            }
        }
        Ok(run_id)
    }

    pub async fn list_channels(&self) -> CmdResult<Channels> {
        match self.settings().mode {
            Mode::Mock => Ok(self.mock.lock().unwrap().list_channels()),
            Mode::Real => channels::list().await.map_err(err),
        }
    }

    /// Adds a channel, or points an existing one at a new URL.
    pub async fn add_channel(&self, name: String, url: String) -> CmdResult<()> {
        let (name, url) = (name.trim(), url.trim());
        match self.settings().mode {
            Mode::Mock => self.mock.lock().unwrap().add_channel(name, url),
            Mode::Real => channels::add(name, url).await,
        }
        .map_err(err)
    }

    pub async fn remove_channel(&self, name: String) -> CmdResult<()> {
        match self.settings().mode {
            Mode::Mock => self.mock.lock().unwrap().remove_channel(&name),
            Mode::Real => channels::remove(&name).await,
        }
        .map_err(err)
    }

    /// Starts `nix-channel --update`, for one channel or all of them.
    pub fn start_channel_update(&self, sink: Sink, name: Option<String>) -> CmdResult<String> {
        let args = channels::update_args(name.as_deref()).map_err(err)?;
        if !self.run_lock.try_acquire() {
            return Err("another command is already running".into());
        }
        let run_id = format!("run-{}", NEXT_RUN.fetch_add(1, Ordering::SeqCst));
        let lock = self.run_lock.clone();
        let id = run_id.clone();
        match self.settings().mode {
            Mode::Mock => {
                let (lines, ok) = self.mock.lock().unwrap().update_channels(name.as_deref());
                self.runtime.spawn(async move {
                    runner::run_simulated(sink, id, lines, ok).await;
                    lock.release();
                });
            }
            Mode::Real => {
                self.runtime.spawn(async move {
                    runner::run_process(sink, id, "nix-channel".into(), args).await;
                    lock.release();
                });
            }
        }
        Ok(run_id)
    }

    /// Undoes the last channel update or change.
    pub async fn rollback_channels(&self) -> CmdResult<String> {
        match self.settings().mode {
            Mode::Mock => self.mock.lock().unwrap().rollback_channels(),
            Mode::Real => channels::rollback().await,
        }
        .map_err(err)
    }
}
