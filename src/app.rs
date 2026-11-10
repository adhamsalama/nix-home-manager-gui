//! Connects the Slint UI to the backend. Everything here runs on the UI
//! thread; slow backend calls run on tokio and report back through
//! `slint::invoke_from_event_loop`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, StyledText, VecModel};

use crate::backend::{AppState, StagedValue};
use crate::editor::{self, FormValue, Widget};
use crate::nix::{PackageDetails, PackageInfo};
use crate::nix_edit::FileValue;
use crate::options::HmOption;
use crate::runner::{RunEvent, Sink};
use crate::settings::{Mode, PackageSearch, Settings};
use crate::{
    ApplyState, ChannelRow, ChannelsState, DiffRow, GenRow, GenerationsState, LogRow, MainWindow,
    MapEntry, OptionRow, OptionsState, PackagesState, PkgRow, Placeholder, PlatformRow, RelatedRow,
    RunInfo, SettingsState, Shell, Theme,
};
use crate::{search, text};

const MAX_OPTION_RESULTS: usize = 300;
const MAX_PACKAGE_RESULTS: usize = 200;

/// Which page started a run, so its output goes to the right log.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunTarget {
    Apply,
    Generations,
    Channels,
}

#[derive(Default)]
struct OptionsPage {
    all: Option<Arc<Vec<HmOption>>>,
    /// Index of the selected option in `all`.
    selected: Option<usize>,
    /// Names typed for `<name>` placeholders, by position in `loc`.
    fills: HashMap<usize, String>,
    widget: Option<Widget>,
    /// Re-initialize the form only when the option or its value in the file changes,
    /// so unsaved form edits survive unrelated refreshes.
    editor_key: String,
    /// Shown once options finish loading.
    pending_focus: Option<String>,
}

#[derive(Default)]
struct PackagesPage {
    results: Vec<PackageInfo>,
    source: String,
    installed: HashSet<String>,
    checking: Option<String>,
    warning: Option<(String, String)>,
    selected: Option<String>,
}

#[derive(Default)]
struct Runs {
    targets: HashMap<String, RunTarget>,
    /// Events that arrived before we learned which page owns the run.
    early: HashMap<String, Vec<RunEvent>>,
}

pub struct App {
    win: MainWindow,
    be: Arc<AppState>,
    rt: tokio::runtime::Handle,
    defined: RefCell<HashSet<String>>,
    opts: RefCell<OptionsPage>,
    pkgs: RefCell<PackagesPage>,
    runs: RefCell<Runs>,
    list_items: Rc<VecModel<SharedString>>,
    map_entries: Rc<VecModel<MapEntry>>,
    logs: [Rc<VecModel<LogRow>>; 3],
    /// Bumped on every selection, so stale async results can be dropped.
    generation: Cell<u64>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Runs `f` with the app, from a callback posted to the event loop.
fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

fn s(v: impl AsRef<str>) -> SharedString {
    SharedString::from(v.as_ref())
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

pub fn run(be: Arc<AppState>, rt: tokio::runtime::Handle) -> Result<(), slint::PlatformError> {
    let win = MainWindow::new()?;
    win.global::<Theme>()
        .set_mono(s(if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "monospace"
        }));
    let app = Rc::new(App {
        win: win.clone_strong(),
        be,
        rt,
        defined: RefCell::default(),
        opts: RefCell::default(),
        pkgs: RefCell::default(),
        runs: RefCell::default(),
        list_items: Rc::new(VecModel::default()),
        map_entries: Rc::new(VecModel::default()),
        logs: std::array::from_fn(|_| Rc::new(VecModel::default())),
        generation: Cell::new(0),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    app.wire();
    app.start();

    // Pick up edits made to the file outside the app.
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), || {
        with_app(|app| {
            if app.be.file_changed_externally() {
                app.refresh();
            }
        })
    });

    if let Ok(path) = std::env::var("HM_GUI_SNAPSHOT") {
        crate::snapshot::schedule(&win, path);
    }
    win.run()
}

impl App {
    /// Runs `fut` on tokio, then `then` on the UI thread with its result.
    fn spawn<T, F, G>(&self, fut: F, then: G)
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
        G: FnOnce(&Rc<App>, T) + Send + 'static,
    {
        self.rt.spawn(async move {
            let v = fut.await;
            let _ = slint::invoke_from_event_loop(move || with_app(|app| then(app, v)));
        });
    }

    fn sink(&self) -> Sink {
        Arc::new(|ev| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.on_run_event(ev)));
        })
    }

    fn bump(&self) -> u64 {
        self.generation.set(self.generation.get() + 1);
        self.generation.get()
    }

    fn start(self: &Rc<Self>) {
        self.load_settings();
        self.load_options(false);
        self.refresh();
        self.load_generations();
        self.load_channels();
        self.win.global::<ChannelsState>().set_suggestions(model(
            text::suggested_channel_urls().iter().map(s).collect(),
        ));
        if let Ok(page) = std::env::var("HM_GUI_PAGE") {
            self.win
                .global::<Shell>()
                .set_page(page.parse().unwrap_or(0));
        }
        if let Ok(attr) = std::env::var("HM_GUI_PACKAGE") {
            self.win.global::<Shell>().set_page(1);
            self.select_package(Some(attr));
        }
        if let Ok(name) = std::env::var("HM_GUI_SELECT") {
            self.focus_option(&name);
        }
    }

    /// Re-reads everything that depends on the config file, after an edit.
    fn refresh(self: &Rc<Self>) {
        let st = self.be.get_status();
        let shell = self.win.global::<Shell>();
        shell.set_dirty(st.dirty);
        shell.set_mock(st.mode == Mode::Mock);
        shell.set_config_file(s(&st.config_file));
        shell.set_file_error(s(st.file_error.unwrap_or_default()));
        shell.set_hm_missing(st.mode == Mode::Real && !st.home_manager_available);
        self.win
            .global::<SettingsState>()
            .set_nix_available(st.nix_available);

        *self.defined.borrow_mut() = self
            .be
            .get_file_overview()
            .map(|o| o.defined_paths.into_iter().collect())
            .unwrap_or_default();
        self.run_search();
        self.load_file_value();
        self.load_packages();
        self.update_related();
        self.load_changes();
    }

    // ---- wiring ---------------------------------------------------------

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        macro_rules! on {
            ($global:ty, $method:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = weak.clone();
                self.win.global::<$global>().$method(move |$($arg),*| {
                    if let Some($app) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }

        on!(OptionsState, on_search_changed, |app| app.run_search());
        on!(OptionsState, on_select, |app, name| app
            .select_option(&name));
        on!(OptionsState, on_retry, |app| app.load_options(true));
        on!(OptionsState, on_fill, |app, i, v| {
            app.opts
                .borrow_mut()
                .fills
                .insert(i as usize, v.to_string());
            app.update_placeholders();
            app.load_file_value();
        });
        on!(OptionsState, on_list_edit, |app, i, v| app
            .list_items
            .set_row_data(i as usize, v));
        on!(OptionsState, on_list_remove, |app, i| {
            app.list_items.remove(i as usize);
        });
        on!(OptionsState, on_list_add, |app| app
            .list_items
            .push(SharedString::new()));
        on!(OptionsState, on_map_edit, |app, i, k, v| {
            app.map_entries
                .set_row_data(i as usize, MapEntry { key: k, value: v })
        });
        on!(OptionsState, on_map_remove, |app, i| {
            app.map_entries.remove(i as usize);
        });
        on!(OptionsState, on_map_add, |app| app
            .map_entries
            .push(MapEntry::default()));
        on!(OptionsState, on_stage, |app| app.stage_option());
        on!(OptionsState, on_remove, |app| {
            if let Some(loc) = app.current_loc() {
                let r = app.be.stage_remove_option(loc);
                app.after_stage(r);
            }
        });
        on!(OptionsState, on_evaluate, |app| app.evaluate());

        on!(PackagesState, on_select, |app, attr| app
            .select_package(Some(attr.to_string())));
        on!(PackagesState, on_back, |app| app.select_package(None));
        on!(PackagesState, on_search, |app| app.search_packages());
        on!(PackagesState, on_set_source, |app, index| {
            let source = if index == 1 {
                PackageSearch::NixosOrg
            } else {
                PackageSearch::Nixpkgs
            };
            let ui = app.win.global::<PackagesState>();
            if let Err(e) = app.be.set_package_search(source) {
                ui.set_error(s(e));
            }
            // Results from the other source would be misleading now.
            app.pkgs.borrow_mut().results.clear();
            ui.set_summary(s(""));
            ui.set_no_results(false);
            app.update_package_rows();
        });
        on!(PackagesState, on_add, |app, attr| app
            .add_package(attr.to_string()));
        on!(PackagesState, on_add_anyway, |app, attr| {
            app.pkgs.borrow_mut().warning = None;
            app.stage_package(attr.to_string());
        });
        on!(PackagesState, on_cancel_warning, |app| {
            app.pkgs.borrow_mut().warning = None;
            app.update_package_rows();
        });
        on!(PackagesState, on_remove, |app, attr| {
            let r = app.be.stage_remove_package(attr.to_string());
            if r.is_ok() {
                app.select_package(None);
            }
            app.after_package_change(r);
        });
        on!(PackagesState, on_open_option, |app, name| app
            .focus_option(&name));
        on!(PackagesState, on_open_url, |_app, url| {
            let _ = open::that_detached(url.as_str());
        });

        on!(ApplyState, on_save, |app| {
            let r = app.be.save_changes();
            app.after_apply_action(r);
        });
        on!(ApplyState, on_discard, |app| {
            let r = app.be.discard_changes();
            app.after_apply_action(r);
        });
        on!(ApplyState, on_start, |app, action| {
            let r = app.be.start_run(app.sink(), action.to_string());
            app.begin_run(RunTarget::Apply, format!("home-manager {action}"), r);
        });

        on!(GenerationsState, on_activate, |app, id| {
            let be = app.be.clone();
            let sink = app.sink();
            app.spawn(
                async move { be.activate_generation(sink, id as u64).await },
                move |app, r| {
                    app.begin_run(
                        RunTarget::Generations,
                        format!("Activating generation {id}"),
                        r,
                    );
                },
            );
        });

        on!(ChannelsState, on_update, |app, name| {
            let name = (!name.is_empty()).then(|| name.to_string());
            let title = format!("nix-channel --update {}", name.as_deref().unwrap_or(""));
            let ch = app.win.global::<ChannelsState>();
            ch.set_message(s(""));
            ch.set_update_ok(false);
            let r = app.be.start_channel_update(app.sink(), name);
            app.begin_run(RunTarget::Channels, title, r);
        });
        on!(ChannelsState, on_save_url, |app, name, url| {
            let be = app.be.clone();
            let msg =
                format!("{name} now points at the new URL. Update it to download the new version.");
            app.channel_action(
                async move { be.add_channel(name.to_string(), url.to_string()).await },
                msg,
                |ch| {
                    ch.set_editing(s(""));
                },
            );
        });
        on!(ChannelsState, on_add, |app, name, url| {
            let be = app.be.clone();
            let msg = format!("Added {}. Update it to download it.", name.trim());
            app.channel_action(
                async move { be.add_channel(name.to_string(), url.to_string()).await },
                msg,
                |ch| {
                    ch.set_new_name(s(""));
                    ch.set_new_url(s(""));
                },
            );
        });
        on!(ChannelsState, on_remove, |app, name| {
            let be = app.be.clone();
            let msg = format!("Removed {name}.");
            app.channel_action(
                async move { be.remove_channel(name.to_string()).await },
                msg,
                |_| {},
            );
        });
        on!(ChannelsState, on_rollback, |app| {
            let be = app.be.clone();
            app.channel_action(
                async move { be.rollback_channels().await.map(|_| ()) },
                "Restored the previous channels.".into(),
                |_| {},
            );
        });
        on!(ChannelsState, on_go_apply, |app| app
            .win
            .global::<Shell>()
            .set_page(2));

        on!(SettingsState, on_set_mode, |app, mode| {
            let st = app.win.global::<SettingsState>();
            let m = if mode == "mock" {
                Mode::Mock
            } else {
                Mode::Real
            };
            st.set_mode(mode);
            st.set_config_file(s(app.be.default_config_file(m)));
        });
        on!(SettingsState, on_save, |app| app.save_settings());
    }

    // ---- options ----------------------------------------------------------

    fn load_options(self: &Rc<Self>, refresh: bool) {
        let ui = self.win.global::<OptionsState>();
        ui.set_loading(true);
        ui.set_load_error(s(""));
        let be = self.be.clone();
        self.spawn(async move { be.load_options(refresh).await }, |app, r| {
            let ui = app.win.global::<OptionsState>();
            ui.set_loading(false);
            match r {
                Ok(all) => {
                    ui.set_total(all.len() as i32);
                    let focus = {
                        let mut o = app.opts.borrow_mut();
                        o.all = Some(all);
                        o.pending_focus.take()
                    };
                    // Keep the selection if it still exists in the new set.
                    let selected = ui.get_selected();
                    app.run_search();
                    if let Some(name) = focus {
                        app.focus_option(&name);
                    } else if !selected.is_empty() {
                        app.select_option(&selected);
                    }
                    app.update_related();
                }
                Err(e) => ui.set_load_error(s(e)),
            }
        });
    }

    fn run_search(&self) {
        let ui = self.win.global::<OptionsState>();
        let o = self.opts.borrow();
        let Some(all) = &o.all else { return };
        let defined = self.defined.borrow();
        let only = ui.get_only_set().then_some(&*defined);
        let hits = search::search(all, &ui.get_query(), only);
        let rows: Vec<OptionRow> = hits
            .iter()
            .take(MAX_OPTION_RESULTS)
            .map(|&i| OptionRow {
                name: s(&all[i].name),
                set: defined.contains(&all[i].name),
            })
            .collect();
        ui.set_results(model(rows));
        ui.set_results_note(s(if hits.is_empty() {
            "No matching options".to_string()
        } else if hits.len() > MAX_OPTION_RESULTS {
            format!(
                "{} more; refine your search",
                hits.len() - MAX_OPTION_RESULTS
            )
        } else {
            String::new()
        }));
    }

    /// Opens an option on the Options page, e.g. from a package's details.
    fn focus_option(self: &Rc<Self>, name: &str) {
        let ui = self.win.global::<OptionsState>();
        self.win.global::<Shell>().set_page(0);
        if self.opts.borrow().all.is_none() {
            self.opts.borrow_mut().pending_focus = Some(name.to_string());
            return;
        }
        ui.set_query(s(name));
        ui.set_only_set(false);
        self.run_search();
        self.select_option(name);
    }

    fn select_option(self: &Rc<Self>, name: &str) {
        let ui = self.win.global::<OptionsState>();
        let (option, changed) = {
            let mut o = self.opts.borrow_mut();
            let Some(all) = o.all.clone() else { return };
            let Some(idx) = all.iter().position(|x| x.name == name) else {
                return;
            };
            let changed = o.selected != Some(idx);
            if changed {
                o.selected = Some(idx);
                o.fills.clear();
                o.editor_key.clear();
                o.widget = Some(editor::widget_for(&all[idx].ty));
            }
            (all[idx].clone(), changed)
        };
        if !changed {
            return;
        }
        self.bump();
        ui.set_selected(s(&option.name));
        ui.set_d_type(s(&option.ty));
        let md = text::description_markdown(&option.description);
        ui.set_d_description(StyledText::from_markdown(&md).unwrap_or_else(|_| {
            StyledText::from_plain_text(&text::description_plain(&option.description))
        }));
        ui.set_d_has_default(option.default.is_some());
        ui.set_d_default(s(option.default.as_deref().unwrap_or("")));
        ui.set_d_has_example(option.example.is_some());
        ui.set_d_example(s(option.example.as_deref().unwrap_or("")));
        ui.set_d_declarations(s(option.declarations.join(", ")));
        ui.set_read_only(option.read_only);
        ui.set_eval_busy(false);
        ui.set_eval_value(s(""));
        ui.set_eval_error(s(""));
        self.update_placeholders();
        self.load_file_value();
    }

    fn selected_option(&self) -> Option<HmOption> {
        let o = self.opts.borrow();
        Some(o.all.as_ref()?[o.selected?].clone())
    }

    fn update_placeholders(&self) {
        let Some(option) = self.selected_option() else {
            return;
        };
        let fills = self.opts.borrow().fills.clone();
        let rows: Vec<Placeholder> = option
            .loc
            .iter()
            .enumerate()
            .filter(|(_, p)| is_placeholder(p))
            .map(|(i, p)| Placeholder {
                index: i as i32,
                prefix: s(option.loc[..i].join(".")),
                name: s(p),
                value: s(fills.get(&i).map(String::as_str).unwrap_or("")),
            })
            .collect();
        // Rebuilding the rows would reset the text fields being typed in.
        let ui = self.win.global::<OptionsState>();
        let current = ui.get_placeholders();
        let same_shape = current.row_count() == rows.len()
            && current
                .iter()
                .zip(&rows)
                .all(|(a, b)| a.index == b.index && a.name == b.name);
        if !same_shape {
            ui.set_placeholders(model(rows));
        }
    }

    /// The selected option's path with placeholders filled in, if they all are.
    fn current_loc(&self) -> Option<Vec<String>> {
        let option = self.selected_option()?;
        let o = self.opts.borrow();
        option
            .loc
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if is_placeholder(p) {
                    o.fills.get(&i).filter(|v| !v.is_empty()).cloned()
                } else {
                    Some(p.clone())
                }
            })
            .collect()
    }

    fn load_file_value(&self) {
        let ui = self.win.global::<OptionsState>();
        let Some(option) = self.selected_option() else {
            return;
        };
        let Some(loc) = self.current_loc() else {
            ui.set_complete(false);
            return;
        };
        ui.set_complete(true);
        let value = match self.be.get_option_in_file(loc.clone()) {
            Ok(v) => v,
            Err(e) => {
                ui.set_file_state(s("error"));
                ui.set_file_error(s(e));
                return;
            }
        };
        let (state, line, expr) = match &value {
            FileValue::NotSet => ("notSet", 0, ""),
            FileValue::Set { expr, line } => ("set", *line, expr.as_str()),
            FileValue::Partial { line } => ("partial", *line, ""),
            FileValue::Opaque { line, expr } => ("opaque", *line, expr.as_str()),
        };
        ui.set_file_state(s(state));
        ui.set_file_line(line as i32);
        ui.set_file_expr(s(expr));

        let key = format!("{}|{state}|{expr}", loc.join("."));
        if self.opts.borrow().editor_key == key {
            return;
        }
        self.opts.borrow_mut().editor_key = key;
        let file_expr = matches!(value, FileValue::Set { .. }).then_some(expr);
        self.init_editor(&option, file_expr);
    }

    /// Fills the form from the file's value, else the default, else empty.
    fn init_editor(&self, option: &HmOption, file_expr: Option<&str>) {
        let ui = self.win.global::<OptionsState>();
        let widget = editor::widget_for(&option.ty);
        let parsed_file = file_expr.and_then(|e| editor::parse_expr(&widget, e));
        let value = parsed_file
            .clone()
            .or_else(|| {
                option
                    .default
                    .as_deref()
                    .and_then(|d| editor::parse_expr(&widget, d))
            })
            .unwrap_or_else(|| editor::empty_value(&widget));

        ui.set_widget(s(widget.kind()));
        ui.set_nullable(widget.nullable());
        ui.set_multiline(matches!(
            widget,
            Widget::Str {
                multiline: true,
                ..
            }
        ));
        ui.set_raw(widget == Widget::Raw || (file_expr.is_some() && parsed_file.is_none()));
        ui.set_raw_expr(s(file_expr
            .or(option.example.as_deref())
            .or(option.default.as_deref())
            .unwrap_or("")));
        ui.set_editor_error(s(""));

        let values = match &widget {
            Widget::Enum { values, .. } => values.clone(),
            _ => vec![],
        };
        ui.set_enum_values(model(values.iter().map(s).collect()));

        // Fields keep a non-null value underneath "null", for when it's unticked.
        let shown = if value == FormValue::Null {
            editor::non_null_empty(&widget)
        } else {
            value.clone()
        };
        ui.set_is_null(value == FormValue::Null);
        ui.set_bool_value(shown == FormValue::Bool(true));
        let text_value = match &shown {
            FormValue::Text(t) => t.clone(),
            _ => String::new(),
        };
        ui.set_enum_index(
            values
                .iter()
                .position(|v| *v == text_value)
                .map_or(0, |i| i as i32),
        );
        ui.set_text_value(s(&text_value));
        self.list_items.set_vec(match &shown {
            FormValue::List(items) => items.iter().map(s).collect(),
            _ => vec![],
        });
        self.map_entries.set_vec(match &shown {
            FormValue::Map(entries) => entries
                .iter()
                .map(|(k, v)| MapEntry {
                    key: s(k),
                    value: s(v),
                })
                .collect(),
            _ => vec![],
        });
        ui.set_list_items(ModelRc::from(self.list_items.clone()));
        ui.set_map_entries(ModelRc::from(self.map_entries.clone()));
        self.opts.borrow_mut().widget = Some(widget);
    }

    /// Reads the form back into a value.
    fn form_value(&self, widget: &Widget) -> FormValue {
        let ui = self.win.global::<OptionsState>();
        if widget.nullable() && ui.get_is_null() {
            return FormValue::Null;
        }
        match widget {
            Widget::Bool { .. } => FormValue::Bool(ui.get_bool_value()),
            Widget::Str { .. } | Widget::Number { .. } => {
                FormValue::Text(ui.get_text_value().to_string())
            }
            Widget::Enum { values, .. } => FormValue::Text(
                values
                    .get(ui.get_enum_index().max(0) as usize)
                    .cloned()
                    .unwrap_or_default(),
            ),
            Widget::StringList => {
                FormValue::List(self.list_items.iter().map(|x| x.to_string()).collect())
            }
            Widget::StringMap => FormValue::Map(
                self.map_entries
                    .iter()
                    .map(|e| (e.key.to_string(), e.value.to_string()))
                    .collect(),
            ),
            Widget::Raw => FormValue::Null,
        }
    }

    fn stage_option(self: &Rc<Self>) {
        let ui = self.win.global::<OptionsState>();
        let Some(loc) = self.current_loc() else {
            return;
        };
        let Some(widget) = self.opts.borrow().widget.clone() else {
            return;
        };
        let staged = if ui.get_raw() {
            StagedValue::Raw {
                expr: ui.get_raw_expr().to_string(),
            }
        } else {
            match editor::to_json(&widget, &self.form_value(&widget)) {
                Ok(value) => StagedValue::Json { value },
                Err(e) => {
                    ui.set_editor_error(s(e));
                    return;
                }
            }
        };
        let r = self.be.stage_option(loc, staged);
        self.after_stage(r);
    }

    fn after_stage(self: &Rc<Self>, r: Result<(), String>) {
        match r {
            Ok(()) => {
                self.win.global::<OptionsState>().set_editor_error(s(""));
                self.refresh();
            }
            Err(e) => self.win.global::<OptionsState>().set_editor_error(s(e)),
        }
    }

    fn evaluate(self: &Rc<Self>) {
        let ui = self.win.global::<OptionsState>();
        let (Some(loc), Some(option)) = (self.current_loc(), self.selected_option()) else {
            return;
        };
        ui.set_eval_busy(true);
        ui.set_eval_value(s(""));
        ui.set_eval_error(s(""));
        let generation = self.generation.get();
        let be = self.be.clone();
        self.spawn(
            async move { be.eval_option(loc, option.default).await },
            move |app, r| {
                if app.generation.get() != generation {
                    return;
                }
                let ui = app.win.global::<OptionsState>();
                ui.set_eval_busy(false);
                match r {
                    Ok(v) => ui.set_eval_value(s(v)),
                    Err(e) => ui.set_eval_error(s(e)),
                }
            },
        );
    }

    // ---- packages ---------------------------------------------------------

    fn load_packages(&self) {
        let ui = self.win.global::<PackagesState>();
        match self.be.get_packages() {
            Ok(p) => {
                ui.set_list_error(s(""));
                ui.set_defined(p.defined);
                ui.set_installed(model(p.items.iter().map(s).collect()));
                let mut pk = self.pkgs.borrow_mut();
                pk.installed = p.items.into_iter().collect();
                ui.set_d_installed(
                    pk.selected
                        .as_ref()
                        .is_some_and(|sel| pk.installed.contains(sel)),
                );
            }
            Err(e) => ui.set_list_error(s(e)),
        }
        self.update_package_rows();
    }

    fn update_package_rows(&self) {
        let ui = self.win.global::<PackagesState>();
        let pk = self.pkgs.borrow();
        let rows: Vec<PkgRow> = pk
            .results
            .iter()
            .take(MAX_PACKAGE_RESULTS)
            .map(|p| PkgRow {
                attr: s(&p.attr),
                description: s(&p.description),
                version: s(&p.version),
                installed: pk.installed.contains(&p.attr),
                checking: pk.checking.as_deref() == Some(&p.attr),
                warning: s(pk
                    .warning
                    .as_ref()
                    .filter(|(a, _)| *a == p.attr)
                    .map_or("", |(_, m)| m)),
            })
            .collect();
        ui.set_results(model(rows));
        ui.set_checking_any(pk.checking.is_some());
    }

    fn search_packages(self: &Rc<Self>) {
        let ui = self.win.global::<PackagesState>();
        ui.set_searching(true);
        ui.set_error(s(""));
        let be = self.be.clone();
        let query = ui.get_query().to_string();
        self.spawn(async move { be.search_packages(query).await }, |app, r| {
            let ui = app.win.global::<PackagesState>();
            ui.set_searching(false);
            match r {
                Ok(res) => {
                    let n = res.packages.len();
                    ui.set_summary(s(format!(
                        "Searched {}: {n} result{}{}.",
                        res.source,
                        if n == 1 { "" } else { "s" },
                        if n > MAX_PACKAGE_RESULTS {
                            ", showing the first 200"
                        } else {
                            ""
                        }
                    )));
                    ui.set_no_results(n == 0);
                    let mut pk = app.pkgs.borrow_mut();
                    pk.results = res.packages;
                    pk.source = res.source;
                    pk.warning = None;
                }
                Err(e) => ui.set_error(s(e)),
            }
            app.update_package_rows();
        });
    }

    /// Checks that the package builds on this machine before adding it.
    fn add_package(self: &Rc<Self>, attr: String) {
        {
            let mut pk = self.pkgs.borrow_mut();
            pk.warning = None;
            pk.checking = Some(attr.clone());
        }
        self.update_package_rows();
        let be = self.be.clone();
        let a = attr.clone();
        self.spawn(async move { be.package_details(a).await }, move |app, r| {
            app.pkgs.borrow_mut().checking = None;
            // A failed check shouldn't block adding; the build will report problems.
            match r.ok().as_ref().and_then(text::unavailable_reason) {
                Some(reason) => {
                    app.pkgs.borrow_mut().warning = Some((attr, reason));
                    app.update_package_rows();
                }
                None => app.stage_package(attr),
            }
        });
    }

    fn stage_package(self: &Rc<Self>, attr: String) {
        let r = self.be.stage_add_package(attr);
        self.after_package_change(r);
    }

    fn after_package_change(self: &Rc<Self>, r: Result<(), String>) {
        let ui = self.win.global::<PackagesState>();
        match r {
            Ok(()) => {
                ui.set_error(s(""));
                self.refresh();
            }
            Err(e) => {
                ui.set_error(s(e));
                self.update_package_rows();
            }
        }
    }

    fn select_package(self: &Rc<Self>, attr: Option<String>) {
        let ui = self.win.global::<PackagesState>();
        let installed = {
            let mut pk = self.pkgs.borrow_mut();
            pk.selected = attr.clone();
            attr.as_ref().is_some_and(|a| pk.installed.contains(a))
        };
        ui.set_selected(s(attr.as_deref().unwrap_or("")));
        ui.set_d_installed(installed);
        let Some(attr) = attr else { return };
        let generation = self.bump();
        ui.set_d_loading(true);
        ui.set_d_error(s(""));
        let be = self.be.clone();
        let a = attr.clone();
        self.spawn(async move { be.package_details(a).await }, move |app, r| {
            if app.generation.get() != generation {
                return;
            }
            let ui = app.win.global::<PackagesState>();
            ui.set_d_loading(false);
            match r {
                Ok(d) => app.show_package_details(&d),
                Err(e) => ui.set_d_error(s(e)),
            }
        });
        self.update_related();
    }

    fn show_package_details(&self, d: &PackageDetails) {
        let ui = self.win.global::<PackagesState>();
        ui.set_d_version(s(d.version.as_deref().unwrap_or("")));
        ui.set_d_unfree(d.unfree);
        ui.set_d_broken(d.broken);
        ui.set_d_source(s(&d.source));
        ui.set_d_reason(s(text::unavailable_reason(d).unwrap_or_default()));
        ui.set_d_description(s(d.description.as_deref().unwrap_or("")));
        ui.set_d_homepage(s(d.homepage.as_deref().unwrap_or("")));
        ui.set_d_platforms(model(
            text::group_platforms(&d.platforms)
                .into_iter()
                .map(|(os, arches)| PlatformRow {
                    os: s(os),
                    arches: s(arches.join(", ")),
                })
                .collect(),
        ));
        ui.set_d_bad_platforms(s(d.bad_platforms.join(", ")));
        ui.set_d_licenses(s(d.licenses.join(", ")));
    }

    /// Shows the options that configure the selected package.
    fn update_related(self: &Rc<Self>) {
        let ui = self.win.global::<PackagesState>();
        let Some(attr) = self.pkgs.borrow().selected.clone() else {
            return;
        };
        let all = self.opts.borrow().all.clone();
        let Some(all) = all else {
            let ou = self.win.global::<OptionsState>();
            ui.set_r_loading(ou.get_loading());
            ui.set_r_error(ou.get_load_error());
            ui.set_related(model(vec![]));
            return;
        };
        ui.set_r_loading(false);
        ui.set_r_error(s(""));
        let related = search::related_options(&all, &attr);
        let defined = self.defined.borrow();
        ui.set_r_modules(s(related.modules.join(", ")));
        ui.set_related(model(
            related
                .options
                .iter()
                .map(|&i| RelatedRow {
                    name: s(&all[i].name),
                    r#type: s(&all[i].ty),
                    set: defined.contains(&all[i].name),
                })
                .collect(),
        ));
    }

    // ---- review & apply ---------------------------------------------------

    fn load_changes(&self) {
        let ui = self.win.global::<ApplyState>();
        match self.be.get_changes() {
            Ok(c) => {
                ui.set_loaded(true);
                ui.set_path(s(&c.path));
                ui.set_dirty(c.dirty);
                ui.set_diff(model(
                    c.diff
                        .iter()
                        .map(|l| {
                            let hunk = l.kind == "hunk";
                            let num = |n: Option<usize>| {
                                s(if hunk {
                                    String::new()
                                } else {
                                    n.map(|n| n.to_string()).unwrap_or_default()
                                })
                            };
                            DiffRow {
                                kind: s(l.kind),
                                old: num(l.old_line),
                                new: num(l.new_line),
                                text: s(&l.text),
                            }
                        })
                        .collect(),
                ));
            }
            Err(e) => ui.set_error(s(e)),
        }
    }

    fn after_apply_action(self: &Rc<Self>, r: Result<(), String>) {
        let ui = self.win.global::<ApplyState>();
        match r {
            Ok(()) => ui.set_error(s("")),
            Err(e) => ui.set_error(s(e)),
        }
        self.refresh();
    }

    // ---- runs -------------------------------------------------------------

    fn set_run(&self, target: RunTarget, info: RunInfo) {
        match target {
            RunTarget::Apply => self.win.global::<ApplyState>().set_run(info),
            RunTarget::Generations => self.win.global::<GenerationsState>().set_run(info),
            RunTarget::Channels => self.win.global::<ChannelsState>().set_run(info),
        }
    }

    fn set_run_error(&self, target: RunTarget, e: &str) {
        match target {
            RunTarget::Apply => self.win.global::<ApplyState>().set_error(s(e)),
            RunTarget::Generations => self.win.global::<GenerationsState>().set_error(s(e)),
            RunTarget::Channels => self.win.global::<ChannelsState>().set_error(s(e)),
        }
    }

    fn current_run(&self, target: RunTarget) -> RunInfo {
        match target {
            RunTarget::Apply => self.win.global::<ApplyState>().get_run(),
            RunTarget::Generations => self.win.global::<GenerationsState>().get_run(),
            RunTarget::Channels => self.win.global::<ChannelsState>().get_run(),
        }
    }

    /// Shows a run that was just started, or the error that stopped it.
    fn begin_run(self: &Rc<Self>, target: RunTarget, title: String, r: Result<String, String>) {
        let log = self.logs[target as usize].clone();
        let id = match r {
            Ok(id) => id,
            Err(e) => {
                self.set_run_error(target, &e);
                return;
            }
        };
        self.set_run_error(target, "");
        log.set_vec(vec![]);
        self.set_run(
            target,
            RunInfo {
                visible: true,
                title: s(title),
                running: true,
                ok: false,
                status: s(""),
            },
        );
        match target {
            RunTarget::Apply => self
                .win
                .global::<ApplyState>()
                .set_log(ModelRc::from(log.clone())),
            RunTarget::Generations => self
                .win
                .global::<GenerationsState>()
                .set_log(ModelRc::from(log.clone())),
            RunTarget::Channels => self
                .win
                .global::<ChannelsState>()
                .set_log(ModelRc::from(log.clone())),
        }
        let early = {
            let mut runs = self.runs.borrow_mut();
            runs.targets.insert(id.clone(), target);
            runs.early.remove(&id).unwrap_or_default()
        };
        for ev in early {
            self.on_run_event(ev);
        }
    }

    fn on_run_event(self: &Rc<Self>, ev: RunEvent) {
        let id = match &ev {
            RunEvent::Log { run_id, .. } | RunEvent::Done { run_id, .. } => run_id.clone(),
        };
        let Some(target) = self.runs.borrow().targets.get(&id).copied() else {
            self.runs.borrow_mut().early.entry(id).or_default().push(ev);
            return;
        };
        let log = self.logs[target as usize].clone();
        match ev {
            RunEvent::Log { stream, line, .. } => log.push(LogRow {
                text: s(line),
                stream: s(stream),
            }),
            RunEvent::Done { success, code, .. } => {
                self.runs.borrow_mut().targets.remove(&id);
                let mut info = self.current_run(target);
                info.running = false;
                info.ok = success;
                info.status = s(match (success, code) {
                    (true, _) => "Succeeded".to_string(),
                    (false, Some(c)) => format!("Failed (exit {c})"),
                    (false, None) => "Failed".to_string(),
                });
                self.set_run(target, info);
                match target {
                    RunTarget::Apply => self.refresh(),
                    RunTarget::Generations => self.load_generations(),
                    RunTarget::Channels => {
                        self.win.global::<ChannelsState>().set_update_ok(success);
                        self.load_channels();
                    }
                }
            }
        }
    }

    // ---- generations ------------------------------------------------------

    fn load_generations(self: &Rc<Self>) {
        let be = self.be.clone();
        self.spawn(async move { be.list_generations().await }, |app, r| {
            let ui = app.win.global::<GenerationsState>();
            ui.set_loading(false);
            match r {
                Ok(gens) => {
                    ui.set_error(s(""));
                    ui.set_rows(model(
                        gens.iter()
                            .map(|g| GenRow {
                                id: g.id as i32,
                                date: s(&g.date),
                                path: s(&g.path),
                                current: g.current,
                            })
                            .collect(),
                    ));
                }
                Err(e) => ui.set_error(s(e)),
            }
        });
    }

    // ---- channels ---------------------------------------------------------

    fn load_channels(self: &Rc<Self>) {
        let ui = self.win.global::<ChannelsState>();
        ui.set_uses_flake(!self.be.get_settings().flake_ref.trim().is_empty());
        let be = self.be.clone();
        self.spawn(async move { be.list_channels().await }, |app, r| {
            let ui = app.win.global::<ChannelsState>();
            ui.set_loading(false);
            match r {
                Ok(c) => {
                    ui.set_error(s(""));
                    let row = |c: &crate::channels::Channel| ChannelRow {
                        name: s(&c.name),
                        url: s(c.url.as_deref().unwrap_or("")),
                        version: s(c.version.as_deref().unwrap_or("")),
                        revision: s(c.revision.as_deref().unwrap_or("")),
                    };
                    ui.set_user(model(c.user.iter().map(row).collect()));
                    ui.set_system(model(c.system.iter().map(row).collect()));
                    ui.set_user_updated(s(text::format_time(c.user_updated)));
                    ui.set_system_updated(s(text::format_time(c.system_updated)));
                    ui.set_warnings(model(c.warnings.iter().map(s).collect()));
                }
                Err(e) => ui.set_error(s(e)),
            }
        });
    }

    fn channel_action<F>(self: &Rc<Self>, fut: F, done: String, on_ok: fn(&ChannelsState))
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let ui = self.win.global::<ChannelsState>();
        ui.set_error(s(""));
        ui.set_message(s(""));
        self.spawn(fut, move |app, r| {
            let ui = app.win.global::<ChannelsState>();
            match r {
                Ok(()) => {
                    on_ok(&ui);
                    ui.set_message(s(done));
                }
                Err(e) => ui.set_error(s(e)),
            }
            app.load_channels();
        });
    }

    // ---- settings ---------------------------------------------------------

    fn load_settings(&self) {
        let st = self.be.get_settings();
        let ui = self.win.global::<SettingsState>();
        ui.set_mode(s(if st.mode == Mode::Mock {
            "mock"
        } else {
            "real"
        }));
        ui.set_config_file(s(&st.config_file));
        ui.set_flake_ref(s(&st.flake_ref));
        ui.set_options_json(s(&st.options_json));
        ui.set_hm_flake(s(&st.home_manager_flake));
        ui.set_nixos_channel(s(&st.nixos_search_channel));
        self.win
            .global::<PackagesState>()
            .set_source(i32::from(st.package_search == PackageSearch::NixosOrg));
    }

    fn save_settings(self: &Rc<Self>) {
        let ui = self.win.global::<SettingsState>();
        let settings = Settings {
            mode: if ui.get_mode() == "mock" {
                Mode::Mock
            } else {
                Mode::Real
            },
            config_file: ui.get_config_file().trim().to_string(),
            flake_ref: ui.get_flake_ref().trim().to_string(),
            options_json: ui.get_options_json().trim().to_string(),
            package_search: self.be.get_settings().package_search,
            nixos_search_channel: ui.get_nixos_channel().trim().to_string(),
            home_manager_flake: ui.get_hm_flake().trim().to_string(),
        };
        match self.be.save_settings(settings) {
            Ok(()) => {
                ui.set_message(s("Settings saved."));
                ui.set_message_ok(true);
                // Everything may now come from somewhere else.
                {
                    let mut o = self.opts.borrow_mut();
                    o.all = None;
                    o.selected = None;
                    o.editor_key.clear();
                }
                self.win.global::<OptionsState>().set_selected(s(""));
                {
                    let mut pk = self.pkgs.borrow_mut();
                    pk.results.clear();
                    pk.warning = None;
                }
                let pu = self.win.global::<PackagesState>();
                pu.set_summary(s(""));
                pu.set_no_results(false);
                self.load_options(false);
                self.refresh();
                self.load_generations();
                self.load_channels();
            }
            Err(e) => {
                ui.set_message(s(e));
                ui.set_message_ok(false);
            }
        }
    }
}

fn is_placeholder(part: &str) -> bool {
    part == "*" || (part.starts_with('<') && part.ends_with('>'))
}
