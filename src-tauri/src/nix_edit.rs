//! Comment- and formatting-preserving edits of a home-manager Nix file.
//!
//! Edits never regenerate the file from the AST. They locate the relevant
//! node with rnix, splice new text into its byte range, and re-parse the
//! result to make sure it is still valid Nix. Everything else in the file
//! (comments, formatting, unrelated code) is left byte-for-byte intact.

use anyhow::{Context, Result, anyhow, bail};
use rnix::Root;
use rnix::ast::{self, AstNode, HasEntry, InterpolPart};
use rowan::ast::AstNode as _;
use serde::Serialize;

const INDENT: &str = "  ";
const KEYWORDS: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

/// Parses `text`, failing on any syntax error.
pub fn parse(text: &str) -> Result<Root> {
    let parse = Root::parse(text);
    if let Some(err) = parse.errors().first() {
        bail!("invalid Nix: {err}");
    }
    Ok(parse.tree())
}

/// Checks that `expr` is a standalone, valid Nix expression.
pub fn validate_expr(expr: &str) -> Result<()> {
    if expr.trim().is_empty() {
        bail!("the expression is empty");
    }
    parse(expr).map(|_| ())
}

/// What the file says about one option.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum FileValue {
    NotSet,
    Set {
        expr: String,
        line: usize,
    },
    /// Only some sub-attributes of this path are set, e.g. `programs.git.enable`.
    Partial {
        line: usize,
    },
    /// A prefix of the path is bound to something other than an attribute set
    /// literal (e.g. `lib.mkIf cond { ... }`), so the GUI can't see inside.
    Opaque {
        line: usize,
        expr: String,
    },
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Packages {
    /// Whether `home.packages` appears in the file at all.
    pub defined: bool,
    /// Whether the list is wrapped in `with pkgs;`.
    pub with_pkgs: bool,
    /// Package attribute names, without any `pkgs.` prefix.
    pub items: Vec<String>,
}

pub fn get_option(text: &str, path: &[String]) -> Result<FileValue> {
    let root = parse(text)?;
    let set = root_attrset(&root)?;
    Ok(match locate(&set, path) {
        Location::Found(apv) => {
            let value = apv
                .value()
                .ok_or_else(|| anyhow!("definition has no value"))?;
            FileValue::Set {
                expr: value.syntax().text().to_string(),
                line: line_of(text, start(&apv)),
            }
        }
        Location::Partial(apv) => FileValue::Partial {
            line: line_of(text, start(&apv)),
        },
        Location::Opaque(apv) => FileValue::Opaque {
            line: line_of(text, start(&apv)),
            expr: apv.syntax().text().to_string(),
        },
        Location::Missing { .. } => FileValue::NotSet,
    })
}

/// Sets `path` to the Nix expression `value`, replacing an existing
/// definition or inserting a new one in the deepest matching attribute set.
pub fn set_option(text: &str, path: &[String], value: &str) -> Result<String> {
    validate_expr(value)?;
    let root = parse(text)?;
    let set = root_attrset(&root)?;
    let new = match locate(&set, path) {
        Location::Found(apv) => {
            let node = apv
                .value()
                .ok_or_else(|| anyhow!("definition has no value"))?;
            let indent = indent_at(text, start(&apv));
            let (s, e) = range(node.syntax());
            splice(text, s, e, &reindent(value, &indent))
        }
        Location::Missing { set, remaining } => {
            let entry = format!("{} = {};", render_attrpath(&remaining), value);
            insert_into_set(text, &set, &entry)?
        }
        Location::Partial(apv) => bail!(
            "`{}` is already set through its sub-attributes (line {}); edit those individually",
            path.join("."),
            line_of(text, start(&apv))
        ),
        Location::Opaque(apv) => bail!(
            "`{}` is inside an expression the GUI can't edit safely (line {})",
            path.join("."),
            line_of(text, start(&apv))
        ),
    };
    parse(&new).context("the edit would produce invalid Nix")?;
    Ok(new)
}

/// Removes the definition of `path`, then any attribute sets it leaves empty.
pub fn remove_option(text: &str, path: &[String]) -> Result<String> {
    let root = parse(text)?;
    let set = root_attrset(&root)?;
    let Location::Found(apv) = locate(&set, path) else {
        bail!("`{}` isn't set directly in this file", path.join("."));
    };
    let (s, e) = range(apv.syntax());
    let (s, e) = removal_span(text, s, e);
    let mut new = splice(text, s, e, "");

    // Clean up parents like `programs.git = { };` that are now empty.
    for len in (1..path.len()).rev() {
        let root = parse(&new)?;
        let set = root_attrset(&root)?;
        let Location::Found(parent) = locate(&set, &path[..len]) else {
            break;
        };
        let Some(ast::Expr::AttrSet(inner)) = parent.value() else {
            break;
        };
        if inner.entries().next().is_some() || inner.syntax().text().to_string().contains('#') {
            break;
        }
        let (s, e) = range(parent.syntax());
        let (s, e) = removal_span(&new, s, e);
        new = splice(&new, s, e, "");
    }

    parse(&new).context("the edit would produce invalid Nix")?;
    Ok(new)
}

/// Every attribute path defined in the file, including intermediate ones
/// (`programs`, `programs.git`, `programs.git.enable`).
pub fn defined_paths(text: &str) -> Result<Vec<String>> {
    fn walk(set: &ast::AttrSet, prefix: &[String], out: &mut Vec<String>) {
        for apv in set.attrpath_values() {
            let Some(keys) = apv.attrpath().and_then(|p| attrpath_names(&p)) else {
                continue;
            };
            let mut path = prefix.to_vec();
            for key in keys {
                path.push(key);
                out.push(path.join("."));
            }
            if let Some(ast::Expr::AttrSet(inner)) = apv.value() {
                walk(&inner, &path, out);
            }
        }
    }
    let root = parse(text)?;
    let set = root_attrset(&root)?;
    let mut out = Vec::new();
    walk(&set, &[], &mut out);
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn get_packages(text: &str) -> Result<Packages> {
    Ok(match packages_list(text)? {
        None => Packages {
            defined: false,
            with_pkgs: false,
            items: vec![],
        },
        Some((list, with_pkgs)) => Packages {
            defined: true,
            with_pkgs,
            items: list.items().map(|i| package_name(&i)).collect(),
        },
    })
}

pub fn add_package(text: &str, name: &str) -> Result<String> {
    if !is_package_attr(name) {
        bail!("`{name}` isn't a valid package attribute name");
    }
    let Some((list, with_pkgs)) = packages_list(text)? else {
        return set_option(
            text,
            &["home".into(), "packages".into()],
            &format!("with pkgs; [\n{INDENT}{name}\n]"),
        );
    };
    if list.items().any(|i| package_name(&i) == name) {
        bail!("`{name}` is already in home.packages");
    }
    let item = if with_pkgs {
        name.to_string()
    } else {
        format!("pkgs.{name}")
    };

    let open = list
        .l_brack_token()
        .ok_or_else(|| anyhow!("malformed list"))?;
    let close = list
        .r_brack_token()
        .ok_or_else(|| anyhow!("malformed list"))?;
    let open_end: usize = open.text_range().end().into();
    let close_start: usize = close.text_range().start().into();
    let last = list.items().last();

    let new = if text[open_end..close_start].contains('\n') {
        let (pos, indent) = match &last {
            Some(last) => {
                let end = range(last.syntax()).1;
                (
                    after_on_line(text, end).unwrap_or(end),
                    indent_at(text, start(last)),
                )
            }
            None => (
                after_on_line(text, open_end).unwrap_or(open_end),
                format!("{}{INDENT}", indent_at(text, open_end)),
            ),
        };
        splice(text, pos, pos, &format!("\n{indent}{item}"))
    } else {
        match &last {
            Some(last) => {
                let end = range(last.syntax()).1;
                splice(text, end, end, &format!(" {item}"))
            }
            None => splice(text, open_end, close_start, &format!(" {item} ")),
        }
    };
    parse(&new).context("the edit would produce invalid Nix")?;
    Ok(new)
}

pub fn remove_package(text: &str, name: &str) -> Result<String> {
    let (list, _) =
        packages_list(text)?.ok_or_else(|| anyhow!("home.packages isn't set in this file"))?;
    let item = list
        .items()
        .find(|i| package_name(i) == name)
        .ok_or_else(|| anyhow!("`{name}` isn't in home.packages"))?;
    let (s, e) = range(item.syntax());
    let (s, e) = removal_span(text, s, e);
    let new = splice(text, s, e, "");
    parse(&new).context("the edit would produce invalid Nix")?;
    Ok(new)
}

/// Renders a JSON value as a Nix expression. Multi-line output is indented
/// relative to column 0; callers re-indent it to where it is inserted.
pub fn to_nix(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => nix_string(s),
        Value::Array(items) if items.is_empty() => "[ ]".into(),
        Value::Array(items) => {
            let body: Vec<String> = items
                .iter()
                .map(|v| {
                    let item = to_nix(v);
                    // Function application or operators would need parens in a list;
                    // to_nix only produces literals, which never do.
                    format!("{INDENT}{}", reindent(&item, INDENT))
                })
                .collect();
            format!("[\n{}\n]", body.join("\n"))
        }
        Value::Object(map) if map.is_empty() => "{ }".into(),
        Value::Object(map) => {
            let body: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{INDENT}{} = {};",
                        render_key(k),
                        reindent(&to_nix(v), INDENT)
                    )
                })
                .collect();
            format!("{{\n{}\n}}", body.join("\n"))
        }
    }
}

fn nix_string(s: &str) -> String {
    if s.contains('\n') {
        // Indented string: '' and ${ need escaping, everything else is literal.
        let escaped = s.replace("''", "'''").replace("${", "''${");
        let body: Vec<String> = escaped
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("{INDENT}{l}")
                }
            })
            .collect();
        let mut body = body.join("\n");
        if !s.ends_with('\n') {
            body.push('\n');
        }
        return format!("''\n{body}''");
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Locating definitions

enum Location {
    Found(ast::AttrpathValue),
    Missing {
        set: ast::AttrSet,
        remaining: Vec<String>,
    },
    Partial(ast::AttrpathValue),
    Opaque(ast::AttrpathValue),
}

fn root_attrset(root: &Root) -> Result<ast::AttrSet> {
    let not_a_set = || {
        anyhow!(
            "the file's top-level expression isn't an attribute set, so it can't be edited safely"
        )
    };
    let mut expr = root.expr().ok_or_else(|| anyhow!("the file is empty"))?;
    loop {
        expr = match expr {
            ast::Expr::AttrSet(set) => return Ok(set),
            ast::Expr::Lambda(l) => l.body(),
            ast::Expr::LetIn(l) => l.body(),
            ast::Expr::With(w) => w.body(),
            ast::Expr::Paren(p) => p.expr(),
            ast::Expr::Assert(a) => a.body(),
            _ => None,
        }
        .ok_or_else(not_a_set)?;
    }
}

/// Finds where `path` is (or would be) defined inside `set`. Definitions can
/// be nested (`programs.git = { enable = true; };`), flattened
/// (`programs.git.enable = true;`) or any mix of the two.
fn locate(set: &ast::AttrSet, path: &[String]) -> Location {
    let mut deepest: Option<(ast::AttrSet, Vec<String>)> = None;
    let mut partial = None;
    for apv in set.attrpath_values() {
        let Some(keys) = apv.attrpath().and_then(|p| attrpath_names(&p)) else {
            continue;
        };
        let common = keys.iter().zip(path).take_while(|(a, b)| a == b).count();
        if common == 0 {
            continue;
        }
        if common == keys.len() && common == path.len() {
            return Location::Found(apv);
        }
        if common == path.len() {
            partial.get_or_insert(apv);
            continue;
        }
        if common < keys.len() {
            continue;
        }
        // `keys` is a proper prefix of `path`: look inside the value.
        match apv.value() {
            Some(ast::Expr::AttrSet(inner)) => match locate(&inner, &path[common..]) {
                Location::Missing { set, remaining } => {
                    if deepest
                        .as_ref()
                        .is_none_or(|(_, r)| r.len() > remaining.len())
                    {
                        deepest = Some((set, remaining));
                    }
                }
                found => return found,
            },
            _ => return Location::Opaque(apv),
        }
    }
    if let Some(apv) = partial {
        return Location::Partial(apv);
    }
    let (set, remaining) = deepest.unwrap_or_else(|| (set.clone(), path.to_vec()));
    Location::Missing { set, remaining }
}

fn insert_into_set(text: &str, set: &ast::AttrSet, entry: &str) -> Result<String> {
    let open = set
        .l_curly_token()
        .ok_or_else(|| anyhow!("malformed attribute set"))?;
    let close = set
        .r_curly_token()
        .ok_or_else(|| anyhow!("malformed attribute set"))?;
    let open_end: usize = open.text_range().end().into();
    let close_start: usize = close.text_range().start().into();
    let last = set.entries().last();

    if text[open_end..close_start].contains('\n') {
        let (pos, indent) = match &last {
            Some(last) => {
                let end = range(last.syntax()).1;
                (
                    after_on_line(text, end).unwrap_or(end),
                    indent_at(text, start(last)),
                )
            }
            None => (
                after_on_line(text, open_end).unwrap_or(open_end),
                format!("{}{INDENT}", indent_at(text, open_end)),
            ),
        };
        return Ok(splice(
            text,
            pos,
            pos,
            &format!("\n{indent}{}", reindent(entry, &indent)),
        ));
    }

    let indent = format!("{}{INDENT}", indent_at(text, open_end));
    Ok(match &last {
        Some(last) => {
            let end = range(last.syntax()).1;
            splice(text, end, end, &format!(" {}", reindent(entry, &indent)))
        }
        None => splice(
            text,
            open_end,
            close_start,
            &format!(" {} ", reindent(entry, &indent)),
        ),
    })
}

fn packages_list(text: &str) -> Result<Option<(ast::List, bool)>> {
    let root = parse(text)?;
    let set = root_attrset(&root)?;
    let apv = match locate(&set, &["home".into(), "packages".into()]) {
        Location::Found(apv) => apv,
        Location::Missing { .. } => return Ok(None),
        _ => bail!("home.packages is defined in a way the GUI can't edit"),
    };
    let mut with_pkgs = false;
    let mut value = apv.value();
    loop {
        value = match value {
            Some(ast::Expr::List(list)) => return Ok(Some((list, with_pkgs))),
            Some(ast::Expr::With(w)) => {
                with_pkgs |= w.namespace().is_some_and(|n| n.syntax().text() == "pkgs");
                w.body()
            }
            Some(ast::Expr::Paren(p)) => p.expr(),
            _ => bail!(
                "home.packages (line {}) isn't a plain list, so the GUI can't edit it",
                line_of(text, start(&apv))
            ),
        };
    }
}

fn package_name(item: &ast::Expr) -> String {
    let text = item.syntax().text().to_string();
    text.strip_prefix("pkgs.")
        .map(str::to_string)
        .unwrap_or(text)
}

fn is_package_attr(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_simple_ident)
}

fn attr_name(attr: &ast::Attr) -> Option<String> {
    match attr {
        ast::Attr::Ident(i) => i.ident_token().map(|t| t.text().to_string()),
        ast::Attr::Str(s) => literal_string(s),
        ast::Attr::Dynamic(d) => match d.expr()? {
            ast::Expr::Str(s) => literal_string(&s),
            _ => None,
        },
    }
}

fn literal_string(s: &ast::Str) -> Option<String> {
    let mut out = String::new();
    for part in s.normalized_parts() {
        match part {
            InterpolPart::Literal(l) => out.push_str(&l),
            InterpolPart::Interpolation(_) => return None,
        }
    }
    Some(out)
}

/// The names in an attribute path, or `None` if any part is dynamic.
fn attrpath_names(path: &ast::Attrpath) -> Option<Vec<String>> {
    path.attrs().map(|a| attr_name(&a)).collect()
}

fn is_simple_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '\''))
        && !KEYWORDS.contains(&s)
}

fn render_key(key: &str) -> String {
    if is_simple_ident(key) {
        key.to_string()
    } else {
        nix_string(key)
    }
}

fn render_attrpath(path: &[String]) -> String {
    path.iter()
        .map(|k| render_key(k))
        .collect::<Vec<_>>()
        .join(".")
}

// ---------------------------------------------------------------------------
// Text helpers

fn range(node: &rnix::SyntaxNode) -> (usize, usize) {
    let r = node.text_range();
    (r.start().into(), r.end().into())
}

fn start(node: &impl AstNode) -> usize {
    range(node.syntax()).0
}

fn splice(text: &str, start: usize, end: usize, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..start]);
    out.push_str(with);
    out.push_str(&text[end..]);
    out
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i)
}

fn line_of(text: &str, pos: usize) -> usize {
    text[..pos].matches('\n').count() + 1
}

fn indent_at(text: &str, pos: usize) -> String {
    text[line_start(text, pos)..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// If the rest of the line after `pos` is blank or a comment, the end of that
/// line; that's where a new line can go without splitting a trailing comment.
fn after_on_line(text: &str, pos: usize) -> Option<usize> {
    let end = line_end(text, pos);
    let rest = text[pos..end].trim();
    (rest.is_empty() || rest.starts_with('#')).then_some(end)
}

/// Indents every line after the first, leaving empty lines empty.
fn reindent(value: &str, indent: &str) -> String {
    value
        .split('\n')
        .enumerate()
        .map(|(i, l)| {
            if i == 0 || l.is_empty() {
                l.to_string()
            } else {
                format!("{indent}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The span to delete when removing the node at `start..end`: its whole line
/// (with any trailing comment) when it sits alone on it, otherwise just the
/// node and the whitespace separating it from its neighbour.
fn removal_span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let ls = line_start(text, start);
    let alone_before = text[ls..start].trim().is_empty();
    if alone_before && after_on_line(text, end).is_some() {
        let le = line_end(text, end);
        return (ls, (le + 1).min(text.len()));
    }
    let bytes = text.as_bytes();
    if alone_before {
        let mut e = end;
        while e < text.len() && matches!(bytes[e], b' ' | b'\t') {
            e += 1;
        }
        (start, e)
    } else {
        let mut s = start;
        while s > 0 && matches!(bytes[s - 1], b' ' | b'\t') {
            s -= 1;
        }
        (s, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(s: &str) -> Vec<String> {
        s.split('.').map(str::to_string).collect()
    }

    const HOME: &str = r#"{ config, pkgs, ... }:

{
  home.username = "adham";
  home.stateVersion = "24.05"; # don't change

  # Git
  programs.git = {
    enable = true;
    userName = "Adham";
  };

  home.packages = with pkgs; [
    ripgrep # search
    fd
  ];
}
"#;

    #[test]
    fn reads_flat_and_nested_definitions() {
        assert_eq!(
            get_option(HOME, &p("home.username")).unwrap(),
            FileValue::Set {
                expr: "\"adham\"".into(),
                line: 4
            }
        );
        assert_eq!(
            get_option(HOME, &p("programs.git.userName")).unwrap(),
            FileValue::Set {
                expr: "\"Adham\"".into(),
                line: 10
            }
        );
        assert_eq!(
            get_option("{\n  programs.git.enable = true;\n}", &p("programs.git")).unwrap(),
            FileValue::Partial { line: 2 }
        );
        assert_eq!(
            get_option(HOME, &p("programs.fish.enable")).unwrap(),
            FileValue::NotSet
        );
    }

    #[test]
    fn replaces_value_in_place() {
        let out = set_option(HOME, &p("programs.git.userName"), "\"Adham S\"").unwrap();
        assert_eq!(
            out,
            HOME.replace("userName = \"Adham\"", "userName = \"Adham S\"")
        );
    }

    #[test]
    fn inserts_into_deepest_nested_set() {
        let out = set_option(HOME, &p("programs.git.userEmail"), "\"a@b.c\"").unwrap();
        assert!(
            out.contains("    userName = \"Adham\";\n    userEmail = \"a@b.c\";\n  };"),
            "{out}"
        );
    }

    #[test]
    fn inserts_new_top_level_entry_after_last_one() {
        let out = set_option(HOME, &p("programs.fish.enable"), "true").unwrap();
        assert!(
            out.ends_with("  ];\n  programs.fish.enable = true;\n}\n"),
            "{out}"
        );
    }

    #[test]
    fn does_not_split_trailing_comments() {
        let src = "{\n  a = 1; # one\n}\n";
        assert_eq!(
            set_option(src, &p("b"), "2").unwrap(),
            "{\n  a = 1; # one\n  b = 2;\n}\n"
        );
    }

    #[test]
    fn handles_single_line_and_empty_sets() {
        assert_eq!(
            set_option("{ a = 1; }", &p("b"), "2").unwrap(),
            "{ a = 1; b = 2; }"
        );
        assert_eq!(set_option("{ }", &p("b"), "2").unwrap(), "{ b = 2; }");
        assert_eq!(
            set_option("{\n}\n", &p("b"), "2").unwrap(),
            "{\n  b = 2;\n}\n"
        );
    }

    #[test]
    fn indents_multiline_values() {
        let out = set_option(
            HOME,
            &p("programs.git.aliases"),
            &to_nix(&json!({"co": "checkout"})),
        )
        .unwrap();
        assert!(
            out.contains("    aliases = {\n      co = \"checkout\";\n    };\n  };"),
            "{out}"
        );
    }

    #[test]
    fn quotes_keys_that_need_it() {
        let out = set_option(
            "{ }",
            &[
                "xdg".into(),
                "configFile".into(),
                "foo.conf".into(),
                "text".into(),
            ],
            "\"x\"",
        )
        .unwrap();
        assert_eq!(out, r#"{ xdg.configFile."foo.conf".text = "x"; }"#);
        assert_eq!(
            get_option(
                &out,
                &[
                    "xdg".into(),
                    "configFile".into(),
                    "foo.conf".into(),
                    "text".into()
                ]
            )
            .unwrap(),
            FileValue::Set {
                expr: "\"x\"".into(),
                line: 1
            }
        );
    }

    #[test]
    fn refuses_to_clobber_partial_or_opaque_definitions() {
        assert!(set_option("{ programs.git.enable = true; }", &p("programs.git"), "{ }").is_err());
        let src = "{ lib, ... }: { programs.git = lib.mkIf true { enable = true; }; }";
        assert!(set_option(src, &p("programs.git.userName"), "\"x\"").is_err());
        assert!(matches!(
            get_option(src, &p("programs.git.enable")).unwrap(),
            FileValue::Opaque { .. }
        ));
    }

    #[test]
    fn rejects_invalid_values_and_unsupported_files() {
        assert!(set_option(HOME, &p("a"), "{ broken").is_err());
        assert!(set_option("{ lib, ... }: lib.mkMerge [ ]", &p("a"), "1").is_err());
    }

    #[test]
    fn unwraps_let_and_with() {
        let src = "{ pkgs, ... }:\nlet x = 1; in\nwith pkgs; {\n  a = x;\n}\n";
        assert_eq!(
            set_option(src, &p("b"), "2").unwrap(),
            "{ pkgs, ... }:\nlet x = 1; in\nwith pkgs; {\n  a = x;\n  b = 2;\n}\n"
        );
    }

    #[test]
    fn removes_line_and_empty_parents() {
        let out = remove_option(HOME, &p("home.stateVersion")).unwrap();
        assert!(!out.contains("stateVersion") && !out.contains("don't change"));
        assert!(
            out.contains("  home.username = \"adham\";\n\n  # Git"),
            "{out}"
        );

        let out = remove_option(
            &remove_option(HOME, &p("programs.git.enable")).unwrap(),
            &p("programs.git.userName"),
        )
        .unwrap();
        assert!(!out.contains("programs.git"), "{out}");
        assert!(out.contains("  # Git\n\n  home.packages"), "{out}");
    }

    #[test]
    fn removes_inline_entries() {
        assert_eq!(
            remove_option("{ a = 1; b = 2; }", &p("b")).unwrap(),
            "{ a = 1; }"
        );
        assert_eq!(
            remove_option("{ a = 1; b = 2; }", &p("a")).unwrap(),
            "{ b = 2; }"
        );
    }

    #[test]
    fn lists_defined_paths() {
        let paths = defined_paths(HOME).unwrap();
        for want in [
            "home.username",
            "programs",
            "programs.git",
            "programs.git.enable",
            "home.packages",
        ] {
            assert!(
                paths.contains(&want.to_string()),
                "{want} missing from {paths:?}"
            );
        }
    }

    #[test]
    fn reads_packages() {
        assert_eq!(
            get_packages(HOME).unwrap(),
            Packages {
                defined: true,
                with_pkgs: true,
                items: vec!["ripgrep".into(), "fd".into()]
            }
        );
        let plain = get_packages("{ pkgs, ... }: { home.packages = [ pkgs.jq ]; }").unwrap();
        assert_eq!(plain.items, vec!["jq"]);
        assert!(!plain.with_pkgs);
    }

    #[test]
    fn adds_packages_in_the_files_style() {
        let out = add_package(HOME, "bat").unwrap();
        assert!(out.contains("    fd\n    bat\n  ];"), "{out}");
        assert!(add_package(HOME, "fd").is_err());
        assert!(add_package(HOME, "rm -rf").is_err());

        let out = add_package("{ pkgs, ... }: { home.packages = [ pkgs.jq ]; }", "bat").unwrap();
        assert_eq!(
            out,
            "{ pkgs, ... }: { home.packages = [ pkgs.jq pkgs.bat ]; }"
        );

        let out = add_package("{\n  a = 1;\n}\n", "python3Packages.black").unwrap();
        assert_eq!(
            out,
            "{\n  a = 1;\n  home.packages = with pkgs; [\n    python3Packages.black\n  ];\n}\n"
        );
    }

    #[test]
    fn removes_packages() {
        let out = remove_package(HOME, "ripgrep").unwrap();
        assert!(out.contains("[\n    fd\n  ];"), "{out}");
        assert_eq!(
            remove_package(
                "{ pkgs, ... }: { home.packages = [ pkgs.jq pkgs.bat ]; }",
                "jq"
            )
            .unwrap(),
            "{ pkgs, ... }: { home.packages = [ pkgs.bat ]; }"
        );
        assert!(remove_package(HOME, "nope").is_err());
    }

    #[test]
    fn refuses_computed_package_lists() {
        let src = "{ pkgs, ... }: { home.packages = [ pkgs.jq ] ++ extra; }";
        assert!(get_packages(src).is_err());
    }

    #[test]
    fn renders_json_as_nix() {
        assert_eq!(to_nix(&json!(null)), "null");
        assert_eq!(to_nix(&json!(42)), "42");
        assert_eq!(to_nix(&json!("a \"q\" ${x} \\")), r#""a \"q\" \${x} \\""#);
        assert_eq!(to_nix(&json!(["a", 1])), "[\n  \"a\"\n  1\n]");
        assert_eq!(
            to_nix(&json!({"a b": true, "c": []})),
            "{\n  \"a b\" = true;\n  c = [ ];\n}"
        );
        assert_eq!(
            to_nix(&json!("line1\nit's ''x'' ${y}")),
            "''\n  line1\n  it's '''x''' ''${y}\n''"
        );
        for v in [json!("x\ny\n"), json!({"k": ["a\nb", {"n": 1}]})] {
            validate_expr(&to_nix(&v)).unwrap();
        }
    }
}
