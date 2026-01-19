use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::diff_algo::{compute_side_by_side, find_hunk_ranges, HunkRange};
use super::state::{AppState, HunkAnnotation};
use super::types::ChangeType;

const STORAGE_VERSION: u32 = 1;
const ANNOTATION_DIR: &str = ".lumen/annotations";
const WORKING_TREE_FILE: &str = "working-tree.json";
const ORPHANS_FILE: &str = "orphans.json";
const CONTEXT_LINES: usize = 3;
const CONTEXT_MATCH_THRESHOLD: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationScope {
    WorkingTree,
    Commit,
    Orphans,
}

#[derive(Debug, Clone)]
pub enum DiffScope {
    WorkingTree { base_commit_id: String },
    Commit { commit_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentAnnotation {
    pub id: String,
    pub filename: String,
    pub old_line_range: Option<(usize, usize)>,
    pub new_line_range: Option<(usize, usize)>,
    pub context_before: Vec<String>,
    pub context_changed: Vec<String>,
    pub context_after: Vec<String>,
    pub diff_text: String,
    pub content: String,
    pub created_at: u64,
    pub diff_reference: Option<String>,
    pub change_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_scope: Option<AnnotationScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_commit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_base_commit_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationFile {
    pub version: u32,
    pub last_updated: u64,
    pub scope: AnnotationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit_id: Option<String>,
    pub annotations: Vec<PersistentAnnotation>,
}

impl AnnotationFile {
    pub fn new(scope: AnnotationScope) -> Self {
        Self {
            version: STORAGE_VERSION,
            last_updated: current_timestamp(),
            scope,
            commit_id: None,
            base_commit_id: None,
            annotations: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.last_updated = current_timestamp();
    }

    pub fn upsert(&mut self, annotation: PersistentAnnotation) {
        if let Some(existing) = self.annotations.iter_mut().find(|a| a.id == annotation.id) {
            *existing = annotation;
        } else {
            self.annotations.push(annotation);
        }
        self.touch();
    }

    pub fn remove_by_id(&mut self, id: &str) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|a| a.id != id);
        let removed = before != self.annotations.len();
        if removed {
            self.touch();
        }
        removed
    }
}

pub struct PersistenceManager {
    annotation_dir: PathBuf,
    working_tree: AnnotationFile,
    orphans: AnnotationFile,
}

impl PersistenceManager {
    pub fn try_new() -> io::Result<Option<Self>> {
        let cwd = std::env::current_dir()?;
        let repo_root = match find_repo_root(&cwd) {
            Some(path) => path,
            None => return Ok(None),
        };
        let annotation_dir = repo_root.join(ANNOTATION_DIR);
        let working_tree_path = annotation_dir.join(WORKING_TREE_FILE);
        let orphans_path = annotation_dir.join(ORPHANS_FILE);

        let working_tree = load_annotation_file(&working_tree_path, AnnotationScope::WorkingTree)?;
        let orphans = load_annotation_file(&orphans_path, AnnotationScope::Orphans)?;

        Ok(Some(Self {
            annotation_dir,
            working_tree,
            orphans,
        }))
    }

    pub fn load_for_scope(&mut self, state: &mut AppState, scope: &DiffScope) -> io::Result<()> {
        state.annotations.clear();

        match scope {
            DiffScope::WorkingTree { base_commit_id } => {
                self.ensure_working_tree_base(base_commit_id)?;
                self.load_annotations_into_state(state, &self.working_tree.annotations);
            }
            DiffScope::Commit { commit_id } => {
                let mut commit_file = self.load_commit_file(commit_id)?;
                self.migrate_to_commit(state, commit_id, &mut commit_file)?;
                self.load_annotations_into_state(state, &commit_file.annotations);
                self.save_commit_file(commit_id, &commit_file)?;
            }
        }

        Ok(())
    }

    pub fn upsert_annotation(
        &mut self,
        state: &AppState,
        annotation: &HunkAnnotation,
        scope: &DiffScope,
    ) -> io::Result<()> {
        let persistent = to_persistent(annotation, state, scope);
        match scope {
            DiffScope::WorkingTree { base_commit_id } => {
                self.ensure_working_tree_base(base_commit_id)?;
                self.working_tree.upsert(persistent);
                save_annotation_file(&self.working_tree, &self.working_tree_path())?;
            }
            DiffScope::Commit { commit_id } => {
                let mut commit_file = self.load_commit_file(commit_id)?;
                commit_file.upsert(persistent);
                self.save_commit_file(commit_id, &commit_file)?;
            }
        }
        Ok(())
    }

    pub fn remove_annotation(
        &mut self,
        annotation: &HunkAnnotation,
        scope: &DiffScope,
    ) -> io::Result<()> {
        match scope {
            DiffScope::WorkingTree { base_commit_id } => {
                self.ensure_working_tree_base(base_commit_id)?;
                self.working_tree.remove_by_id(&annotation.id);
                save_annotation_file(&self.working_tree, &self.working_tree_path())?;
            }
            DiffScope::Commit { commit_id } => {
                let mut commit_file = self.load_commit_file(commit_id)?;
                commit_file.remove_by_id(&annotation.id);
                self.save_commit_file(commit_id, &commit_file)?;
            }
        }
        Ok(())
    }

    fn ensure_working_tree_base(&mut self, base_commit_id: &str) -> io::Result<()> {
        if let Some(existing) = &self.working_tree.base_commit_id {
            if existing != base_commit_id && !self.working_tree.annotations.is_empty() {
                let mut moved = Vec::new();
                for mut ann in self.working_tree.annotations.drain(..) {
                    ann.origin_scope = Some(AnnotationScope::WorkingTree);
                    ann.origin_base_commit_id = Some(existing.clone());
                    moved.push(ann);
                }
                for ann in moved {
                    self.orphans.upsert(ann);
                }
                save_annotation_file(&self.orphans, &self.orphans_path())?;
            }
        }

        if self.working_tree.base_commit_id.as_deref() != Some(base_commit_id) {
            self.working_tree.base_commit_id = Some(base_commit_id.to_string());
            self.working_tree.touch();
            save_annotation_file(&self.working_tree, &self.working_tree_path())?;
        }

        Ok(())
    }

    fn migrate_to_commit(
        &mut self,
        state: &AppState,
        commit_id: &str,
        commit_file: &mut AnnotationFile,
    ) -> io::Result<()> {
        let mut remaining_working = Vec::new();
        for ann in self.working_tree.annotations.drain(..) {
            if should_migrate_annotation(state, &ann) {
                let mut migrated = ann.clone();
                migrated.origin_scope = Some(AnnotationScope::Commit);
                migrated.origin_commit_id = Some(commit_id.to_string());
                commit_file.upsert(migrated);
            } else {
                remaining_working.push(ann);
            }
        }
        self.working_tree.annotations = remaining_working;
        save_annotation_file(&self.working_tree, &self.working_tree_path())?;

        let mut remaining_orphans = Vec::new();
        for ann in self.orphans.annotations.drain(..) {
            if should_migrate_annotation(state, &ann) {
                let mut migrated = ann.clone();
                migrated.origin_scope = Some(AnnotationScope::Commit);
                migrated.origin_commit_id = Some(commit_id.to_string());
                commit_file.upsert(migrated);
            } else {
                remaining_orphans.push(ann);
            }
        }
        self.orphans.annotations = remaining_orphans;
        save_annotation_file(&self.orphans, &self.orphans_path())?;

        Ok(())
    }

    fn load_commit_file(&self, commit_id: &str) -> io::Result<AnnotationFile> {
        let path = self.commit_path(commit_id);
        let mut file = load_annotation_file(&path, AnnotationScope::Commit)?;
        file.commit_id = Some(commit_id.to_string());
        Ok(file)
    }

    fn save_commit_file(&self, commit_id: &str, file: &AnnotationFile) -> io::Result<()> {
        let path = self.commit_path(commit_id);
        save_annotation_file(file, &path)
    }

    fn load_annotations_into_state(
        &self,
        state: &mut AppState,
        annotations: &[PersistentAnnotation],
    ) {
        for ann in annotations {
            if let Some(file_index) = state
                .file_diffs
                .iter()
                .position(|f| f.filename == ann.filename)
            {
                if let Some(hunk_index) = find_matching_hunk(state, file_index, ann) {
                    let line_range = display_line_range(ann);
                    state.annotations.push(HunkAnnotation {
                        id: ann.id.clone(),
                        file_index,
                        hunk_index,
                        content: ann.content.clone(),
                        line_range,
                        filename: ann.filename.clone(),
                        created_at: UNIX_EPOCH + std::time::Duration::from_secs(ann.created_at),
                    });
                }
            }
        }
    }

    fn working_tree_path(&self) -> PathBuf {
        self.annotation_dir.join(WORKING_TREE_FILE)
    }

    fn orphans_path(&self) -> PathBuf {
        self.annotation_dir.join(ORPHANS_FILE)
    }

    fn commit_path(&self, commit_id: &str) -> PathBuf {
        self.annotation_dir.join(format!("{}.json", commit_id))
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

fn load_annotation_file(path: &Path, scope: AnnotationScope) -> io::Result<AnnotationFile> {
    if !path.exists() {
        return Ok(AnnotationFile::new(scope));
    }
    let content = fs::read_to_string(path)?;
    let mut file: AnnotationFile = serde_json::from_str(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid annotation file {}: {}", path.display(), e),
        )
    })?;

    if file.version > STORAGE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported annotation version {} in {}",
                file.version,
                path.display()
            ),
        ));
    }
    file.scope = scope;
    Ok(file)
}

fn save_annotation_file(file: &AnnotationFile, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(file)?;
    fs::write(path, json)?;
    Ok(())
}

fn to_persistent(
    annotation: &HunkAnnotation,
    state: &AppState,
    scope: &DiffScope,
) -> PersistentAnnotation {
    let hunk_context = extract_hunk_context(state, annotation);
    let created_at = annotation
        .created_at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (origin_scope, origin_commit_id, origin_base_commit_id) = match scope {
        DiffScope::WorkingTree { base_commit_id } => (
            Some(AnnotationScope::WorkingTree),
            None,
            Some(base_commit_id.clone()),
        ),
        DiffScope::Commit { commit_id } => {
            (Some(AnnotationScope::Commit), Some(commit_id.clone()), None)
        }
    };

    PersistentAnnotation {
        id: annotation.id.clone(),
        filename: annotation.filename.clone(),
        old_line_range: hunk_context.old_line_range,
        new_line_range: hunk_context.new_line_range,
        context_before: hunk_context.context_before,
        context_changed: hunk_context.context_changed,
        context_after: hunk_context.context_after,
        diff_text: hunk_context.diff_text,
        content: annotation.content.clone(),
        created_at,
        diff_reference: state.diff_reference.clone(),
        change_type: hunk_context.change_type,
        origin_scope,
        origin_commit_id,
        origin_base_commit_id,
    }
}

fn should_migrate_annotation(state: &AppState, annotation: &PersistentAnnotation) -> bool {
    if let Some(file_index) = state
        .file_diffs
        .iter()
        .position(|f| f.filename == annotation.filename)
    {
        find_matching_hunk(state, file_index, annotation).is_some()
    } else {
        false
    }
}

fn display_line_range(annotation: &PersistentAnnotation) -> (usize, usize) {
    if let Some(range) = annotation.new_line_range {
        return range;
    }
    if let Some(range) = annotation.old_line_range {
        return range;
    }
    (0, 0)
}

struct HunkContext {
    old_line_range: Option<(usize, usize)>,
    new_line_range: Option<(usize, usize)>,
    context_before: Vec<String>,
    context_changed: Vec<String>,
    context_after: Vec<String>,
    diff_text: String,
    change_type: String,
}

fn hunk_change_bounds(
    side_by_side: &[super::types::DiffLine],
    hunk_range: HunkRange,
) -> Option<(usize, usize)> {
    let mut first = None;
    let mut last = None;
    for i in hunk_range.start..hunk_range.end {
        if let Some(dl) = side_by_side.get(i) {
            if !matches!(dl.change_type, ChangeType::Equal) {
                if first.is_none() {
                    first = Some(i);
                }
                last = Some(i);
            }
        }
    }
    first.zip(last)
}

fn extract_hunk_context(state: &AppState, annotation: &HunkAnnotation) -> HunkContext {
    let diff = &state.file_diffs[annotation.file_index];
    let side_by_side = compute_side_by_side(
        &diff.old_content,
        &diff.new_content,
        state.settings.tab_width,
    );
    let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);
    let hunk_range = hunks
        .get(annotation.hunk_index)
        .copied()
        .unwrap_or(HunkRange {
            start: 0,
            end: side_by_side.len(),
        });
    let (change_start, change_end) =
        hunk_change_bounds(&side_by_side, hunk_range).unwrap_or((hunk_range.start, hunk_range.end.saturating_sub(1)));

    let context_before: Vec<String> = side_by_side
        .get(change_start.saturating_sub(CONTEXT_LINES)..change_start)
        .unwrap_or(&[])
        .iter()
        .filter_map(|dl| {
            dl.new_line
                .as_ref()
                .or(dl.old_line.as_ref())
                .map(|(_, text)| text.clone())
        })
        .collect();

    let mut context_changed = Vec::new();
    let mut diff_text = String::new();
    let mut old_start = None;
    let mut old_end = None;
    let mut new_start = None;
    let mut new_end = None;

    for i in hunk_range.start..hunk_range.end {
        let dl = &side_by_side[i];
        if matches!(dl.change_type, ChangeType::Equal) {
            continue;
        }

        match dl.change_type {
            ChangeType::Delete => {
                if let Some((num, text)) = &dl.old_line {
                    let line = format!("- {}", text);
                    context_changed.push(line.clone());
                    diff_text.push_str(&format!("{}\n", line));
                    if old_start.is_none() {
                        old_start = Some(*num);
                    }
                    old_end = Some(*num);
                }
            }
            ChangeType::Insert => {
                if let Some((num, text)) = &dl.new_line {
                    let line = format!("+ {}", text);
                    context_changed.push(line.clone());
                    diff_text.push_str(&format!("{}\n", line));
                    if new_start.is_none() {
                        new_start = Some(*num);
                    }
                    new_end = Some(*num);
                }
            }
            ChangeType::Modified => {
                if let Some((num, text)) = &dl.old_line {
                    let line = format!("- {}", text);
                    context_changed.push(line.clone());
                    diff_text.push_str(&format!("{}\n", line));
                    if old_start.is_none() {
                        old_start = Some(*num);
                    }
                    old_end = Some(*num);
                }
                if let Some((num, text)) = &dl.new_line {
                    let line = format!("+ {}", text);
                    context_changed.push(line.clone());
                    diff_text.push_str(&format!("{}\n", line));
                    if new_start.is_none() {
                        new_start = Some(*num);
                    }
                    new_end = Some(*num);
                }
            }
            ChangeType::Equal => {}
        }
    }

    let context_after: Vec<String> = side_by_side
        .get(
            change_end
                .saturating_add(1)
                ..change_end
                    .saturating_add(1)
                    .saturating_add(CONTEXT_LINES)
                    .min(side_by_side.len()),
        )
        .unwrap_or(&[])
        .iter()
        .filter_map(|dl| {
            dl.new_line
                .as_ref()
                .or(dl.old_line.as_ref())
                .map(|(_, text)| text.clone())
        })
        .collect();

    let change_type = if old_start.is_some() && new_start.is_some() {
        "modification"
    } else if old_start.is_some() {
        "deletion"
    } else {
        "addition"
    };

    HunkContext {
        old_line_range: old_start.zip(old_end),
        new_line_range: new_start.zip(new_end),
        context_before,
        context_changed,
        context_after,
        diff_text,
        change_type: change_type.to_string(),
    }
}

fn find_matching_hunk(
    state: &AppState,
    file_index: usize,
    annotation: &PersistentAnnotation,
) -> Option<usize> {
    let diff = state.file_diffs.get(file_index)?;
    let side_by_side = compute_side_by_side(
        &diff.old_content,
        &diff.new_content,
        state.settings.tab_width,
    );
    let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);

    if let Some(range) = annotation.new_line_range {
        if let Some(idx) = match_hunk_by_range(&side_by_side, &hunks, range, true) {
            return Some(idx);
        }
    }

    if let Some(range) = annotation.old_line_range {
        if let Some(idx) = match_hunk_by_range(&side_by_side, &hunks, range, false) {
            return Some(idx);
        }
    }

    if !annotation.context_changed.is_empty() {
        for (idx, hunk) in hunks.iter().enumerate() {
            let hunk_changed = hunk_changed_lines(&side_by_side, hunk.start, hunk.end);
            if context_similarity(&hunk_changed, &annotation.context_changed)
                >= CONTEXT_MATCH_THRESHOLD
            {
                return Some(idx);
            }
        }
    }

    None
}

fn match_hunk_by_range(
    side_by_side: &[super::types::DiffLine],
    hunks: &[HunkRange],
    range: (usize, usize),
    use_new: bool,
) -> Option<usize> {
    for (idx, hunk) in hunks.iter().enumerate() {
        for i in hunk.start..hunk.end {
            let dl = &side_by_side[i];
            if matches!(dl.change_type, ChangeType::Equal) {
                continue;
            }
            let line_opt = if use_new { &dl.new_line } else { &dl.old_line };
            if let Some((line, _)) = line_opt {
                if *line >= range.0 && *line <= range.1 {
                    return Some(idx);
                }
            }
        }
    }
    None
}

fn hunk_changed_lines(
    side_by_side: &[super::types::DiffLine],
    start: usize,
    end: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    for i in start..end {
        let dl = &side_by_side[i];
        if matches!(dl.change_type, ChangeType::Equal) {
            continue;
        }
        match dl.change_type {
            ChangeType::Delete => {
                if let Some((_, text)) = &dl.old_line {
                    lines.push(format!("- {}", text));
                }
            }
            ChangeType::Insert => {
                if let Some((_, text)) = &dl.new_line {
                    lines.push(format!("+ {}", text));
                }
            }
            ChangeType::Modified => {
                if let Some((_, text)) = &dl.old_line {
                    lines.push(format!("- {}", text));
                }
                if let Some((_, text)) = &dl.new_line {
                    lines.push(format!("+ {}", text));
                }
            }
            ChangeType::Equal => {}
        }
    }
    lines
}

fn context_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let matches = a
        .iter()
        .filter(|line_a| b.iter().any(|line_b| line_a == &line_b))
        .count();
    matches as f64 / a.len().max(b.len()) as f64
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_annotation_file_missing() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("missing.json");
        let file = load_annotation_file(&path, AnnotationScope::WorkingTree).unwrap();
        assert_eq!(file.version, STORAGE_VERSION);
        assert_eq!(file.scope, AnnotationScope::WorkingTree);
        assert!(file.annotations.is_empty());
    }

    #[test]
    fn test_save_and_load_annotation_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("store.json");
        let mut file = AnnotationFile::new(AnnotationScope::WorkingTree);
        file.base_commit_id = Some("abc".to_string());
        file.annotations.push(PersistentAnnotation {
            id: "id".to_string(),
            filename: "file.rs".to_string(),
            old_line_range: Some((1, 2)),
            new_line_range: Some((1, 2)),
            context_before: vec![],
            context_changed: vec!["- old".to_string(), "+ new".to_string()],
            context_after: vec![],
            diff_text: "- old\n+ new\n".to_string(),
            content: "note".to_string(),
            created_at: 1,
            diff_reference: None,
            change_type: "modification".to_string(),
            origin_scope: Some(AnnotationScope::WorkingTree),
            origin_commit_id: None,
            origin_base_commit_id: Some("abc".to_string()),
        });

        save_annotation_file(&file, &path).unwrap();
        let loaded = load_annotation_file(&path, AnnotationScope::WorkingTree).unwrap();
        assert_eq!(loaded.annotations.len(), 1);
        assert_eq!(loaded.base_commit_id.as_deref(), Some("abc"));
    }
}
