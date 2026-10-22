//! Mock backend for developing without nix: fixture options and packages,
//! in-memory generations and simulated command output. File edits are not
//! mocked; they go to a real sample file.

use anyhow::Result;

use crate::nix::{Generation, PackageInfo};
use crate::nix_edit::{self, FileValue};
use crate::options::HmOption;

pub const SAMPLE_HOME: &str = include_str!("../fixtures/sample-home.nix");
const OPTIONS_JSON: &str = include_str!("../fixtures/options.json");
const PACKAGES_JSON: &str = include_str!("../fixtures/packages.json");

type Lines = Vec<(&'static str, String)>;

pub fn options() -> Result<Vec<HmOption>> {
    crate::options::parse_options_json(OPTIONS_JSON)
}

pub fn search_packages(query: &str) -> Result<Vec<PackageInfo>> {
    let all: Vec<serde_json::Value> = serde_json::from_str(PACKAGES_JSON)?;
    let q = query.to_lowercase();
    Ok(all
        .into_iter()
        .map(|v| PackageInfo {
            attr: v["attr"].as_str().unwrap_or_default().into(),
            pname: v["pname"].as_str().unwrap_or_default().into(),
            version: v["version"].as_str().unwrap_or_default().into(),
            description: v["description"].as_str().unwrap_or_default().into(),
        })
        .filter(|p| p.attr.to_lowercase().contains(&q) || p.description.to_lowercase().contains(&q))
        .collect())
}

/// Pretends to evaluate: the file's own expression if it sets the option,
/// otherwise the option's default.
pub fn eval_option(file_text: &str, loc: &[String], default: Option<&str>) -> Result<String> {
    Ok(match nix_edit::get_option(file_text, loc)? {
        FileValue::Set { expr, .. } => expr,
        _ => default.unwrap_or("null").to_string(),
    })
}

pub struct MockState {
    pub generations: Vec<Generation>,
}

impl Default for MockState {
    fn default() -> Self {
        let dates = [
            "2026-09-30 21:04",
            "2026-10-08 10:22",
            "2026-10-15 18:47",
            "2026-10-21 09:13",
        ];
        let generations = dates
            .iter()
            .enumerate()
            .map(|(i, date)| Generation {
                id: 40 + i as u64,
                date: date.to_string(),
                path: format!("/nix/store/{:0>32}-home-manager-generation", 40 + i),
                current: i == dates.len() - 1,
            })
            .rev()
            .collect();
        MockState { generations }
    }
}

impl MockState {
    /// Simulates `home-manager build|switch`. A switch adds a generation.
    pub fn run_home_manager(&mut self, action: &str, file: &str, file_text: &str) -> (Lines, bool) {
        let mut lines: Lines = vec![(
            "info",
            format!("$ home-manager {action} -f {file}   (mock)"),
        )];
        if let Err(e) = nix_edit::parse(file_text) {
            lines.push(("stderr", format!("error: syntax error in {file}: {e}")));
            return (lines, false);
        }
        lines.extend([
            ("stderr", "these 3 derivations will be built:".into()),
            ("stderr", "  /nix/store/0rx…-home-manager-files.drv".into()),
            ("stderr", "  /nix/store/9dz…-home-manager-path.drv".into()),
            (
                "stderr",
                "  /nix/store/k2a…-home-manager-generation.drv".into(),
            ),
            (
                "stderr",
                "building '/nix/store/0rx…-home-manager-files.drv'...".into(),
            ),
            (
                "stderr",
                "building '/nix/store/9dz…-home-manager-path.drv'...".into(),
            ),
            (
                "stderr",
                "building '/nix/store/k2a…-home-manager-generation.drv'...".into(),
            ),
        ]);
        if action == "build" {
            lines.push((
                "stdout",
                "No change so far; the result is linked at ./result".into(),
            ));
            return (lines, true);
        }
        let id = self.generations.iter().map(|g| g.id).max().unwrap_or(0) + 1;
        lines.extend([
            ("stdout", "Starting Home Manager activation".to_string()),
            ("stdout", "Activating checkFilesChanged".into()),
            ("stdout", "Activating checkLinkTargets".into()),
            ("stdout", "Activating writeBoundary".into()),
            ("stdout", "Activating linkGeneration".into()),
            ("stdout", "Creating home file links in /Users/demo".into()),
            ("stdout", "Activating onFilesChange".into()),
            ("stdout", format!("Creating profile generation {id}")),
        ]);
        for g in &mut self.generations {
            g.current = false;
        }
        self.generations.insert(
            0,
            Generation {
                id,
                date: "2026-10-22 12:00".into(),
                path: format!("/nix/store/{id:0>32}-home-manager-generation"),
                current: true,
            },
        );
        (lines, true)
    }

    pub fn activate(&mut self, id: u64) -> (Lines, bool) {
        let Some(path) = self
            .generations
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.path.clone())
        else {
            return (vec![("stderr", format!("no generation {id}"))], false);
        };
        for g in &mut self.generations {
            g.current = g.id == id;
        }
        let lines = vec![
            ("info", format!("$ {path}/activate   (mock)")),
            ("stdout", "Starting Home Manager activation".into()),
            ("stdout", "Activating linkGeneration".into()),
            ("stdout", format!("Switched to generation {id}")),
        ];
        (lines, true)
    }
}
