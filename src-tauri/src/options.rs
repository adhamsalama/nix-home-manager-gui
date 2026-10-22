//! Normalizes home-manager's `options.json` (as produced by `nixosOptionsDoc`)
//! into a flat list the UI can search.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HmOption {
    pub name: String,
    /// Attribute path; placeholders like `<name>` stand for user-chosen keys.
    pub loc: Vec<String>,
    #[serde(rename = "type")]
    pub ty: String,
    pub description: String,
    pub default: Option<String>,
    pub example: Option<String>,
    pub declarations: Vec<String>,
    pub read_only: bool,
}

pub fn parse_options_json(json: &str) -> Result<Vec<HmOption>> {
    let raw: serde_json::Map<String, Value> =
        serde_json::from_str(json).context("options.json isn't a JSON object")?;
    let mut options: Vec<HmOption> = raw
        .into_iter()
        .filter(|(name, _)| !name.starts_with("_module."))
        .map(|(name, v)| HmOption {
            loc: v["loc"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_else(|| name.split('.').map(str::to_string).collect()),
            ty: v["type"].as_str().unwrap_or("unspecified").to_string(),
            description: doc_text(&v["description"]).unwrap_or_default(),
            default: doc_text(&v["default"]),
            example: doc_text(&v["example"]),
            declarations: v["declarations"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| {
                            d.as_str()
                                .or_else(|| d["name"].as_str())
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            read_only: v["readOnly"].as_bool().unwrap_or(false),
            name,
        })
        .collect();
    options.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(options)
}

/// Defaults, examples and descriptions are either plain JSON values or
/// `{ "_type": "literalExpression" | "literalMD" | "mdDoc", "text": ... }`.
fn doc_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Object(o) if o.contains_key("_type") => {
            o.get("text").and_then(Value::as_str).map(str::to_string)
        }
        other => Some(crate::nix_edit::to_nix(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_entries() {
        let json = r#"{
          "programs.git.enable": {
            "declarations": [{"name": "<home-manager/modules/programs/git.nix>", "url": "https://x"}],
            "default": {"_type": "literalExpression", "text": "false"},
            "description": "Whether to enable Git.",
            "example": {"_type": "literalExpression", "text": "true"},
            "loc": ["programs", "git", "enable"],
            "readOnly": false,
            "type": "boolean"
          },
          "_module.args": {"type": "lazy attribute set of raw value", "description": "internal"},
          "home.packages": {"default": [], "description": "Packages", "type": "list of package"}
        }"#;
        let opts = parse_options_json(json).unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].name, "home.packages");
        assert_eq!(opts[0].default.as_deref(), Some("[ ]"));
        assert_eq!(opts[0].loc, vec!["home", "packages"]);
        assert_eq!(opts[1].default.as_deref(), Some("false"));
        assert_eq!(
            opts[1].declarations,
            vec!["<home-manager/modules/programs/git.nix>"]
        );
    }
}
