//! Nix channels for classic (non-flake) setups.
//!
//! The user's own channels are managed with `nix-channel`. Root's channels
//! can't be changed without sudo, so they are only read, from the
//! world-readable `~/.nix-defexpr/channels_root` link.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub name: String,
    /// Only known for the user's channels; root's list isn't readable.
    pub url: Option<String>,
    /// e.g. `25.05.806123.abc1234` for nixpkgs or `25.05` for home-manager.
    pub version: Option<String>,
    pub revision: Option<String>,
    /// `nixpkgs`, `home-manager` or `other`, judged from the channel's files.
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channels {
    pub user: Vec<Channel>,
    pub system: Vec<Channel>,
    /// Unix seconds of the last update of each channel profile.
    pub user_updated: Option<u64>,
    pub system_updated: Option<u64>,
    pub warnings: Vec<String>,
}

/// Parses `nix-channel --list` output: one `name url` pair per line.
pub fn parse_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect()
}

pub fn validate(name: &str, url: Option<&str>) -> Result<()> {
    let name_ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(['-', '.']);
    if !name_ok {
        bail!("`{name}` isn't a valid channel name (letters, digits, - _ . only)");
    }
    if let Some(url) = url
        && (!url.starts_with("https://") || url.chars().any(char::is_whitespace))
    {
        bail!("channel URLs must start with https://");
    }
    Ok(())
}

/// Reads version information from an unpacked channel directory.
pub fn read_channel_dir(dir: &Path) -> (&'static str, Option<String>, Option<String>) {
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if let Some(release) = read("release.json")
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
        .and_then(|v| v["release"].as_str().map(str::to_string))
    {
        return ("home-manager", Some(release), None);
    }
    if let Some(version) = read(".version") {
        let suffix = read(".version-suffix").unwrap_or_default();
        let revision = read(".git-revision").map(|r| r.chars().take(12).collect());
        return ("nixpkgs", Some(format!("{version}{suffix}")), revision);
    }
    ("other", None, None)
}

/// Channel directories in a channels profile, sorted by name.
fn channel_dirs(profile: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(profile) else {
        return vec![];
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    dirs.sort();
    dirs
}

/// When the profile link was last switched, i.e. the last channel update.
fn profile_updated(link: &Path) -> Option<u64> {
    let target = std::fs::read_link(link).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        link.parent()?.join(target)
    };
    let modified = std::fs::symlink_metadata(target).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

fn defexpr_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    vec![
        home.join(".nix-defexpr"),
        home.join(".local/state/nix/defexpr"),
    ]
}

fn find_link(name: &str) -> Option<PathBuf> {
    defexpr_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.exists())
}

fn channels_from(link: Option<&Path>, urls: &[(String, String)], with_urls: bool) -> Vec<Channel> {
    let mut found: Vec<Channel> = link
        .map(channel_dirs)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, dir)| {
            let (kind, version, revision) = read_channel_dir(&dir);
            let url = urls
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, u)| u.clone());
            Channel {
                name,
                url,
                version,
                revision,
                kind,
            }
        })
        .collect();
    // Channels that were added but never downloaded have no directory yet.
    if with_urls {
        for (name, url) in urls {
            if !found.iter().any(|c| &c.name == name) {
                found.push(Channel {
                    name: name.clone(),
                    url: Some(url.clone()),
                    version: None,
                    revision: None,
                    kind: "other",
                });
            }
        }
    }
    found
}

/// Warns when the nixpkgs and home-manager that `<nixpkgs>` and
/// `<home-manager>` resolve to come from different releases. User channels
/// shadow root's, as in the default NIX_PATH.
pub fn warnings(user: &[Channel], system: &[Channel]) -> Vec<String> {
    let pick = |kind: &str| {
        user.iter()
            .chain(system)
            .find(|c| c.kind == kind && c.version.is_some())
    };
    let mut warnings = Vec::new();
    if let (Some(np), Some(hm)) = (pick("nixpkgs"), pick("home-manager")) {
        let np_release: String = np
            .version
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(5)
            .collect();
        let hm_release = hm.version.as_deref().unwrap_or_default();
        if np_release != hm_release {
            warnings.push(format!(
                "home-manager ({hm_release}) and nixpkgs ({np_release}) are on different releases. \
                 Point both channels at the same release to avoid evaluation errors."
            ));
        }
    }
    if !user.iter().chain(system).any(|c| c.name == "home-manager") {
        warnings.push(
            "There's no home-manager channel. Standalone home-manager in classic mode needs one."
                .into(),
        );
    }
    warnings
}

pub async fn list() -> Result<Channels> {
    let urls = parse_list(&crate::nix::capture("nix-channel", &["--list".into()]).await?);
    let user_link = find_link("channels");
    let system_link = find_link("channels_root");
    let user = channels_from(user_link.as_deref(), &urls, true);
    let system = channels_from(system_link.as_deref(), &[], false);
    Ok(Channels {
        warnings: warnings(&user, &system),
        user_updated: user_link.as_deref().and_then(profile_updated),
        system_updated: system_link.as_deref().and_then(profile_updated),
        user,
        system,
    })
}

pub async fn add(name: &str, url: &str) -> Result<()> {
    validate(name, Some(url))?;
    crate::nix::capture("nix-channel", &["--add".into(), url.into(), name.into()]).await?;
    Ok(())
}

pub async fn remove(name: &str) -> Result<()> {
    validate(name, None)?;
    crate::nix::capture("nix-channel", &["--remove".into(), name.into()]).await?;
    Ok(())
}

pub async fn rollback() -> Result<String> {
    let out = crate::nix::capture("nix-channel", &["--rollback".into()]).await?;
    Ok(out.trim().to_string())
}

/// Arguments for `nix-channel --update [name]`, streamed by the runner.
pub fn update_args(name: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec!["--update".to_string()];
    if let Some(name) = name {
        validate(name, None)?;
        args.push(name.to_string());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, kind: &'static str, version: &str) -> Channel {
        Channel {
            name: name.into(),
            url: None,
            version: Some(version.into()),
            revision: None,
            kind,
        }
    }

    #[test]
    fn parses_channel_list() {
        let out = "home-manager https://github.com/nix-community/home-manager/archive/master.tar.gz\n\
                   nixpkgs https://nixos.org/channels/nixpkgs-unstable\n\n";
        let list = parse_list(out);
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[1],
            (
                "nixpkgs".into(),
                "https://nixos.org/channels/nixpkgs-unstable".into()
            )
        );
    }

    #[test]
    fn validates_names_and_urls() {
        assert!(validate("nixpkgs", Some("https://nixos.org/channels/nixos-25.05")).is_ok());
        assert!(validate("home-manager", None).is_ok());
        assert!(validate("--remove", None).is_err());
        assert!(validate("a b", None).is_err());
        assert!(validate("nixpkgs", Some("http://insecure")).is_err());
        assert!(validate("nixpkgs", Some("https://x y")).is_err());
    }

    #[test]
    fn reads_versions_from_channel_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let np = dir.path().join("nixpkgs");
        std::fs::create_dir(&np).unwrap();
        std::fs::write(np.join(".version"), "25.05\n").unwrap();
        std::fs::write(np.join(".version-suffix"), ".806123.abc1234").unwrap();
        std::fs::write(np.join(".git-revision"), "abc1234def567890abcdef").unwrap();
        assert_eq!(
            read_channel_dir(&np),
            (
                "nixpkgs",
                Some("25.05.806123.abc1234".into()),
                Some("abc1234def56".into())
            )
        );

        let hm = dir.path().join("home-manager");
        std::fs::create_dir(&hm).unwrap();
        std::fs::write(
            hm.join("release.json"),
            r#"{"release": "25.05", "isReleaseBranch": true}"#,
        )
        .unwrap();
        assert_eq!(
            read_channel_dir(&hm),
            ("home-manager", Some("25.05".into()), None)
        );

        let names: Vec<String> = channel_dirs(dir.path())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["home-manager", "nixpkgs"]);
    }

    #[test]
    fn lists_added_but_undownloaded_channels() {
        let urls = vec![(
            "unstable".to_string(),
            "https://nixos.org/channels/nixos-unstable".to_string(),
        )];
        let found = channels_from(None, &urls, true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].version, None);
        assert!(channels_from(None, &urls, false).is_empty());
    }

    #[test]
    fn warns_about_release_mismatches() {
        let user = vec![
            channel("nixpkgs", "nixpkgs", "25.05.806123.abc"),
            channel("home-manager", "home-manager", "25.05"),
        ];
        assert!(warnings(&user, &[]).is_empty());

        let mismatched = vec![
            channel("nixpkgs", "nixpkgs", "25.11pre880123.abc"),
            channel("home-manager", "home-manager", "25.05"),
        ];
        assert_eq!(warnings(&mismatched, &[]).len(), 1);

        // The user's nixpkgs shadows root's.
        let system = vec![channel("nixpkgs", "nixpkgs", "24.11.1.x")];
        assert!(warnings(&user, &system).is_empty());

        assert_eq!(
            warnings(&[channel("nixpkgs", "nixpkgs", "25.05.1")], &[]).len(),
            1
        );
    }
}
