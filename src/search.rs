//! Searching options, and finding the options that configure a package.

use std::collections::{BTreeSet, HashSet};

use crate::options::HmOption;

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
