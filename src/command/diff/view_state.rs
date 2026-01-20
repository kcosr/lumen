use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::diff_algo::{compute_side_by_side, find_hunk_ranges};
use super::persistence::DiffScope;
use super::state::{AppState, ReviewFilter, TagFilter};
use super::types::FocusedPanel;

const VIEW_STATE_VERSION: u32 = 1;
const VIEW_STATE_DIR: &str = ".lumen/state";
const WORKING_TREE_STATE_FILE: &str = "working-tree.json";

#[derive(Debug, Serialize, Deserialize)]
struct ViewStateFile {
    version: u32,
    last_updated: u64,
    current_file: Option<String>,
    scroll: u16,
    h_scroll: u16,
    focused_hunk: Option<usize>,
    viewed_files: Vec<String>,
    #[serde(default)]
    tag_filter: Option<String>,
    #[serde(default)]
    review_filter: Option<String>,
}

pub struct ViewStateManager {
    state_dir: PathBuf,
}

impl ViewStateManager {
    pub fn try_new() -> io::Result<Option<Self>> {
        let cwd = std::env::current_dir()?;
        let repo_root = match find_repo_root(&cwd) {
            Some(path) => path,
            None => return Ok(None),
        };
        let state_dir = repo_root.join(VIEW_STATE_DIR);
        Ok(Some(Self { state_dir }))
    }

    pub fn load_for_scope(&self, state: &mut AppState, scope: &DiffScope) -> io::Result<()> {
        let path = self.path_for_scope(scope);
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&path)?;
        let view_state: ViewStateFile = serde_json::from_str(&content).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid view state file {}: {}", path.display(), e),
            )
        })?;

        if view_state.version > VIEW_STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported view state version {} in {}",
                    view_state.version,
                    path.display()
                ),
            ));
        }
        let _ = view_state.last_updated;

        apply_view_state(state, view_state);
        Ok(())
    }

    pub fn save_for_scope(&self, state: &AppState, scope: &DiffScope) -> io::Result<()> {
        let path = self.path_for_scope(scope);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tag_filter = state.tag_filter.as_ref().map(|filter| match filter {
            TagFilter::Tag(tag) => format!("tag:{}", tag),
            TagFilter::Untagged => "tag:untagged".to_string(),
        });
        let review_filter = match state.review_filter {
            ReviewFilter::Reviewed => Some("reviewed".to_string()),
            ReviewFilter::Unreviewed => Some("unreviewed".to_string()),
            ReviewFilter::All => None,
        };
        let view_state = ViewStateFile {
            version: VIEW_STATE_VERSION,
            last_updated: current_timestamp(),
            current_file: state
                .file_diffs
                .get(state.current_file)
                .map(|diff| diff.filename.clone()),
            scroll: state.scroll,
            h_scroll: state.h_scroll,
            focused_hunk: state.focused_hunk,
            viewed_files: state
                .viewed_files
                .iter()
                .filter_map(|&idx| state.file_diffs.get(idx).map(|diff| diff.filename.clone()))
                .collect(),
            tag_filter,
            review_filter,
        };
        let json = serde_json::to_string_pretty(&view_state)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn path_for_scope(&self, scope: &DiffScope) -> PathBuf {
        match scope {
            DiffScope::WorkingTree { .. } => self.state_dir.join(WORKING_TREE_STATE_FILE),
            DiffScope::Commit { commit_id } => self.state_dir.join(format!("{}.json", commit_id)),
        }
    }
}

fn apply_view_state(state: &mut AppState, view_state: ViewStateFile) {
    let viewed_files: HashSet<String> = view_state.viewed_files.into_iter().collect();
    state.viewed_files = state
        .file_diffs
        .iter()
        .enumerate()
        .filter(|(_, diff)| viewed_files.contains(&diff.filename))
        .map(|(idx, _)| idx)
        .collect();

    if let Some(filename) = view_state.current_file {
        if let Some(file_index) = state
            .file_diffs
            .iter()
            .position(|diff| diff.filename == filename)
        {
            state.current_file = file_index;
            state.reveal_file(file_index);
        }
    }

    state.focused_panel = FocusedPanel::DiffView;

    if let Some(diff) = state.file_diffs.get(state.current_file) {
        let side_by_side =
            compute_side_by_side(&diff.old_content, &diff.new_content, state.settings.tab_width);
        let max_scroll = side_by_side.len().saturating_sub(1);
        state.scroll = view_state.scroll.min(max_scroll as u16);
        state.h_scroll = view_state.h_scroll;

        let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);
        if let Some(hunk_index) = view_state.focused_hunk {
            if hunk_index < hunks.len() {
                state.focused_hunk = Some(hunk_index);
            }
        }
    }

    state.tag_filter = match view_state.tag_filter.as_deref() {
        Some(filter) if filter == "tag:untagged" => Some(TagFilter::Untagged),
        Some(filter) if filter.starts_with("tag:") => Some(TagFilter::Tag(filter[4..].to_string())),
        _ => None,
    };
    state.review_filter = match view_state.review_filter.as_deref() {
        Some("reviewed") => ReviewFilter::Reviewed,
        Some("unreviewed") => ReviewFilter::Unreviewed,
        _ => ReviewFilter::All,
    };
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
