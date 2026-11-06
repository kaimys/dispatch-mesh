//! The ignore policy of a file tree, computed from the rule files the tree itself
//! contains (ADR-0013). Mesh decides what a tree may hold from that tree, never
//! from the rules currently on disk, so a receiving device, a replayed apply
//! journal and two devices computing the same tree all reach the same answer.

use crate::model::Files;
use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Rule files in the precedence order of the folder walker: a match in a later
/// kind wins over any match in an earlier one, whatever the directory depth.
pub const RULE_FILES: [&str; 3] = [".gitignore", ".ignore", ".meshignore"];

pub fn is_rule_file(path: &str) -> bool {
    RULE_FILES.contains(&path.rsplit('/').next().unwrap_or_default())
}

/// Paths Mesh never synchronizes, whatever the rules say.
pub fn fixed_exclude(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    let top = path.split('/').next().unwrap_or_default();
    top == ".mesh"
        || top == ".git"
        || name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

pub struct Policy {
    root: PathBuf,
    /// Directory ("" for the root) -> one matcher per kind in `RULE_FILES`.
    rules: BTreeMap<String, [Gitignore; 3]>,
}

impl Policy {
    pub fn from_files(root: &Path, files: &Files) -> Result<Self> {
        let mut builders = BTreeMap::<String, [Option<GitignoreBuilder>; 3]>::new();
        for (path, bytes) in files {
            let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
            let Some(kind) = RULE_FILES.iter().position(|rule| *rule == name) else {
                continue;
            };
            let base = if directory.is_empty() {
                root.to_path_buf()
            } else {
                root.join(directory)
            };
            let builder = builders.entry(directory.to_string()).or_default()[kind]
                .get_or_insert_with(|| GitignoreBuilder::new(&base));
            let source = root.join(path);
            // The same line handling as reading the file from disk: BOM on the
            // first line stripped, trailing whitespace trimmed by `add_line`.
            for (index, line) in String::from_utf8_lossy(bytes).lines().enumerate() {
                let line = if index == 0 {
                    line.trim_start_matches('\u{feff}')
                } else {
                    line
                };
                builder.add_line(Some(source.clone()), line)?;
            }
        }
        let mut rules = BTreeMap::new();
        for (directory, kinds) in builders {
            let [gitignore, ignore, meshignore] =
                kinds.map(|builder| builder.map_or_else(|| Ok(Gitignore::empty()), |b| b.build()));
            rules.insert(directory, [gitignore?, ignore?, meshignore?]);
        }
        Ok(Self {
            root: root.to_path_buf(),
            rules,
        })
    }

    /// Whether the tree may not hold `path`: a fixed exclude, an ignored file, or
    /// a file inside an ignored directory. Like the walker, a `!` rule cannot
    /// re-include a file whose directory is ignored. A rule file is never ignored
    /// by a rule that matches the file itself (ADR-0013).
    pub fn ignores(&self, path: &str) -> bool {
        if fixed_exclude(path) {
            return true;
        }
        let parts: Vec<_> = path.split('/').collect();
        // A rule file always belongs to the tree; only its folders can exclude it.
        let last = if is_rule_file(path) {
            parts.len() - 1
        } else {
            parts.len()
        };
        for depth in 1..=last {
            let candidate = self.root.join(parts[..depth].join("/"));
            let is_dir = depth < parts.len();
            let mut kinds = [None; 3];
            // Rules from the candidate's ancestors; the deepest match of each kind wins.
            for owner in 0..depth {
                let Some(matchers) = self.rules.get(&parts[..owner].join("/")) else {
                    continue;
                };
                for (kind, matcher) in matchers.iter().enumerate() {
                    let matched = matcher.matched(&candidate, is_dir);
                    if !matched.is_none() {
                        kinds[kind] = Some(matched.is_ignore());
                    }
                }
            }
            if kinds.iter().rev().flatten().next() == Some(&true) {
                return true;
            }
        }
        false
    }
}
