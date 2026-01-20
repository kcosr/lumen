use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::diff_algo::{compute_side_by_side, find_hunk_ranges};
use super::persistence::{build_hunk_context, find_matching_hunk, DiffScope};
use super::state::{AppState, HunkTags};

const TAGS_VERSION: u32 = 2;
const TAG_DIR: &str = ".lumen/tags";
const TAG_INDEX_FILE: &str = "index.json";
const WORKING_TREE_FILE: &str = "working-tree.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TagScope {
    WorkingTree,
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TagIndexFile {
    version: u32,
    last_updated: u64,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TagFile {
    version: u32,
    last_updated: u64,
    scope: TagScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_commit_id: Option<String>,
    hunks: Vec<PersistentHunkTags>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistentHunkTags {
    filename: String,
    hunk_index: usize,
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_line_range: Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_line_range: Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context_changed: Vec<String>,
}

pub struct TagManager {
    tag_dir: PathBuf,
    index: TagIndexFile,
}

impl TagManager {
    pub fn try_new() -> io::Result<Option<Self>> {
        let cwd = std::env::current_dir()?;
        let repo_root = match find_repo_root(&cwd) {
            Some(path) => path,
            None => return Ok(None),
        };
        let tag_dir = repo_root.join(TAG_DIR);
        let index_path = tag_dir.join(TAG_INDEX_FILE);
        let index = load_index_file(&index_path)?;
        Ok(Some(Self { tag_dir, index }))
    }

    pub fn tags(&self) -> &[String] {
        &self.index.tags
    }

    pub fn load_for_scope(&mut self, state: &mut AppState, scope: &DiffScope) -> io::Result<()> {
        state.hunk_tags.clear();
        state.tag_inventory = self.index.tags.clone();

        let tag_file = self.load_tag_file(scope)?;
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

        for hunk in tag_file.hunks {
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
                    state.hunk_tags.push(HunkTags {
                        file_index,
                        hunk_index,
                        filename: hunk.filename,
                        tags: hunk.tags,
                    });
                }
            }
        }

        Ok(())
    }

    pub fn set_hunk_tags(
        &mut self,
        state: &AppState,
        file_index: usize,
        hunk_index: usize,
        tags: Vec<String>,
        scope: &DiffScope,
    ) -> io::Result<()> {
        let mut tag_file = self.load_tag_file(scope)?;
        let filename = match state.file_diffs.get(file_index) {
            Some(diff) => diff.filename.clone(),
            None => return Ok(()),
        };

        let hunk_context = build_hunk_context(state, file_index, hunk_index);
        let tags = normalize_tags(tags);
        if tags.is_empty() {
            tag_file
                .hunks
                .retain(|hunk| !(hunk.filename == filename && hunk.hunk_index == hunk_index));
        } else {
            let mut matched_pos = tag_file
                .hunks
                .iter()
                .position(|hunk| hunk.filename == filename && hunk.hunk_index == hunk_index);
            if matched_pos.is_none() {
                matched_pos = tag_file.hunks.iter().position(|hunk| {
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
                let existing = &mut tag_file.hunks[pos];
                existing.hunk_index = hunk_index;
                existing.tags = tags.clone();
                existing.old_line_range =
                    hunk_context.as_ref().and_then(|ctx| ctx.old_line_range);
                existing.new_line_range =
                    hunk_context.as_ref().and_then(|ctx| ctx.new_line_range);
                existing.context_changed = hunk_context
                    .as_ref()
                    .map(|ctx| ctx.context_changed.clone())
                    .unwrap_or_default();
            } else {
                tag_file.hunks.push(PersistentHunkTags {
                    filename,
                    hunk_index,
                    tags: tags.clone(),
                    old_line_range: hunk_context.as_ref().and_then(|ctx| ctx.old_line_range),
                    new_line_range: hunk_context.as_ref().and_then(|ctx| ctx.new_line_range),
                    context_changed: hunk_context
                        .as_ref()
                        .map(|ctx| ctx.context_changed.clone())
                        .unwrap_or_default(),
                });
            }
        }

        tag_file.last_updated = current_timestamp();
        self.save_tag_file(scope, &tag_file)?;

        if !tags.is_empty() {
            self.merge_tags(tags)?;
        }
        Ok(())
    }

    fn merge_tags(&mut self, tags: Vec<String>) -> io::Result<()> {
        let mut updated = false;
        for tag in tags {
            if !self.index.tags.contains(&tag) {
                self.index.tags.push(tag);
                updated = true;
            }
        }
        if updated {
            self.index.tags.sort();
            self.index.tags.dedup();
            self.index.last_updated = current_timestamp();
            let index_path = self.tag_dir.join(TAG_INDEX_FILE);
            save_index_file(&self.index, &index_path)?;
        }
        Ok(())
    }

    fn load_tag_file(&self, scope: &DiffScope) -> io::Result<TagFile> {
        let path = self.scope_path(scope);
        if !path.exists() {
            return Ok(TagFile {
                version: TAGS_VERSION,
                last_updated: current_timestamp(),
                scope: match scope {
                    DiffScope::WorkingTree { .. } => TagScope::WorkingTree,
                    DiffScope::Commit { .. } => TagScope::Commit,
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
        let mut file: TagFile = serde_json::from_str(&content).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid tag file {}: {}", path.display(), e),
            )
        })?;

        if file.version > TAGS_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported tag version {} in {}",
                    file.version,
                    path.display()
                ),
            ));
        }

        file.scope = match scope {
            DiffScope::WorkingTree { .. } => TagScope::WorkingTree,
            DiffScope::Commit { .. } => TagScope::Commit,
        };
        Ok(file)
    }

    fn save_tag_file(&self, scope: &DiffScope, file: &TagFile) -> io::Result<()> {
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
            DiffScope::WorkingTree { .. } => self.tag_dir.join(WORKING_TREE_FILE),
            DiffScope::Commit { commit_id } => self.tag_dir.join(format!("{}.json", commit_id)),
        }
    }
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<String> = tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn load_index_file(path: &Path) -> io::Result<TagIndexFile> {
    if !path.exists() {
        return Ok(TagIndexFile {
            version: TAGS_VERSION,
            last_updated: current_timestamp(),
            tags: Vec::new(),
        });
    }
    let content = fs::read_to_string(path)?;
    let mut file: TagIndexFile = serde_json::from_str(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid tag index {}: {}", path.display(), e),
        )
    })?;
    if file.version > TAGS_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported tag index version {} in {}",
                file.version,
                path.display()
            ),
        ));
    }
    file.version = TAGS_VERSION;
    Ok(file)
}

fn save_index_file(file: &TagIndexFile, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(file)?;
    fs::write(path, json)?;
    Ok(())
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
