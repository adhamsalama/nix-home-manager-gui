//! Mock backend for developing without nix: fixture options and packages,
//! in-memory generations and simulated command output. File edits are not
//! mocked; they go to a real sample file.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::channels::{self, Channel, Channels};
use crate::nix::{Generation, PackageDetails, PackageInfo, SearchResults};
use crate::nix_edit::{self, FileValue};
use crate::options::HmOption;

pub const SAMPLE_HOME: &str = include_str!("../fixtures/sample-home.nix");
const OPTIONS_JSON: &str = include_str!("../fixtures/options.json");
const PACKAGES_JSON: &str = include_str!("../fixtures/packages.json");

type Lines = Vec<(&'static str, String)>;

pub fn options() -> Result<Vec<HmOption>> {
    crate::options::parse_options_json(OPTIONS_JSON)
}

pub fn search_packages(query: &str) -> Result<SearchResults> {
    let all: Vec<serde_json::Value> = serde_json::from_str(PACKAGES_JSON)?;
    let q = query.to_lowercase();
    let packages = all
        .into_iter()
        .map(|v| PackageInfo {
            attr: v["attr"].as_str().unwrap_or_default().into(),
            pname: v["pname"].as_str().unwrap_or_default().into(),
            version: v["version"].as_str().unwrap_or_default().into(),
            description: v["description"].as_str().unwrap_or_default().into(),
        })
        .filter(|p| p.attr.to_lowercase().contains(&q) || p.description.to_lowercase().contains(&q))
        .collect();
    Ok(SearchResults {
        source: "mock data".into(),
        packages,
    })
}

pub fn package_details(attr: &str) -> Result<PackageDetails> {
    let found = search_packages(attr)?
        .packages
        .into_iter()
        .find(|p| p.attr == attr);
    Ok(PackageDetails {
        attr: attr.to_string(),
        name: found.as_ref().map(|p| p.pname.clone()),
        version: found.as_ref().map(|p| p.version.clone()),
        description: found.map(|p| p.description),
        homepage: None,
        licenses: vec![],
        source: "mock data".into(),
    })
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
    channels: Vec<Channel>,
    /// Earlier channel states, for `nix-channel --rollback`.
    channel_history: Vec<(Vec<Channel>, Option<u64>)>,
    channels_updated: Option<u64>,
    /// Bumped on each update so versions visibly change.
    channel_serial: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What a channel at `url` would contain after downloading it.
fn mock_channel(name: &str, url: &str, serial: u64) -> Channel {
    let release = url.rsplit(['/', '-']).find_map(|part| {
        let part = part.trim_end_matches(".tar.gz");
        let ok = part.len() == 5
            && part.as_bytes()[2] == b'.'
            && part.chars().filter(char::is_ascii_digit).count() == 4;
        ok.then(|| part.to_string())
    });
    let revision = format!("{:0>12x}", 0x5eed_0000_u64 + serial * 7919);
    let (kind, version) = if url.contains("home-manager") {
        ("home-manager", release.unwrap_or_else(|| "26.11".into()))
    } else if url.contains("nixos.org/channels") {
        let version = match release {
            Some(r) => format!("{r}.{}.{}", 880_000 + serial, &revision[..7]),
            None => format!("26.11pre{}.{}", 880_000 + serial, &revision[..7]),
        };
        ("nixpkgs", version)
    } else {
        ("other", String::new())
    };
    Channel {
        name: name.into(),
        url: Some(url.into()),
        version: (!version.is_empty()).then_some(version),
        revision: (kind == "nixpkgs").then_some(revision),
        kind,
    }
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
        let channels = vec![
            mock_channel(
                "home-manager",
                "https://github.com/nix-community/home-manager/archive/master.tar.gz",
                0,
            ),
            mock_channel("nixpkgs", "https://nixos.org/channels/nixpkgs-unstable", 0),
        ];
        MockState {
            generations,
            channels,
            channel_history: Vec::new(),
            channels_updated: Some(now() - 3 * 86_400),
            channel_serial: 1,
        }
    }
}

impl MockState {
    pub fn list_channels(&self) -> Channels {
        let system = vec![mock_channel(
            "nixpkgs",
            "https://nixos.org/channels/nixpkgs-26.05-darwin",
            0,
        )];
        Channels {
            warnings: channels::warnings(&self.channels, &system),
            user: self.channels.clone(),
            system,
            user_updated: self.channels_updated,
            system_updated: Some(now() - 40 * 86_400),
        }
    }

    /// Adds a channel, or changes the URL of an existing one. Like
    /// `nix-channel --add`, nothing is downloaded until the next update.
    pub fn add_channel(&mut self, name: &str, url: &str) -> Result<()> {
        channels::validate(name, Some(url))?;
        let pending = Channel {
            name: name.into(),
            url: Some(url.into()),
            version: None,
            revision: None,
            kind: "other",
        };
        match self.channels.iter_mut().find(|c| c.name == name) {
            Some(existing) => existing.url = Some(url.into()),
            None => {
                self.channels.push(pending);
                self.channels.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
        Ok(())
    }

    pub fn remove_channel(&mut self, name: &str) -> Result<()> {
        let before = self.channels.len();
        self.channels.retain(|c| c.name != name);
        if self.channels.len() == before {
            bail!("there's no channel named {name}");
        }
        Ok(())
    }

    pub fn update_channels(&mut self, name: Option<&str>) -> (Lines, bool) {
        if let Some(name) = name
            && !self.channels.iter().any(|c| c.name == name)
        {
            return (
                vec![("stderr", format!("error: channel '{name}' does not exist"))],
                false,
            );
        }
        self.channel_history
            .push((self.channels.clone(), self.channels_updated));
        let serial = self.channel_serial;
        self.channel_serial += 1;
        let mut lines: Lines = vec![(
            "info",
            format!("$ nix-channel --update {}   (mock)", name.unwrap_or("")),
        )];
        for c in &mut self.channels {
            if name.is_some_and(|n| n != c.name) {
                continue;
            }
            let url = c.url.clone().unwrap_or_default();
            lines.push(("stderr", "unpacking 1 channels...".into()));
            lines.push(("stderr", format!("downloading '{url}'...")));
            *c = mock_channel(&c.name, &url, serial);
        }
        lines.push((
            "stderr",
            "building '/nix/store/7x1…-user-environment.drv'...".into(),
        ));
        lines.push(("stdout", "created new channels generation".into()));
        self.channels_updated = Some(now());
        (lines, true)
    }

    pub fn rollback_channels(&mut self) -> Result<String> {
        let Some((channels, updated)) = self.channel_history.pop() else {
            bail!("no generation older than the current channels generation");
        };
        self.channels = channels;
        self.channels_updated = updated;
        Ok("switched to the previous channels generation".into())
    }

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
