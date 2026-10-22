//! An in-memory working copy of the config file. Edits are staged here and
//! only written to disk on save, so the user can review a diff first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};

pub struct WorkingCopy {
    pub path: PathBuf,
    /// The file as it was on disk when loaded or last saved.
    original: String,
    pub current: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// "hunk", "context", "add" or "del".
    pub kind: &'static str,
    pub text: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
}

impl WorkingCopy {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("couldn't read {}", path.display()))?;
        Ok(WorkingCopy {
            path: path.to_path_buf(),
            original: text.clone(),
            current: text,
        })
    }

    pub fn dirty(&self) -> bool {
        self.original != self.current
    }

    /// Whether the file on disk no longer matches what was loaded.
    pub fn changed_on_disk(&self) -> bool {
        std::fs::read_to_string(&self.path).map_or(true, |disk| disk != self.original)
    }

    pub fn stage(&mut self, edit: impl FnOnce(&str) -> Result<String>) -> Result<()> {
        self.current = edit(&self.current)?;
        Ok(())
    }

    pub fn discard(&mut self) {
        self.current = self.original.clone();
    }

    /// Writes the staged text, keeping the previous contents in a `.bak` file.
    pub fn save(&mut self) -> Result<()> {
        if self.changed_on_disk() {
            bail!(
                "{} was changed outside the app since it was loaded; discard your changes to reload it",
                self.path.display()
            );
        }
        let backup = self.path.with_extension("nix.bak");
        std::fs::write(&backup, &self.original)
            .with_context(|| format!("couldn't write {}", backup.display()))?;
        std::fs::write(&self.path, &self.current)
            .with_context(|| format!("couldn't write {}", self.path.display()))?;
        self.original = self.current.clone();
        Ok(())
    }

    pub fn diff(&self) -> Vec<DiffLine> {
        let diff = TextDiff::from_lines(&self.original, &self.current);
        let mut out = Vec::new();
        for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
            out.push(DiffLine {
                kind: "hunk",
                text: hunk.header().to_string(),
                old_line: None,
                new_line: None,
            });
            for change in hunk.iter_changes() {
                out.push(DiffLine {
                    kind: match change.tag() {
                        ChangeTag::Equal => "context",
                        ChangeTag::Insert => "add",
                        ChangeTag::Delete => "del",
                    },
                    text: change.value().trim_end_matches('\n').to_string(),
                    old_line: change.old_index().map(|i| i + 1),
                    new_line: change.new_index().map(|i| i + 1),
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_diffs_and_saves_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.nix");
        std::fs::write(&path, "{\n  a = 1;\n}\n").unwrap();

        let mut wc = WorkingCopy::load(&path).unwrap();
        wc.stage(|t| crate::nix_edit::set_option(t, &["a".into()], "2"))
            .unwrap();
        assert!(wc.dirty());
        let diff = wc.diff();
        assert!(diff.iter().any(|l| l.kind == "del" && l.text == "  a = 1;"));
        assert!(
            diff.iter()
                .any(|l| l.kind == "add" && l.text == "  a = 2;" && l.new_line == Some(2))
        );

        wc.save().unwrap();
        assert!(!wc.dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n  a = 2;\n}\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("home.nix.bak")).unwrap(),
            "{\n  a = 1;\n}\n"
        );
    }

    #[test]
    fn refuses_to_overwrite_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.nix");
        std::fs::write(&path, "{ }").unwrap();
        let mut wc = WorkingCopy::load(&path).unwrap();
        wc.stage(|t| crate::nix_edit::set_option(t, &["a".into()], "1"))
            .unwrap();
        std::fs::write(&path, "{ b = 1; }").unwrap();
        assert!(wc.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ b = 1; }");
    }
}
