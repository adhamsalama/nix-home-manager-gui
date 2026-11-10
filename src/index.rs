//! An on-disk SQLite index of the user's nixpkgs, so package searches don't
//! re-evaluate nixpkgs each time. Built once per nixpkgs revision; searching
//! reads only matching rows, keeping memory use low.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};

use crate::nix::PackageInfo;

/// Bump when the schema changes, so old indexes are rebuilt.
const SCHEMA: &str = "1";

/// What an index was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The nixpkgs source's store path; see `nix::nixpkgs_identity`.
    pub nixpkgs: String,
    /// Which nixpkgs it is, for display, e.g. "your nixpkgs channel".
    pub source: String,
}

fn open_read(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

/// What the index at `path` was built from, if it exists and is current.
pub fn built(path: &Path) -> Option<Built> {
    let conn = open_read(path).ok()?;
    let get = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    };
    if get("schema")? != SCHEMA {
        return None;
    }
    Some(Built {
        nixpkgs: get("nixpkgs")?,
        source: get("source")?,
    })
}

/// Writes a new index to `path`, replacing any old one only once complete.
pub fn write(path: &Path, built: &Built, packages: &[PackageInfo]) -> Result<()> {
    let dir = path.parent().context("index path has no directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    {
        let mut conn = Connection::open(tmp.path())?;
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE packages (
                 attr TEXT PRIMARY KEY,
                 pname TEXT NOT NULL,
                 version TEXT NOT NULL,
                 description TEXT NOT NULL,
                 -- JSON array of systems; NULL when they couldn't be evaluated.
                 platforms TEXT
             );",
        )?;
        let tx = conn.transaction()?;
        {
            let mut meta = tx.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
            meta.execute(params!["schema", SCHEMA])?;
            meta.execute(params!["nixpkgs", built.nixpkgs])?;
            meta.execute(params!["source", built.source])?;
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO packages (attr, pname, version, description, platforms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for p in packages {
                let platforms = p
                    .platforms
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                insert.execute(params![
                    p.attr,
                    p.pname,
                    p.version,
                    p.description,
                    platforms
                ])?;
            }
        }
        tx.commit()?;
    }
    tmp.persist(path)
        .context("couldn't save the package index")?;
    Ok(())
}

/// Escapes `%`, `_` and `\` for a LIKE pattern with `ESCAPE '\'`.
fn like_pattern(term: &str) -> String {
    let mut out = String::from("%");
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Packages whose attribute path, name or description contains every word of
/// `query` (case-insensitively), like `nix search` does. Unranked.
pub fn search(path: &Path, query: &str) -> Result<Vec<PackageInfo>> {
    let conn = open_read(path)?;
    let terms: Vec<String> = query.split_whitespace().map(like_pattern).collect();
    let mut sql = String::from("SELECT attr, pname, version, description, platforms FROM packages");
    for i in 0..terms.len() {
        sql.push_str(if i == 0 { " WHERE " } else { " AND " });
        let n = i + 1;
        sql.push_str(&format!(
            "(attr LIKE ?{n} ESCAPE '\\' OR pname LIKE ?{n} ESCAPE '\\' OR description LIKE ?{n} ESCAPE '\\')"
        ));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(&terms), |r| {
        let platforms: Option<String> = r.get(4)?;
        Ok(PackageInfo {
            attr: r.get(0)?,
            pname: r.get(1)?,
            version: r.get(2)?,
            description: r.get(3)?,
            platforms: platforms.and_then(|p| serde_json::from_str(&p).ok()),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(attr: &str, desc: &str, platforms: Option<&[&str]>) -> PackageInfo {
        PackageInfo {
            attr: attr.into(),
            pname: attr.rsplit('.').next().unwrap().into(),
            version: "1.0".into(),
            description: desc.into(),
            platforms: platforms.map(|ps| ps.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn writes_and_searches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("packages.sqlite");
        assert_eq!(built(&path), None);
        let b = Built {
            nixpkgs: "/nix/store/abc-source".into(),
            source: "your nixpkgs channel".into(),
        };
        write(
            &path,
            &b,
            &[
                pkg(
                    "neovim",
                    "Vim text editor fork",
                    Some(&["x86_64-linux", "aarch64-darwin"]),
                ),
                pkg("neovim-qt", "Neovim client library and GUI, in Qt5", None),
                pkg(
                    "ripgrep",
                    "Search tool that ignores 100% of .gitignore'd files",
                    Some(&[]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(built(&path), Some(b));

        let attrs = |q: &str| -> Vec<String> {
            let mut a: Vec<String> = search(&path, q)
                .unwrap()
                .into_iter()
                .map(|p| p.attr)
                .collect();
            a.sort();
            a
        };
        assert_eq!(attrs("NEOVIM"), ["neovim", "neovim-qt"]);
        assert_eq!(attrs("neovim qt"), ["neovim-qt"]);
        assert_eq!(attrs("editor"), ["neovim"]);
        // LIKE wildcards in the query are literal.
        assert_eq!(attrs("100%"), ["ripgrep"]);
        assert!(attrs("_").is_empty());

        let nvim = search(&path, "vim text").unwrap().remove(0);
        assert_eq!(nvim.platforms.unwrap(), ["x86_64-linux", "aarch64-darwin"]);
        assert_eq!(search(&path, "ripgrep").unwrap()[0].platforms, Some(vec![]));
        assert_eq!(search(&path, "neovim-qt").unwrap()[0].platforms, None);
    }

    #[test]
    fn rebuilding_replaces_the_old_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("packages.sqlite");
        let old = Built {
            nixpkgs: "old".into(),
            source: "s".into(),
        };
        write(&path, &old, &[pkg("hello", "", None)]).unwrap();
        let new = Built {
            nixpkgs: "new".into(),
            source: "s".into(),
        };
        write(&path, &new, &[pkg("cowsay", "", None)]).unwrap();
        assert_eq!(built(&path), Some(new));
        assert!(search(&path, "hello").unwrap().is_empty());
        assert_eq!(search(&path, "cowsay").unwrap().len(), 1);
    }
}
