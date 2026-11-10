//! Package search through search.nixos.org's Elasticsearch backend, the same
//! one its web frontend queries. Requests go through the system `curl`, like
//! the app's other commands, so the binary doesn't carry a TLS stack.

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::nix::{PackageInfo, SearchResults};

const BACKEND: &str = "https://search.nixos.org/backend";
/// Read-only credentials published in the nixos-search frontend
/// (frontend/src/index.js).
const AUTH: &str = "aWVSALXpZv:X8gPHnzL52wFEekuxsfQ9cSh";
/// Where nixos-search records the index schema version the frontend uses.
const VERSION_URL: &str = "https://raw.githubusercontent.com/NixOS/nixos-search/main/version.nix";
const MAX_RESULTS: usize = 200;

/// Index names embed a schema version that changes now and then. Start with
/// the one current when this was written, and look it up again on a 404.
static SCHEMA_VERSION: AtomicU32 = AtomicU32::new(53);

/// `unstable` → `nixos-unstable`, `25.11` → `nixos-25.11`.
fn branch(channel: &str) -> String {
    let c = channel.trim();
    let c = if c.is_empty() { "unstable" } else { c };
    if c.starts_with("nixos-") {
        c.to_string()
    } else {
        format!("nixos-{c}")
    }
}

/// A filter on `package_platforms`: an OS like `linux` matches any
/// architecture, a system like `aarch64-darwin` only itself.
fn platform_filter(platform: &str) -> Value {
    if platform.contains('-') {
        json!({ "term": { "package_platforms": platform } })
    } else {
        json!({ "wildcard": { "package_platforms": { "value": format!("*-{platform}") } } })
    }
}

fn query_body(query: &str, platform: Option<&str>) -> Value {
    let wildcard = format!("*{}*", query.to_lowercase());
    let mut filter = vec![json!({ "term": { "type": { "value": "package" } } })];
    filter.extend(platform.map(platform_filter));
    json!({
        "from": 0,
        "size": MAX_RESULTS,
        "_source": ["package_attr_name", "package_pname", "package_pversion", "package_description", "package_platforms"],
        "query": {
            "bool": {
                "filter": filter,
                "must": [{
                    "dis_max": {
                        "tie_breaker": 0.7,
                        "queries": [
                            {
                                "multi_match": {
                                    "type": "cross_fields",
                                    "query": query,
                                    "analyzer": "whitespace",
                                    "auto_generate_synonyms_phrase_query": false,
                                    "operator": "and",
                                    "fields": [
                                        "package_attr_name^9",
                                        "package_attr_name.*^5.4",
                                        "package_programs^9",
                                        "package_pname^6",
                                        "package_description^1.3",
                                        "package_longDescription^1"
                                    ]
                                }
                            },
                            { "wildcard": { "package_attr_name": { "value": wildcard, "case_insensitive": true } } }
                        ]
                    }
                }]
            }
        }
    })
}

/// Runs curl and returns the HTTP status and body.
async fn curl(args: &[&str]) -> Result<(u16, String)> {
    let out = tokio::process::Command::new("curl")
        .args(["-sS", "--max-time", "30", "-w", "\n%{http_code}"])
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .context("couldn't run curl; is it installed?")?;
    if !out.status.success() {
        bail!(
            "couldn't reach search.nixos.org: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text
        .rsplit_once('\n')
        .ok_or_else(|| anyhow!("empty response from curl"))?;
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

async fn post_search(index: &str, body: &str) -> Result<(u16, String)> {
    let url = format!("{BACKEND}/{index}/_search");
    curl(&[
        "-u",
        AUTH,
        "-H",
        "Content-Type: application/json",
        "-X",
        "POST",
        "--data-binary",
        body,
        &url,
    ])
    .await
}

/// Reads `frontend = "53";` from nixos-search's version.nix.
fn parse_frontend_version(text: &str) -> Option<u32> {
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("frontend"))
        .and_then(|rest| rest.split('"').nth(1))
        .and_then(|v| v.parse().ok())
}

pub async fn search(channel: &str, query: &str, platform: Option<&str>) -> Result<SearchResults> {
    let branch = branch(channel);
    let body = query_body(query, platform).to_string();
    let mut version = SCHEMA_VERSION.load(Ordering::Relaxed);
    let (mut status, mut text) = post_search(&format!("latest-{version}-{branch}"), &body).await?;
    if status == 404 {
        // Either the schema version moved on or the channel doesn't exist.
        let (vstatus, vtext) = curl(&[VERSION_URL]).await?;
        if let Some(v) = parse_frontend_version(&vtext).filter(|&v| vstatus == 200 && v != version)
        {
            SCHEMA_VERSION.store(v, Ordering::Relaxed);
            version = v;
            (status, text) = post_search(&format!("latest-{version}-{branch}"), &body).await?;
        }
    }
    match status {
        200 => {}
        404 => bail!(
            "search.nixos.org has no channel {branch:?}. Check the channel in Settings, e.g. unstable or 25.11."
        ),
        _ => bail!(
            "search.nixos.org answered HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        ),
    }
    Ok(SearchResults {
        source: format!("search.nixos.org ({branch})"),
        packages: parse_hits(&text)?,
    })
}

fn parse_hits(text: &str) -> Result<Vec<PackageInfo>> {
    let v: Value =
        serde_json::from_str(text).context("search.nixos.org sent something that isn't JSON")?;
    let hits = v["hits"]["hits"]
        .as_array()
        .ok_or_else(|| anyhow!("unexpected response from search.nixos.org"))?;
    let mut seen = std::collections::HashSet::new();
    Ok(hits
        .iter()
        .map(|h| &h["_source"])
        .filter_map(|s| {
            let attr = s["package_attr_name"].as_str()?.to_string();
            seen.insert(attr.clone()).then(|| PackageInfo {
                attr,
                pname: s["package_pname"].as_str().unwrap_or_default().into(),
                version: s["package_pversion"].as_str().unwrap_or_default().into(),
                description: s["package_description"].as_str().unwrap_or_default().into(),
                platforms: s["package_platforms"].as_array().map(|ps| {
                    ps.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                }),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_channels_to_branches() {
        assert_eq!(branch("unstable"), "nixos-unstable");
        assert_eq!(branch(" 25.11 "), "nixos-25.11");
        assert_eq!(branch("nixos-25.05"), "nixos-25.05");
        assert_eq!(branch(""), "nixos-unstable");
    }

    #[test]
    fn reads_the_frontend_version() {
        let text = "{\n  # comment\n  import = \"54\";\n  frontend = \"53\";\n}";
        assert_eq!(parse_frontend_version(text), Some(53));
        assert_eq!(parse_frontend_version("{}"), None);
    }

    #[test]
    fn parses_hits_keeping_order_and_dropping_duplicates() {
        let text = r#"{"hits":{"hits":[
            {"_source":{"package_attr_name":"neovim","package_pname":"neovim","package_pversion":"0.11.4","package_description":"Vim text editor fork","package_platforms":["x86_64-linux","aarch64-darwin"]}},
            {"_source":{"package_attr_name":"neovim-unwrapped","package_pname":"neovim-unwrapped","package_pversion":"0.11.4","package_description":null}},
            {"_source":{"package_attr_name":"neovim","package_pname":"neovim","package_pversion":"0.11.4"}}
        ]}}"#;
        let p = parse_hits(text).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].attr, "neovim");
        assert_eq!(p[0].version, "0.11.4");
        assert_eq!(p[1].attr, "neovim-unwrapped");
        assert_eq!(p[1].description, "");
        assert_eq!(
            p[0].platforms.as_deref(),
            Some(&["x86_64-linux".to_string(), "aarch64-darwin".to_string()][..])
        );
        assert_eq!(p[1].platforms, None);
    }

    #[test]
    fn filters_by_os_or_system() {
        assert_eq!(
            platform_filter("linux"),
            json!({ "wildcard": { "package_platforms": { "value": "*-linux" } } })
        );
        assert_eq!(
            platform_filter("aarch64-darwin"),
            json!({ "term": { "package_platforms": "aarch64-darwin" } })
        );
        let body = query_body("vim", Some("darwin"));
        assert_eq!(body["query"]["bool"]["filter"].as_array().unwrap().len(), 2);
        assert_eq!(
            query_body("vim", None)["query"]["bool"]["filter"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
