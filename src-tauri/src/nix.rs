//! The real backend: shells out to nix and home-manager.

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::Serialize;

use crate::options::HmOption;
use crate::settings::{Settings, expand_home};

const NIX_FEATURES: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    pub attr: String,
    pub pname: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    pub id: u64,
    pub date: String,
    pub path: String,
    pub current: bool,
}

/// Apps launched from Finder or a desktop menu don't inherit the shell's PATH,
/// so add the places nix and home-manager are usually installed.
fn search_path() -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".nix-profile/bin"));
    }
    if let Ok(user) = std::env::var("USER") {
        dirs.push(PathBuf::from(format!("/etc/profiles/per-user/{user}/bin")));
    }
    dirs.push("/run/current-system/sw/bin".into());
    dirs.push("/nix/var/nix/profiles/default/bin".into());
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.env("PATH", search_path()).kill_on_drop(true);
    cmd
}

fn find_in_path(program: &str) -> bool {
    std::env::split_paths(&search_path()).any(|d| d.join(program).is_file())
}

pub fn nix_available() -> bool {
    find_in_path("nix")
}

pub fn home_manager_available() -> bool {
    find_in_path("home-manager")
}

/// Runs a command to completion and returns its stdout.
pub(crate) async fn capture(program: &str, args: &[String]) -> Result<String> {
    let out = command(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("couldn't run {program}; is it installed?"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("{program} failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn nix_args(rest: &[&str]) -> Vec<String> {
    NIX_FEATURES
        .iter()
        .chain(rest)
        .map(|s| s.to_string())
        .collect()
}

pub async fn load_options(settings: &Settings) -> Result<Vec<HmOption>> {
    let path = if !settings.options_json.trim().is_empty() {
        expand_home(settings.options_json.trim())
    } else {
        PathBuf::from(build_docs(settings).await?).join("share/doc/home-manager/options.json")
    };
    let json = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("couldn't read {}", path.display()))?;
    crate::options::parse_options_json(&json)
}

/// Builds home-manager's docs and returns the output path. Prefers the
/// home-manager the user actually has (their channel, or the input pinned in
/// their flake) so options match their version, and only then falls back to
/// the configured home-manager flake.
async fn build_docs(settings: &Settings) -> Result<String> {
    let mut errors = Vec::new();
    let flake = settings.flake_ref.trim();

    let first = if flake.is_empty() {
        let args = ["--no-out-link", "<home-manager>", "-A", "docs.json"].map(String::from);
        (
            "your home-manager channel",
            capture("nix-build", &args).await,
        )
    } else {
        let dir = flake.split_once('#').map_or(flake, |(dir, _)| dir);
        let dir = expand_home(dir).display().to_string();
        let args = nix_args(&[
            "build",
            "--no-link",
            "--print-out-paths",
            "--inputs-from",
            &dir,
            "home-manager#docs-json",
        ]);
        (
            "the home-manager input of your flake",
            capture("nix", &args).await,
        )
    };
    match first {
        (_, Ok(out)) => return last_line(&out),
        (source, Err(e)) => errors.push(format!("From {source}: {e:#}")),
    }

    let installable = format!("{}#docs-json", settings.home_manager_flake.trim());
    let args = nix_args(&["build", "--no-link", "--print-out-paths", &installable]);
    match capture("nix", &args).await {
        Ok(out) => return last_line(&out),
        Err(e) => errors.push(format!("From {installable}: {e:#}")),
    }

    let mut message = errors.join("\n\n");
    if message.contains("unsupported tarball input attribute") {
        message.push_str(
            "\n\nThis usually means your Nix is older than the one that wrote home-manager's \
             flake.lock. Check `nix --version` and upgrade Nix, or set an options.json path in Settings.",
        );
    }
    bail!("couldn't build home-manager's option docs:\n\n{message}")
}

fn last_line(out: &str) -> Result<String> {
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("the build printed no output path"))
}

/// Renders an option path as a Nix attribute selection, quoting every part.
fn select_path(loc: &[String]) -> Result<String> {
    if loc.is_empty() {
        bail!("empty option path");
    }
    loc.iter()
        .map(|k| {
            if k.contains('\n') || (k.starts_with('<') && k.ends_with('>')) || k == "*" {
                bail!("fill in the `{k}` placeholder first");
            }
            Ok(crate::nix_edit::to_nix(&serde_json::Value::String(
                k.clone(),
            )))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("."))
}

/// The option's final value after all modules are merged.
pub async fn eval_option(settings: &Settings, loc: &[String]) -> Result<String> {
    let select = select_path(loc)?;
    let out = match settings.flake_ref.trim().split_once('#') {
        Some((dir, name)) => {
            let target = format!(
                "{}#homeConfigurations.{}.config.{select}",
                expand_home(dir).display(),
                crate::nix_edit::to_nix(&name.into())
            );
            capture("nix", &nix_args(&["eval", "--json", &target])).await?
        }
        None if !settings.flake_ref.trim().is_empty() => {
            bail!("the flake reference needs a `#name` part")
        }
        None => {
            let expr = format!(
                "{{ confPath }}: (import <home-manager/home-manager/home-manager.nix> {{ confPath = /. + confPath; }}).config.{select}"
            );
            let conf = settings.config_path().display().to_string();
            let args = [
                "--eval", "--strict", "--json", "-E", &expr, "--argstr", "confPath", &conf,
            ];
            capture("nix-instantiate", &args.map(String::from)).await?
        }
    };
    let value: serde_json::Value =
        serde_json::from_str(out.trim()).context("nix returned invalid JSON")?;
    Ok(serde_json::to_string_pretty(&value)?)
}

pub async fn search_packages(query: &str) -> Result<Vec<PackageInfo>> {
    let out = capture("nix", &nix_args(&["search", "nixpkgs", query, "--json"])).await?;
    parse_search(&out)
}

fn parse_search(json: &str) -> Result<Vec<PackageInfo>> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)?;
    let mut out: Vec<PackageInfo> = map
        .into_iter()
        .map(|(key, v)| PackageInfo {
            // Keys look like `legacyPackages.aarch64-darwin.ripgrep`.
            attr: key.splitn(3, '.').nth(2).unwrap_or(&key).to_string(),
            pname: v["pname"].as_str().unwrap_or_default().to_string(),
            version: v["version"].as_str().unwrap_or_default().to_string(),
            description: v["description"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    out.sort_by(|a, b| a.attr.cmp(&b.attr));
    Ok(out)
}

/// Arguments for `home-manager build|switch`.
pub fn home_manager_args(settings: &Settings, action: &str) -> Vec<String> {
    let mut args = vec![action.to_string()];
    let flake = settings.flake_ref.trim();
    if flake.is_empty() {
        args.extend(["-f".into(), settings.config_path().display().to_string()]);
    } else {
        args.extend([
            "--flake".into(),
            match flake.split_once('#') {
                Some((dir, name)) => format!("{}#{name}", expand_home(dir).display()),
                None => expand_home(flake).display().to_string(),
            },
        ]);
    }
    args
}

pub async fn list_generations() -> Result<Vec<Generation>> {
    let out = capture("home-manager", &["generations".to_string()]).await?;
    Ok(parse_generations(&out))
}

static GENERATION_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+ \S+) : id (\d+) -> (\S+)(\s+\(current\))?\s*$").unwrap());

fn parse_generations(text: &str) -> Vec<Generation> {
    let mut gens: Vec<Generation> = text
        .lines()
        .filter_map(|l| GENERATION_LINE.captures(l.trim()))
        .map(|c| Generation {
            id: c[2].parse().unwrap_or(0),
            date: c[1].to_string(),
            path: c[3].to_string(),
            current: c.get(4).is_some(),
        })
        .collect();
    // Older home-manager versions don't mark the current generation; it's the newest.
    if !gens.iter().any(|g| g.current)
        && let Some(newest) = gens.iter_mut().max_by_key(|g| g.id)
    {
        newest.current = true;
    }
    gens
}

/// A generation is activated by running its `activate` script.
pub fn activation_script(path: &str) -> Result<String> {
    if !path.starts_with("/nix/store/") || path.contains("..") {
        bail!("refusing to activate a path outside /nix/store: {path}");
    }
    Ok(format!("{path}/activate"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generations() {
        let text = "2024-06-02 09:15 : id 12 -> /nix/store/abc-home-manager-generation (current)\n\
                    2024-06-01 18:40 : id 11 -> /nix/store/def-home-manager-generation\n";
        let gens = parse_generations(text);
        assert_eq!(gens.len(), 2);
        assert!(gens[0].current && !gens[1].current);
        assert_eq!(gens[1].id, 11);

        let unmarked = parse_generations(
            "2024-06-01 18:40 : id 11 -> /nix/store/def\n2024-06-02 09:15 : id 12 -> /nix/store/abc\n",
        );
        assert!(unmarked[1].current && !unmarked[0].current);
    }

    #[test]
    fn parses_search_results() {
        let json = r#"{"legacyPackages.aarch64-darwin.ripgrep":{"description":"fast grep","pname":"ripgrep","version":"14.1.0"},
                      "legacyPackages.aarch64-darwin.python3Packages.black":{"description":"","pname":"black","version":"24"}}"#;
        let res = parse_search(json).unwrap();
        assert_eq!(res[0].attr, "python3Packages.black");
        assert_eq!(res[1].pname, "ripgrep");
    }

    #[test]
    fn quotes_selection_paths() {
        let loc = ["xdg", "configFile", "a\"b.conf", "text"].map(String::from);
        assert_eq!(
            select_path(&loc).unwrap(),
            r#""xdg"."configFile"."a\"b.conf"."text""#
        );
        assert!(select_path(&["xdg", "configFile", "<name>"].map(String::from)).is_err());
    }

    #[test]
    fn builds_home_manager_args() {
        let mut s = Settings {
            mode: crate::settings::Mode::Real,
            config_file: "/cfg/home.nix".into(),
            flake_ref: String::new(),
            options_json: String::new(),
            home_manager_flake: String::new(),
        };
        assert_eq!(
            home_manager_args(&s, "build"),
            vec!["build", "-f", "/cfg/home.nix"]
        );
        s.flake_ref = "/cfg#me".into();
        assert_eq!(
            home_manager_args(&s, "switch"),
            vec!["switch", "--flake", "/cfg#me"]
        );
    }

    #[test]
    fn takes_the_last_output_path() {
        let out = "these 2 paths will be fetched\n/nix/store/abc-docs-json\n\n";
        assert_eq!(last_line(out).unwrap(), "/nix/store/abc-docs-json");
        assert!(last_line("\n").is_err());
    }

    #[test]
    fn only_activates_store_paths() {
        assert!(activation_script("/nix/store/abc-hm").is_ok());
        assert!(activation_script("/tmp/evil").is_err());
    }
}
