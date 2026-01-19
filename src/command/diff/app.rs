use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;

use super::annotation::{AnnotationEditor, AnnotationEditorResult};
use super::api::{start_api_server, ApiCommand, ApiResponse};
use super::diff_algo::{compute_side_by_side, find_hunk_ranges, HunkRange};
use super::git::{
    get_current_branch, load_file_diffs, load_pr_file_diffs, load_single_commit_diffs,
};
use super::highlight;
use super::persistence::{DiffScope, PersistenceManager};
use super::tag_editor::{TagEditor, TagEditorResult};
use super::tags::TagManager;
use super::view_state::ViewStateManager;
use super::render::{
    render_diff, render_empty_state, truncate_path, FilePickerItem, KeyBind, KeyBindSection, Modal,
    ModalContent, ModalFileStatus, ModalResult,
};
use super::state::{
    adjust_scroll_for_hunk, adjust_scroll_to_line, AppState, PendingKey, TagFilter,
};
use super::theme;
use super::types::{ChangeType, DiffFullscreen, FileStatus, FocusedPanel, SidebarItem};
use super::watcher::{setup_watcher, WatchEvent};
use super::{
    fetch_viewed_files, mark_file_as_viewed_async, unmark_file_as_viewed_async, DiffOptions, PrInfo,
};
use crate::commit_reference::CommitReference;
use crate::vcs::{StackedCommitInfo, VcsBackend};
use serde_json::Value;
use std::time::UNIX_EPOCH;

/// Navigate to a different commit in stacked mode.
/// Returns true if navigation was successful.
fn navigate_stacked_commit(
    state: &mut AppState,
    new_index: usize,
    options: &DiffOptions,
    backend: &dyn VcsBackend,
) -> bool {
    if new_index >= state.stacked_commits.len() {
        return false;
    }
    state.save_stacked_viewed_files();
    state.current_commit_index = new_index;
    if let Some(commit) = state.stacked_commits.get(new_index) {
        let file_diffs = load_single_commit_diffs(&commit.commit_id, &options.file, backend);
        state.reload(file_diffs, None);
        state.load_stacked_viewed_files();
        true
    } else {
        false
    }
}

/// Adjust sidebar scroll to ensure the selected item is visible.
fn ensure_sidebar_visible(state: &mut AppState, visible_height: usize) {
    if state.sidebar_selected >= state.sidebar_scroll + visible_height {
        state.sidebar_scroll = state.sidebar_selected.saturating_sub(visible_height) + 1;
    } else if state.sidebar_selected < state.sidebar_scroll {
        state.sidebar_scroll = state.sidebar_selected;
    }
}

/// Format an annotation for display in the annotations list.
fn format_annotation_preview(annotation: &super::state::HunkAnnotation) -> String {
    let preview = annotation.content.lines().next().unwrap_or("");
    let preview = if preview.len() > 40 {
        format!("{}...", &preview[..40])
    } else {
        preview.to_string()
    };
    let truncated_filename = truncate_path(&annotation.filename, 30);
    format!(
        "{}:{}-{} | {} | {}",
        truncated_filename,
        annotation.line_range.0,
        annotation.line_range.1,
        preview,
        annotation.format_time()
    )
}

fn scope_json(scope: Option<&DiffScope>, state: &AppState) -> Value {
    match scope {
        Some(DiffScope::WorkingTree { base_commit_id }) => serde_json::json!({
            "type": "working_tree",
            "base_commit_id": base_commit_id,
            "diff_reference": state.diff_reference.clone(),
            "vcs": state.vcs_name,
        }),
        Some(DiffScope::Commit { commit_id }) => serde_json::json!({
            "type": "commit",
            "commit_id": commit_id,
            "diff_reference": state.diff_reference.clone(),
            "vcs": state.vcs_name,
        }),
        None => serde_json::json!({
            "type": "unknown",
            "diff_reference": state.diff_reference.clone(),
            "vcs": state.vcs_name,
        }),
    }
}

fn status_json(state: &AppState, scope: Option<&DiffScope>) -> Value {
    let diff = state.file_diffs.get(state.current_file);
    let hunk_count = diff
        .map(|diff| {
            let side_by_side = compute_side_by_side(
                &diff.old_content,
                &diff.new_content,
                state.settings.tab_width,
            );
            find_hunk_ranges(&side_by_side, state.settings.unified_context).len()
        })
        .unwrap_or(0);

    let cwd = std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().to_string());

    serde_json::json!({
        "cwd": cwd,
        "scope": scope_json(scope, state),
        "current_file": diff.map(|d| d.filename.clone()),
        "current_file_index": state.current_file,
        "focused_hunk": state.focused_hunk,
        "hunk_count": hunk_count,
        "annotations_count": state.annotations.len(),
    })
}

fn file_status_str(status: &FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Modified => "modified",
    }
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

struct HunkContext {
    old_line_range: Option<(usize, usize)>,
    new_line_range: Option<(usize, usize)>,
    context_before: Vec<String>,
    context_changed: Vec<String>,
    context_after: Vec<String>,
    diff_text: String,
    change_type: String,
}

fn build_hunk_context(
    state: &AppState,
    file_index: usize,
    hunk_index: usize,
) -> Option<HunkContext> {
    let diff = state.file_diffs.get(file_index)?;
    let side_by_side = compute_side_by_side(
        &diff.old_content,
        &diff.new_content,
        state.settings.tab_width,
    );
    let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);

    let hunk_range = *hunks.get(hunk_index)?;
    let (change_start, change_end) = hunk_change_bounds(&side_by_side, hunk_range)?;

    let context_before: Vec<String> = side_by_side
        .get(change_start.saturating_sub(3)..change_start)
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
            change_end.saturating_add(1)
                ..change_end
                    .saturating_add(1)
                    .saturating_add(3)
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

    Some(HunkContext {
        old_line_range: old_start.zip(old_end),
        new_line_range: new_start.zip(new_end),
        context_before,
        context_changed,
        context_after,
        diff_text,
        change_type: change_type.to_string(),
    })
}

fn compute_hunk_line_range(
    state: &AppState,
    file_index: usize,
    hunk_index: usize,
) -> Option<(usize, usize)> {
    let diff = state.file_diffs.get(file_index)?;
    let side_by_side = compute_side_by_side(
        &diff.old_content,
        &diff.new_content,
        state.settings.tab_width,
    );
    let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);
    let hunk_range = *hunks.get(hunk_index)?;
    let (actual_hunk_start, actual_hunk_end) = hunk_change_bounds(&side_by_side, hunk_range)?;

    let start_line = side_by_side
        .get(actual_hunk_start)
        .and_then(|dl| {
            dl.new_line
                .as_ref()
                .map(|(n, _)| *n)
                .or(dl.old_line.as_ref().map(|(n, _)| *n))
        })
        .unwrap_or(1);
    let end_line = side_by_side
        .get(actual_hunk_end)
        .and_then(|dl| {
            dl.new_line
                .as_ref()
                .map(|(n, _)| *n)
                .or(dl.old_line.as_ref().map(|(n, _)| *n))
        })
        .unwrap_or(start_line);

    Some((start_line, end_line))
}

fn hunk_matches_filter(
    state: &AppState,
    file_index: usize,
    hunk_index: usize,
    filter: Option<&TagFilter>,
) -> bool {
    match filter {
        None => true,
        Some(TagFilter::Tag(tag)) => state
            .get_hunk_tags(file_index, hunk_index)
            .map(|tags| tags.tags.iter().any(|t| t == tag))
            .unwrap_or(false),
        Some(TagFilter::Untagged) => state
            .get_hunk_tags(file_index, hunk_index)
            .map(|tags| tags.tags.is_empty())
            .unwrap_or(true),
    }
}

fn matching_hunk_indices(
    state: &AppState,
    file_index: usize,
    hunks: &[HunkRange],
    filter: Option<&TagFilter>,
) -> Vec<usize> {
    hunks
        .iter()
        .enumerate()
        .filter(|(idx, _)| hunk_matches_filter(state, file_index, *idx, filter))
        .map(|(idx, _)| idx)
        .collect()
}

fn matching_files_for_filter(state: &AppState, filter: &TagFilter) -> HashSet<usize> {
    match filter {
        TagFilter::Tag(tag) => state
            .hunk_tags
            .iter()
            .filter(|hunk| hunk.tags.iter().any(|t| t == tag))
            .map(|hunk| hunk.file_index)
            .collect(),
        TagFilter::Untagged => {
            let mut files = HashSet::new();
            for (idx, diff) in state.file_diffs.iter().enumerate() {
                let side_by_side = compute_side_by_side(
                    &diff.old_content,
                    &diff.new_content,
                    state.settings.tab_width,
                );
                let hunks = find_hunk_ranges(&side_by_side, state.settings.unified_context);
                for hunk_idx in 0..hunks.len() {
                    if hunk_matches_filter(state, idx, hunk_idx, Some(filter)) {
                        files.insert(idx);
                        break;
                    }
                }
            }
            files
        }
    }
}

fn apply_tag_filter(state: &mut AppState) {
    if let Some(filter) = state.tag_filter.as_ref() {
        let matching_files = matching_files_for_filter(state, filter);
        state.rebuild_sidebar_visible_filtered(&matching_files);
        if matching_files.contains(&state.current_file) {
            return;
        }
        if let Some(file_index) = state.sidebar_visible.iter().find_map(|idx| {
            if let SidebarItem::File { file_index, .. } = state.sidebar_items[*idx] {
                Some(file_index)
            } else {
                None
            }
        }) {
            state.select_file(file_index);
        } else {
            state.current_file = 0;
            state.focused_hunk = None;
        }
    } else {
        state.rebuild_sidebar_visible();
    }
}

fn next_tag_filter(current: Option<&TagFilter>, tags: &[String]) -> Option<TagFilter> {
    let mut sequence: Vec<TagFilter> = tags.iter().map(|t| TagFilter::Tag(t.clone())).collect();
    sequence.push(TagFilter::Untagged);

    match current {
        None => sequence.first().cloned(),
        Some(current_filter) => {
            if let Some(pos) = sequence.iter().position(|f| f == current_filter) {
                if pos + 1 < sequence.len() {
                    Some(sequence[pos + 1].clone())
                } else {
                    None
                }
            } else {
                sequence.first().cloned()
            }
        }
    }
}

fn annotation_json(annotation: &super::state::HunkAnnotation) -> Value {
    let created_at = annotation
        .created_at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::json!({
        "id": annotation.id.clone(),
        "file": annotation.filename.clone(),
        "file_index": annotation.file_index,
        "hunk_index": annotation.hunk_index,
        "line_range": [annotation.line_range.0, annotation.line_range.1],
        "content": annotation.content.clone(),
        "created_at": created_at,
    })
}

fn update_annotation_content(
    state: &mut AppState,
    id: &str,
    content: String,
) -> Option<super::state::HunkAnnotation> {
    if let Some(existing) = state.annotations.iter_mut().find(|ann| ann.id == id) {
        existing.content = content;
        return Some(existing.clone());
    }
    None
}

fn remove_annotation_by_id(state: &mut AppState, id: &str) -> Option<super::state::HunkAnnotation> {
    state
        .annotations
        .iter()
        .position(|ann| ann.id == id)
        .map(|index| state.annotations.remove(index))
}

fn handle_api_command(
    command: ApiCommand,
    state: &mut AppState,
    current_scope: Option<&DiffScope>,
    persistence: Option<&mut PersistenceManager>,
) {
    match command {
        ApiCommand::Status { respond_to } => {
            let _ = respond_to.send(ApiResponse {
                status: 200,
                body: status_json(state, current_scope),
            });
        }
        ApiCommand::CurrentHunk { respond_to } => {
            let response = if let Some(hunk_index) = state.focused_hunk {
                let file_index = state.current_file;
                if let Some(context) = build_hunk_context(state, file_index, hunk_index) {
                    let display_range = context
                        .new_line_range
                        .or(context.old_line_range)
                        .unwrap_or((0, 0));
                    let annotation = state.get_annotation(file_index, hunk_index);
                    ApiResponse {
                        status: 200,
                        body: serde_json::json!({
                            "scope": scope_json(current_scope, state),
                            "file": state.file_diffs[file_index].filename.clone(),
                            "file_index": file_index,
                            "hunk_index": hunk_index,
                            "line_range": [display_range.0, display_range.1],
                            "old_line_range": context.old_line_range,
                            "new_line_range": context.new_line_range,
                            "diff_text": context.diff_text,
                            "context_before": context.context_before,
                            "context_changed": context.context_changed,
                            "context_after": context.context_after,
                            "change_type": context.change_type,
                            "annotation": annotation.map(annotation_json),
                        }),
                    }
                } else {
                    ApiResponse {
                        status: 404,
                        body: serde_json::json!({ "error": "focused hunk not found" }),
                    }
                }
            } else {
                ApiResponse {
                    status: 404,
                    body: serde_json::json!({ "error": "no focused hunk" }),
                }
            };
            let _ = respond_to.send(response);
        }
        ApiCommand::CurrentFile { respond_to } => {
            let response = if let Some(diff) = state.file_diffs.get(state.current_file) {
                ApiResponse {
                    status: 200,
                    body: serde_json::json!({
                        "scope": scope_json(current_scope, state),
                        "file": diff.filename.clone(),
                        "file_index": state.current_file,
                        "status": file_status_str(&diff.status),
                        "is_binary": diff.is_binary,
                        "old_content": diff.old_content.clone(),
                        "new_content": diff.new_content.clone(),
                    }),
                }
            } else {
                ApiResponse {
                    status: 404,
                    body: serde_json::json!({ "error": "file not found" }),
                }
            };
            let _ = respond_to.send(response);
        }
        ApiCommand::FullContext { respond_to } => {
            let current_hunk = if state.focused_hunk.is_some() {
                if let Some(hunk_index) = state.focused_hunk {
                    let file_index = state.current_file;
                    build_hunk_context(state, file_index, hunk_index).map(|context| {
                        let display_range = context
                            .new_line_range
                            .or(context.old_line_range)
                            .unwrap_or((0, 0));
                        serde_json::json!({
                            "file": state.file_diffs[file_index].filename.clone(),
                            "file_index": file_index,
                            "hunk_index": hunk_index,
                            "line_range": [display_range.0, display_range.1],
                            "old_line_range": context.old_line_range,
                            "new_line_range": context.new_line_range,
                            "diff_text": context.diff_text,
                            "context_before": context.context_before,
                            "context_changed": context.context_changed,
                            "context_after": context.context_after,
                            "change_type": context.change_type,
                        })
                    })
                } else {
                    None
                }
            } else {
                None
            };

            let annotations: Vec<Value> = state.annotations.iter().map(annotation_json).collect();
            let _ = respond_to.send(ApiResponse {
                status: 200,
                body: serde_json::json!({
                    "status": status_json(state, current_scope),
                    "current_hunk": current_hunk,
                    "annotations": annotations,
                }),
            });
        }
        ApiCommand::Annotations {
            current_only,
            respond_to,
        } => {
            let annotations: Vec<Value> = state
                .annotations
                .iter()
                .filter(|ann| !current_only || ann.file_index == state.current_file)
                .map(annotation_json)
                .collect();
            let _ = respond_to.send(ApiResponse {
                status: 200,
                body: serde_json::json!({
                    "scope": scope_json(current_scope, state),
                    "annotations": annotations,
                }),
            });
        }
        ApiCommand::CreateAnnotation {
            content,
            respond_to,
        } => {
            let response = if content.trim().is_empty() {
                ApiResponse {
                    status: 400,
                    body: serde_json::json!({ "error": "content cannot be empty" }),
                }
            } else if let Some(hunk_index) = state.focused_hunk {
                let file_index = state.current_file;
                if let Some(line_range) = compute_hunk_line_range(state, file_index, hunk_index) {
                    let annotation = super::state::HunkAnnotation {
                        id: uuid::Uuid::new_v4().to_string(),
                        file_index,
                        hunk_index,
                        content: content.clone(),
                        line_range,
                        filename: state.file_diffs[file_index].filename.clone(),
                        created_at: std::time::SystemTime::now(),
                    };
                    state.set_annotation(annotation.clone());
                    if let (Some(scope), Some(persistence)) = (current_scope, persistence) {
                        if let Err(err) = persistence.upsert_annotation(state, &annotation, scope) {
                            eprintln!("Warning: failed to persist annotation: {}", err);
                        }
                    }
                    ApiResponse {
                        status: 200,
                        body: serde_json::json!({
                            "scope": scope_json(current_scope, state),
                            "annotation": annotation_json(&annotation),
                        }),
                    }
                } else {
                    ApiResponse {
                        status: 404,
                        body: serde_json::json!({ "error": "focused hunk not found" }),
                    }
                }
            } else {
                ApiResponse {
                    status: 404,
                    body: serde_json::json!({ "error": "no focused hunk" }),
                }
            };
            let _ = respond_to.send(response);
        }
        ApiCommand::UpdateAnnotation {
            id,
            content,
            respond_to,
        } => {
            let response = if content.trim().is_empty() {
                ApiResponse {
                    status: 400,
                    body: serde_json::json!({ "error": "content cannot be empty" }),
                }
            } else if let Some(annotation) = update_annotation_content(state, &id, content) {
                if let (Some(scope), Some(persistence)) = (current_scope, persistence) {
                    if let Err(err) = persistence.upsert_annotation(state, &annotation, scope) {
                        eprintln!("Warning: failed to persist annotation: {}", err);
                    }
                }
                ApiResponse {
                    status: 200,
                    body: serde_json::json!({
                        "scope": scope_json(current_scope, state),
                        "annotation": annotation_json(&annotation),
                    }),
                }
            } else {
                ApiResponse {
                    status: 404,
                    body: serde_json::json!({ "error": "annotation not found" }),
                }
            };
            let _ = respond_to.send(response);
        }
        ApiCommand::DeleteAnnotation { id, respond_to } => {
            let response = if let Some(annotation) = remove_annotation_by_id(state, &id) {
                if let (Some(scope), Some(persistence)) = (current_scope, persistence) {
                    if let Err(err) = persistence.remove_annotation(&annotation, scope) {
                        eprintln!("Warning: failed to remove annotation: {}", err);
                    }
                }
                ApiResponse {
                    status: 200,
                    body: serde_json::json!({
                        "scope": scope_json(current_scope, state),
                        "deleted": true,
                        "annotation": annotation_json(&annotation),
                    }),
                }
            } else {
                ApiResponse {
                    status: 404,
                    body: serde_json::json!({ "error": "annotation not found" }),
                }
            };
            let _ = respond_to.send(response);
        }
    }
}

fn resolve_working_base_commit(backend: &dyn VcsBackend) -> Option<String> {
    let base_ref = backend.working_copy_parent_ref();
    backend.resolve_ref(base_ref).ok()
}

fn resolve_commit_id_from_options(
    options: &DiffOptions,
    backend: &dyn VcsBackend,
) -> Option<String> {
    match &options.reference {
        Some(CommitReference::Single(reference)) => backend.resolve_ref(reference).ok(),
        Some(CommitReference::Range { to, .. }) => backend.resolve_ref(to).ok(),
        Some(CommitReference::TripleDots { to, .. }) => backend.resolve_ref(to).ok(),
        None => None,
    }
}

fn determine_scope(
    state: &AppState,
    options: &DiffOptions,
    backend: &dyn VcsBackend,
) -> Option<DiffScope> {
    if state.stacked_mode {
        if let Some(commit) = state.current_commit() {
            return Some(DiffScope::Commit {
                commit_id: commit.commit_id.clone(),
            });
        }
        return None;
    }

    if let Some(commit_id) = resolve_commit_id_from_options(options, backend) {
        return Some(DiffScope::Commit { commit_id });
    }

    resolve_working_base_commit(backend)
        .map(|base_commit_id| DiffScope::WorkingTree { base_commit_id })
}

fn load_persistence_for_state(
    persistence: &mut PersistenceManager,
    view_state: Option<&mut ViewStateManager>,
    tag_manager: Option<&mut TagManager>,
    state: &mut AppState,
    options: &DiffOptions,
    backend: &dyn VcsBackend,
) -> Option<DiffScope> {
    let scope = determine_scope(state, options, backend)?;
    if let Err(err) = persistence.load_for_scope(state, &scope) {
        eprintln!("Warning: failed to load annotations: {}", err);
    }
    if let Some(view_state) = view_state {
        if let Err(err) = view_state.load_for_scope(state, &scope) {
            eprintln!("Warning: failed to load view state: {}", err);
        }
    }
    if let Some(tag_manager) = tag_manager {
        if let Err(err) = tag_manager.load_for_scope(state, &scope) {
            eprintln!("Warning: failed to load tags: {}", err);
        }
    }
    Some(scope)
}

fn save_view_state_for_scope(
    view_state: Option<&mut ViewStateManager>,
    state: &AppState,
    scope: Option<&DiffScope>,
) {
    if let (Some(view_state), Some(scope)) = (view_state, scope) {
        if let Err(err) = view_state.save_for_scope(state, scope) {
            eprintln!("Warning: failed to save view state: {}", err);
        }
    }
}

pub fn run_app_with_pr(
    options: DiffOptions,
    pr_info: PrInfo,
    backend: &dyn VcsBackend,
) -> io::Result<()> {
    match load_pr_file_diffs(&pr_info) {
        Ok(file_diffs) => run_app_internal(options, Some(pr_info), file_diffs, None, backend),
        Err(e) => {
            eprintln!("\x1b[91merror:\x1b[0m {}", e);
            std::process::exit(1);
        }
    }
}

pub fn run_app(
    options: DiffOptions,
    pr_info: Option<PrInfo>,
    backend: &dyn VcsBackend,
) -> io::Result<()> {
    let file_diffs = load_file_diffs(&options, backend);
    run_app_internal(options, pr_info, file_diffs, None, backend)
}

pub fn run_app_stacked(
    options: DiffOptions,
    commits: Vec<StackedCommitInfo>,
    backend: &dyn VcsBackend,
) -> io::Result<()> {
    // Load the first commit's diff
    let first_commit = &commits[0];
    let file_diffs = load_single_commit_diffs(&first_commit.commit_id, &options.file, backend);
    run_app_internal(options, None, file_diffs, Some(commits), backend)
}

/// Sync viewed files from GitHub to local state
fn sync_viewed_files_from_github(pr_info: &PrInfo, state: &mut AppState) {
    if let Ok(viewed_paths) = fetch_viewed_files(pr_info) {
        state.viewed_files.clear();
        for (idx, diff) in state.file_diffs.iter().enumerate() {
            if viewed_paths.contains(&diff.filename) {
                state.viewed_files.insert(idx);
            }
        }
    }
}

fn run_app_internal(
    options: DiffOptions,
    pr_info: Option<PrInfo>,
    file_diffs: Vec<super::types::FileDiff>,
    stacked_commits: Option<Vec<StackedCommitInfo>>,
    backend: &dyn VcsBackend,
) -> io::Result<()> {
    theme::init(options.theme.as_deref());
    highlight::init();

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let watch_rx = if options.watch && pr_info.is_none() {
        setup_watcher()
    } else {
        None
    };

    let settings = super::types::DiffViewSettings {
        unified_context: options.unified_context,
        ..super::types::DiffViewSettings::default()
    };
    let mut state = AppState::new(file_diffs, settings);
    state.set_vcs_name(backend.name());

    // Set diff reference for annotation export context
    let diff_ref_str = if let Some(pr) = &pr_info {
        Some(format!(
            "PR #{} ({}...{})",
            pr.number, pr.base_ref, pr.head_ref
        ))
    } else {
        options.reference.as_ref().map(|r| match r {
            CommitReference::Single(s) => s.clone(),
            CommitReference::Range { from, to } => format!("{}..{}", from, to),
            CommitReference::TripleDots { from, to } => format!("{}...{}", from, to),
        })
    };
    state.set_diff_reference(diff_ref_str);

    let mut active_modal: Option<Modal> = None;
    let mut annotation_editor: Option<AnnotationEditor> = None;
    let mut tag_editor: Option<TagEditor> = None;
    let mut pending_watch_event: Option<WatchEvent> = None;
    let mut pending_events: VecDeque<Event> = VecDeque::new();
    let mut api_rx: Option<mpsc::Receiver<ApiCommand>> = None;
    let _api_handle = if options.api.enabled {
        let (tx, rx) = mpsc::channel();
        api_rx = Some(rx);
        match start_api_server(&options.api.bind, tx) {
            Ok(handle) => Some(handle),
            Err(err) => {
                eprintln!("Warning: failed to start API server: {}", err);
                None
            }
        }
    } else {
        None
    };

    // Initialize stacked mode if commits were provided
    if let Some(commits) = stacked_commits {
        state.init_stacked_mode(commits);
    }

    let mut persistence = if pr_info.is_some() {
        None
    } else {
        PersistenceManager::try_new()?
    };
    let mut tag_manager = TagManager::try_new()?;
    let mut view_state = if pr_info.is_some() {
        None
    } else {
        ViewStateManager::try_new()?
    };
    let mut current_scope = None;
    if let Some(ref mut persistence) = persistence {
        current_scope = load_persistence_for_state(
            persistence,
            view_state.as_mut(),
            tag_manager.as_mut(),
            &mut state,
            &options,
            backend,
        );
    } else if let Some(ref mut tag_manager) = tag_manager {
        current_scope = determine_scope(&state, &options, backend);
        if let Some(ref scope) = current_scope {
            if let Err(err) = tag_manager.load_for_scope(&mut state, scope) {
                eprintln!("Warning: failed to load tags: {}", err);
            }
        }
    }

    // Load viewed files from GitHub on startup in PR mode
    if let Some(ref pr) = pr_info {
        sync_viewed_files_from_github(pr, &mut state);
    }

    'main: loop {
        if let Some(ref api_rx) = api_rx {
            while let Ok(command) = api_rx.try_recv() {
                handle_api_command(
                    command,
                    &mut state,
                    current_scope.as_ref(),
                    persistence.as_mut(),
                );
            }
        }

        if let Some(ref rx) = watch_rx {
            match rx.try_recv() {
                Ok(event) => {
                    state.needs_reload = true;
                    pending_watch_event = Some(event);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {}
            }
        }

        if state.needs_reload {
            let file_diffs = if let Some(ref pr) = pr_info {
                // In PR mode, reload from GitHub
                match load_pr_file_diffs(pr) {
                    Ok(diffs) => diffs,
                    Err(e) => {
                        eprintln!("Warning: failed to reload PR diffs: {}", e);
                        Vec::new()
                    }
                }
            } else {
                load_file_diffs(&options, backend)
            };

            // Pass changed files to reload so it can unmark them from viewed
            let changed_files = pending_watch_event.take().map(|e| e.changed_files);
            state.reload(file_diffs, changed_files.as_ref());

            // Re-sync viewed files from GitHub in PR mode
            if let Some(ref pr) = pr_info {
                sync_viewed_files_from_github(pr, &mut state);
            }
            apply_tag_filter(&mut state);
        }

        let has_visible_files = if state.tag_filter.is_some() {
            !state.sidebar_visible.is_empty()
        } else {
            !state.file_diffs.is_empty()
        };

        if !has_visible_files {
            terminal.draw(|frame| {
                render_empty_state(frame, options.watch);
                if let Some(ref modal) = active_modal {
                    modal.render(frame);
                }
            })?;
        } else {
            let diff = &state.file_diffs[state.current_file];
            let side_by_side = compute_side_by_side(
                &diff.old_content,
                &diff.new_content,
                state.settings.tab_width,
            );
            let hunk_ranges = find_hunk_ranges(&side_by_side, state.settings.unified_context);
            let matching_hunks = matching_hunk_indices(
                &state,
                state.current_file,
                &hunk_ranges,
                state.tag_filter.as_ref(),
            );
            let hunk_count = matching_hunks.len();
            let footer_focused_hunk = state
                .focused_hunk
                .and_then(|idx| matching_hunks.iter().position(|hunk| *hunk == idx));
            let tag_filter_label = state.tag_filter.as_ref().map(|filter| match filter {
                TagFilter::Tag(tag) => format!("tag: {}", tag),
                TagFilter::Untagged => "tag: untagged".to_string(),
            });
            let focused_tags = state
                .focused_hunk
                .and_then(|idx| state.get_hunk_tags(state.current_file, idx))
                .and_then(|tags| {
                    if tags.tags.is_empty() {
                        None
                    } else {
                        Some(tags.tags.join(", "))
                    }
                });
            state
                .search_state
                .update_matches(&side_by_side, state.diff_fullscreen);
            let branch_fallback = get_current_branch(backend);
            let commit_ref = state.diff_reference.as_deref().unwrap_or(&branch_fallback);
            terminal.draw(|frame| {
                render_diff(
                    frame,
                    diff,
                    &state.file_diffs,
                    &state.sidebar_items,
                    &state.sidebar_visible,
                    &state.collapsed_dirs,
                    state.current_file,
                    state.scroll,
                    state.h_scroll,
                    options.watch,
                    state.show_sidebar,
                    state.focused_panel,
                    state.sidebar_selected,
                    state.sidebar_scroll,
                    state.sidebar_h_scroll,
                    &state.viewed_files,
                    &state.settings,
                    hunk_count,
                    state.diff_fullscreen,
                    &state.search_state,
                    commit_ref,
                    pr_info.as_ref(),
                    state.focused_hunk,
                    &hunk_ranges,
                    footer_focused_hunk,
                    tag_filter_label,
                    focused_tags,
                    state.stacked_mode,
                    state.current_commit(),
                    state.current_commit_index,
                    state.stacked_commits.len(),
                    state.vcs_name,
                    &state.annotations,
                );
                // Render annotation editor (on top of everything except modal)
                if let Some(ref editor) = annotation_editor {
                    editor.render(frame);
                }
                if let Some(ref editor) = tag_editor {
                    editor.render(frame);
                }
                if let Some(ref modal) = active_modal {
                    modal.render(frame);
                }
            })?;
        }

        // Poll for new events if no pending events
        if pending_events.is_empty() && event::poll(Duration::from_millis(100))? {
            pending_events.push_back(event::read()?);
        }

        // Process all pending events
        while let Some(current_event) = pending_events.pop_front() {
            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
            let bottom_padding = 5;
            let max_scroll = if !state.file_diffs.is_empty() {
                let diff = &state.file_diffs[state.current_file];
                let total_lines = compute_side_by_side(
                    &diff.old_content,
                    &diff.new_content,
                    state.settings.tab_width,
                )
                .len();
                total_lines.saturating_sub(visible_height.saturating_sub(bottom_padding))
            } else {
                0
            };

            match current_event {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && state.search_state.is_active() =>
                {
                    match key.code {
                        KeyCode::Esc => {
                            state.search_state.cancel();
                        }
                        KeyCode::Enter => {
                            state.search_state.confirm();
                            if state.search_state.has_query() {
                                if let Some(line) = state
                                    .search_state
                                    .jump_to_first_match(state.scroll as usize)
                                {
                                    state.scroll = line.saturating_sub(5) as u16;
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            state.search_state.pop_char();
                        }
                        KeyCode::Char(c) => {
                            state.search_state.push_char(c);
                        }
                        _ => {}
                    }
                }
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && tag_editor.is_some()
                        && active_modal.is_none() =>
                {
                    if let Some(editor) = tag_editor.as_mut() {
                        match editor.handle_input(key) {
                            TagEditorResult::Continue => {}
                            TagEditorResult::Save(tags) => {
                                let file_index = editor.file_index;
                                let hunk_index = editor.hunk_index;
                                if let Some(diff) = state.file_diffs.get(file_index) {
                                    if tags.is_empty() {
                                        state.remove_hunk_tags(file_index, hunk_index);
                                    } else {
                                        state.set_hunk_tags(super::state::HunkTags {
                                            file_index,
                                            hunk_index,
                                            filename: diff.filename.clone(),
                                            tags: tags.clone(),
                                        });
                                    }
                                    if let (Some(ref mut tag_manager), Some(scope)) =
                                        (tag_manager.as_mut(), current_scope.as_ref())
                                    {
                                        if let Err(err) = tag_manager.set_hunk_tags(
                                            &state,
                                            file_index,
                                            hunk_index,
                                            tags,
                                            scope,
                                        ) {
                                            eprintln!(
                                                "Warning: failed to persist tags: {}",
                                                err
                                            );
                                        }
                                        state.tag_inventory = tag_manager.tags().to_vec();
                                    }
                                }
                                apply_tag_filter(&mut state);
                                tag_editor = None;
                            }
                            TagEditorResult::Cancel => {
                                tag_editor = None;
                            }
                        }
                    }
                }
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && annotation_editor.is_some()
                        && active_modal.is_none() =>
                {
                    if let Some(editor) = annotation_editor.as_mut() {
                        match editor.handle_input(key) {
                            AnnotationEditorResult::Continue => {}
                            AnnotationEditorResult::Save => {
                                let annotation = editor.to_annotation();
                                state.set_annotation(annotation.clone());
                                if let (Some(ref mut persistence), Some(scope)) =
                                    (persistence.as_mut(), current_scope.as_ref())
                                {
                                    if let Err(err) =
                                        persistence.upsert_annotation(&state, &annotation, scope)
                                    {
                                        eprintln!("Warning: failed to save annotation: {}", err);
                                    }
                                }
                                annotation_editor = None;
                            }
                            AnnotationEditorResult::Delete => {
                                if let Some(existing) = state
                                    .get_annotation(editor.file_index, editor.hunk_index)
                                    .cloned()
                                {
                                    state.remove_annotation(editor.file_index, editor.hunk_index);
                                    if let (Some(ref mut persistence), Some(scope)) =
                                        (persistence.as_mut(), current_scope.as_ref())
                                    {
                                        if let Err(err) =
                                            persistence.remove_annotation(&existing, scope)
                                        {
                                            eprintln!(
                                                "Warning: failed to remove annotation: {}",
                                                err
                                            );
                                        }
                                    }
                                }
                                annotation_editor = None;
                            }
                            AnnotationEditorResult::Cancel => {
                                annotation_editor = None;
                            }
                        }
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press && active_modal.is_some() => {
                    if let Some(ref mut modal) = active_modal {
                        let term_height = terminal.size()?.height;
                        if let Some(result) = modal.handle_input(key, term_height) {
                            match result {
                                ModalResult::FileSelected(file_index) => {
                                    state.reveal_file(file_index);
                                    state.select_file(file_index);
                                    if state.tag_filter.is_some() {
                                        apply_tag_filter(&mut state);
                                    }
                                    if let Some(idx) =
                                        state.sidebar_visible_index_for_file(state.current_file)
                                    {
                                        state.sidebar_selected = idx;
                                        let visible_height =
                                            terminal.size()?.height.saturating_sub(5) as usize;
                                        ensure_sidebar_visible(&mut state, visible_height);
                                    }
                                    active_modal = None;
                                }
                                ModalResult::AnnotationJump {
                                    file_index,
                                    hunk_index,
                                } => {
                                    // Jump to the file and hunk
                                    state.select_file(file_index);
                                    state.focused_hunk = Some(hunk_index);
                                    // Scroll to the hunk
                                    let diff = &state.file_diffs[file_index];
                                    let side_by_side = compute_side_by_side(
                                        &diff.old_content,
                                        &diff.new_content,
                                        state.settings.tab_width,
                                    );
                                    let hunks = find_hunk_ranges(
                                        &side_by_side,
                                        state.settings.unified_context,
                                    );
                                    if let Some(hunk) = hunks.get(hunk_index) {
                                        state.scroll = adjust_scroll_for_hunk(
                                            hunk.start,
                                            state.scroll,
                                            visible_height,
                                            max_scroll,
                                        );
                                    }
                                    active_modal = None;
                                }
                                ModalResult::AnnotationEdit {
                                    file_index,
                                    hunk_index,
                                } => {
                                    // Close modal and open annotation editor for editing
                                    if let Some(ann) = state.get_annotation(file_index, hunk_index)
                                    {
                                        let editor = AnnotationEditor::new(
                                            file_index,
                                            hunk_index,
                                            ann.filename.clone(),
                                            ann.line_range,
                                        )
                                        .with_content(&ann.content, ann.created_at, ann.id.clone());
                                        annotation_editor = Some(editor);
                                        // Also jump to the hunk
                                        state.select_file(file_index);
                                        state.focused_hunk = Some(hunk_index);
                                    }
                                    active_modal = None;
                                }
                                ModalResult::AnnotationDelete {
                                    file_index,
                                    hunk_index,
                                } => {
                                    if let Some(existing) =
                                        state.get_annotation(file_index, hunk_index).cloned()
                                    {
                                        state.remove_annotation(file_index, hunk_index);
                                        if let (Some(ref mut persistence), Some(scope)) =
                                            (persistence.as_mut(), current_scope.as_ref())
                                        {
                                            if let Err(err) =
                                                persistence.remove_annotation(&existing, scope)
                                            {
                                                eprintln!(
                                                    "Warning: failed to remove annotation: {}",
                                                    err
                                                );
                                            }
                                        }
                                    }
                                    // Refresh the modal if there are still annotations
                                    if !state.annotations.is_empty() {
                                        let mut sorted_annotations = state.annotations.clone();
                                        sorted_annotations.sort_by_key(|a| a.created_at);
                                        let items: Vec<String> = sorted_annotations
                                            .iter()
                                            .map(format_annotation_preview)
                                            .collect();
                                        active_modal = Some(Modal::annotations(
                                            "Annotations",
                                            items,
                                            sorted_annotations,
                                        ));
                                    } else {
                                        active_modal = None;
                                    }
                                }
                                ModalResult::AnnotationCopyAll => {
                                    // Copy all annotations to clipboard
                                    let formatted = state.format_annotations_for_export();
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        let _ = clipboard.set_text(&formatted);
                                    }
                                    active_modal = None;
                                }
                                ModalResult::AnnotationExport(filename) => {
                                    // Write annotations to file
                                    let formatted = state.format_annotations_for_export();
                                    match std::fs::write(&filename, &formatted) {
                                        Ok(_) => {
                                            active_modal = None;
                                        }
                                        Err(e) => {
                                            // Set error message on the modal
                                            if let Some(ref mut modal) = active_modal {
                                                if let ModalContent::Annotations {
                                                    error_message,
                                                    export_input,
                                                    ..
                                                } = &mut modal.content
                                                {
                                                    *error_message =
                                                        Some(format!("Failed to write: {}", e));
                                                    *export_input = None; // Close input, keep modal open
                                                }
                                            }
                                        }
                                    }
                                }
                                ModalResult::Dismissed | ModalResult::Selected(_, _) => {
                                    active_modal = None;
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse) if active_modal.is_some() => {
                    if let Some(ref mut modal) = active_modal {
                        let term_height = terminal.size()?.height;
                        modal.handle_mouse(mouse, term_height);
                    }
                }
                Event::Mouse(mouse) if active_modal.is_none() => {
                    let term_size = terminal.size()?;
                    let footer_height = 1u16;
                    let header_height = if state.stacked_mode { 1u16 } else { 0u16 };
                    let sidebar_width = if state.show_sidebar { 40u16 } else { 0u16 };

                    match mouse.kind {
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            // Check for stacked mode header arrow clicks
                            if state.stacked_mode && mouse.row < header_height {
                                // Left arrow click (first 4 columns to cover " < ")
                                if mouse.column < 4 && state.current_commit_index > 0 {
                                    let new_index = state.current_commit_index - 1;
                                    if navigate_stacked_commit(
                                        &mut state, new_index, &options, backend,
                                    ) {
                                        if let Some(ref mut persistence) = persistence {
                                            current_scope = load_persistence_for_state(
                                                persistence,
                                                view_state.as_mut(),
                                                tag_manager.as_mut(),
                                                &mut state,
                                                &options,
                                                backend,
                                            );
                                            apply_tag_filter(&mut state);
                                        }
                                    }
                                }
                                // Right arrow click (last 4 columns to cover " > ")
                                else if mouse.column >= term_size.width.saturating_sub(4)
                                    && state.current_commit_index
                                        < state.stacked_commits.len().saturating_sub(1)
                                {
                                    let new_index = state.current_commit_index + 1;
                                    if navigate_stacked_commit(
                                        &mut state, new_index, &options, backend,
                                    ) {
                                        if let Some(ref mut persistence) = persistence {
                                            current_scope = load_persistence_for_state(
                                                persistence,
                                                view_state.as_mut(),
                                                tag_manager.as_mut(),
                                                &mut state,
                                                &options,
                                                backend,
                                            );
                                            apply_tag_filter(&mut state);
                                        }
                                    }
                                }
                            } else if state.show_sidebar
                                && mouse.column < sidebar_width
                                && mouse.row >= header_height
                                && mouse.row < term_size.height.saturating_sub(footer_height)
                            {
                                let clicked_row = (mouse.row.saturating_sub(header_height + 1))
                                    as usize
                                    + state.sidebar_scroll;
                                if clicked_row < state.sidebar_visible_len() {
                                    let item = state.sidebar_item_at_visible(clicked_row).cloned();
                                    if let Some(item) = item {
                                        state.sidebar_selected = clicked_row;
                                        match item {
                                            SidebarItem::File { file_index, .. } => {
                                                state.focused_panel = FocusedPanel::DiffView;
                                                state.select_file(file_index);
                                            }
                                            SidebarItem::Directory { path, .. } => {
                                                state.focused_panel = FocusedPanel::Sidebar;
                                                state.toggle_directory(&path);
                                                let visible_height =
                                                    term_size.height.saturating_sub(5) as usize;
                                                if state.sidebar_selected < state.sidebar_scroll {
                                                    state.sidebar_scroll = state.sidebar_selected;
                                                } else if state.sidebar_selected
                                                    >= state.sidebar_scroll + visible_height
                                                {
                                                    state.sidebar_scroll = state
                                                        .sidebar_selected
                                                        .saturating_sub(visible_height)
                                                        + 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if mouse.column >= sidebar_width {
                                state.focused_panel = FocusedPanel::DiffView;
                            }
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // Coalesce consecutive scroll events to handle fast scrolling.
                            // Non-scroll events are preserved in pending_events queue.
                            let mut scroll_delta: i32 = match mouse.kind {
                                MouseEventKind::ScrollDown => 3,
                                MouseEventKind::ScrollUp => -3,
                                _ => 0,
                            };

                            // Coalesce scroll events, but preserve non-scroll events
                            while event::poll(Duration::from_millis(0))? {
                                let next_event = event::read()?;
                                match &next_event {
                                    Event::Mouse(m) => match m.kind {
                                        MouseEventKind::ScrollDown => scroll_delta += 3,
                                        MouseEventKind::ScrollUp => scroll_delta -= 3,
                                        _ => {
                                            // Non-scroll mouse event - queue for processing
                                            pending_events.push_back(next_event);
                                            break;
                                        }
                                    },
                                    _ => {
                                        // Non-mouse event - queue for processing
                                        pending_events.push_back(next_event);
                                        break;
                                    }
                                }
                            }

                            // Apply the accumulated scroll delta
                            let in_sidebar = state.show_sidebar
                                && mouse.column < sidebar_width
                                && mouse.row < term_size.height.saturating_sub(footer_height);
                            let in_diff = mouse.column >= sidebar_width
                                && mouse.row < term_size.height.saturating_sub(footer_height);

                            if in_sidebar {
                                let max_sidebar_scroll =
                                    state.sidebar_visible_len().saturating_sub(1);
                                if scroll_delta > 0 {
                                    state.sidebar_scroll = (state.sidebar_scroll
                                        + scroll_delta as usize)
                                        .min(max_sidebar_scroll);
                                } else {
                                    state.sidebar_scroll = state
                                        .sidebar_scroll
                                        .saturating_sub((-scroll_delta) as usize);
                                }
                            } else if in_diff {
                                if scroll_delta > 0 {
                                    state.scroll =
                                        (state.scroll + scroll_delta as u16).min(max_scroll as u16);
                                } else {
                                    state.scroll =
                                        state.scroll.saturating_sub((-scroll_delta) as u16);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press && active_modal.is_none() => {
                    if key.code != KeyCode::Char('g') {
                        state.pending_key = PendingKey::None;
                    }
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('c')
                            if (key.code == KeyCode::Esc
                                || key.modifiers.contains(KeyModifiers::CONTROL))
                                && state.search_state.has_query() =>
                        {
                            state.search_state.clear();
                        }
                        KeyCode::Char('q') | KeyCode::Esc => {
                            save_view_state_for_scope(
                                view_state.as_mut(),
                                &state,
                                current_scope.as_ref(),
                            );
                            break 'main
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            save_view_state_for_scope(
                                view_state.as_mut(),
                                &state,
                                current_scope.as_ref(),
                            );
                            break 'main
                        }
                        KeyCode::Char('1') => {
                            state.focused_panel = FocusedPanel::Sidebar;
                            state.show_sidebar = true;
                            if !matches!(
                                state.sidebar_item_at_visible(state.sidebar_selected),
                                Some(SidebarItem::File { .. })
                            ) {
                                if let Some(idx) = state.sidebar_visible.iter().position(|idx| {
                                    matches!(state.sidebar_items[*idx], SidebarItem::File { .. })
                                }) {
                                    state.sidebar_selected = idx;
                                }
                            }
                        }
                        KeyCode::Char('2') => {
                            state.focused_panel = FocusedPanel::DiffView;
                        }
                        KeyCode::Tab => {
                            state.show_sidebar = !state.show_sidebar;
                            if !state.show_sidebar {
                                state.focused_panel = FocusedPanel::DiffView;
                            }
                        }
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !state.file_diffs.is_empty() {
                                let mut next = state.sidebar_selected + 1;
                                while next < state.sidebar_visible_len() {
                                    if let Some(SidebarItem::File { file_index, .. }) =
                                        state.sidebar_item_at_visible(next).cloned()
                                    {
                                        state.sidebar_selected = next;
                                        state.select_file(file_index);
                                        let visible_height =
                                            terminal.size()?.height.saturating_sub(5) as usize;
                                        ensure_sidebar_visible(&mut state, visible_height);
                                        break;
                                    }
                                    next += 1;
                                }
                            }
                        }
                        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !state.file_diffs.is_empty() && state.sidebar_selected > 0 {
                                let mut prev = state.sidebar_selected - 1;
                                loop {
                                    if let Some(SidebarItem::File { file_index, .. }) =
                                        state.sidebar_item_at_visible(prev).cloned()
                                    {
                                        state.sidebar_selected = prev;
                                        state.select_file(file_index);
                                        ensure_sidebar_visible(&mut state, usize::MAX);
                                        break;
                                    }
                                    if prev == 0 {
                                        break;
                                    }
                                    prev -= 1;
                                }
                            }
                        }
                        // Stacked mode: navigate to next commit
                        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if state.stacked_mode
                                && state.current_commit_index < state.stacked_commits.len() - 1
                            {
                                let new_index = state.current_commit_index + 1;
                                if navigate_stacked_commit(&mut state, new_index, &options, backend)
                                {
                                    if let Some(ref mut persistence) = persistence {
                                        current_scope = load_persistence_for_state(
                                            persistence,
                                            view_state.as_mut(),
                                            tag_manager.as_mut(),
                                            &mut state,
                                            &options,
                                            backend,
                                        );
                                        apply_tag_filter(&mut state);
                                    }
                                }
                            }
                        }
                        // Stacked mode: navigate to previous commit
                        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if state.stacked_mode && state.current_commit_index > 0 {
                                let new_index = state.current_commit_index - 1;
                                if navigate_stacked_commit(&mut state, new_index, &options, backend)
                                {
                                    if let Some(ref mut persistence) = persistence {
                                        current_scope = load_persistence_for_state(
                                            persistence,
                                            view_state.as_mut(),
                                            tag_manager.as_mut(),
                                            &mut state,
                                            &options,
                                            backend,
                                        );
                                        apply_tag_filter(&mut state);
                                    }
                                }
                            }
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let half_screen = (visible_height / 2) as u16;
                            state.scroll = (state.scroll + half_screen).min(max_scroll as u16);
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let half_screen = (visible_height / 2) as u16;
                            state.scroll = state.scroll.saturating_sub(half_screen);
                        }
                        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !state.file_diffs.is_empty() {
                                let items: Vec<FilePickerItem> = state
                                    .file_diffs
                                    .iter()
                                    .enumerate()
                                    .map(|(i, diff)| {
                                        let status = match diff.status {
                                            FileStatus::Added => ModalFileStatus::Added,
                                            FileStatus::Modified => ModalFileStatus::Modified,
                                            FileStatus::Deleted => ModalFileStatus::Deleted,
                                        };
                                        FilePickerItem {
                                            name: diff.filename.clone(),
                                            file_index: i,
                                            status,
                                            viewed: state.viewed_files.contains(&i),
                                        }
                                    })
                                    .collect();
                                active_modal = Some(Modal::file_picker("Find File", items));
                            }
                        }
                        KeyCode::Char(']') => {
                            if !state.file_diffs.is_empty() {
                                let diff = &state.file_diffs[state.current_file];
                                if !diff.new_content.is_empty() {
                                    state.diff_fullscreen = match state.diff_fullscreen {
                                        DiffFullscreen::NewOnly => DiffFullscreen::None,
                                        _ => DiffFullscreen::NewOnly,
                                    };
                                }
                            }
                        }
                        KeyCode::Char('[') => {
                            if !state.file_diffs.is_empty() {
                                let diff = &state.file_diffs[state.current_file];
                                if !diff.old_content.is_empty() {
                                    state.diff_fullscreen = match state.diff_fullscreen {
                                        DiffFullscreen::OldOnly => DiffFullscreen::None,
                                        _ => DiffFullscreen::OldOnly,
                                    };
                                }
                            }
                        }
                        KeyCode::Char('=') => {
                            state.diff_fullscreen = DiffFullscreen::None;
                        }
                        KeyCode::Down
                            if state.search_state.has_query()
                                && state.focused_panel == FocusedPanel::DiffView =>
                        {
                            if let Some(line) = state.search_state.find_next() {
                                state.scroll = adjust_scroll_to_line(
                                    line,
                                    state.scroll,
                                    visible_height,
                                    max_scroll,
                                );
                            }
                        }
                        KeyCode::Up
                            if state.search_state.has_query()
                                && state.focused_panel == FocusedPanel::DiffView =>
                        {
                            if let Some(line) = state.search_state.find_prev() {
                                state.scroll = adjust_scroll_to_line(
                                    line,
                                    state.scroll,
                                    visible_height,
                                    max_scroll,
                                );
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if state.focused_panel == FocusedPanel::Sidebar {
                                if state.sidebar_selected + 1 < state.sidebar_visible_len() {
                                    state.sidebar_selected += 1;
                                }
                                let visible_height =
                                    terminal.size()?.height.saturating_sub(5) as usize;
                                ensure_sidebar_visible(&mut state, visible_height);
                            } else {
                                state.scroll = (state.scroll + 1).min(max_scroll as u16);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if state.focused_panel == FocusedPanel::Sidebar {
                                if state.sidebar_selected > 0 {
                                    state.sidebar_selected =
                                        state.sidebar_selected.saturating_sub(1);
                                }
                                ensure_sidebar_visible(&mut state, usize::MAX);
                            } else {
                                state.scroll = state.scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Char('h') | KeyCode::Left => {
                            if state.focused_panel == FocusedPanel::DiffView {
                                state.h_scroll = state.h_scroll.saturating_sub(4);
                            } else if state.focused_panel == FocusedPanel::Sidebar {
                                state.sidebar_h_scroll = state.sidebar_h_scroll.saturating_sub(4);
                            }
                        }
                        KeyCode::Char('l') | KeyCode::Right => {
                            if state.focused_panel == FocusedPanel::DiffView {
                                state.h_scroll = state.h_scroll.saturating_add(4);
                            } else if state.focused_panel == FocusedPanel::Sidebar {
                                state.sidebar_h_scroll = state.sidebar_h_scroll.saturating_add(4);
                            }
                        }
                        KeyCode::Enter => {
                            if state.focused_panel == FocusedPanel::Sidebar
                                && state.sidebar_selected < state.sidebar_visible_len()
                            {
                                if let Some(item) = state
                                    .sidebar_item_at_visible(state.sidebar_selected)
                                    .cloned()
                                {
                                    match item {
                                        SidebarItem::File { file_index, .. } => {
                                            state.select_file(file_index);
                                            state.focused_panel = FocusedPanel::DiffView;
                                        }
                                        SidebarItem::Directory { path, .. } => {
                                            state.toggle_directory(&path);
                                            if state.tag_filter.is_some() {
                                                apply_tag_filter(&mut state);
                                            }
                                            if state.tag_filter.is_some() {
                                                apply_tag_filter(&mut state);
                                            }
                                            let visible_height =
                                                terminal.size()?.height.saturating_sub(5) as usize;
                                            if state.sidebar_selected < state.sidebar_scroll {
                                                state.sidebar_scroll = state.sidebar_selected;
                                            } else if state.sidebar_selected
                                                >= state.sidebar_scroll + visible_height
                                            {
                                                state.sidebar_scroll = state
                                                    .sidebar_selected
                                                    .saturating_sub(visible_height)
                                                    + 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char(' ') => {
                            let mut should_save_view_state = false;
                            if state.focused_panel == FocusedPanel::Sidebar
                                && state.sidebar_selected < state.sidebar_visible_len()
                            {
                                let selected = state
                                    .sidebar_item_at_visible(state.sidebar_selected)
                                    .cloned();
                                if let Some(selected) = selected {
                                    match selected {
                                        SidebarItem::File { file_index, .. } => {
                                            let file_idx = file_index;
                                            let filename =
                                                state.file_diffs[file_idx].filename.clone();
                                            let was_viewed = state.viewed_files.contains(&file_idx);

                                            // Optimistic update - update local state immediately
                                            if was_viewed {
                                                state.viewed_files.remove(&file_idx);
                                            } else {
                                                state.viewed_files.insert(file_idx);
                                            }
                                            should_save_view_state = true;

                                            // Fire off async API call if in PR mode
                                            if let Some(ref pr) = pr_info {
                                                if was_viewed {
                                                    unmark_file_as_viewed_async(pr, &filename);
                                                } else {
                                                    mark_file_as_viewed_async(pr, &filename);
                                                }
                                            }
                                        }
                                        SidebarItem::Directory { path, .. } => {
                                            let dir_prefix = format!("{}/", path);
                                            let child_indices: Vec<usize> = state
                                                .sidebar_items
                                                .iter()
                                                .filter_map(|item| {
                                                    if let SidebarItem::File {
                                                        path: file_path,
                                                        file_index,
                                                        ..
                                                    } = item
                                                    {
                                                        if file_path.starts_with(&dir_prefix) {
                                                            return Some(*file_index);
                                                        }
                                                    }
                                                    None
                                                })
                                                .collect();

                                            let all_viewed = child_indices
                                                .iter()
                                                .all(|i| state.viewed_files.contains(i));

                                            // Optimistic update - update local state immediately
                                            if all_viewed {
                                                for idx in &child_indices {
                                                    state.viewed_files.remove(idx);
                                                }
                                            } else {
                                                for idx in &child_indices {
                                                    state.viewed_files.insert(*idx);
                                                }
                                            }
                                            should_save_view_state = true;

                                            // Fire off async API calls if in PR mode
                                            if let Some(ref pr) = pr_info {
                                                for &idx in &child_indices {
                                                    let filename = &state.file_diffs[idx].filename;
                                                    if all_viewed {
                                                        unmark_file_as_viewed_async(pr, filename);
                                                    } else {
                                                        mark_file_as_viewed_async(pr, filename);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if state.focused_panel == FocusedPanel::DiffView {
                                let current_file = state.current_file;
                                let filename = state.file_diffs[current_file].filename.clone();
                                let was_viewed = state.viewed_files.contains(&current_file);

                                // Optimistic update - update local state immediately
                                if was_viewed {
                                    state.viewed_files.remove(&current_file);
                                } else {
                                    state.viewed_files.insert(current_file);
                                    // Move to next unviewed file
                                    let mut next_file: Option<(usize, usize)> = None;
                                    for (visible_idx, item_idx) in state
                                        .sidebar_visible
                                        .iter()
                                        .enumerate()
                                        .skip(state.sidebar_selected + 1)
                                    {
                                        if let SidebarItem::File { file_index, .. } =
                                            &state.sidebar_items[*item_idx]
                                        {
                                            if !state.viewed_files.contains(file_index) {
                                                next_file = Some((visible_idx, *file_index));
                                                break;
                                            }
                                        }
                                    }
                                    if next_file.is_none() {
                                        for (visible_idx, item_idx) in state
                                            .sidebar_visible
                                            .iter()
                                            .enumerate()
                                            .take(state.sidebar_selected)
                                        {
                                            if let SidebarItem::File { file_index, .. } =
                                                &state.sidebar_items[*item_idx]
                                            {
                                                if !state.viewed_files.contains(file_index) {
                                                    next_file = Some((visible_idx, *file_index));
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if let Some((idx, file_idx)) = next_file {
                                        state.sidebar_selected = idx;
                                        state.select_file(file_idx);
                                        let visible_height =
                                            terminal.size()?.height.saturating_sub(5) as usize;
                                        ensure_sidebar_visible(&mut state, visible_height);
                                    }
                                }
                                should_save_view_state = true;

                                // Fire off async API call if in PR mode
                                if let Some(ref pr) = pr_info {
                                    if was_viewed {
                                        unmark_file_as_viewed_async(pr, &filename);
                                    } else {
                                        mark_file_as_viewed_async(pr, &filename);
                                    }
                                }
                            }
                            if should_save_view_state {
                                save_view_state_for_scope(
                                    view_state.as_mut(),
                                    &state,
                                    current_scope.as_ref(),
                                );
                            }
                        }
                        KeyCode::PageDown => {
                            state.scroll = (state.scroll + 20).min(max_scroll as u16);
                        }
                        KeyCode::PageUp => {
                            state.scroll = state.scroll.saturating_sub(20);
                        }
                        KeyCode::Char('}') => {
                            if !state.file_diffs.is_empty() {
                                let diff = &state.file_diffs[state.current_file];
                                let side_by_side = compute_side_by_side(
                                    &diff.old_content,
                                    &diff.new_content,
                                    state.settings.tab_width,
                                );
                                let hunks =
                                    find_hunk_ranges(&side_by_side, state.settings.unified_context);
                                let matching = matching_hunk_indices(
                                    &state,
                                    state.current_file,
                                    &hunks,
                                    state.tag_filter.as_ref(),
                                );
                                if !matching.is_empty() {
                                    let current_pos = state.focused_hunk.and_then(|idx| {
                                        matching.iter().position(|hunk| *hunk == idx)
                                    });
                                    let next_hunk = if let Some(pos) = current_pos {
                                        if pos + 1 < matching.len() {
                                            matching[pos + 1]
                                        } else {
                                            matching[pos]
                                        }
                                    } else {
                                        matching
                                            .iter()
                                            .copied()
                                            .find(|idx| {
                                                hunks[*idx].start > state.scroll as usize + 5
                                            })
                                            .unwrap_or(matching[0])
                                    };
                                    state.focused_hunk = Some(next_hunk);
                                    state.scroll = adjust_scroll_for_hunk(
                                        hunks[next_hunk].start,
                                        state.scroll,
                                        visible_height,
                                        max_scroll,
                                    );
                                } else {
                                    state.focused_hunk = None;
                                }
                            }
                        }
                        KeyCode::Char('{') => {
                            if !state.file_diffs.is_empty() {
                                let diff = &state.file_diffs[state.current_file];
                                let side_by_side = compute_side_by_side(
                                    &diff.old_content,
                                    &diff.new_content,
                                    state.settings.tab_width,
                                );
                                let hunks =
                                    find_hunk_ranges(&side_by_side, state.settings.unified_context);
                                let matching = matching_hunk_indices(
                                    &state,
                                    state.current_file,
                                    &hunks,
                                    state.tag_filter.as_ref(),
                                );
                                if !matching.is_empty() {
                                    let current_pos = state.focused_hunk.and_then(|idx| {
                                        matching.iter().position(|hunk| *hunk == idx)
                                    });
                                    let prev_hunk = if let Some(pos) = current_pos {
                                        if pos > 0 {
                                            matching[pos - 1]
                                        } else {
                                            matching[pos]
                                        }
                                    } else {
                                        matching
                                            .iter()
                                            .copied()
                                            .rfind(|idx| {
                                                (hunks[*idx].start as u16)
                                                    < state.scroll.saturating_sub(5)
                                            })
                                            .unwrap_or(matching[matching.len() - 1])
                                    };
                                    state.focused_hunk = Some(prev_hunk);
                                    state.scroll = adjust_scroll_for_hunk(
                                        hunks[prev_hunk].start,
                                        state.scroll,
                                        visible_height,
                                        max_scroll,
                                    );
                                } else {
                                    state.focused_hunk = None;
                                }
                            }
                        }
                        KeyCode::Char('i') => {
                            // Add annotation to focused hunk
                            if let Some(hunk_index) = state.focused_hunk {
                                let file_index = state.current_file;
                                let diff = &state.file_diffs[file_index];

                                // Calculate line range for this hunk
                                let side_by_side = compute_side_by_side(
                                    &diff.old_content,
                                    &diff.new_content,
                                    state.settings.tab_width,
                                );
                                let hunks =
                                    find_hunk_ranges(&side_by_side, state.settings.unified_context);
                                if let Some(hunk_range) = hunks.get(hunk_index) {
                                    if let Some((actual_hunk_start, actual_hunk_end)) =
                                        hunk_change_bounds(&side_by_side, *hunk_range)
                                    {
                                        let start_line = side_by_side
                                            .get(actual_hunk_start)
                                            .and_then(|dl| {
                                                dl.new_line
                                                    .as_ref()
                                                    .map(|(n, _)| *n)
                                                    .or(dl.old_line.as_ref().map(|(n, _)| *n))
                                            })
                                            .unwrap_or(1);
                                        let end_line = side_by_side
                                            .get(actual_hunk_end)
                                            .and_then(|dl| {
                                                dl.new_line
                                                    .as_ref()
                                                    .map(|(n, _)| *n)
                                                    .or(dl.old_line.as_ref().map(|(n, _)| *n))
                                            })
                                            .unwrap_or(start_line);

                                        let editor = AnnotationEditor::new(
                                            file_index,
                                            hunk_index,
                                            diff.filename.clone(),
                                            (start_line, end_line),
                                        );

                                        // If editing existing, pre-fill content
                                        let editor = if let Some(ann) =
                                            state.get_annotation(file_index, hunk_index)
                                        {
                                            editor.with_content(
                                                &ann.content,
                                                ann.created_at,
                                                ann.id.clone(),
                                            )
                                        } else {
                                            editor
                                        };

                                        annotation_editor = Some(editor);
                                    }
                                }
                            }
                        }
                        KeyCode::Char('t') => {
                            if tag_editor.is_none() {
                                if let Some(hunk_index) = state.focused_hunk {
                                    let file_index = state.current_file;
                                    if let Some(line_range) =
                                        compute_hunk_line_range(&state, file_index, hunk_index)
                                    {
                                        if let Some(diff) = state.file_diffs.get(file_index) {
                                            let existing_tags = state
                                                .get_hunk_tags(file_index, hunk_index)
                                                .map(|tags| tags.tags.clone())
                                                .unwrap_or_default();
                                            let editor = TagEditor::new(
                                                file_index,
                                                hunk_index,
                                                diff.filename.clone(),
                                                line_range,
                                                existing_tags,
                                                state.tag_inventory.clone(),
                                            );
                                            tag_editor = Some(editor);
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('I') => {
                            // Open annotations menu
                            if !state.annotations.is_empty() {
                                let mut sorted_annotations = state.annotations.clone();
                                sorted_annotations.sort_by_key(|a| a.created_at);
                                let items: Vec<String> = sorted_annotations
                                    .iter()
                                    .map(format_annotation_preview)
                                    .collect();
                                active_modal = Some(Modal::annotations(
                                    "Annotations",
                                    items,
                                    sorted_annotations,
                                ));
                            }
                        }
                        KeyCode::Char('T') => {
                            let next_filter =
                                next_tag_filter(state.tag_filter.as_ref(), &state.tag_inventory);
                            state.tag_filter = next_filter;
                            apply_tag_filter(&mut state);
                            if let Some(filter) = state.tag_filter.as_ref() {
                                if let Some(diff) = state.file_diffs.get(state.current_file) {
                                    let side_by_side = compute_side_by_side(
                                        &diff.old_content,
                                        &diff.new_content,
                                        state.settings.tab_width,
                                    );
                                    let hunks = find_hunk_ranges(
                                        &side_by_side,
                                        state.settings.unified_context,
                                    );
                                    let matching =
                                        matching_hunk_indices(&state, state.current_file, &hunks, Some(filter));
                                    if let Some(first) = matching.first().copied() {
                                        state.focused_hunk = Some(first);
                                        state.scroll = adjust_scroll_for_hunk(
                                            hunks[first].start,
                                            state.scroll,
                                            visible_height,
                                            max_scroll,
                                        );
                                    } else {
                                        state.focused_hunk = None;
                                    }
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            state.needs_reload = true;
                        }
                        KeyCode::Char('y') => {
                            if !state.file_diffs.is_empty() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard
                                        .set_text(&state.file_diffs[state.current_file].filename);
                                }
                            }
                        }
                        KeyCode::Char('e') => {
                            if !state.file_diffs.is_empty() {
                                io::stdout().execute(DisableMouseCapture)?;
                                io::stdout().execute(LeaveAlternateScreen)?;
                                disable_raw_mode()?;

                                let editor =
                                    std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
                                let filename = &state.file_diffs[state.current_file].filename;

                                let line_arg = if let Some(hunk_idx) = state.focused_hunk {
                                    let diff = &state.file_diffs[state.current_file];
                                    let side_by_side = compute_side_by_side(
                                        &diff.old_content,
                                        &diff.new_content,
                                        state.settings.tab_width,
                                    );
                                    let hunks = find_hunk_ranges(
                                        &side_by_side,
                                        state.settings.unified_context,
                                    );
                                    if let Some(hunk) = hunks.get(hunk_idx) {
                                        hunk_change_bounds(&side_by_side, *hunk)
                                            .and_then(|(start, _)| side_by_side.get(start))
                                            .and_then(|dl| {
                                                dl.new_line
                                                    .as_ref()
                                                    .map(|(n, _)| *n)
                                                    .or(dl.old_line.as_ref().map(|(n, _)| *n))
                                            })
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };

                                let status = if let Some(line) = line_arg {
                                    std::process::Command::new(&editor)
                                        .arg(format!("+{}", line))
                                        .arg(filename)
                                        .status()
                                } else {
                                    std::process::Command::new(&editor).arg(filename).status()
                                };
                                let _ = status;

                                enable_raw_mode()?;
                                io::stdout().execute(EnterAlternateScreen)?;
                                io::stdout().execute(EnableMouseCapture)?;
                                terminal.clear()?;
                            }
                        }
                        KeyCode::Char('o') => {
                            if let Some(ref pr) = pr_info {
                                if !state.file_diffs.is_empty() {
                                    let filename = &state.file_diffs[state.current_file].filename;
                                    let file_url = format!(
                                        "https://github.com/{}/{}/pull/{}/files#diff-{}",
                                        pr.repo_owner,
                                        pr.repo_name,
                                        pr.number,
                                        generate_file_anchor(filename)
                                    );
                                    let _ = open_url(&file_url);
                                }
                            }
                        }
                        KeyCode::Char('g') => {
                            if state.pending_key == PendingKey::G {
                                state.scroll = 0;
                                state.pending_key = PendingKey::None;
                            } else {
                                state.pending_key = PendingKey::G;
                            }
                        }
                        KeyCode::Char('G') => {
                            state.scroll = max_scroll as u16;
                        }
                        KeyCode::Char('/') | KeyCode::Char('f')
                            if key.code == KeyCode::Char('/')
                                || key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.search_state.start_forward();
                        }
                        KeyCode::Char('n') if state.search_state.has_query() => {
                            if let Some(line) = state.search_state.find_next() {
                                state.scroll = adjust_scroll_to_line(
                                    line,
                                    state.scroll,
                                    visible_height,
                                    max_scroll,
                                );
                            }
                        }
                        KeyCode::Char('N') if state.search_state.has_query() => {
                            if let Some(line) = state.search_state.find_prev() {
                                state.scroll = adjust_scroll_to_line(
                                    line,
                                    state.scroll,
                                    visible_height,
                                    max_scroll,
                                );
                            }
                        }
                        KeyCode::Char('?') => {
                            active_modal = Some(Modal::keybindings(
                                "Keybindings",
                                vec![
                                    KeyBindSection {
                                        title: "Global",
                                        bindings: vec![
                                            KeyBind {
                                                key: "q / esc",
                                                description: "Quit",
                                            },
                                            KeyBind {
                                                key: "tab",
                                                description: "Toggle sidebar",
                                            },
                                            KeyBind {
                                                key: "1 / 2",
                                                description: "Focus sidebar / diff",
                                            },
                                            KeyBind {
                                                key: "ctrl+j / ctrl+k",
                                                description: "Next / previous file",
                                            },
                                            KeyBind {
                                                key: "ctrl+d / ctrl+u",
                                                description: "Scroll half page down / up",
                                            },
                                            KeyBind {
                                                key: "ctrl+p",
                                                description: "Open file picker",
                                            },
                                            KeyBind {
                                                key: "r",
                                                description: "Refresh diff / PR",
                                            },
                                            KeyBind {
                                                key: "y",
                                                description: "Copy current filename",
                                            },
                                            KeyBind {
                                                key: "e",
                                                description: "Edit file (at hunk line if focused)",
                                            },
                                            KeyBind {
                                                key: "o",
                                                description: "Open file in browser (PR mode)",
                                            },
                                            KeyBind {
                                                key: "ctrl+l / ctrl+h",
                                                description: "Next / prev commit (stacked)",
                                            },
                                            KeyBind {
                                                key: "?",
                                                description: "Show keybindings",
                                            },
                                        ],
                                    },
                                    KeyBindSection {
                                        title: "Sidebar",
                                        bindings: vec![
                                            KeyBind {
                                                key: "j/k or up/down",
                                                description: "Navigate files",
                                            },
                                            KeyBind {
                                                key: "h/l or left/right",
                                                description: "Scroll horizontally",
                                            },
                                            KeyBind {
                                                key: "enter",
                                                description:
                                                    "Open file in diff view / toggle directory",
                                            },
                                            KeyBind {
                                                key: "space",
                                                description: "Toggle file as viewed",
                                            },
                                        ],
                                    },
                                    KeyBindSection {
                                        title: "Diff View",
                                        bindings: vec![
                                            KeyBind {
                                                key: "j/k or up/down",
                                                description: "Scroll vertically",
                                            },
                                            KeyBind {
                                                key: "h/l or left/right",
                                                description: "Scroll horizontally",
                                            },
                                            KeyBind {
                                                key: "gg / G",
                                                description: "Scroll to top / bottom",
                                            },
                                            KeyBind {
                                                key: "{ / }",
                                                description: "Focus prev / next hunk",
                                            },
                                            KeyBind {
                                                key: "pageup / pagedown",
                                                description: "Scroll by page",
                                            },
                                            KeyBind {
                                                key: "space",
                                                description: "Mark viewed & next file",
                                            },
                                            KeyBind {
                                                key: "]",
                                                description: "Toggle new panel fullscreen",
                                            },
                                            KeyBind {
                                                key: "[",
                                                description: "Toggle old panel fullscreen",
                                            },
                                            KeyBind {
                                                key: "=",
                                                description: "Reset fullscreen to side-by-side",
                                            },
                                        ],
                                    },
                                    KeyBindSection {
                                        title: "Search",
                                        bindings: vec![
                                            KeyBind {
                                                key: "/ or ctrl+f",
                                                description: "Start search",
                                            },
                                            KeyBind {
                                                key: "n or down",
                                                description: "Next match",
                                            },
                                            KeyBind {
                                                key: "N or up",
                                                description: "Previous match",
                                            },
                                            KeyBind {
                                                key: "ctrl+c or esc",
                                                description: "Cancel search",
                                            },
                                        ],
                                    },
                                    KeyBindSection {
                                        title: "Annotations",
                                        bindings: vec![
                                            KeyBind {
                                                key: "i",
                                                description: "Add annotation to focused hunk",
                                            },
                                            KeyBind {
                                                key: "I",
                                                description: "View all annotations",
                                            },
                                        ],
                                    },
                                    KeyBindSection {
                                        title: "Tags",
                                        bindings: vec![
                                            KeyBind {
                                                key: "t",
                                                description: "Tag focused hunk",
                                            },
                                            KeyBind {
                                                key: "T",
                                                description: "Cycle tag filter",
                                            },
                                        ],
                                    },
                                ],
                            ));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    io::stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

fn open_url(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()?;
    }
    Ok(())
}

fn generate_file_anchor(filename: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(filename.as_bytes());
    format!("{:x}", hasher.finalize())
}
