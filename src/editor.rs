//! Form widgets for option values: picks a widget from a home-manager type,
//! reads a Nix expression back into a form value, and turns the form value
//! into JSON for the backend to render as Nix.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Bool { nullable: bool },
    Str { nullable: bool, multiline: bool },
    Number { nullable: bool, integer: bool },
    Enum { nullable: bool, values: Vec<String> },
    StringList,
    StringMap,
    Raw,
}

impl Widget {
    pub fn nullable(&self) -> bool {
        match self {
            Widget::Bool { nullable }
            | Widget::Str { nullable, .. }
            | Widget::Number { nullable, .. }
            | Widget::Enum { nullable, .. } => *nullable,
            _ => false,
        }
    }

    /// The name the UI switches on.
    pub fn kind(&self) -> &'static str {
        match self {
            Widget::Bool { .. } => "bool",
            Widget::Str { .. } => "string",
            Widget::Number { .. } => "number",
            Widget::Enum { .. } => "enum",
            Widget::StringList => "stringList",
            Widget::StringMap => "stringMap",
            Widget::Raw => "raw",
        }
    }
}

/// A value as the form holds it. Numbers stay text until staged.
#[derive(Debug, Clone, PartialEq)]
pub enum FormValue {
    Null,
    Bool(bool),
    Text(String),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

static ENUM_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap());
static WORD_OR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bor\b|list|attribute").unwrap());
static STRING_MAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(lazy )?attribute set of string$").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+(\.\d+)?$").unwrap());

/// Picks a form widget from a home-manager type description.
pub fn widget_for(ty: &str) -> Widget {
    let (nullable, t) = match ty.strip_prefix("null or ") {
        Some(rest) => (true, rest),
        None => (false, ty),
    };
    if t == "boolean" {
        return Widget::Bool { nullable };
    }
    if t.starts_with("strings concatenated with") {
        return Widget::Str {
            nullable,
            multiline: true,
        };
    }
    if [
        "string",
        "absolute path",
        "path",
        "non-empty string",
        "string or path",
    ]
    .contains(&t)
    {
        return Widget::Str {
            nullable,
            multiline: false,
        };
    }
    if t.starts_with("one of ") {
        let values = ENUM_VALUE
            .captures_iter(t)
            .map(|c| c[1].to_string())
            .collect();
        return Widget::Enum { nullable, values };
    }
    if !WORD_OR.is_match(t) && t.contains("integer") {
        return Widget::Number {
            nullable,
            integer: true,
        };
    }
    if t == "floating point number" || t == "integer or floating point number" {
        return Widget::Number {
            nullable,
            integer: false,
        };
    }
    if !nullable && t == "list of string" {
        return Widget::StringList;
    }
    if !nullable && STRING_MAP.is_match(t) {
        return Widget::StringMap;
    }
    Widget::Raw
}

/// Parses a double-quoted Nix string at the start of `s`, returning its value
/// and the rest. Fails on interpolation, which the form can't show.
fn parse_string(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices().peekable();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[1 + i + 1..])),
            '\\' => {
                let (_, e) = chars.next()?;
                out.push(match e {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                });
            }
            '$' if chars.peek().is_some_and(|&(_, n)| n == '{') => return None,
            other => out.push(other),
        }
    }
    None
}

/// A whole expression that is exactly one double-quoted string.
fn parse_whole_string(e: &str) -> Option<String> {
    match parse_string(e)? {
        (s, "") => Some(s),
        _ => None,
    }
}

/// Reads a `''…''` string, stripping its common indentation.
fn parse_indented_string(expr: &str) -> Option<String> {
    let body = expr.strip_prefix("''")?.strip_suffix("''")?;
    if expr.len() < 4 {
        return None;
    }
    // `${` is interpolation unless escaped as `''${`.
    for (i, _) in body.match_indices("${") {
        if i < 2 || &body[i - 2..i] != "''" {
            return None;
        }
    }
    let mut lines: Vec<&str> = body.split('\n').collect();
    if lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if let Some(last) = lines.last_mut()
        && last.trim().is_empty()
    {
        *last = "";
    }
    let strip = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let joined = lines
        .iter()
        .map(|l| l.get(strip..).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    Some(joined.replace("'''", "''").replace("''$", "$"))
}

fn parse_list(e: &str) -> Option<Vec<String>> {
    let mut rest = e.strip_prefix('[')?.strip_suffix(']')?.trim_start();
    let mut items = Vec::new();
    while !rest.is_empty() {
        let (s, r) = parse_string(rest)?;
        items.push(s);
        rest = r.trim_start();
    }
    Some(items)
}

fn parse_key(s: &str) -> Option<(String, &str)> {
    if s.starts_with('"') {
        return parse_string(s);
    }
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = chars
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == '\'' || c == '-'))
        .map_or(s.len(), |(i, _)| i);
    Some((s[..end].to_string(), &s[end..]))
}

fn parse_map(e: &str) -> Option<Vec<(String, String)>> {
    let mut rest = e.strip_prefix('{')?.strip_suffix('}')?.trim_start();
    let mut entries = Vec::new();
    while !rest.is_empty() {
        let (k, r) = parse_key(rest)?;
        let r = r.trim_start().strip_prefix('=')?.trim_start();
        let (v, r) = parse_string(r)?;
        rest = r.trim_start().strip_prefix(';')?.trim_start();
        entries.push((k, v));
    }
    Some(entries)
}

/// Reads a Nix expression back into a form value, or None if the form can't show it.
pub fn parse_expr(widget: &Widget, expr: &str) -> Option<FormValue> {
    let e = expr.trim();
    if widget.nullable() && e == "null" {
        return Some(FormValue::Null);
    }
    match widget {
        Widget::Bool { .. } => match e {
            "true" => Some(FormValue::Bool(true)),
            "false" => Some(FormValue::Bool(false)),
            _ => None,
        },
        Widget::Number { .. } => NUMBER.is_match(e).then(|| FormValue::Text(e.to_string())),
        Widget::Str { .. } => parse_whole_string(e)
            .or_else(|| parse_indented_string(e))
            .map(FormValue::Text),
        Widget::Enum { values, .. } => parse_whole_string(e)
            .filter(|s| values.contains(s))
            .map(FormValue::Text),
        Widget::StringList => parse_list(e).map(FormValue::List),
        Widget::StringMap => parse_map(e).map(FormValue::Map),
        Widget::Raw => None,
    }
}

/// The value a fresh form starts with.
pub fn empty_value(widget: &Widget) -> FormValue {
    if widget.nullable() {
        return FormValue::Null;
    }
    non_null_empty(widget)
}

/// The value a nullable field takes when "null" is unticked.
pub fn non_null_empty(widget: &Widget) -> FormValue {
    match widget {
        Widget::Bool { .. } => FormValue::Bool(false),
        Widget::Str { .. } => FormValue::Text(String::new()),
        Widget::Number { .. } => FormValue::Text("0".into()),
        Widget::Enum { values, .. } => FormValue::Text(values.first().cloned().unwrap_or_default()),
        Widget::StringList => FormValue::List(vec![]),
        Widget::StringMap => FormValue::Map(vec![]),
        Widget::Raw => FormValue::Null,
    }
}

/// Turns a form value into JSON for `nix_edit::to_nix`.
pub fn to_json(widget: &Widget, value: &FormValue) -> Result<Value, String> {
    Ok(match value {
        FormValue::Null => Value::Null,
        FormValue::Bool(b) => Value::Bool(*b),
        FormValue::Text(t) => match widget {
            Widget::Number { integer: true, .. } => {
                let n: i64 = t
                    .trim()
                    .parse()
                    .map_err(|_| format!("{t:?} isn't a whole number"))?;
                Value::from(n)
            }
            Widget::Number { integer: false, .. } => {
                let n: f64 = t
                    .trim()
                    .parse()
                    .map_err(|_| format!("{t:?} isn't a number"))?;
                serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .ok_or(format!("{t:?} isn't a finite number"))?
            }
            _ => Value::String(t.clone()),
        },
        FormValue::List(items) => Value::Array(
            items
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.clone()))
                .collect(),
        ),
        FormValue::Map(entries) => Value::Object(
            entries
                .iter()
                .filter(|(k, _)| !k.trim().is_empty())
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<Map<_, _>>(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_widgets() {
        assert_eq!(widget_for("boolean"), Widget::Bool { nullable: false });
        assert_eq!(
            widget_for("null or string"),
            Widget::Str {
                nullable: true,
                multiline: false
            }
        );
        assert_eq!(
            widget_for("strings concatenated with \"\\n\""),
            Widget::Str {
                nullable: false,
                multiline: true
            }
        );
        assert_eq!(
            widget_for(r#"one of "a", "b""#),
            Widget::Enum {
                nullable: false,
                values: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            widget_for("unsigned integer, meaning >=0"),
            Widget::Number {
                nullable: false,
                integer: true
            }
        );
        assert_eq!(widget_for("list of integer"), Widget::Raw);
        assert_eq!(widget_for("list of string"), Widget::StringList);
        assert_eq!(
            widget_for("lazy attribute set of string"),
            Widget::StringMap
        );
        assert_eq!(widget_for("null or list of string"), Widget::Raw);
    }

    #[test]
    fn parses_strings() {
        let w = Widget::Str {
            nullable: true,
            multiline: false,
        };
        assert_eq!(
            parse_expr(&w, r#""a\"b\n""#),
            Some(FormValue::Text("a\"b\n".into()))
        );
        assert_eq!(parse_expr(&w, r#""a${b}""#), None);
        assert_eq!(
            parse_expr(&w, r#""a\${b}""#),
            Some(FormValue::Text("a${b}".into()))
        );
        assert_eq!(
            parse_expr(&w, r#""cost: $5""#),
            Some(FormValue::Text("cost: $5".into()))
        );
        assert_eq!(parse_expr(&w, r#""a" + "b""#), None);
        assert_eq!(parse_expr(&w, "null"), Some(FormValue::Null));
    }

    #[test]
    fn parses_indented_strings() {
        let w = Widget::Str {
            nullable: false,
            multiline: true,
        };
        assert_eq!(
            parse_expr(&w, "''\n    set -x\n      echo hi\n  ''"),
            Some(FormValue::Text("set -x\n  echo hi\n".into()))
        );
        assert_eq!(parse_expr(&w, "''\n  a ${b}\n''"), None);
        assert_eq!(
            parse_expr(&w, "''\n  a ''${b}\n''"),
            Some(FormValue::Text("a ${b}\n".into()))
        );
    }

    #[test]
    fn parses_lists_and_maps() {
        assert_eq!(
            parse_expr(&Widget::StringList, r#"[ "a" "b c" ]"#),
            Some(FormValue::List(vec!["a".into(), "b c".into()]))
        );
        assert_eq!(
            parse_expr(&Widget::StringList, "[ ]"),
            Some(FormValue::List(vec![]))
        );
        assert_eq!(parse_expr(&Widget::StringList, r#"[ "a" b ]"#), None);
        assert_eq!(
            parse_expr(
                &Widget::StringMap,
                r#"{ ll = "ls -l"; "g s" = "git status"; }"#
            ),
            Some(FormValue::Map(vec![
                ("ll".into(), "ls -l".into()),
                ("g s".into(), "git status".into())
            ]))
        );
        assert_eq!(parse_expr(&Widget::StringMap, r#"{ ll = x; }"#), None);
    }

    #[test]
    fn enums_must_be_known_values() {
        let w = Widget::Enum {
            nullable: false,
            values: vec!["a".into()],
        };
        assert_eq!(parse_expr(&w, r#""a""#), Some(FormValue::Text("a".into())));
        assert_eq!(parse_expr(&w, r#""z""#), None);
    }

    #[test]
    fn converts_to_json() {
        let int = Widget::Number {
            nullable: false,
            integer: true,
        };
        assert_eq!(
            to_json(&int, &FormValue::Text("42".into())),
            Ok(Value::from(42))
        );
        assert!(to_json(&int, &FormValue::Text("4.2".into())).is_err());
        assert_eq!(
            to_json(
                &Widget::StringList,
                &FormValue::List(vec!["a".into(), "".into()])
            ),
            Ok(serde_json::json!(["a"]))
        );
        assert_eq!(
            to_json(
                &Widget::StringMap,
                &FormValue::Map(vec![(" ".into(), "x".into()), ("k".into(), "v".into())])
            ),
            Ok(serde_json::json!({"k": "v"}))
        );
    }
}
