//! Text shown in the UI that needs some computing: option descriptions,
//! package platforms, timestamps and channel URL suggestions.

use std::sync::LazyLock;

use regex::Regex;

use crate::nix::PackageDetails;

static ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\w+\}(`[^`]*`)").unwrap());

/// Option descriptions are CommonMark with MyST roles like {command}`nvim`.
/// Returns Markdown that `StyledText` can render, with roles turned into code
/// spans and raw HTML escaped, since `StyledText` only accepts a few tags.
pub fn description_markdown(text: &str) -> String {
    let text = ROLE.replace_all(text.trim(), "$1");
    // Escape `<` outside code spans; inside them it's literal already.
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for c in text.chars() {
        match c {
            '`' => in_code = !in_code,
            '<' if !in_code => {
                out.push('\\');
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// The description as plain text, for when the Markdown can't be rendered.
pub fn description_plain(text: &str) -> String {
    ROLE.replace_all(text.trim(), "$1").replace('`', "")
}

const OS_LABELS: &[(&str, &str)] = &[
    ("linux", "Linux"),
    ("darwin", "macOS"),
    ("freebsd", "FreeBSD"),
    ("openbsd", "OpenBSD"),
    ("netbsd", "NetBSD"),
    ("windows", "Windows"),
    ("cygwin", "Cygwin"),
];

fn os_label(os: &str) -> String {
    OS_LABELS
        .iter()
        .find(|(k, _)| *k == os)
        .map_or_else(|| os.to_string(), |(_, v)| v.to_string())
}

/// "Linux" or "macOS", for comparing with `group_platforms` labels.
pub fn current_os_label() -> String {
    os_label(match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    })
}

/// The OSes a package supports, e.g. "Linux · macOS"; "Any platform" when
/// unrestricted. The bool says whether this machine's OS is among them.
pub fn platforms_summary(platforms: &[String]) -> (String, bool) {
    let oses: Vec<String> = group_platforms(platforms)
        .into_iter()
        .map(|(os, _)| os)
        .filter(|os| !os.starts_with('('))
        .collect();
    if oses.is_empty() {
        return ("Any platform".into(), true);
    }
    let here = oses.contains(&current_os_label());
    (oses.join(" · "), here)
}

/// Groups systems like `x86_64-linux` by OS: `[("Linux", ["x86_64", "aarch64"]), ...]`.
pub fn group_platforms(platforms: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for p in platforms {
        let (arch, os) = p.split_once('-').unwrap_or(("", p));
        let label = os_label(os);
        let idx = match groups.iter().position(|(l, _)| *l == label) {
            Some(i) => i,
            None => {
                groups.push((label, vec![]));
                groups.len() - 1
            }
        };
        // `*-linux` is a pattern meaning every architecture.
        let name = if arch == "*" {
            "all architectures"
        } else {
            arch
        };
        let arches = &mut groups[idx].1;
        if !name.is_empty() && !arches.iter().any(|a| a == name) {
            arches.push(name.to_string());
        }
    }
    groups
}

fn matches(platform: &str, system: &str) -> bool {
    platform == system
        || platform
            .strip_prefix('*')
            .is_some_and(|rest| rest.starts_with('-') && system.ends_with(rest))
}

/// Why a package won't build on this machine, or None if it will.
pub fn unavailable_reason(d: &PackageDetails) -> Option<String> {
    if d.available {
        return None;
    }
    let attr = &d.attr;
    let system = d.system.as_deref().unwrap_or("this system");
    if d.broken {
        return Some(format!(
            "{attr} is marked broken in nixpkgs, so it won't build."
        ));
    }
    if let Some(sys) = d.system.as_deref() {
        if !d.platforms.is_empty() && !d.platforms.iter().any(|p| matches(p, sys)) {
            let supported: Vec<String> = group_platforms(&d.platforms)
                .into_iter()
                .map(|(os, _)| os)
                .collect();
            return Some(format!(
                "{attr} isn't available on {system}. It supports {}.",
                supported.join(", ")
            ));
        }
        if d.bad_platforms.iter().any(|p| matches(p, sys)) {
            return Some(format!("{attr} is marked as not working on {system}."));
        }
    }
    if d.unfree {
        return Some(format!(
            "{attr} is unfree. It only builds with nixpkgs.config.allowUnfree = true."
        ));
    }
    Some(format!(
        "{attr} can't be built on {system} with your current nixpkgs settings (it may be marked insecure)."
    ))
}

/// Local date and time for Unix seconds, or "never".
pub fn format_time(secs: Option<u64>) -> String {
    secs.and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "never".into())
}

const NIXPKGS_URL: &str = "https://nixos.org/channels/";
const HM_URL: &str = "https://github.com/nix-community/home-manager/archive/";

/// The two newest stable releases (they come out in May and November).
fn stable_releases(year: i32, month: u32) -> [String; 2] {
    let yy = year % 100;
    let latest = if month >= 11 {
        (yy, 11)
    } else if month >= 5 {
        (yy, 5)
    } else {
        (yy - 1, 11)
    };
    let previous = if latest.1 == 11 {
        (latest.0, 5)
    } else {
        (latest.0 - 1, 11)
    };
    [latest, previous].map(|(y, m)| format!("{y}.{m:02}"))
}

pub fn suggested_channel_urls() -> Vec<String> {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let releases = stable_releases(now.year(), now.month());
    let mac = cfg!(target_os = "macos");
    let mut urls = vec![format!("{NIXPKGS_URL}nixpkgs-unstable")];
    if !mac {
        urls.push(format!("{NIXPKGS_URL}nixos-unstable"));
    }
    for r in &releases {
        urls.push(if mac {
            format!("{NIXPKGS_URL}nixpkgs-{r}-darwin")
        } else {
            format!("{NIXPKGS_URL}nixos-{r}")
        });
    }
    urls.push(format!("{HM_URL}master.tar.gz"));
    for r in &releases {
        urls.push(format!("{HM_URL}release-{r}.tar.gz"));
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_descriptions() {
        assert_eq!(
            description_markdown("Run {command}`nvim` with <name> and `<x>`."),
            "Run `nvim` with \\<name> and `<x>`."
        );
        assert_eq!(description_plain("Set {env}`EDITOR`."), "Set EDITOR.");
    }

    #[test]
    fn groups_platforms() {
        let p: Vec<String> = ["x86_64-linux", "aarch64-linux", "*-darwin"]
            .map(String::from)
            .into();
        assert_eq!(
            group_platforms(&p),
            vec![
                ("Linux".into(), vec!["x86_64".into(), "aarch64".into()]),
                ("macOS".into(), vec!["all architectures".into()])
            ]
        );
        assert!(matches("*-darwin", "aarch64-darwin"));
        assert!(!matches("*-darwin", "aarch64-linux"));
    }

    #[test]
    fn summarizes_platforms() {
        let p: Vec<String> = ["x86_64-linux", "aarch64-darwin", "(pattern)"]
            .map(String::from)
            .into();
        assert_eq!(platforms_summary(&p), ("Linux · macOS".into(), true));
        assert_eq!(platforms_summary(&[]), ("Any platform".into(), true));
        let other = if cfg!(target_os = "macos") {
            "x86_64-linux"
        } else {
            "aarch64-darwin"
        };
        assert!(!platforms_summary(&[other.to_string()]).1);
    }

    #[test]
    fn picks_releases() {
        assert_eq!(stable_releases(2026, 11), ["26.11", "26.05"]);
        assert_eq!(stable_releases(2026, 6), ["26.05", "25.11"]);
        assert_eq!(stable_releases(2026, 2), ["25.11", "25.05"]);
    }
}
