use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::diff_algo::{compute_side_by_side, find_hunk_ranges};
use super::persistence::{build_hunk_context, find_matching_hunk, DiffScope};
use super::state::{AppState, HunkReview};

const REVIEW_VERSION: u32 = 2;
const REVIEW_DIR: &str = ".lumen/review";
const REVIEW_INDEX_FILE: &str = "working-tree.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReviewScope {
    WorkingTree,
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewFile {
    version: u32,
    last_updated: u64,
    scope: ReviewScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_commit_id: Option<String>,
    hunks: Vec<PersistentReview>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistentReview {
    filename: String,
    hunk_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_line_range: Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_line_range:  Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context_changed: Vec<String>,
}

pub struct ReviewManager {
    review_dir: PathBuf,
}

impl ReviewManager {
    pub fn try_new() -> io::Result<Option<Self>> {
        let cwd = std::env::current_dir()?;
        let repo_root = match find_repo_root(&cwd) {
            Some(path) => path,
            None => return Ok(None),
        };
        let review_dir = repo_root.join(REVIEW_DIR);
        Ok(Some(Self { review_dir }))
    }

    pub fn load_for_scope(&self, state: &mut AppState, scope: &DiffScope) -> io::Result<()> {
        state.reviewed_hunks.clear();
        let review_file = self.load_review_file(scope)?;
        let file_info: std::collections::HashMap<&str, (usize, usize)> = state
            .file_diffs
            .iter()
            .enumerate()
            .map(|(idx, diff)| {
                let side_by_side = compute_side_by_side(
                    &diff.old_content,
                    &diff.new_content,
                    state.settings.tab_width,
                );
                let hunk_count =
                    find_hunk_ranges(&side_by_side, state.settings.unified_context).len();
                (diff.filename.as_str(), (idx, hunk_count))
            })
            .collect();

        for hunk in review_file.hunks {
            if let Some(&(file_index, hunk_count)) = file_info.get(hunk.filename.as_str()) {
                let mut matched = None;
                if hunk.new_line_range.is_some()
                    || hunk.old_line_range.is_some()
                    || !hunk.context_changed.is_empty()
                {
                    matched = find_matching_hunk(
                        state,
                        file_index,
                        hunk.new_line_range,
                        hunk.old_line_range,
                        &hunk.context_changed,
                    );
                }
                if matched.is_none() && hunk.hunk_index < hunk_count {
                    matched = Some(hunk.hunk_index);
                }
                if let Some(hunk_index) = matched {
                    state.reviewed_hunks.push(HunkReview {
                        file_index,
                        hunk_index,
                        filename: hunk.filename,
                    });
                }
            }
        }

        Ok(())
    }

    pub fn set_hunk_reviewed(
        &self,
        state: &AppState,
        file_index: usize,
        hunk_index: usize,
        reviewed: bool,
        scope: &DiffScope,
    ) -> io::Result<()> {
        let mut review_file = self.load_review_file(scope)?;
        let filename = match state.file_diffs.get(file_index) {
            Some(diff) => diff.filename.clone(),
            None => return Ok(()),
        };

        let hunk_context = build_hunk_context(state, file_index, hunk_index);
        if reviewed {
            let mut matched_pos = review_file
                .hunks
                .iter()
                .position(|hunk| hunk.filename == filename && hunk.hunk_index == hunk_index);
            if matched_pos.is_none() {
                matched_pos = review_file.hunks.iter().position(|hunk| {
                    if hunk.filename != filename {
                        return false;
                    }
                    find_matching_hunk(
                        state,
                        file_index,
                        hunk.new_line_range,
                        hunk.old_line_range,
                        &hunk.context_changed,
                    ) == Some(hunk_index)
                });
            }
            if let Some(pos) = matched_pos {
                let existing = &mut review_file.hunks[pos];
                existing.hunk_index = hunk_index;
                existing.old_line_range =
                    hunk_context.as_ref().and_then(|ctx| ctx.old_line_range);
                existing.new_line_range =
                    hunk_context.as_ref().and_then(|ctx| ctx.new_line_range);
                existing.context_changed = hunk_context
                    .as_ref()
                    .map(|ctx| ctx.context_changed.clone())
                    .unwrap_or_default();
            } else {
                review_file.hunks.push(PersistentReview {
                    filename,
                    hunk_index,
                    old_line_range: hunk_context.as_ref().and_then(|ctx| ctx.old_line_range),
                    new_line_range: hunk_context.as_ref().and_then(|ctx| ctx.new_line_range),
                    context_changed: hunk_context
                        .as_ref()
                        .map(|ctx| ctx.context_changed.clone())
                        .unwrap_or_default(),
                });
            }
        } else {
            review_file
                .hunks
                .retain(|hunk| !(hunk.filename == filename && hunk.hunk_index == hunk_index));
        }

        review_file.last_updated = current_timestamp();
        self.save_review_file(scope, &review_file)?;
        Ok(())
    }

    fn load_review_file(&self, scope: &DiffScope) -> io::Result<ReviewFile> {
        let path = self.scope_path(scope);
        if !path.exists() {
            return Ok(ReviewFile {
                version: REVIEW_VERSION,
                last_updated: current_timestamp(),
                scope: match scope {
                    DiffScope::WorkingTree { .. } => ReviewScope::WorkingTree,
                    DiffScope::Commit { .. } => ReviewScope::Commit,
                },
                commit_id: match scope {
                    DiffScope::Commit { commit_id } => Some(commit_id.clone()),
                    _ => None,
                },
                base_commit_id: match scope {
                    DiffScope::WorkingTree { base_commit_id } => Some(base_commit_id.clone()),
                    _ => None,
                },
                hunks: Vec::new(),
            });
        }

        let content = fs::read_to_string(&path)?;
        let mut file: ReviewFile = serde_json::from_str(&content).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid review file {}: {}", path.display(), e),
            )
        })?;

        if file.version > REVIEW_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported review version {} in {}",
                    file.version,
                    path.display()
                ),
            ));
        }

        file.scope = match scope {
            DiffScope::WorkingTree { .. } => ReviewScope::WorkingTree,
            DiffScope::Commit { .. } => ReviewScope::Commit,
        };
        Ok(file)
    }

    fn save_review_file(&self, scope: &DiffScope, file: &ReviewFile) -> io::Result<()> {
        let path = self.scope_path(scope);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(file)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn scope_path(&self, scope: &DiffScope) -> PathBuf {
        match scope {
            DiffScope::WorkingTree { .. } => self.review_dir.join(REVIEW_INDEX_FILE),
            DiffScope::Commit { commit_id } => self.review_dir.join(format!("{}.json", commit_id)),
        }
    }
}

fn find_repo_root(start_dir: &Path) -> Option<PathBuf> {
    let mut current = start_dir;
    loop {
        if current.join(".git").exists() || current.join(".jj").is_dir() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
