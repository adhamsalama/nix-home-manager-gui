//! Searching options, and finding the options that configure a package.

use std::collections::{BTreeSet, HashSet};

use crate::nix::PackageInfo;
use crate::options::HmOption;

/// Packages shown per page of search results.
pub const PACKAGE_PAGE_SIZE: usize = 50;

/// Orders `nix search` results, which come back alphabetically, so the
/// package named like the query comes first: exact name, then names that
/// start with the query, then names containing it, then description-only
/// matches. Top-level packages beat nested ones (`vimPlugins.…`), and shorter
/// names beat longer ones.
pub fn rank_packages(packages: &mut [PackageInfo], query: &str) {
    let q = query.trim().to_lowercase();
    let first = q.split_whitespace().next().unwrap_or("").to_string();
    let tier = |p: &PackageInfo| -> u8 {
        let attr = p.attr.to_lowercase();
        let pname = p.pname.to_lowercase();
        let leaf = attr.rsplit('.').next().unwrap_or(&attr).to_string();
        if attr == q || pname == q {
            0
        } else if leaf == first {
            1
        } else if attr.starts_with(&first) || pname.starts_with(&first) {
            2
        } else if leaf.starts_with(&first) {
            3
        } else if attr.contains(&first) || pname.contains(&first) {
            4
        } else {
            5
        }
    };
    packages.sort_by_cached_key(|p| {
        (
            tier(p),
            p.attr.contains('.'),
            p.attr.len(),
            p.attr.to_lowercase(),
        )
    });
}

/// Whether a package with these `meta.platforms` runs on `filter`: an OS
/// (`linux`, `darwin`) or a system (`aarch64-darwin`). Empty platforms mean
/// unrestricted.
pub fn supports_platform(platforms: &[String], filter: &str) -> bool {
    if platforms.is_empty() {
        return true;
    }
    platforms.iter().any(|p| {
        let (arch, os) = p.split_once('-').unwrap_or(("", p));
        if filter.contains('-') {
            let (farch, fos) = filter.split_once('-').unwrap_or(("", filter));
            os == fos && (arch == farch || arch == "*")
        } else {
            os == filter
        }
    })
}

/// Indices of matching options, best first. Every term must appear in the
/// name or description; name matches rank higher, prefix matches highest.
pub fn search(options: &[HmOption], query: &str, only: Option<&HashSet<String>>) -> Vec<usize> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut scored: Vec<(u32, usize)> = Vec::new();
    'options: for (i, o) in options.iter().enumerate() {
        if only.is_some_and(|set| !set.contains(&o.name)) {
            continue;
        }
        let name = o.name.to_lowercase();
        let mut desc: Option<String> = None;
        let mut score = 0;
        for t in &terms {
            if name.starts_with(t.as_str()) {
                score += 3;
            } else if name.contains(t.as_str()) {
                score += 2;
            } else if desc
                .get_or_insert_with(|| o.description.to_lowercase())
                .contains(t.as_str())
            {
                score += 1;
            } else {
                continue 'options;
            }
        }
        scored.push((score, i));
    }
    // Stable sort keeps alphabetical order within a score.
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(_, i)| i).collect()
}

pub struct Related {
    /// Module prefixes such as `programs.git`.
    pub modules: Vec<String>,
    pub options: Vec<usize>,
}

/// Finds the home-manager modules that configure a package: modules whose
/// `package` option defaults to `pkgs.<attr>`, plus `programs.<name>` and
/// `services.<name>` by name, since not every module has a `package` option.
pub fn related_options(options: &[HmOption], attr: &str) -> Related {
    let wanted = format!("pkgs.{attr}");
    let mut modules = BTreeSet::new();
    for o in options {
        if o.loc.last().is_some_and(|l| l == "package")
            && o.default.as_deref().is_some_and(|d| d.trim() == wanted)
        {
            modules.insert(o.loc[..o.loc.len() - 1].join("."));
        }
    }
    let name = attr.rsplit('.').next().unwrap_or(attr);
    for prefix in [format!("programs.{name}."), format!("services.{name}.")] {
        if options.iter().any(|o| o.name.starts_with(&prefix)) {
            modules.insert(prefix.trim_end_matches('.').to_string());
        }
    }

    let prefixes: Vec<String> = modules.iter().map(|m| format!("{m}.")).collect();
    // Each module's own `enable` first: it's the option people want.
    let enables: HashSet<String> = modules.iter().map(|m| format!("{m}.enable")).collect();
    let mut related: Vec<usize> = (0..options.len())
        .filter(|&i| prefixes.iter().any(|p| options[i].name.starts_with(p)))
        .collect();
    related.sort_by(|&a, &b| {
        let (a, b) = (&options[a].name, &options[b].name);
        enables
            .contains(b)
            .cmp(&enables.contains(a))
            .then_with(|| a.cmp(b))
    });
    Related {
        modules: modules.into_iter().collect(),
        options: related,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(name: &str, default: Option<&str>, desc: &str) -> HmOption {
        HmOption {
            name: name.into(),
            loc: name.split('.').map(str::to_string).collect(),
            ty: "boolean".into(),
            description: desc.into(),
            default: default.map(str::to_string),
            example: None,
            declarations: vec![],
            read_only: false,
        }
    }

    #[test]
    fn ranks_name_matches_first() {
        let opts = vec![
            opt("a.foo", None, "git things"),
            opt("programs.git.enable", None, ""),
            opt("x.ggit", None, ""),
        ];
        let names: Vec<&str> = search(&opts, "git", None)
            .iter()
            .map(|&i| opts[i].name.as_str())
            .collect();
        assert_eq!(names, ["programs.git.enable", "x.ggit", "a.foo"]);
        assert!(search(&opts, "git nope", None).is_empty());
    }

    fn pkg(attr: &str) -> PackageInfo {
        PackageInfo {
            attr: attr.into(),
            pname: attr.rsplit('.').next().unwrap().into(),
            version: String::new(),
            description: String::new(),
            platforms: None,
        }
    }

    #[test]
    fn ranks_the_named_package_first() {
        let mut ps: Vec<PackageInfo> = [
            "gnvim",
            "neovim-qt",
            "neovim-unwrapped",
            "vimPlugins.neovim-ayu",
            "neovim",
            "vimPlugins.nvim-tree",
            "neovide",
        ]
        .map(pkg)
        .into();
        rank_packages(&mut ps, "neovim");
        let names: Vec<&str> = ps.iter().map(|p| p.attr.as_str()).collect();
        assert_eq!(
            names,
            [
                "neovim",
                "neovim-qt",
                "neovim-unwrapped",
                "vimPlugins.neovim-ayu",
                "gnvim",
                "neovide",
                "vimPlugins.nvim-tree"
            ]
        );
    }

    #[test]
    fn matches_platforms() {
        let p: Vec<String> = ["x86_64-linux", "aarch64-darwin"].map(String::from).into();
        assert!(supports_platform(&p, "linux"));
        assert!(supports_platform(&p, "aarch64-darwin"));
        assert!(!supports_platform(&p, "x86_64-darwin"));
        assert!(supports_platform(
            &["*-darwin".to_string()],
            "x86_64-darwin"
        ));
        assert!(supports_platform(&[], "darwin"));
    }

    #[test]
    fn finds_related_modules() {
        let opts = vec![
            opt("programs.bat.config", None, ""),
            opt("programs.bat.enable", None, ""),
            opt("programs.bat.package", Some("pkgs.bat"), ""),
            opt("programs.rg.package", Some("pkgs.ripgrep"), ""),
            opt("programs.rg.enable", None, ""),
        ];
        let r = related_options(&opts, "ripgrep");
        assert_eq!(r.modules, ["programs.rg"]);
        let names: Vec<&str> = r.options.iter().map(|&i| opts[i].name.as_str()).collect();
        assert_eq!(names, ["programs.rg.enable", "programs.rg.package"]);
        let r = related_options(&opts, "bat");
        assert_eq!(opts[r.options[0]].name, "programs.bat.enable");
    }
}
