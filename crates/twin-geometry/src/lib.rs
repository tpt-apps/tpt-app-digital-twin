//! Parametric 3D modelling and Git-like geometry history (wraps tpt-vertex).
//!
//! This module provides the history layer: a content-addressed commit graph
//! with branches, diff and three-way merge. It works on parameter maps for
//! now. Geometry snapshots from `tpt-vertex` will use the same graph once the
//! kernel is wired in.
//!
//! Commits are identified by the SHA-256 of their parents, message and content.
//! Equal histories therefore give equal IDs on every machine, which matters
//! when twins move between machines.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Parameter values at one point in history.
pub type Content = BTreeMap<String, f64>;

#[derive(Debug, Error, PartialEq)]
pub enum HistoryError {
    #[error("no branch named `{0}`")]
    NoSuchBranch(String),
    #[error("branch `{0}` already exists")]
    BranchExists(String),
    #[error("no commit with id {0}")]
    NoSuchCommit(String),
    #[error("nothing to commit on `{0}`: content is unchanged")]
    NothingToCommit(String),
    #[error("merge conflict on parameters: {0:?}")]
    Conflict(Vec<String>),
    #[error("branches have no common ancestor")]
    NoCommonAncestor,
    #[error("cannot merge `{0}` into itself")]
    SelfMerge(String),
}

/// A commit ID: lowercase hex SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommitId(String);

impl CommitId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Short form, like git.
        f.write_str(&self.0[..12.min(self.0.len())])
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
    pub message: String,
    pub content: Content,
}

/// One parameter that differs between two commits. `None` means absent.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: String,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

/// A commit graph with named branches. `head` is the branch new commits go to.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    commits: BTreeMap<CommitId, Commit>,
    branches: BTreeMap<String, CommitId>,
    head: String,
    /// Working content: what the next commit will record.
    working: Content,
}

impl History {
    /// Start a history on branch `main` with an initial commit.
    pub fn new(initial: Content, message: &str) -> Self {
        let root = make_commit(Vec::new(), message, &initial);
        let mut commits = BTreeMap::new();
        let id = root.id.clone();
        commits.insert(id.clone(), root);
        let mut branches = BTreeMap::new();
        branches.insert("main".to_string(), id);
        Self {
            commits,
            branches,
            head: "main".to_string(),
            working: initial,
        }
    }

    pub fn head_branch(&self) -> &str {
        &self.head
    }

    pub fn working(&self) -> &Content {
        &self.working
    }

    /// Set a parameter in the working content. Commit to record it.
    pub fn set(&mut self, key: impl Into<String>, value: f64) {
        self.working.insert(key.into(), value);
    }

    pub fn branches(&self) -> impl Iterator<Item = (&str, &CommitId)> {
        self.branches.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn commit(&self, id: &CommitId) -> Result<&Commit, HistoryError> {
        self.commits
            .get(id)
            .ok_or_else(|| HistoryError::NoSuchCommit(id.0.clone()))
    }

    fn tip(&self) -> &CommitId {
        &self.branches[&self.head]
    }

    /// Record the working content on the current branch.
    pub fn commit_changes(&mut self, message: &str) -> Result<CommitId, HistoryError> {
        let parent = self.tip().clone();
        if self.commits[&parent].content == self.working {
            return Err(HistoryError::NothingToCommit(self.head.clone()));
        }
        let commit = make_commit(vec![parent], message, &self.working);
        let id = commit.id.clone();
        self.commits.insert(id.clone(), commit);
        self.branches.insert(self.head.clone(), id.clone());
        Ok(id)
    }

    /// Create a branch at the current tip.
    pub fn create_branch(&mut self, name: &str) -> Result<(), HistoryError> {
        if self.branches.contains_key(name) {
            return Err(HistoryError::BranchExists(name.to_string()));
        }
        self.branches.insert(name.to_string(), self.tip().clone());
        Ok(())
    }

    /// Switch to a branch and load its content into the working set.
    pub fn switch(&mut self, name: &str) -> Result<(), HistoryError> {
        let tip = self
            .branches
            .get(name)
            .ok_or_else(|| HistoryError::NoSuchBranch(name.to_string()))?
            .clone();
        self.working = self.commits[&tip].content.clone();
        self.head = name.to_string();
        Ok(())
    }

    /// Commits reachable from the current branch, newest first, following the
    /// first parent.
    pub fn log(&self) -> Vec<&Commit> {
        let mut out = Vec::new();
        let mut cur = Some(self.tip().clone());
        while let Some(id) = cur {
            let commit = &self.commits[&id];
            out.push(commit);
            cur = commit.parents.first().cloned();
        }
        out
    }

    /// Parameter changes from commit `a` to commit `b`, sorted by key.
    pub fn diff(&self, a: &CommitId, b: &CommitId) -> Result<Vec<Change>, HistoryError> {
        let before = &self.commit(a)?.content;
        let after = &self.commit(b)?.content;
        Ok(diff_content(before, after))
    }

    /// Merge branch `from` into the current branch. The working content is
    /// replaced by the merge result, and a merge commit is recorded.
    ///
    /// Three-way: a key changed on one side only takes that side's value. A key
    /// changed differently on both sides is a conflict, and nothing is merged.
    pub fn merge(&mut self, from: &str, message: &str) -> Result<CommitId, HistoryError> {
        if from == self.head {
            return Err(HistoryError::SelfMerge(from.to_string()));
        }
        let ours = self.tip().clone();
        let theirs = self
            .branches
            .get(from)
            .ok_or_else(|| HistoryError::NoSuchBranch(from.to_string()))?
            .clone();
        let base = self.common_ancestor(&ours, &theirs)?;

        let base_c = &self.commits[&base].content;
        let ours_c = &self.commits[&ours].content;
        let theirs_c = &self.commits[&theirs].content;

        let keys: BTreeSet<&String> = base_c
            .keys()
            .chain(ours_c.keys())
            .chain(theirs_c.keys())
            .collect();

        let mut merged = Content::new();
        let mut conflicts = Vec::new();
        for key in keys {
            let b = base_c.get(key);
            let o = ours_c.get(key);
            let t = theirs_c.get(key);
            let chosen = if o == t {
                o
            } else if o == b {
                t
            } else if t == b {
                o
            } else {
                conflicts.push(key.clone());
                continue;
            };
            if let Some(v) = chosen {
                merged.insert(key.clone(), *v);
            }
        }
        if !conflicts.is_empty() {
            return Err(HistoryError::Conflict(conflicts));
        }

        let commit = make_commit(vec![ours, theirs], message, &merged);
        let id = commit.id.clone();
        self.commits.insert(id.clone(), commit);
        self.branches.insert(self.head.clone(), id.clone());
        self.working = merged;
        Ok(id)
    }

    /// Nearest common ancestor, found by breadth-first search from `a`.
    fn common_ancestor(&self, a: &CommitId, b: &CommitId) -> Result<CommitId, HistoryError> {
        let reachable_b = self.ancestors(b);
        let mut queue = VecDeque::from([a.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if reachable_b.contains(&id) {
                return Ok(id);
            }
            queue.extend(self.commits[&id].parents.iter().cloned());
        }
        Err(HistoryError::NoCommonAncestor)
    }

    fn ancestors(&self, from: &CommitId) -> BTreeSet<CommitId> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::from([from.clone()]);
        while let Some(id) = queue.pop_front() {
            if out.insert(id.clone()) {
                queue.extend(self.commits[&id].parents.iter().cloned());
            }
        }
        out
    }
}

fn diff_content(before: &Content, after: &Content) -> Vec<Change> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let b = before.get(key).copied();
            let a = after.get(key).copied();
            (b != a).then(|| Change {
                key: key.clone(),
                before: b,
                after: a,
            })
        })
        .collect()
}

fn make_commit(parents: Vec<CommitId>, message: &str, content: &Content) -> Commit {
    // Hash the canonical JSON of the parts that define the commit.
    #[derive(Serialize)]
    struct Body<'a> {
        parents: &'a [CommitId],
        message: &'a str,
        content: &'a Content,
    }
    let body = serde_json::to_vec(&Body {
        parents: &parents,
        message,
        content,
    })
    .expect("commit body is always serialisable");
    let digest = Sha256::digest(&body);
    let id = CommitId(digest.iter().map(|b| format!("{b:02x}")).collect());
    Commit {
        id,
        parents,
        message: message.to_string(),
        content: content.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, f64)]) -> Content {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn base() -> History {
        History::new(params(&[("width", 10.0), ("height", 5.0)]), "initial")
    }

    #[test]
    fn commits_record_changes_in_order() {
        let mut h = base();
        h.set("width", 12.0);
        h.commit_changes("wider").unwrap();
        h.set("height", 6.0);
        h.commit_changes("taller").unwrap();

        let messages: Vec<&str> = h.log().iter().map(|c| c.message.as_str()).collect();
        assert_eq!(messages, ["taller", "wider", "initial"]);
    }

    #[test]
    fn unchanged_content_is_not_committed() {
        let mut h = base();
        assert_eq!(
            h.commit_changes("noop"),
            Err(HistoryError::NothingToCommit("main".into()))
        );
    }

    #[test]
    fn identical_histories_have_identical_ids() {
        let a = base();
        let b = base();
        assert_eq!(a.log()[0].id, b.log()[0].id);
    }

    #[test]
    fn switch_loads_branch_content() {
        let mut h = base();
        h.create_branch("exp").unwrap();
        h.switch("exp").unwrap();
        h.set("width", 99.0);
        h.commit_changes("experiment").unwrap();
        h.switch("main").unwrap();
        assert_eq!(h.working()["width"], 10.0);
    }

    #[test]
    fn diff_reports_changed_added_and_removed_keys() {
        let mut h = base();
        let first = h.log()[0].id.clone();
        h.set("width", 12.0);
        h.working.remove("height");
        h.set("depth", 3.0);
        let second = h.commit_changes("edit").unwrap();

        let changes = h.diff(&first, &second).unwrap();
        assert_eq!(
            changes,
            vec![
                Change {
                    key: "depth".into(),
                    before: None,
                    after: Some(3.0)
                },
                Change {
                    key: "height".into(),
                    before: Some(5.0),
                    after: None
                },
                Change {
                    key: "width".into(),
                    before: Some(10.0),
                    after: Some(12.0)
                },
            ]
        );
    }

    #[test]
    fn merges_non_conflicting_changes_from_both_sides() {
        let mut h = base();
        h.create_branch("feature").unwrap();

        h.set("width", 20.0); // main changes width
        h.commit_changes("main width").unwrap();

        h.switch("feature").unwrap();
        h.set("height", 8.0); // feature changes height
        h.commit_changes("feature height").unwrap();

        h.switch("main").unwrap();
        h.merge("feature", "merge feature").unwrap();
        assert_eq!(h.working()["width"], 20.0);
        assert_eq!(h.working()["height"], 8.0);
        assert_eq!(h.log()[0].parents.len(), 2);
    }

    #[test]
    fn conflicting_changes_are_reported_and_not_applied() {
        let mut h = base();
        h.create_branch("feature").unwrap();

        h.set("width", 20.0);
        h.commit_changes("main width").unwrap();
        let main_tip = h.log()[0].id.clone();

        h.switch("feature").unwrap();
        h.set("width", 30.0);
        h.commit_changes("feature width").unwrap();

        h.switch("main").unwrap();
        assert_eq!(
            h.merge("feature", "merge"),
            Err(HistoryError::Conflict(vec!["width".into()]))
        );
        // The branch did not move.
        assert_eq!(h.log()[0].id, main_tip);
    }

    #[test]
    fn same_change_on_both_sides_is_not_a_conflict() {
        let mut h = base();
        h.create_branch("feature").unwrap();
        h.set("width", 20.0);
        h.commit_changes("main").unwrap();
        h.switch("feature").unwrap();
        h.set("width", 20.0);
        h.commit_changes("feature").unwrap();
        h.switch("main").unwrap();
        assert!(h.merge("feature", "merge").is_ok());
    }

    #[test]
    fn merging_a_branch_into_itself_is_refused() {
        let mut h = base();
        assert_eq!(
            h.merge("main", "x"),
            Err(HistoryError::SelfMerge("main".into()))
        );
    }

    #[test]
    fn unknown_branch_is_reported() {
        let mut h = base();
        assert_eq!(
            h.switch("nope"),
            Err(HistoryError::NoSuchBranch("nope".into()))
        );
    }
}
