use crate::anchored_fs::{AnchoredFsError, AnchoredFsErrorCode, AnchoredRoot, ExclusiveStage};
use crate::domain::{
    managed_attachment_relative_path, new_id, validate_attachment_directory,
    validate_notebook_relative_path, Notebook, NotebookTargetState, NumberingStyle, Record,
    Workspace,
};
use crate::managed_markdown::{
    detach_notebook_controls, managed_region_sha256_unchecked, managed_snapshot,
    overwrite_managed_region_for_path, parse_adoptable_file_version, synchronize_notebook_for_path,
    MANAGED_SCHEMA_VERSION,
};
use crate::platform_trash;
use crate::storage::{
    NotebookConflictEvidence, NotebookConflictRecordUpdate, NotebookFileReceipt,
    NotebookFileReceiptInput, RecoveryFileStep, RecoveryFileStepInput, RecoveryOperation, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::UNIX_EPOCH;

const MAX_NOTEBOOK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NOTEBOOK_SCAN_ENTRIES: usize = 20_000;
const MAX_NOTEBOOK_SCAN_DEPTH: usize = 32;
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug)]
pub(crate) enum SyncError {
    Store(StoreError),
    Managed(String),
    Io(&'static str),
    Conflict(&'static str),
    RecoveryRequired(&'static str),
    LockPoisoned,
}

impl SyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(_) => "storage_error",
            Self::Managed(_) => "managed_markdown_conflict",
            Self::Io(code) | Self::Conflict(code) | Self::RecoveryRequired(code) => code,
            Self::LockPoisoned => "sync_lock_unavailable",
        }
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "local storage error: {error}"),
            Self::Managed(message) => formatter.write_str(message),
            Self::Io(_) => formatter.write_str("the Markdown target is unavailable"),
            Self::Conflict(_) => formatter.write_str("the Markdown target changed"),
            Self::RecoveryRequired(_) => {
                formatter.write_str("the file operation requires recovery")
            }
            Self::LockPoisoned => formatter.write_str("the file sync lock is unavailable"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<StoreError> for SyncError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

pub(crate) struct SyncCoordinator {
    gate: Mutex<()>,
    recovery_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub recovered_operations: usize,
    pub recovered_attachment_operations: usize,
    pub initialized_notebooks: usize,
    pub pending: Vec<RecoveryIssue>,
    pub pending_notebooks: Vec<NotebookRecoveryIssue>,
    pub pending_attachment_operations: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryIssue {
    pub operation_id: String,
    pub record_id: String,
    pub code: &'static str,
    pub attempt_count: u64,
    pub previous_error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookRecoveryIssue {
    pub notebook_id: String,
    pub code: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookDocument {
    pub notebook_id: String,
    pub markdown: String,
    pub file_sha256: String,
    pub receipt_generation: u64,
    pub line_ending: String,
    pub had_utf8_bom: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookConflictActionAvailability {
    pub available: bool,
    pub blocker_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookConflictRecordDiff {
    pub record_id: String,
    pub wakegpt_markdown: String,
    pub file_markdown: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookConflictInspection {
    pub notebook_id: String,
    pub reason_code: String,
    pub detected_at_ms: i64,
    pub conflict_token: String,
    pub file_markdown: String,
    pub wakegpt_markdown: String,
    pub record_diffs: Vec<NotebookConflictRecordDiff>,
    pub adopt_wakegpt: NotebookConflictActionAvailability,
    pub adopt_file: NotebookConflictActionAvailability,
    pub unbind: NotebookConflictActionAvailability,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NotebookConflictResolutionAction {
    AdoptWakegpt,
    AdoptFile,
    Unbind,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookNumberingPreview {
    pub notebook_id: String,
    pub current_numbering_style: NumberingStyle,
    pub current_numbering_start: u32,
    pub next_numbering_style: NumberingStyle,
    pub next_numbering_start: u32,
    pub markdown: String,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookAttachmentDirectoryPreview {
    pub notebook_id: String,
    pub current_attachment_directory: String,
    pub next_attachment_directory: String,
    pub attachment_count: usize,
    pub markdown: String,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct NotebookNumberingConfiguration {
    pub style: NumberingStyle,
    pub start: u32,
}

pub struct NotebookAttachmentDirectoryChange {
    pub directory: String,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
}

pub(crate) struct RecoveryState {
    report: Mutex<RecoveryReport>,
}

impl RecoveryState {
    pub fn new(report: RecoveryReport) -> Self {
        Self {
            report: Mutex::new(report),
        }
    }

    pub fn snapshot(&self) -> Result<RecoveryReport, SyncError> {
        self.report
            .lock()
            .map(|report| report.clone())
            .map_err(|_| SyncError::LockPoisoned)
    }

    pub fn replace(&self, report: RecoveryReport) -> Result<RecoveryReport, SyncError> {
        let mut current = self.report.lock().map_err(|_| SyncError::LockPoisoned)?;
        *current = report.clone();
        Ok(report)
    }
}

#[derive(Debug)]
pub(crate) enum CoordinatedMutationError {
    Storage(StoreError),
    Coordination(SyncError),
    Saved {
        error: SyncError,
        operation_id: Option<String>,
        entity_id: String,
    },
}

#[derive(Debug)]
pub(crate) enum DocumentSaveError {
    Unchanged(SyncError),
    Saved(SyncError),
}

impl SyncCoordinator {
    pub fn new(recovery_root: PathBuf) -> Result<Self, SyncError> {
        fs::create_dir_all(&recovery_root).map_err(|_| SyncError::Io("recovery_dir_create"))?;
        Ok(Self {
            gate: Mutex::new(()),
            recovery_root,
        })
    }

    #[cfg(test)]
    fn apply_record(&self, store: &Store, record: Record) -> Result<Record, SyncError> {
        let _guard = self.lock()?;
        self.apply_record_locked(store, record)
    }

    pub fn execute_record_mutation<F>(
        &self,
        store: &Store,
        mutation: F,
    ) -> Result<Record, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Record, StoreError>,
    {
        self.execute_record_mutation_with_prepare(store, mutation, |_, _| Ok(()))
    }

    pub fn execute_record_mutation_with_attachments<F>(
        &self,
        store: &Store,
        attachments: &crate::attachments::AttachmentCoordinator,
        mutation: F,
    ) -> Result<Record, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Record, StoreError>,
    {
        let _sync_guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        let _attachment_guard = attachments
            .lock()
            .map_err(|error| CoordinatedMutationError::Coordination(SyncError::Io(error.code())))?;
        self.execute_record_mutation_locked(store, mutation, |_, _| Ok(()))
    }

    pub fn execute_record_mutation_with_prepare<F, P>(
        &self,
        store: &Store,
        mutation: F,
        prepare: P,
    ) -> Result<Record, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Record, StoreError>,
        P: FnOnce(&Store, &Record) -> Result<(), SyncError>,
    {
        let _guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        self.execute_record_mutation_locked(store, mutation, prepare)
    }

    fn execute_record_mutation_locked<F, P>(
        &self,
        store: &Store,
        mutation: F,
        prepare: P,
    ) -> Result<Record, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Record, StoreError>,
        P: FnOnce(&Store, &Record) -> Result<(), SyncError>,
    {
        let record = mutation(store).map_err(CoordinatedMutationError::Storage)?;
        let record_id = record.id.clone();
        let operation_id = store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .map_err(|error| CoordinatedMutationError::Saved {
                error: SyncError::Store(error),
                operation_id: None,
                entity_id: record_id.clone(),
            })?
            .map(|operation| operation.id);
        prepare(store, &record).map_err(|error| CoordinatedMutationError::Saved {
            error,
            operation_id: operation_id.clone(),
            entity_id: record_id.clone(),
        })?;
        self.apply_record_locked(store, record)
            .map_err(|error| CoordinatedMutationError::Saved {
                error,
                operation_id,
                entity_id: record_id,
            })
    }

    pub fn execute_notebook_creation<F>(
        &self,
        store: &Store,
        workspace_id: &str,
        relative_path: &str,
        mutation: F,
    ) -> Result<Notebook, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Notebook, StoreError>,
    {
        let _guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        let workspace = store
            .get_workspace(workspace_id)
            .map_err(CoordinatedMutationError::Storage)?;
        let target = resolve_workspace_target(&workspace, relative_path)
            .map_err(CoordinatedMutationError::Coordination)?;
        let existing = read_target(&target).map_err(CoordinatedMutationError::Coordination)?;
        if existing.existed {
            return Err(CoordinatedMutationError::Coordination(SyncError::Conflict(
                "notebook_target_already_exists",
            )));
        }
        let notebook = mutation(store).map_err(CoordinatedMutationError::Storage)?;
        let entity_id = notebook.id.clone();
        self.initialize_notebook_target(store, &notebook)
            .map_err(|error| CoordinatedMutationError::Saved {
                error,
                operation_id: None,
                entity_id,
            })
    }

    pub fn execute_notebook_binding<F>(
        &self,
        store: &Store,
        workspace_id: &str,
        relative_path: &str,
        numbering_style: NumberingStyle,
        numbering_start: u32,
        mutation: F,
    ) -> Result<Notebook, CoordinatedMutationError>
    where
        F: FnOnce(&Store) -> Result<Notebook, StoreError>,
    {
        let _guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        let workspace = store
            .get_workspace(workspace_id)
            .map_err(CoordinatedMutationError::Storage)?;
        let target = resolve_workspace_target(&workspace, relative_path)
            .map_err(CoordinatedMutationError::Coordination)?;
        let existing = read_target(&target).map_err(CoordinatedMutationError::Coordination)?;
        if !existing.existed {
            return Err(CoordinatedMutationError::Coordination(SyncError::Conflict(
                "notebook_binding_target_missing",
            )));
        }
        let (had_bom, existing_text) =
            decode_markdown(&existing.bytes).map_err(CoordinatedMutationError::Coordination)?;
        let line_ending = preferred_line_ending(existing_text);
        synchronize_notebook_for_path(
            existing_text,
            &new_id(),
            &[],
            numbering_style,
            numbering_start,
            line_ending,
            Some(relative_path),
        )
        .map_err(|error| {
            CoordinatedMutationError::Coordination(SyncError::Managed(error.to_string()))
        })?;

        let notebook = mutation(store).map_err(CoordinatedMutationError::Storage)?;
        let entity_id = notebook.id.clone();
        let rendered = synchronize_notebook_for_path(
            existing_text,
            &notebook.target_id,
            &[],
            notebook.numbering_style,
            notebook.numbering_start,
            line_ending,
            Some(&notebook.relative_path),
        )
        .map_err(|error| CoordinatedMutationError::Saved {
            error: SyncError::Managed(error.to_string()),
            operation_id: None,
            entity_id: entity_id.clone(),
        })?;
        let after_bytes = encode_markdown(had_bom, &rendered).map_err(|error| {
            CoordinatedMutationError::Saved {
                error,
                operation_id: None,
                entity_id: entity_id.clone(),
            }
        })?;
        let stage_name = format!(".wakegpt-bind-{}.stage", notebook.id);
        let mut stage = target
            .root
            .create_exclusive_stage(
                &target.relative_path,
                std::ffi::OsStr::new(&stage_name),
                false,
            )
            .map_err(|error| CoordinatedMutationError::Saved {
                error: map_anchored_target_error(error),
                operation_id: None,
                entity_id: entity_id.clone(),
            })?;
        stage
            .file_mut()
            .write_all(&after_bytes)
            .map_err(|_| CoordinatedMutationError::Saved {
                error: SyncError::Io("notebook_binding_stage_write"),
                operation_id: None,
                entity_id: entity_id.clone(),
            })?;
        stage
            .replace_existing_cas(&existing.file_sha256, MAX_NOTEBOOK_BYTES)
            .map_err(|error| CoordinatedMutationError::Saved {
                error: map_document_replace_error(error),
                operation_id: None,
                entity_id: entity_id.clone(),
            })?;
        self.initialize_notebook_target(store, &notebook)
            .map_err(|error| CoordinatedMutationError::Saved {
                error,
                operation_id: None,
                entity_id,
            })
    }

    pub fn read_notebook_document(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<NotebookDocument, SyncError> {
        let _guard = self.lock()?;
        self.read_notebook_document_locked(store, workspace_id, notebook_id)
    }

    pub fn inspect_notebook_conflict(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<NotebookConflictInspection, SyncError> {
        let _guard = self.lock()?;
        let notebook = store.get_notebook(workspace_id, notebook_id)?;
        if notebook.target_state != NotebookTargetState::Conflict {
            return Err(SyncError::Conflict("notebook_has_no_conflict"));
        }
        let workspace = store.get_workspace(workspace_id)?;
        let target = resolve_notebook_target(&workspace, &notebook)?;
        let current = read_target(&target)?;
        if !current.existed {
            return Err(SyncError::Io("notebook_target_missing"));
        }
        let (_, markdown) = decode_markdown(&current.bytes)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let mut evidence = store
            .notebook_conflict(workspace_id, notebook_id)?
            .ok_or(SyncError::Conflict("notebook_conflict_evidence_missing"))?;
        if evidence.receipt_generation != receipt.generation
            || evidence.expected_managed_sha256 != receipt.managed_sha256
            || evidence.target_id != notebook.target_id
        {
            return Err(SyncError::Conflict("conflict_evidence_stale"));
        }
        if evidence.reason_code == "managed_region_changed"
            && current.file_sha256 != evidence.observed_file_sha256
        {
            let observed_managed = managed_region_sha256_unchecked(markdown, &notebook.target_id)
                .ok()
                .flatten();
            store.record_notebook_conflict(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &current.file_sha256,
                observed_managed.as_deref(),
                "managed_region_changed",
            )?;
            evidence = store
                .notebook_conflict(workspace_id, notebook_id)?
                .ok_or(SyncError::Conflict("notebook_conflict_evidence_missing"))?;
        }
        let records = store.list_records(workspace_id, Some(notebook_id), false)?;
        let wakegpt_markdown = overwrite_managed_region_for_path(
            markdown,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(markdown),
            &notebook.relative_path,
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        if evidence.reason_code == "file_version_adoption_pending"
            && current.file_sha256 != evidence.observed_file_sha256
            && markdown != wakegpt_markdown
        {
            refresh_notebook_conflict_evidence(store, &notebook, &receipt, &current)?;
            evidence = store
                .notebook_conflict(workspace_id, notebook_id)?
                .ok_or(SyncError::Conflict("notebook_conflict_evidence_missing"))?;
        }
        let adoptable = parse_adoptable_file_version(
            markdown,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            &notebook.relative_path,
        );
        let record_diffs = match &adoptable {
            Ok(version) => records
                .iter()
                .map(|record| {
                    match version
                        .records
                        .iter()
                        .find(|candidate| candidate.id == record.id)
                    {
                        Some(file) => NotebookConflictRecordDiff {
                            record_id: record.id.clone(),
                            wakegpt_markdown: record.body_markdown.clone(),
                            file_markdown: Some(file.body_markdown.clone()),
                            state: if record.body_markdown == file.body_markdown {
                                "unchanged".to_owned()
                            } else {
                                "modified".to_owned()
                            },
                        },
                        None => NotebookConflictRecordDiff {
                            record_id: record.id.clone(),
                            wakegpt_markdown: record.body_markdown.clone(),
                            file_markdown: None,
                            state: "invalid".to_owned(),
                        },
                    }
                })
                .collect(),
            Err(_) => records
                .iter()
                .map(|record| NotebookConflictRecordDiff {
                    record_id: record.id.clone(),
                    wakegpt_markdown: record.body_markdown.clone(),
                    file_markdown: None,
                    state: "invalid".to_owned(),
                })
                .collect(),
        };
        let file_adoption_pending = evidence.reason_code == "file_version_adoption_pending";
        let adopt_file_available = adoptable.is_ok() || file_adoption_pending;
        Ok(NotebookConflictInspection {
            notebook_id: notebook.id,
            reason_code: evidence.reason_code.clone(),
            detected_at_ms: evidence.created_at_ms,
            conflict_token: notebook_conflict_token(&evidence),
            file_markdown: markdown.to_owned(),
            wakegpt_markdown,
            record_diffs,
            adopt_wakegpt: NotebookConflictActionAvailability {
                available: !file_adoption_pending,
                blocker_code: file_adoption_pending
                    .then(|| "file_version_adoption_pending".to_owned()),
            },
            adopt_file: NotebookConflictActionAvailability {
                available: adopt_file_available,
                blocker_code: (!adopt_file_available)
                    .then(|| "file_version_not_adoptable".to_owned()),
            },
            unbind: NotebookConflictActionAvailability {
                available: true,
                blocker_code: None,
            },
        })
    }

    pub fn check_workspace_conflicts(
        &self,
        store: &Store,
        workspace_id: &str,
    ) -> Result<usize, SyncError> {
        let _guard = self.lock()?;
        let workspace = store.get_workspace(workspace_id)?;
        let notebooks = store.list_notebooks(workspace_id)?;
        let mut detected = 0_usize;
        for notebook in notebooks.into_iter().filter(|notebook| {
            notebook.target_state == NotebookTargetState::Ready
                && !notebook.numbering_sync_pending
                && !notebook.attachment_directory_sync_pending
        }) {
            let Some(receipt) = store.notebook_file_receipt(&notebook.id)? else {
                continue;
            };
            let target = match resolve_notebook_target(&workspace, &notebook) {
                Ok(target) => target,
                Err(_) => continue,
            };
            let current = match read_target(&target) {
                Ok(current) if current.existed => current,
                _ => continue,
            };
            let observed_managed =
                decode_markdown(&current.bytes)
                    .ok()
                    .and_then(|(_, markdown)| {
                        managed_region_sha256_unchecked(markdown, &notebook.target_id)
                            .ok()
                            .flatten()
                    });
            let is_current = decode_markdown(&current.bytes)
                .ok()
                .and_then(|(_, markdown)| managed_snapshot(markdown, &notebook.target_id).ok())
                .flatten()
                .is_some_and(|snapshot| snapshot.managed_sha256 == receipt.managed_sha256);
            if is_current {
                continue;
            }
            store.record_notebook_conflict(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &current.file_sha256,
                observed_managed.as_deref(),
                "managed_region_changed",
            )?;
            detected += 1;
        }
        Ok(detected)
    }

    pub fn resolve_notebook_conflict(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        conflict_token: &str,
        action: NotebookConflictResolutionAction,
    ) -> Result<Notebook, SyncError> {
        let _guard = self.lock()?;
        self.resolve_notebook_conflict_locked(
            store,
            workspace_id,
            notebook_id,
            conflict_token,
            action,
        )
    }

    fn resolve_notebook_conflict_locked(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        conflict_token: &str,
        action: NotebookConflictResolutionAction,
    ) -> Result<Notebook, SyncError> {
        let notebook = store.get_notebook(workspace_id, notebook_id)?;
        if notebook.target_state != NotebookTargetState::Conflict {
            return Err(SyncError::Conflict("notebook_has_no_conflict"));
        }
        let evidence = store
            .notebook_conflict(workspace_id, notebook_id)?
            .ok_or(SyncError::Conflict("notebook_conflict_evidence_missing"))?;
        if notebook_conflict_token(&evidence) != conflict_token {
            return Err(SyncError::Conflict("conflict_evidence_stale"));
        }
        let workspace = store.get_workspace(workspace_id)?;
        let target = resolve_notebook_target(&workspace, &notebook)?;
        let current = read_target(&target)?;
        if !current.existed {
            return Err(SyncError::Io("notebook_target_missing"));
        }
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        if evidence.receipt_generation != receipt.generation
            || evidence.expected_managed_sha256 != receipt.managed_sha256
        {
            return Err(SyncError::Conflict("conflict_evidence_stale"));
        }
        let (_, current_markdown) = decode_markdown(&current.bytes)?;
        if action == NotebookConflictResolutionAction::Unbind {
            if current.file_sha256 != evidence.observed_file_sha256 {
                refresh_notebook_conflict_evidence(store, &notebook, &receipt, &current)?;
                return Err(SyncError::Conflict("conflict_evidence_stale"));
            }
            return store
                .unbind_notebook_conflict(&notebook, &evidence)
                .map_err(SyncError::from);
        }

        let (evidence, records, mark_records_synced) = match action {
            NotebookConflictResolutionAction::AdoptWakegpt => {
                if evidence.reason_code != "managed_region_changed" {
                    return Err(SyncError::Conflict("file_version_adoption_pending"));
                }
                (
                    evidence,
                    store.list_records(workspace_id, Some(notebook_id), false)?,
                    false,
                )
            }
            NotebookConflictResolutionAction::AdoptFile => {
                if evidence.reason_code == "managed_region_changed" {
                    if current.file_sha256 != evidence.observed_file_sha256 {
                        refresh_notebook_conflict_evidence(store, &notebook, &receipt, &current)?;
                        return Err(SyncError::Conflict("conflict_evidence_stale"));
                    }
                    let records = store.list_records(workspace_id, Some(notebook_id), false)?;
                    let adopted = parse_adoptable_file_version(
                        current_markdown,
                        &notebook.target_id,
                        &records,
                        notebook.numbering_style,
                        notebook.numbering_start,
                        &notebook.relative_path,
                    )
                    .map_err(|_| SyncError::Conflict("file_version_not_adoptable"))?;
                    let updates = adopted
                        .records
                        .into_iter()
                        .map(|record| NotebookConflictRecordUpdate {
                            record_id: record.id,
                            expected_revision: record.expected_revision,
                            body_markdown: record.body_markdown,
                        })
                        .collect::<Vec<_>>();
                    let intent = store
                        .begin_notebook_file_version_adoption(&notebook, &evidence, &updates)?;
                    (
                        intent,
                        store.list_records(workspace_id, Some(notebook_id), false)?,
                        true,
                    )
                } else if evidence.reason_code == "file_version_adoption_pending" {
                    (
                        evidence,
                        store.list_records(workspace_id, Some(notebook_id), false)?,
                        true,
                    )
                } else {
                    return Err(SyncError::Conflict("conflict_resolution_state_invalid"));
                }
            }
            NotebookConflictResolutionAction::Unbind => unreachable!(),
        };
        let (had_bom, current_markdown) = decode_markdown(&current.bytes)?;
        let rendered = overwrite_managed_region_for_path(
            current_markdown,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(current_markdown),
            &notebook.relative_path,
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let desired_bytes = encode_markdown(had_bom, &rendered)?;
        let desired_file_sha256 = sha256_hex(&desired_bytes);
        let installed = if current.file_sha256 == desired_file_sha256 {
            current
        } else {
            if current.file_sha256 != evidence.observed_file_sha256 {
                refresh_notebook_conflict_evidence(store, &notebook, &receipt, &current)?;
                return Err(SyncError::Conflict("conflict_evidence_stale"));
            }
            let stage_name = format!(
                ".wakegpt-conflict-{}-{}.stage",
                notebook.id, evidence.receipt_generation
            );
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            let mut stage = target
                .root
                .create_exclusive_stage(
                    &target.relative_path,
                    std::ffi::OsStr::new(&stage_name),
                    false,
                )
                .map_err(map_anchored_target_error)?;
            stage
                .file_mut()
                .write_all(&desired_bytes)
                .map_err(|_| SyncError::Io("conflict_resolution_stage_write"))?;
            stage
                .replace_existing_cas(&current.file_sha256, MAX_NOTEBOOK_BYTES)
                .map_err(|error| {
                    if error.code() == AnchoredFsErrorCode::TargetIdentityChanged {
                        SyncError::Conflict("conflict_resolution_target_changed")
                    } else {
                        map_anchored_target_error(error)
                    }
                })?;
            let installed = read_target(&target)?;
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            installed
        };
        if !installed.existed || installed.file_sha256 != desired_file_sha256 {
            return Err(SyncError::RecoveryRequired(
                "conflict_resolution_install_mismatch",
            ));
        }
        let (_, installed_markdown) = decode_markdown(&installed.bytes)?;
        let installed_snapshot = managed_snapshot(installed_markdown, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::RecoveryRequired(
                "conflict_resolution_region_missing",
            ))?;
        let completed = store
            .complete_notebook_conflict_resolution(
                &notebook,
                &evidence,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: installed_snapshot.managed_sha256,
                    file_sha256: installed.file_sha256,
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
                mark_records_synced,
            )
            .map_err(|_| SyncError::RecoveryRequired("conflict_resolution_receipt_pending"))?;
        if !mark_records_synced {
            self.resume_pending_for_notebook_locked(store, notebook_id);
        }
        match store.get_notebook(workspace_id, notebook_id) {
            Ok(notebook) => Ok(notebook),
            Err(_) => Ok(completed),
        }
    }

    pub fn preview_notebook_numbering(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        next_style: NumberingStyle,
        next_start: u32,
    ) -> Result<NotebookNumberingPreview, SyncError> {
        let _guard = self.lock()?;
        self.preview_notebook_numbering_locked(
            store,
            workspace_id,
            notebook_id,
            next_style,
            next_start,
        )
    }

    pub fn change_notebook_numbering(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        next: NotebookNumberingConfiguration,
        expected_file_sha256: &str,
        expected_receipt_generation: u64,
    ) -> Result<Notebook, CoordinatedMutationError> {
        let _guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        let preview = self
            .preview_notebook_numbering_locked(
                store,
                workspace_id,
                notebook_id,
                next.style,
                next.start,
            )
            .map_err(CoordinatedMutationError::Coordination)?;
        if preview.expected_file_sha256 != expected_file_sha256
            || preview.expected_receipt_generation != expected_receipt_generation
        {
            return Err(CoordinatedMutationError::Coordination(SyncError::Conflict(
                "notebook_numbering_preview_stale",
            )));
        }
        let current = store
            .get_notebook(workspace_id, notebook_id)
            .map_err(CoordinatedMutationError::Storage)?;
        if current.numbering_style == next.style
            && current.numbering_start == next.start
            && !current.numbering_sync_pending
        {
            return Ok(current);
        }
        let queued = store
            .queue_notebook_numbering_configuration(
                &current,
                next.style,
                next.start,
                expected_receipt_generation,
            )
            .map_err(CoordinatedMutationError::Storage)?;
        match self.apply_pending_notebook_numbering_locked(store, &queued) {
            Ok(notebook) => Ok(notebook),
            Err(error)
                if matches!(
                    error,
                    SyncError::Conflict("notebook_numbering_changed_during_apply")
                ) =>
            {
                match store.cancel_notebook_numbering_configuration(
                    &queued,
                    current.numbering_style,
                    current.numbering_start,
                ) {
                    Ok(_) => Err(CoordinatedMutationError::Coordination(error)),
                    Err(cancel_error) => Err(CoordinatedMutationError::Saved {
                        error: SyncError::Store(cancel_error),
                        operation_id: None,
                        entity_id: queued.id,
                    }),
                }
            }
            Err(error) => Err(CoordinatedMutationError::Saved {
                error,
                operation_id: None,
                entity_id: queued.id,
            }),
        }
    }

    pub fn preview_notebook_attachment_directory(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        next_directory: &str,
    ) -> Result<NotebookAttachmentDirectoryPreview, SyncError> {
        let _guard = self.lock()?;
        self.preview_notebook_attachment_directory_locked(
            store,
            workspace_id,
            notebook_id,
            next_directory,
        )
    }

    pub fn change_notebook_attachment_directory<P>(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        change: NotebookAttachmentDirectoryChange,
        prepare: P,
    ) -> Result<Notebook, CoordinatedMutationError>
    where
        P: FnOnce(&Store, &Notebook) -> Result<(), SyncError>,
    {
        let _guard = self
            .lock()
            .map_err(CoordinatedMutationError::Coordination)?;
        let preview = self
            .preview_notebook_attachment_directory_locked(
                store,
                workspace_id,
                notebook_id,
                &change.directory,
            )
            .map_err(CoordinatedMutationError::Coordination)?;
        if preview.expected_file_sha256 != change.expected_file_sha256
            || preview.expected_receipt_generation != change.expected_receipt_generation
        {
            return Err(CoordinatedMutationError::Coordination(SyncError::Conflict(
                "notebook_attachment_directory_preview_stale",
            )));
        }
        let current = store
            .get_notebook(workspace_id, notebook_id)
            .map_err(CoordinatedMutationError::Storage)?;
        if current.attachment_directory == preview.next_attachment_directory
            && !current.attachment_directory_sync_pending
        {
            return Ok(current);
        }
        let queued = store
            .queue_notebook_attachment_directory(
                &current,
                &preview.next_attachment_directory,
                change.expected_receipt_generation,
            )
            .map_err(CoordinatedMutationError::Storage)?;
        prepare(store, &queued).map_err(|error| CoordinatedMutationError::Saved {
            error,
            operation_id: None,
            entity_id: queued.id.clone(),
        })?;
        self.apply_pending_notebook_attachment_directory_locked(store, &queued)
            .map_err(|error| CoordinatedMutationError::Saved {
                error,
                operation_id: None,
                entity_id: queued.id,
            })
    }

    pub fn rebind_notebook(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, SyncError> {
        let _guard = self.lock()?;
        let notebook = store.get_notebook(workspace_id, notebook_id)?;
        if notebook.target_state != NotebookTargetState::Unbound {
            return Ok(notebook);
        }
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, current) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_rebind_receipt_missing"))?;
        let (_, markdown) = decode_markdown(&current.bytes)?;
        let snapshot = managed_snapshot(markdown, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("notebook_rebind_marker_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("notebook_rebind_managed_changed"));
        }
        store.update_notebook_document_receipt(
            &notebook,
            receipt.generation,
            &receipt.managed_sha256,
            &NotebookFileReceiptInput {
                notebook_id: notebook.id.clone(),
                target_id: notebook.target_id.clone(),
                marker_schema: MANAGED_SCHEMA_VERSION,
                managed_sha256: snapshot.managed_sha256,
                file_sha256: current.file_sha256,
                file_modified_ns: current.modified_ns,
                file_size_bytes: current.bytes.len() as u64,
            },
        )?;
        store
            .get_notebook(workspace_id, notebook_id)
            .map_err(Into::into)
    }

    pub fn recover_notebook_target(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, SyncError> {
        let _guard = self.lock()?;
        let notebook = store.get_notebook(workspace_id, notebook_id)?;
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, _) = resolve_existing_notebook_target(store, &workspace, &notebook)?;
        Ok(notebook)
    }

    pub fn convert_notebook_to_plain(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, SyncError> {
        let _guard = self.lock()?;
        let notebook = store.notebook_lifecycle_available(workspace_id, notebook_id)?;
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, target, before) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_bom, before_text) = decode_markdown(&before.bytes)?;
        let snapshot = managed_snapshot(before_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("managed_region_changed"));
        }
        let detached = detach_notebook_controls(before_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?;
        let after_bytes = encode_markdown(had_bom, &detached)?;
        let after_sha256 = sha256_hex(&after_bytes);
        let stage_name = format!(".wakegpt-detach-{}.stage", new_id());
        let mut stage = target
            .root
            .create_exclusive_stage(
                &target.relative_path,
                std::ffi::OsStr::new(&stage_name),
                false,
            )
            .map_err(map_anchored_target_error)?;
        stage
            .file_mut()
            .write_all(&after_bytes)
            .map_err(|_| SyncError::Io("notebook_detach_stage_write"))?;
        stage
            .replace_existing_cas(&before.file_sha256, MAX_NOTEBOOK_BYTES)
            .map_err(map_document_replace_error)?;
        let installed = read_target(&target)?;
        if !installed.existed || installed.file_sha256 != after_sha256 {
            return Err(SyncError::RecoveryRequired(
                "notebook_detach_install_mismatch",
            ));
        }
        let (_, installed_text) = decode_markdown(&installed.bytes)?;
        if managed_snapshot(installed_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .is_some()
        {
            return Err(SyncError::RecoveryRequired(
                "notebook_detach_markers_remain",
            ));
        }
        store
            .mark_notebook_target_unavailable(&notebook, "notebook_converted_to_plain")
            .map_err(|_| SyncError::RecoveryRequired("notebook_detach_state_update_failed"))
    }

    pub fn trash_notebook_file(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<(Notebook, PathBuf), SyncError> {
        let _guard = self.lock()?;
        let notebook = store.notebook_lifecycle_available(workspace_id, notebook_id)?;
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, current) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (_, markdown) = decode_markdown(&current.bytes)?;
        let snapshot = managed_snapshot(markdown, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("managed_region_changed"));
        }
        let absolute = Path::new(&workspace.root_path).join(&notebook.relative_path);
        verify_absolute_notebook(&absolute, &current.file_sha256)?;
        let trashed = platform_trash::move_to_system_trash(&absolute)
            .map_err(|error| SyncError::Io(error.code()))?;
        let updated = store
            .mark_notebook_target_unavailable(&notebook, "notebook_moved_to_trash")
            .map_err(|_| SyncError::RecoveryRequired("notebook_trash_state_update_failed"))?;
        Ok((updated, trashed))
    }

    pub fn save_notebook_document(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        expected_file_sha256: &str,
        expected_receipt_generation: u64,
        markdown: &str,
    ) -> Result<NotebookDocument, DocumentSaveError> {
        if markdown.len() as u64 > MAX_NOTEBOOK_BYTES {
            return Err(DocumentSaveError::Unchanged(SyncError::Conflict(
                "notebook_document_too_large",
            )));
        }
        let _guard = self.lock().map_err(DocumentSaveError::Unchanged)?;
        let notebook = store
            .get_notebook(workspace_id, notebook_id)
            .map_err(|error| DocumentSaveError::Unchanged(SyncError::Store(error)))?;
        let workspace = store
            .get_workspace(workspace_id)
            .map_err(|error| DocumentSaveError::Unchanged(SyncError::Store(error)))?;
        let (notebook, target, before) =
            resolve_existing_notebook_target(store, &workspace, &notebook)
                .map_err(DocumentSaveError::Unchanged)?;
        if !before.existed || before.file_sha256 != expected_file_sha256 {
            return Err(DocumentSaveError::Unchanged(SyncError::Conflict(
                "notebook_document_changed",
            )));
        }
        let receipt = store
            .notebook_file_receipt(notebook_id)
            .map_err(|error| DocumentSaveError::Unchanged(SyncError::Store(error)))?
            .ok_or(DocumentSaveError::Unchanged(SyncError::Conflict(
                "notebook_receipt_missing",
            )))?;
        if receipt.generation != expected_receipt_generation {
            return Err(DocumentSaveError::Unchanged(SyncError::Conflict(
                "notebook_receipt_changed",
            )));
        }
        let (had_bom, before_text) =
            decode_markdown(&before.bytes).map_err(DocumentSaveError::Unchanged)?;
        let before_snapshot = managed_snapshot(before_text, &notebook.target_id)
            .map_err(|error| DocumentSaveError::Unchanged(SyncError::Managed(error.to_string())))?
            .ok_or(DocumentSaveError::Unchanged(SyncError::Conflict(
                "managed_region_missing",
            )))?;
        if before_snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(DocumentSaveError::Unchanged(SyncError::Conflict(
                "managed_region_changed",
            )));
        }
        let after_snapshot = managed_snapshot(markdown, &notebook.target_id)
            .map_err(|error| DocumentSaveError::Unchanged(SyncError::Managed(error.to_string())))?
            .ok_or(DocumentSaveError::Unchanged(SyncError::Conflict(
                "managed_region_missing",
            )))?;
        if after_snapshot.managed_sha256 != before_snapshot.managed_sha256 {
            return Err(DocumentSaveError::Unchanged(SyncError::Conflict(
                "managed_region_edit_not_allowed",
            )));
        }
        let after_bytes =
            encode_markdown(had_bom, markdown).map_err(DocumentSaveError::Unchanged)?;
        if after_bytes == before.bytes {
            return self
                .read_notebook_document_locked(store, workspace_id, notebook_id)
                .map_err(DocumentSaveError::Unchanged);
        }
        let stage_name = format!(".wakegpt-document-{}.stage", new_id());
        let mut stage = target
            .root
            .create_exclusive_stage(
                &target.relative_path,
                std::ffi::OsStr::new(&stage_name),
                false,
            )
            .map_err(map_anchored_target_error)
            .map_err(DocumentSaveError::Unchanged)?;
        stage.file_mut().write_all(&after_bytes).map_err(|_| {
            DocumentSaveError::Unchanged(SyncError::Io("notebook_document_stage_write"))
        })?;
        stage
            .replace_existing_cas(&before.file_sha256, MAX_NOTEBOOK_BYTES)
            .map_err(map_document_replace_error)
            .map_err(DocumentSaveError::Unchanged)?;
        let installed = read_target(&target).map_err(DocumentSaveError::Saved)?;
        let (_, installed_text) =
            decode_markdown(&installed.bytes).map_err(DocumentSaveError::Saved)?;
        let installed_snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .map_err(|error| DocumentSaveError::Saved(SyncError::Managed(error.to_string())))?
            .ok_or(DocumentSaveError::Saved(SyncError::RecoveryRequired(
                "managed_region_missing_after_save",
            )))?;
        if installed_snapshot.managed_sha256 != before_snapshot.managed_sha256 {
            return Err(DocumentSaveError::Saved(SyncError::RecoveryRequired(
                "managed_region_changed_after_save",
            )));
        }
        store
            .update_notebook_document_receipt(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: installed_snapshot.managed_sha256,
                    file_sha256: installed.file_sha256.clone(),
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
            )
            .map_err(|error| DocumentSaveError::Saved(SyncError::Store(error)))?;
        self.read_notebook_document_locked(store, workspace_id, notebook_id)
            .map_err(DocumentSaveError::Saved)
    }

    fn preview_notebook_numbering_locked(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        next_style: NumberingStyle,
        next_start: u32,
    ) -> Result<NotebookNumberingPreview, SyncError> {
        let notebook = store.notebook_lifecycle_available(workspace_id, notebook_id)?;
        if notebook.target_state != NotebookTargetState::Ready {
            return Err(SyncError::Conflict(
                "notebook_numbering_requires_ready_target",
            ));
        }
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, current) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_bom, existing) = decode_markdown(&current.bytes)?;
        let snapshot = managed_snapshot(existing, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("managed_region_changed"));
        }
        let records = store.list_records(workspace_id, Some(notebook_id), false)?;
        let rendered = synchronize_notebook_for_path(
            existing,
            &notebook.target_id,
            &records,
            next_style,
            next_start,
            preferred_line_ending(existing),
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let rendered_bytes = encode_markdown(had_bom, &rendered)?;
        let (_, rendered_text) = decode_markdown(&rendered_bytes)?;
        Ok(NotebookNumberingPreview {
            notebook_id: notebook.id,
            current_numbering_style: notebook.numbering_style,
            current_numbering_start: notebook.numbering_start,
            next_numbering_style: next_style,
            next_numbering_start: next_start,
            markdown: rendered_text.to_owned(),
            expected_file_sha256: current.file_sha256,
            expected_receipt_generation: receipt.generation,
        })
    }

    fn apply_pending_notebook_numbering_locked(
        &self,
        store: &Store,
        notebook: &Notebook,
    ) -> Result<Notebook, SyncError> {
        if !notebook.numbering_sync_pending {
            return Ok(notebook.clone());
        }
        if notebook.target_state != NotebookTargetState::Ready {
            return Err(SyncError::Conflict(
                "notebook_numbering_requires_ready_target",
            ));
        }
        let workspace = store.get_workspace(&notebook.workspace_id)?;
        let (notebook, target, current) =
            resolve_existing_notebook_target(store, &workspace, notebook)?;
        let receipt = store
            .notebook_file_receipt(&notebook.id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_bom, existing) = decode_markdown(&current.bytes)?;
        let current_snapshot = managed_snapshot(existing, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        let records = store.list_records(&notebook.workspace_id, Some(&notebook.id), false)?;
        let rendered = synchronize_notebook_for_path(
            existing,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(existing),
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let desired_snapshot = managed_snapshot(&rendered, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_not_rendered"))?;
        if current_snapshot.managed_sha256 != receipt.managed_sha256
            && current_snapshot.managed_sha256 != desired_snapshot.managed_sha256
        {
            return Err(SyncError::Conflict("notebook_numbering_managed_changed"));
        }

        let after_bytes = encode_markdown(had_bom, &rendered)?;
        let installed = if after_bytes == current.bytes {
            current
        } else {
            let stage_name = format!(
                ".wakegpt-numbering-{}-{}.stage",
                notebook.id, receipt.generation
            );
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            let mut stage = target
                .root
                .create_exclusive_stage(
                    &target.relative_path,
                    std::ffi::OsStr::new(&stage_name),
                    false,
                )
                .map_err(map_anchored_target_error)?;
            stage
                .file_mut()
                .write_all(&after_bytes)
                .map_err(|_| SyncError::Io("notebook_numbering_stage_write"))?;
            stage
                .replace_existing_cas(&current.file_sha256, MAX_NOTEBOOK_BYTES)
                .map_err(map_numbering_replace_error)?;
            let installed = read_target(&target)?;
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            installed
        };
        let (_, installed_text) = decode_markdown(&installed.bytes)?;
        let installed_snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::RecoveryRequired(
                "notebook_numbering_region_missing_after_apply",
            ))?;
        if installed_snapshot.managed_sha256 != desired_snapshot.managed_sha256 {
            return Err(SyncError::RecoveryRequired(
                "notebook_numbering_install_mismatch",
            ));
        }
        store
            .complete_notebook_numbering_sync(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: installed_snapshot.managed_sha256,
                    file_sha256: installed.file_sha256,
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
            )
            .map_err(SyncError::Store)
    }

    fn preview_notebook_attachment_directory_locked(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
        next_directory: &str,
    ) -> Result<NotebookAttachmentDirectoryPreview, SyncError> {
        let next_directory = validate_attachment_directory(next_directory)
            .map_err(|_| SyncError::Conflict("attachment_directory_invalid"))?;
        let notebook = store.notebook_lifecycle_available(workspace_id, notebook_id)?;
        if notebook.target_state != NotebookTargetState::Ready
            || notebook.numbering_sync_pending
            || notebook.attachment_directory_sync_pending
        {
            return Err(SyncError::Conflict(
                "notebook_attachment_directory_requires_ready_target",
            ));
        }
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, current) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_bom, existing) = decode_markdown(&current.bytes)?;
        let snapshot = managed_snapshot(existing, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("managed_region_changed"));
        }
        let mut records = store.list_records(workspace_id, Some(notebook_id), false)?;
        let mut attachment_count = 0_usize;
        for record in &mut records {
            for attachment in &mut record.attachments {
                attachment.managed_relative_path = managed_attachment_relative_path(
                    &next_directory,
                    &attachment.content_sha256,
                    &attachment.media_type,
                )
                .map_err(|_| SyncError::Conflict("attachment_directory_invalid"))?;
                attachment_count += 1;
            }
        }
        let rendered = synchronize_notebook_for_path(
            existing,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(existing),
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let rendered_bytes = encode_markdown(had_bom, &rendered)?;
        let (_, rendered_text) = decode_markdown(&rendered_bytes)?;
        Ok(NotebookAttachmentDirectoryPreview {
            notebook_id: notebook.id,
            current_attachment_directory: notebook.attachment_directory,
            next_attachment_directory: next_directory,
            attachment_count,
            markdown: rendered_text.to_owned(),
            expected_file_sha256: current.file_sha256,
            expected_receipt_generation: receipt.generation,
        })
    }

    fn apply_pending_notebook_attachment_directory_locked(
        &self,
        store: &Store,
        notebook: &Notebook,
    ) -> Result<Notebook, SyncError> {
        if !notebook.attachment_directory_sync_pending {
            return Ok(notebook.clone());
        }
        if notebook.target_state != NotebookTargetState::Ready {
            return Err(SyncError::Conflict(
                "notebook_attachment_directory_requires_ready_target",
            ));
        }
        if store.notebook_has_unprepared_attachment_relocations(&notebook.id)? {
            return Err(SyncError::RecoveryRequired(
                "attachment_relocation_not_prepared",
            ));
        }
        let workspace = store.get_workspace(&notebook.workspace_id)?;
        let (notebook, target, current) =
            resolve_existing_notebook_target(store, &workspace, notebook)?;
        let receipt = store
            .notebook_file_receipt(&notebook.id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_bom, existing) = decode_markdown(&current.bytes)?;
        let current_snapshot = managed_snapshot(existing, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        let records = store.list_records(&notebook.workspace_id, Some(&notebook.id), false)?;
        let rendered = synchronize_notebook_for_path(
            existing,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(existing),
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let desired_snapshot = managed_snapshot(&rendered, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_not_rendered"))?;
        if current_snapshot.managed_sha256 != receipt.managed_sha256
            && current_snapshot.managed_sha256 != desired_snapshot.managed_sha256
        {
            return Err(SyncError::Conflict(
                "notebook_attachment_directory_managed_changed",
            ));
        }

        let after_bytes = encode_markdown(had_bom, &rendered)?;
        let installed = if after_bytes == current.bytes {
            current
        } else {
            let stage_name = format!(
                ".wakegpt-attachments-{}-{}.stage",
                notebook.id, receipt.generation
            );
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            let mut stage = target
                .root
                .create_exclusive_stage(
                    &target.relative_path,
                    std::ffi::OsStr::new(&stage_name),
                    false,
                )
                .map_err(map_anchored_target_error)?;
            stage
                .file_mut()
                .write_all(&after_bytes)
                .map_err(|_| SyncError::Io("notebook_attachment_directory_stage_write"))?;
            stage
                .replace_existing_cas(&current.file_sha256, MAX_NOTEBOOK_BYTES)
                .map_err(map_attachment_directory_replace_error)?;
            let installed = read_target(&target)?;
            let _ = target
                .root
                .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
            installed
        };
        let (_, installed_text) = decode_markdown(&installed.bytes)?;
        let installed_snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::RecoveryRequired(
                "notebook_attachment_directory_region_missing_after_apply",
            ))?;
        if installed_snapshot.managed_sha256 != desired_snapshot.managed_sha256 {
            return Err(SyncError::RecoveryRequired(
                "notebook_attachment_directory_install_mismatch",
            ));
        }
        store
            .complete_notebook_attachment_directory_sync(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: installed_snapshot.managed_sha256,
                    file_sha256: installed.file_sha256,
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
            )
            .map_err(SyncError::Store)
    }

    fn read_notebook_document_locked(
        &self,
        store: &Store,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<NotebookDocument, SyncError> {
        let notebook = store.get_notebook(workspace_id, notebook_id)?;
        let workspace = store.get_workspace(workspace_id)?;
        let (notebook, _, current) =
            resolve_existing_notebook_target(store, &workspace, &notebook)?;
        let receipt = store
            .notebook_file_receipt(notebook_id)?
            .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
        let (had_utf8_bom, markdown) = decode_markdown(&current.bytes)?;
        let snapshot = managed_snapshot(markdown, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_missing"))?;
        if snapshot.managed_sha256 != receipt.managed_sha256 {
            return Err(SyncError::Conflict("managed_region_changed"));
        }
        Ok(NotebookDocument {
            notebook_id: notebook.id,
            markdown: markdown.to_owned(),
            file_sha256: current.file_sha256,
            receipt_generation: receipt.generation,
            line_ending: preferred_line_ending(markdown).to_owned(),
            had_utf8_bom,
        })
    }

    fn initialize_notebook_target(
        &self,
        store: &Store,
        notebook: &Notebook,
    ) -> Result<Notebook, SyncError> {
        let workspace = store.get_workspace(&notebook.workspace_id)?;
        let target = resolve_notebook_target(&workspace, notebook)?;
        let rendered = synchronize_notebook_for_path(
            "",
            &notebook.target_id,
            &[],
            notebook.numbering_style,
            notebook.numbering_start,
            "\n",
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let expected_bytes = rendered.as_bytes();
        let expected_sha256 = sha256_hex(expected_bytes);
        let current = read_target(&target)?;
        let stage_name = format!(".wakegpt-notebook-{}.stage", notebook.id);
        if current.existed {
            let (_, current_text) = decode_markdown(&current.bytes)?;
            let snapshot = managed_snapshot(current_text, &notebook.target_id)
                .map_err(|error| SyncError::Managed(error.to_string()))?;
            if snapshot
                .as_ref()
                .is_none_or(|snapshot| !snapshot.records.is_empty())
            {
                return Err(SyncError::Conflict("notebook_target_already_exists"));
            }
        } else {
            write_or_reuse_stage(&target, &stage_name, expected_bytes)?;
            let stage = open_verified_stage(&target, &stage_name, &expected_sha256)?;
            stage.install_new().map_err(map_anchored_recovery_error)?;
        }
        let installed = read_target(&target)?;
        if !installed.existed {
            return Err(SyncError::RecoveryRequired(
                "notebook_initialization_digest_mismatch",
            ));
        }
        let (_, installed_text) = decode_markdown(&installed.bytes)?;
        let snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::RecoveryRequired(
                "notebook_initialization_marker_missing",
            ))?;
        let initialized = store.initialize_notebook_file_receipt(&NotebookFileReceiptInput {
            notebook_id: notebook.id.clone(),
            target_id: notebook.target_id.clone(),
            marker_schema: MANAGED_SCHEMA_VERSION,
            managed_sha256: snapshot.managed_sha256,
            file_sha256: installed.file_sha256,
            file_modified_ns: installed.modified_ns,
            file_size_bytes: installed.bytes.len() as u64,
        })?;
        let _ = target
            .root
            .remove_stage_if_present(&target.relative_path, std::ffi::OsStr::new(&stage_name));
        Ok(initialized)
    }

    pub fn replay_pending(&self, store: &Store) -> Result<RecoveryReport, SyncError> {
        let _guard = self.lock()?;
        let mut report = RecoveryReport {
            recovered_operations: 0,
            recovered_attachment_operations: 0,
            initialized_notebooks: 0,
            pending: Vec::new(),
            pending_notebooks: Vec::new(),
            pending_attachment_operations: 0,
        };
        for evidence in store.pending_file_version_adoptions()? {
            let token = notebook_conflict_token(&evidence);
            match self.resolve_notebook_conflict_locked(
                store,
                &evidence.workspace_id,
                &evidence.notebook_id,
                &token,
                NotebookConflictResolutionAction::AdoptFile,
            ) {
                Ok(_) => report.recovered_operations += 1,
                Err(error) => report.pending_notebooks.push(NotebookRecoveryIssue {
                    notebook_id: evidence.notebook_id,
                    code: error.code(),
                }),
            }
        }
        let operations = store.pending_recovery_operations()?;
        for operation in operations {
            match self.resume_operation_locked(store, &operation) {
                Ok(_) => report.recovered_operations += 1,
                Err(error) => {
                    let _ = store.mark_recovery_needed(&operation.id, error.code());
                    report.pending.push(RecoveryIssue {
                        operation_id: operation.id,
                        record_id: operation.record_id,
                        code: error.code(),
                        attempt_count: operation.attempt_count,
                        previous_error_code: operation.last_error_code,
                    });
                }
            }
        }
        for notebook in store.pending_numbering_notebooks()? {
            match self.apply_pending_notebook_numbering_locked(store, &notebook) {
                Ok(_) => report.recovered_operations += 1,
                Err(error) => report.pending_notebooks.push(NotebookRecoveryIssue {
                    notebook_id: notebook.id,
                    code: error.code(),
                }),
            }
        }
        for notebook in store.pending_attachment_directory_notebooks()? {
            match self.apply_pending_notebook_attachment_directory_locked(store, &notebook) {
                Ok(_) => report.recovered_operations += 1,
                Err(error) => report.pending_notebooks.push(NotebookRecoveryIssue {
                    notebook_id: notebook.id,
                    code: error.code(),
                }),
            }
        }
        for notebook in store.unverified_notebooks()? {
            match self.initialize_notebook_target(store, &notebook) {
                Ok(_) => report.initialized_notebooks += 1,
                Err(error) => report.pending_notebooks.push(NotebookRecoveryIssue {
                    notebook_id: notebook.id,
                    code: error.code(),
                }),
            }
        }
        Ok(report)
    }

    fn apply_record_locked(&self, store: &Store, record: Record) -> Result<Record, SyncError> {
        let Some(operation) =
            store.recovery_operation_for_record_revision(&record.id, record.revision)?
        else {
            return Ok(record);
        };
        self.resume_operation_locked(store, &operation)
    }

    fn resume_pending_for_notebook_locked(&self, store: &Store, notebook_id: &str) {
        let Ok(operations) = store.pending_recovery_operations() else {
            return;
        };
        for operation in operations {
            if !affected_notebooks(&operation)
                .iter()
                .any(|candidate| candidate == notebook_id)
            {
                continue;
            }
            if let Err(error) = self.resume_operation_locked(store, &operation) {
                let _ = store.mark_recovery_needed(&operation.id, error.code());
            }
        }
    }

    fn resume_operation_locked(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
    ) -> Result<Record, SyncError> {
        if matches!(operation.phase.as_str(), "completed" | "superseded") {
            return store
                .get_record(&operation.workspace_id, &operation.record_id)
                .map_err(SyncError::from);
        }
        self.validate_operation_record(store, operation)?;
        let workspace = store.get_workspace(&operation.workspace_id)?;
        let needs_fresh_staging = operation.phase == "queued"
            || (operation.phase == "needs_recovery"
                && store.recovery_file_steps(&operation.id)?.is_empty());
        let prepared = if needs_fresh_staging {
            let prepared = self.prepare_operation(store, operation, &workspace)?;
            let steps = prepared
                .iter()
                .map(PreparedTarget::recovery_step)
                .collect::<Vec<_>>();
            store.stage_recovery_files(&operation.id, &steps)?;
            prepared
        } else {
            self.restore_prepared_targets(store, operation, &workspace)?
        };

        match operation.phase.as_str() {
            "queued" | "staged" | "needs_recovery" => {
                store.mark_recovery_applying(&operation.id)?;
            }
            "applying" | "files_applied" => {}
            _ => return Err(SyncError::RecoveryRequired("operation_phase_invalid")),
        }

        if operation.phase != "files_applied" {
            for target in &prepared {
                if let Err(error) = target.reconcile_install() {
                    let _ = store.mark_recovery_needed(&operation.id, error.code());
                    return Err(error);
                }
                if let Err(error) =
                    store.mark_recovery_step_applied(&operation.id, target.step_index)
                {
                    let _ = store.mark_recovery_needed(&operation.id, "step_receipt_failed");
                    return Err(SyncError::RecoveryRequired(match error {
                        StoreError::Sqlite(_) => "step_receipt_failed",
                        _ => "step_state_failed",
                    }));
                }
            }
            if let Err(error) = store.mark_recovery_files_applied(&operation.id) {
                let _ = store.mark_recovery_needed(&operation.id, "files_applied_receipt_failed");
                return Err(SyncError::Store(error));
            }
        }

        let mut receipts = Vec::with_capacity(prepared.len());
        for target in &prepared {
            match target.verify_installed() {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => {
                    let _ = store.mark_recovery_needed(&operation.id, error.code());
                    return Err(SyncError::RecoveryRequired("installed_verification_failed"));
                }
            }
        }
        let completed = match store.complete_recovery_operation(operation, &receipts) {
            Ok(record) => record,
            Err(error) => {
                let _ = store.mark_recovery_needed(&operation.id, "operation_complete_failed");
                return Err(SyncError::Store(error));
            }
        };
        self.cleanup_completed(operation, &prepared);
        Ok(completed)
    }

    fn validate_operation_record(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
    ) -> Result<(), SyncError> {
        if store.record_has_unprepared_attachment_relocations(&operation.record_id)? {
            return Err(SyncError::RecoveryRequired(
                "attachment_relocation_not_prepared",
            ));
        }
        let record = store.get_record(&operation.workspace_id, &operation.record_id)?;
        if record.revision != operation.desired_revision
            || record.applied_revision != operation.expected_revision
            || record.state != operation.desired_record_state
        {
            return Err(SyncError::RecoveryRequired(
                "operation_record_state_changed",
            ));
        }
        if record.notebook_id != operation.destination_notebook_id {
            return Err(SyncError::RecoveryRequired(
                "operation_record_target_changed",
            ));
        }
        if operation.destination_logical_order.is_some()
            && Some(record.logical_order) != operation.destination_logical_order
        {
            return Err(SyncError::RecoveryRequired(
                "operation_record_order_changed",
            ));
        }
        if operation.source_notebook_id.is_some() != operation.source_logical_order.is_some() {
            return Err(SyncError::RecoveryRequired(
                "operation_source_order_invalid",
            ));
        }
        Ok(())
    }

    fn prepare_operation(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
        workspace: &Workspace,
    ) -> Result<Vec<PreparedTarget>, SyncError> {
        let notebook_ids = affected_notebooks(operation);
        if notebook_ids.is_empty() {
            return Err(SyncError::Conflict("operation_has_no_file_target"));
        }
        let mut prepared = Vec::with_capacity(notebook_ids.len());
        for (index, notebook_id) in notebook_ids.iter().enumerate() {
            let notebook = store.get_notebook(&operation.workspace_id, notebook_id)?;
            let records = store.list_records(&operation.workspace_id, Some(notebook_id), false)?;
            match self.prepare_target(store, operation, workspace, &notebook, &records, index) {
                Ok(target) => prepared.push(target),
                Err(error) => {
                    let _ = store.mark_recovery_needed(&operation.id, error.code());
                    return Err(error);
                }
            }
        }
        Ok(prepared)
    }

    fn restore_prepared_targets(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
        workspace: &Workspace,
    ) -> Result<Vec<PreparedTarget>, SyncError> {
        let steps = store.recovery_file_steps(&operation.id)?;
        if steps.is_empty() || steps.len() > 2 {
            return Err(SyncError::RecoveryRequired("recovery_steps_invalid"));
        }
        steps
            .into_iter()
            .enumerate()
            .map(|(expected_index, step)| {
                self.restore_prepared_target(store, operation, workspace, step, expected_index)
            })
            .collect()
    }

    fn restore_prepared_target(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
        workspace: &Workspace,
        step: RecoveryFileStep,
        expected_index: usize,
    ) -> Result<PreparedTarget, SyncError> {
        if step.operation_id != operation.id || step.step_index != expected_index {
            return Err(SyncError::RecoveryRequired("recovery_step_order_invalid"));
        }
        if !matches!(step.state.as_str(), "staged" | "applied" | "verified") {
            return Err(SyncError::RecoveryRequired("recovery_step_state_invalid"));
        }
        let notebook = store.get_notebook(&operation.workspace_id, &step.notebook_id)?;
        if notebook.relative_path != step.target_workspace_relative_path
            || notebook.target_id != step.expected_target_id
        {
            return Err(SyncError::RecoveryRequired("recovery_step_target_changed"));
        }
        let expected_stage_name = format!(".wakegpt-{}-{expected_index}.stage", operation.id);
        if step.staged_sibling_name != expected_stage_name {
            return Err(SyncError::RecoveryRequired("recovery_stage_name_invalid"));
        }
        let expected_backup = PathBuf::from(&operation.id).join(format!("{expected_index}.before"));
        if step.backup_app_data_relative_path != path_to_portable_string(&expected_backup)? {
            return Err(SyncError::RecoveryRequired("recovery_backup_path_invalid"));
        }
        let role = match step.role.as_str() {
            "source" => "source",
            "destination" => "destination",
            "single" => "single",
            _ => return Err(SyncError::RecoveryRequired("recovery_step_role_invalid")),
        };
        let effect = match step.effect.as_str() {
            "upsert" => "upsert",
            "remove" => "remove",
            _ => return Err(SyncError::RecoveryRequired("recovery_step_effect_invalid")),
        };
        if role != role_for(operation, &notebook.id) || effect != effect_for(operation, role) {
            return Err(SyncError::RecoveryRequired(
                "recovery_step_semantics_invalid",
            ));
        }
        let target = resolve_notebook_target(workspace, &notebook)?;
        Ok(PreparedTarget {
            step_index: step.step_index,
            notebook,
            target,
            staged_sibling_name: step.staged_sibling_name,
            backup_path: self.recovery_root.join(&expected_backup),
            backup_relative: step.backup_app_data_relative_path,
            target_existed: step.target_existed,
            before_file_sha256: step.before_file_sha256,
            before_managed_sha256: step.before_managed_sha256,
            before_modified_ns: step.before_modified_ns,
            after_file_sha256: step.after_file_sha256,
            after_managed_sha256: step.after_managed_sha256,
            role,
            effect,
        })
    }

    fn prepare_target(
        &self,
        store: &Store,
        operation: &RecoveryOperation,
        workspace: &Workspace,
        notebook: &Notebook,
        records: &[Record],
        step_index: usize,
    ) -> Result<PreparedTarget, SyncError> {
        let (notebook, target, before) =
            resolve_existing_notebook_target(store, workspace, notebook)?;
        let (had_bom, existing) = decode_markdown(&before.bytes)?;
        let receipt = store.notebook_file_receipt(&notebook.id)?;
        let before_snapshot = match managed_snapshot(existing, &notebook.target_id) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                if let Some(receipt) = &receipt {
                    let observed_managed_sha256 =
                        managed_region_sha256_unchecked(existing, &notebook.target_id)
                            .ok()
                            .flatten();
                    store.record_notebook_conflict(
                        &notebook,
                        receipt.generation,
                        &receipt.managed_sha256,
                        &before.file_sha256,
                        observed_managed_sha256.as_deref(),
                        "managed_region_changed",
                    )?;
                    return Err(SyncError::Conflict("managed_region_changed"));
                }
                return Err(SyncError::Managed(
                    "the WakeGPT managed region is invalid".to_owned(),
                ));
            }
        };
        match receipt {
            Some(receipt) => {
                if receipt.generation == 0 {
                    return Err(SyncError::Conflict("invalid_file_receipt"));
                }
                let Some(snapshot) = &before_snapshot else {
                    store.record_notebook_conflict(
                        &notebook,
                        receipt.generation,
                        &receipt.managed_sha256,
                        &before.file_sha256,
                        None,
                        "managed_region_changed",
                    )?;
                    return Err(SyncError::Conflict("managed_region_missing"));
                };
                if snapshot.managed_sha256 != receipt.managed_sha256 {
                    store.record_notebook_conflict(
                        &notebook,
                        receipt.generation,
                        &receipt.managed_sha256,
                        &before.file_sha256,
                        Some(&snapshot.managed_sha256),
                        "managed_region_changed",
                    )?;
                    return Err(SyncError::Conflict("managed_region_changed"));
                }
                let _outside_region_changed = before.file_sha256 != receipt.file_sha256;
            }
            None if before_snapshot.is_some() => {
                return Err(SyncError::Conflict("managed_region_has_no_receipt"));
            }
            None => {}
        }

        let line_ending = preferred_line_ending(existing);
        let rendered = synchronize_notebook_for_path(
            existing,
            &notebook.target_id,
            records,
            notebook.numbering_style,
            notebook.numbering_start,
            line_ending,
            Some(&notebook.relative_path),
        )
        .map_err(|error| SyncError::Managed(error.to_string()))?;
        let after_snapshot = managed_snapshot(&rendered, &notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::Conflict("managed_region_not_rendered"))?;
        let mut after_bytes = Vec::with_capacity(rendered.len() + usize::from(had_bom) * 3);
        if had_bom {
            after_bytes.extend_from_slice(UTF8_BOM);
        }
        after_bytes.extend_from_slice(rendered.as_bytes());
        let after_file_sha256 = sha256_hex(&after_bytes);

        let staged_sibling_name = format!(".wakegpt-{}-{step_index}.stage", operation.id);
        write_or_reuse_stage(&target, &staged_sibling_name, &after_bytes)?;

        let backup_relative = PathBuf::from(&operation.id).join(format!("{step_index}.before"));
        let backup_path = self.recovery_root.join(&backup_relative);
        if let Some(backup_parent) = backup_path.parent() {
            fs::create_dir_all(backup_parent)
                .map_err(|_| SyncError::Io("recovery_backup_dir_create"))?;
        }
        write_or_reuse(&backup_path, &before.bytes, "backup")?;

        let role = role_for(operation, &notebook.id);
        let effect = effect_for(operation, role);
        Ok(PreparedTarget {
            step_index,
            notebook,
            target,
            staged_sibling_name,
            backup_path,
            backup_relative: path_to_portable_string(&backup_relative)?,
            target_existed: before.existed,
            before_file_sha256: before.file_sha256,
            before_managed_sha256: before_snapshot.map(|snapshot| snapshot.managed_sha256),
            before_modified_ns: before.modified_ns,
            after_file_sha256,
            after_managed_sha256: after_snapshot.managed_sha256,
            role,
            effect,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, ()>, SyncError> {
        self.gate.lock().map_err(|_| SyncError::LockPoisoned)
    }

    pub(crate) fn freeze_for_update(&self) -> Result<MutexGuard<'_, ()>, SyncError> {
        self.lock()
    }

    fn cleanup_completed(&self, operation: &RecoveryOperation, prepared: &[PreparedTarget]) {
        for target in prepared {
            let _ = target.target.root.remove_stage_if_present(
                &target.target.relative_path,
                std::ffi::OsStr::new(&target.staged_sibling_name),
            );
            let _ = fs::remove_file(&target.backup_path);
        }
        let operation_dir = self.recovery_root.join(&operation.id);
        if operation_dir.starts_with(&self.recovery_root) {
            let _ = fs::remove_dir(&operation_dir);
        }
    }
}

struct ReadTarget {
    existed: bool,
    bytes: Vec<u8>,
    file_sha256: String,
    modified_ns: u64,
}

struct AnchoredTarget {
    root: AnchoredRoot,
    relative_path: PathBuf,
}

struct PreparedTarget {
    step_index: usize,
    notebook: Notebook,
    target: AnchoredTarget,
    staged_sibling_name: String,
    backup_path: PathBuf,
    backup_relative: String,
    target_existed: bool,
    before_file_sha256: String,
    before_managed_sha256: Option<String>,
    before_modified_ns: u64,
    after_file_sha256: String,
    after_managed_sha256: String,
    role: &'static str,
    effect: &'static str,
}

impl PreparedTarget {
    fn recovery_step(&self) -> RecoveryFileStepInput {
        RecoveryFileStepInput {
            step_index: self.step_index,
            notebook_id: self.notebook.id.clone(),
            role: self.role,
            effect: self.effect,
            target_workspace_relative_path: self.notebook.relative_path.clone(),
            staged_sibling_name: self.staged_sibling_name.clone(),
            backup_app_data_relative_path: self.backup_relative.clone(),
            expected_target_id: self.notebook.target_id.clone(),
            before_file_sha256: self.before_file_sha256.clone(),
            after_file_sha256: self.after_file_sha256.clone(),
            before_managed_sha256: self.before_managed_sha256.clone(),
            after_managed_sha256: self.after_managed_sha256.clone(),
            before_modified_ns: self.before_modified_ns,
            target_existed: self.target_existed,
        }
    }

    fn reconcile_install(&self) -> Result<(), SyncError> {
        let current = read_target(&self.target)?;
        if current.existed && current.file_sha256 == self.after_file_sha256 {
            return self
                .target
                .root
                .sync_target_parent(&self.target.relative_path)
                .map_err(map_anchored_recovery_error);
        }
        if current.existed != self.target_existed || current.file_sha256 != self.before_file_sha256
        {
            return Err(SyncError::RecoveryRequired(
                "target_changed_during_recovery",
            ));
        }
        verify_owned_file(
            &self.backup_path,
            &self.before_file_sha256,
            "backup_missing_or_changed",
        )?;
        let stage = open_verified_stage(
            &self.target,
            &self.staged_sibling_name,
            &self.after_file_sha256,
        )?;
        if self.target_existed {
            stage
                .replace_existing_cas(&self.before_file_sha256, MAX_NOTEBOOK_BYTES)
                .map_err(map_anchored_recovery_error)?;
        } else {
            stage.install_new().map_err(map_anchored_recovery_error)?;
        }
        let installed = read_target(&self.target)?;
        if !installed.existed || installed.file_sha256 != self.after_file_sha256 {
            return Err(SyncError::RecoveryRequired("installed_digest_mismatch"));
        }
        Ok(())
    }

    fn verify_installed(&self) -> Result<NotebookFileReceiptInput, SyncError> {
        let installed = read_target(&self.target)?;
        if !installed.existed || installed.file_sha256 != self.after_file_sha256 {
            return Err(SyncError::RecoveryRequired("installed_digest_mismatch"));
        }
        self.target
            .root
            .sync_target_parent(&self.target.relative_path)
            .map_err(map_anchored_recovery_error)?;
        let (_, text) = decode_markdown(&installed.bytes)?;
        let snapshot = managed_snapshot(text, &self.notebook.target_id)
            .map_err(|error| SyncError::Managed(error.to_string()))?
            .ok_or(SyncError::RecoveryRequired(
                "installed_managed_region_missing",
            ))?;
        if snapshot.managed_sha256 != self.after_managed_sha256 {
            return Err(SyncError::RecoveryRequired(
                "installed_managed_digest_mismatch",
            ));
        }
        Ok(NotebookFileReceiptInput {
            notebook_id: self.notebook.id.clone(),
            target_id: self.notebook.target_id.clone(),
            marker_schema: MANAGED_SCHEMA_VERSION,
            managed_sha256: snapshot.managed_sha256,
            file_sha256: installed.file_sha256,
            file_modified_ns: installed.modified_ns,
            file_size_bytes: installed.bytes.len() as u64,
        })
    }
}

fn affected_notebooks(operation: &RecoveryOperation) -> Vec<String> {
    let mut notebooks = Vec::with_capacity(2);
    if let Some(destination) = &operation.destination_notebook_id {
        notebooks.push(destination.clone());
    }
    if let Some(source) = &operation.source_notebook_id {
        if !notebooks.iter().any(|notebook| notebook == source) {
            notebooks.push(source.clone());
        }
    }
    notebooks
}

fn notebook_conflict_token(evidence: &NotebookConflictEvidence) -> String {
    let mut hasher = Sha256::new();
    for part in [
        evidence.notebook_id.as_str(),
        evidence.workspace_id.as_str(),
        evidence.target_id.as_str(),
        evidence.expected_managed_sha256.as_str(),
        evidence.observed_file_sha256.as_str(),
        evidence.observed_managed_sha256.as_deref().unwrap_or(""),
        evidence.reason_code.as_str(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(evidence.receipt_generation.to_be_bytes());
    hasher.update(evidence.created_at_ms.to_be_bytes());
    hasher.update(evidence.updated_at_ms.to_be_bytes());
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn refresh_notebook_conflict_evidence(
    store: &Store,
    notebook: &Notebook,
    receipt: &NotebookFileReceipt,
    current: &ReadTarget,
) -> Result<(), SyncError> {
    let (_, markdown) = decode_markdown(&current.bytes)?;
    let observed_managed = managed_region_sha256_unchecked(markdown, &notebook.target_id)
        .ok()
        .flatten();
    store.record_notebook_conflict(
        notebook,
        receipt.generation,
        &receipt.managed_sha256,
        &current.file_sha256,
        observed_managed.as_deref(),
        "managed_region_changed",
    )?;
    Ok(())
}

fn role_for(operation: &RecoveryOperation, notebook_id: &str) -> &'static str {
    if operation.source_notebook_id.as_deref() == operation.destination_notebook_id.as_deref() {
        "single"
    } else if operation.destination_notebook_id.as_deref() == Some(notebook_id) {
        "destination"
    } else {
        "source"
    }
}

fn effect_for(operation: &RecoveryOperation, role: &str) -> &'static str {
    if role == "source" || operation.operation_kind == "trash" {
        "remove"
    } else {
        "upsert"
    }
}

fn resolve_existing_notebook_target(
    store: &Store,
    workspace: &Workspace,
    notebook: &Notebook,
) -> Result<(Notebook, AnchoredTarget, ReadTarget), SyncError> {
    let target = resolve_notebook_target(workspace, notebook)?;
    let current = read_target(&target)?;
    if current.existed && notebook.target_state != NotebookTargetState::Unavailable {
        return Ok((notebook.clone(), target, current));
    }
    if !current.existed
        && notebook.target_state == NotebookTargetState::Unverified
        && store.notebook_file_receipt(&notebook.id)?.is_none()
    {
        return Ok((notebook.clone(), target, current));
    }

    if current.existed {
        return adopt_notebook_target(store, workspace, notebook, &notebook.relative_path, current);
    }

    let ignored_directories = store.notebook_scan_ignore_directories()?;
    let candidates =
        discover_notebook_targets(workspace, &notebook.target_id, &ignored_directories)?;
    let relative_path = match candidates.as_slice() {
        [] => return Err(SyncError::Io("notebook_target_missing")),
        [relative_path] => relative_path,
        _ => return Err(SyncError::Conflict("notebook_target_ambiguous")),
    };
    let candidate = resolve_workspace_target(workspace, relative_path)?;
    let current = read_target(&candidate)?;
    if !current.existed {
        return Err(SyncError::Io("notebook_target_moved_during_scan"));
    }
    adopt_notebook_target(store, workspace, notebook, relative_path, current)
}

fn adopt_notebook_target(
    store: &Store,
    workspace: &Workspace,
    notebook: &Notebook,
    relative_path: &str,
    current: ReadTarget,
) -> Result<(Notebook, AnchoredTarget, ReadTarget), SyncError> {
    let receipt = store
        .notebook_file_receipt(&notebook.id)?
        .ok_or(SyncError::Conflict("notebook_receipt_missing"))?;
    let (_, markdown) = decode_markdown(&current.bytes)?;
    let snapshot = managed_snapshot(markdown, &notebook.target_id)
        .map_err(|error| SyncError::Managed(error.to_string()))?
        .ok_or(SyncError::Conflict("notebook_target_marker_missing"))?;
    if snapshot.managed_sha256 != receipt.managed_sha256 {
        return Err(SyncError::Conflict("notebook_target_managed_changed"));
    }
    let updated = store.relocate_notebook_target(
        notebook,
        relative_path,
        receipt.generation,
        &receipt.managed_sha256,
        &NotebookFileReceiptInput {
            notebook_id: notebook.id.clone(),
            target_id: notebook.target_id.clone(),
            marker_schema: MANAGED_SCHEMA_VERSION,
            managed_sha256: snapshot.managed_sha256,
            file_sha256: current.file_sha256.clone(),
            file_modified_ns: current.modified_ns,
            file_size_bytes: current.bytes.len() as u64,
        },
    )?;
    let target = resolve_notebook_target(workspace, &updated)?;
    Ok((updated, target, current))
}

fn discover_notebook_targets(
    workspace: &Workspace,
    target_id: &str,
    ignored_directories: &[String],
) -> Result<Vec<String>, SyncError> {
    let root = PathBuf::from(&workspace.root_path);
    let marker = format!("<!-- wakegpt:target id=\"{target_id}\"");
    let mut stack = vec![(PathBuf::new(), 0_usize)];
    let mut visited = 0_usize;
    let mut incomplete = false;
    let mut matches = Vec::new();

    while let Some((relative_dir, depth)) = stack.pop() {
        let directory = root.join(&relative_dir);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            visited = visited.saturating_add(1);
            if visited > MAX_NOTEBOOK_SCAN_ENTRIES {
                return Err(SyncError::Conflict("notebook_target_scan_limit"));
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    incomplete = true;
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let relative = relative_dir.join(&name);
            if file_type.is_dir() {
                if ignored_notebook_scan_directory(&relative, ignored_directories) {
                    continue;
                }
                if depth >= MAX_NOTEBOOK_SCAN_DEPTH {
                    return Err(SyncError::Conflict("notebook_target_scan_depth"));
                }
                stack.push((relative, depth + 1));
                continue;
            }
            if !file_type.is_file()
                || relative.extension().and_then(|value| value.to_str()) != Some("md")
            {
                continue;
            }
            let mut file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(entry.path())
            {
                Ok(file) => file,
                Err(_) => {
                    incomplete = true;
                    continue;
                }
            };
            let metadata = match file.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    incomplete = true;
                    continue;
                }
            };
            if !metadata.is_file() || metadata.len() > MAX_NOTEBOOK_BYTES {
                continue;
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            if Read::by_ref(&mut file)
                .take(MAX_NOTEBOOK_BYTES + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() as u64 > MAX_NOTEBOOK_BYTES
            {
                incomplete = true;
                continue;
            }
            let Ok((_, markdown)) = decode_markdown(&bytes) else {
                continue;
            };
            if !markdown.contains(&marker) {
                continue;
            }
            if managed_snapshot(markdown, target_id)
                .map_err(|_| SyncError::Conflict("notebook_target_candidate_invalid"))?
                .is_none()
            {
                continue;
            }
            let relative = relative
                .to_str()
                .ok_or(SyncError::Conflict("notebook_target_path_invalid"))?;
            matches.push(
                validate_notebook_relative_path(relative)
                    .map_err(|_| SyncError::Conflict("notebook_target_path_invalid"))?,
            );
            if matches.len() > 1 {
                return Ok(matches);
            }
        }
    }

    if matches.is_empty() && incomplete {
        return Err(SyncError::Io("notebook_target_scan_incomplete"));
    }
    Ok(matches)
}

fn ignored_notebook_scan_directory(relative: &Path, ignored_directories: &[String]) -> bool {
    ignored_directories.iter().any(|ignored| {
        let ignored = Path::new(ignored);
        relative == ignored || relative.starts_with(ignored)
    })
}

fn verify_absolute_notebook(path: &Path, expected_sha256: &str) -> Result<(), SyncError> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(SyncError::Conflict("notebook_absolute_path_invalid"));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| SyncError::Io("notebook_absolute_target_missing"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_NOTEBOOK_BYTES
    {
        return Err(SyncError::Conflict("notebook_absolute_target_invalid"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| SyncError::Io("notebook_absolute_target_open"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_NOTEBOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SyncError::Io("notebook_absolute_target_read"))?;
    if bytes.len() as u64 > MAX_NOTEBOOK_BYTES || sha256_hex(&bytes) != expected_sha256 {
        return Err(SyncError::Conflict("notebook_absolute_target_changed"));
    }
    Ok(())
}

fn resolve_notebook_target(
    workspace: &Workspace,
    notebook: &Notebook,
) -> Result<AnchoredTarget, SyncError> {
    resolve_workspace_target(workspace, &notebook.relative_path)
}

fn resolve_workspace_target(
    workspace: &Workspace,
    relative_path: &str,
) -> Result<AnchoredTarget, SyncError> {
    let relative = validate_notebook_relative_path(relative_path)
        .map_err(|_| SyncError::Conflict("stored_notebook_path_invalid"))?;
    let root = open_workspace_root(workspace)?;
    Ok(AnchoredTarget {
        root,
        relative_path: PathBuf::from(relative),
    })
}

pub(crate) fn open_workspace_root(workspace: &Workspace) -> Result<AnchoredRoot, SyncError> {
    let root = PathBuf::from(&workspace.root_path);
    let root_metadata =
        fs::symlink_metadata(&root).map_err(|_| SyncError::Io("workspace_root_unavailable"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(SyncError::Conflict("workspace_root_changed"));
    }
    let canonical_root =
        fs::canonicalize(&root).map_err(|_| SyncError::Io("workspace_root_unavailable"))?;
    if canonical_root != root {
        return Err(SyncError::Conflict("workspace_root_alias_changed"));
    }

    let root_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&root)
        .map_err(|_| SyncError::Io("workspace_root_open"))?;
    AnchoredRoot::from_owned_fd(root_file.into()).map_err(map_anchored_target_error)
}

fn read_target(target: &AnchoredTarget) -> Result<ReadTarget, SyncError> {
    let file = match target.root.open_regular_file_read(&target.relative_path) {
        Ok(file) => file,
        Err(error) if error.code() == AnchoredFsErrorCode::EntryNotFound => {
            let bytes = Vec::new();
            return Ok(ReadTarget {
                existed: false,
                file_sha256: sha256_hex(&bytes),
                bytes,
                modified_ns: 0,
            });
        }
        Err(error) => return Err(map_anchored_target_error(error)),
    };
    let metadata = file
        .metadata()
        .map_err(|_| SyncError::Io("target_metadata_read"))?;
    if metadata.len() > MAX_NOTEBOOK_BYTES {
        return Err(SyncError::Conflict("target_too_large"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_NOTEBOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SyncError::Io("target_read"))?;
    if bytes.len() as u64 > MAX_NOTEBOOK_BYTES {
        return Err(SyncError::Conflict("target_too_large"));
    }
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0);
    Ok(ReadTarget {
        existed: true,
        file_sha256: sha256_hex(&bytes),
        bytes,
        modified_ns,
    })
}

fn map_anchored_target_error(error: AnchoredFsError) -> SyncError {
    match error.code() {
        AnchoredFsErrorCode::InvalidRelativePath
        | AnchoredFsErrorCode::NameContainsNul
        | AnchoredFsErrorCode::SymlinkEncountered
        | AnchoredFsErrorCode::NotDirectory
        | AnchoredFsErrorCode::NotRegularFile
        | AnchoredFsErrorCode::EntryAlreadyExists => SyncError::Conflict(error.code_str()),
        AnchoredFsErrorCode::StageIdentityChanged
        | AnchoredFsErrorCode::TargetIdentityChanged
        | AnchoredFsErrorCode::FileSyncFailed
        | AnchoredFsErrorCode::DirectorySyncFailed => SyncError::RecoveryRequired(error.code_str()),
        AnchoredFsErrorCode::EntryNotFound
        | AnchoredFsErrorCode::PermissionDenied
        | AnchoredFsErrorCode::Io => SyncError::Io(error.code_str()),
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        AnchoredFsErrorCode::UnsupportedPlatform => SyncError::Conflict(error.code_str()),
    }
}

fn map_anchored_recovery_error(error: AnchoredFsError) -> SyncError {
    match map_anchored_target_error(error) {
        SyncError::Conflict(code) | SyncError::Io(code) => SyncError::RecoveryRequired(code),
        error => error,
    }
}

fn map_document_replace_error(error: AnchoredFsError) -> SyncError {
    if error.code() == AnchoredFsErrorCode::TargetIdentityChanged {
        SyncError::Conflict("notebook_document_changed_during_save")
    } else {
        map_anchored_target_error(error)
    }
}

fn map_numbering_replace_error(error: AnchoredFsError) -> SyncError {
    if error.code() == AnchoredFsErrorCode::TargetIdentityChanged {
        SyncError::Conflict("notebook_numbering_changed_during_apply")
    } else {
        map_anchored_target_error(error)
    }
}

fn map_attachment_directory_replace_error(error: AnchoredFsError) -> SyncError {
    if error.code() == AnchoredFsErrorCode::TargetIdentityChanged {
        SyncError::Conflict("notebook_attachment_directory_changed_during_apply")
    } else {
        map_anchored_target_error(error)
    }
}

fn decode_markdown(bytes: &[u8]) -> Result<(bool, &str), SyncError> {
    let (had_bom, content) = if bytes.starts_with(UTF8_BOM) {
        (true, &bytes[UTF8_BOM.len()..])
    } else {
        (false, bytes)
    };
    let text =
        std::str::from_utf8(content).map_err(|_| SyncError::Conflict("target_is_not_utf8"))?;
    Ok((had_bom, text))
}

fn encode_markdown(had_bom: bool, markdown: &str) -> Result<Vec<u8>, SyncError> {
    let total = markdown
        .len()
        .checked_add(usize::from(had_bom) * UTF8_BOM.len())
        .ok_or(SyncError::Conflict("notebook_document_too_large"))?;
    if total as u64 > MAX_NOTEBOOK_BYTES {
        return Err(SyncError::Conflict("notebook_document_too_large"));
    }
    let mut bytes = Vec::with_capacity(total);
    if had_bom {
        bytes.extend_from_slice(UTF8_BOM);
    }
    bytes.extend_from_slice(markdown.as_bytes());
    Ok(bytes)
}

fn preferred_line_ending(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    let mut lf = 0usize;
    let mut crlf = 0usize;
    let mut cr = 0usize;
    let mut offset = 0usize;
    while offset < bytes.len() {
        match bytes[offset] {
            b'\r' if bytes.get(offset + 1) == Some(&b'\n') => {
                crlf += 1;
                offset += 2;
            }
            b'\r' => {
                cr += 1;
                offset += 1;
            }
            b'\n' => {
                lf += 1;
                offset += 1;
            }
            _ => offset += 1,
        }
    }
    if crlf >= cr && crlf >= lf && crlf > 0 {
        "\r\n"
    } else if cr > lf && cr > 0 {
        "\r"
    } else {
        "\n"
    }
}

fn write_or_reuse_stage(
    target: &AnchoredTarget,
    stage_name: &str,
    bytes: &[u8],
) -> Result<(), SyncError> {
    match target.root.create_exclusive_stage(
        &target.relative_path,
        std::ffi::OsStr::new(stage_name),
        true,
    ) {
        Ok(mut stage) => {
            stage
                .file_mut()
                .write_all(bytes)
                .map_err(|_| SyncError::Io("stage_write"))?;
            stage.persist().map_err(map_anchored_recovery_error)
        }
        Err(error) if error.code() == AnchoredFsErrorCode::EntryAlreadyExists => {
            let _ = open_verified_stage(target, stage_name, &sha256_hex(bytes))?;
            Ok(())
        }
        Err(error) => Err(map_anchored_target_error(error)),
    }
}

fn open_verified_stage(
    target: &AnchoredTarget,
    stage_name: &str,
    expected_sha256: &str,
) -> Result<ExclusiveStage, SyncError> {
    let mut stage = target
        .root
        .open_existing_stage(&target.relative_path, std::ffi::OsStr::new(stage_name))
        .map_err(map_anchored_recovery_error)?;
    let metadata = stage
        .file_mut()
        .metadata()
        .map_err(|_| SyncError::RecoveryRequired("stage_metadata_read"))?;
    if metadata.len() > MAX_NOTEBOOK_BYTES {
        return Err(SyncError::RecoveryRequired("stage_too_large"));
    }
    stage
        .file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|_| SyncError::RecoveryRequired("stage_seek"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    stage
        .file_mut()
        .take(MAX_NOTEBOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SyncError::RecoveryRequired("stage_read"))?;
    if bytes.len() as u64 > MAX_NOTEBOOK_BYTES || sha256_hex(&bytes) != expected_sha256 {
        return Err(SyncError::RecoveryRequired("stage_missing_or_changed"));
    }
    stage
        .file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|_| SyncError::RecoveryRequired("stage_seek"))?;
    Ok(stage)
}

fn write_or_reuse(path: &Path, bytes: &[u8], kind: &'static str) -> Result<(), SyncError> {
    let expected_sha256 = sha256_hex(bytes);
    let file = OpenOptions::new().write(true).create_new(true).open(path);
    let mut file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return verify_owned_file(
                path,
                &expected_sha256,
                match kind {
                    "stage" => "stage_existing_digest_mismatch",
                    _ => "backup_existing_digest_mismatch",
                },
            );
        }
        Err(_) => {
            return Err(SyncError::Io(match kind {
                "stage" => "stage_create",
                _ => "backup_create",
            }));
        }
    };
    if let Err(error) = file.write_all(bytes) {
        let _ = fs::remove_file(path);
        let _ = error;
        return Err(SyncError::Io(match kind {
            "stage" => "stage_write",
            _ => "backup_write",
        }));
    }
    full_sync_file(&file).map_err(|_| {
        SyncError::Io(match kind {
            "stage" => "stage_sync",
            _ => "backup_sync",
        })
    })?;
    Ok(())
}

fn verify_owned_file(
    path: &Path,
    expected_sha256: &str,
    mismatch_code: &'static str,
) -> Result<(), SyncError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| SyncError::RecoveryRequired(mismatch_code))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_NOTEBOOK_BYTES
    {
        return Err(SyncError::RecoveryRequired(mismatch_code));
    }
    let file = File::open(path).map_err(|_| SyncError::RecoveryRequired(mismatch_code))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_NOTEBOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SyncError::RecoveryRequired(mismatch_code))?;
    if bytes.len() as u64 > MAX_NOTEBOOK_BYTES || sha256_hex(&bytes) != expected_sha256 {
        return Err(SyncError::RecoveryRequired(mismatch_code));
    }
    Ok(())
}

fn full_sync_file(file: &File) -> std::io::Result<()> {
    file.sync_all()?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: fcntl does not outlive the borrowed File descriptor and F_FULLFSYNC takes no
        // pointer argument.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn path_to_portable_string(path: &Path) -> Result<String, SyncError> {
    path.to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or(SyncError::Conflict("recovery_path_not_utf8"))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NumberingStyle, SyncState, DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES};

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wakegpt-file-sync-{label}-{}",
                crate::domain::new_id()
            ));
            fs::create_dir(&path).unwrap();
            let path = fs::canonicalize(path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn setup(label: &str) -> (TestRoot, Store, SyncCoordinator, Workspace, Notebook) {
        let root = TestRoot::new(label);
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let notebook = store
            .create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::Numeric)
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        (root, store, coordinator, workspace, notebook)
    }

    fn operation_for(store: &Store, record: &Record) -> RecoveryOperation {
        store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .unwrap()
            .unwrap()
    }

    fn stage_operation(
        coordinator: &SyncCoordinator,
        store: &Store,
        operation: &RecoveryOperation,
    ) -> Vec<PreparedTarget> {
        let workspace = store.get_workspace(&operation.workspace_id).unwrap();
        let prepared = coordinator
            .prepare_operation(store, operation, &workspace)
            .unwrap();
        let steps = prepared
            .iter()
            .map(PreparedTarget::recovery_step)
            .collect::<Vec<_>>();
        store.stage_recovery_files(&operation.id, &steps).unwrap();
        prepared
    }

    #[test]
    fn applies_a_queued_record_and_commits_file_receipt() {
        let (root, store, coordinator, workspace, notebook) = setup("create");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "First note")
            .unwrap();
        let completed = coordinator.apply_record(&store, record).unwrap();

        assert_eq!(completed.sync_state, SyncState::Synced);
        assert_eq!(completed.applied_revision, completed.revision);
        let markdown = fs::read_to_string(root.path.join("notes.md")).unwrap();
        assert!(markdown.contains("1. First note"));
        assert!(store.notebook_file_receipt(&notebook.id).unwrap().is_some());
    }

    #[test]
    fn numbering_preview_confirmation_rewrites_only_rendering_configuration() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-confirm");
        let queued = store
            .create_record(&workspace.id, Some(&notebook.id), "First note")
            .unwrap();
        let synced = coordinator.apply_record(&store, queued).unwrap();
        let preview = coordinator
            .preview_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NumberingStyle::Bullet,
                1,
            )
            .unwrap();

        assert!(preview.markdown.contains("- First note"));
        assert!(!preview.markdown.contains("1. First note"));
        let updated = coordinator
            .change_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NotebookNumberingConfiguration {
                    style: NumberingStyle::Bullet,
                    start: 1,
                },
                &preview.expected_file_sha256,
                preview.expected_receipt_generation,
            )
            .unwrap();

        assert_eq!(updated.numbering_style, NumberingStyle::Bullet);
        assert!(!updated.numbering_sync_pending);
        let markdown = fs::read_to_string(root.path.join("notes.md")).unwrap();
        assert!(markdown.contains("- First note"));
        assert!(!markdown.contains("1. First note"));
        let unchanged_record = store.get_record(&workspace.id, &synced.id).unwrap();
        assert_eq!(unchanged_record.id, synced.id);
        assert_eq!(unchanged_record.logical_order, synced.logical_order);
        assert_eq!(unchanged_record.revision, synced.revision);
        assert_eq!(
            store
                .notebook_file_receipt(&notebook.id)
                .unwrap()
                .unwrap()
                .generation,
            preview.expected_receipt_generation + 1
        );
    }

    #[test]
    fn numbering_start_preview_confirmation_preserves_record_identity() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-start");
        let queued = store
            .create_record(&workspace.id, Some(&notebook.id), "Start here")
            .unwrap();
        let synced = coordinator.apply_record(&store, queued).unwrap();
        let preview = coordinator
            .preview_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NumberingStyle::Numeric,
                41,
            )
            .unwrap();

        assert_eq!(preview.current_numbering_start, 1);
        assert_eq!(preview.next_numbering_start, 41);
        assert!(preview.markdown.contains("41. Start here"));
        let updated = coordinator
            .change_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NotebookNumberingConfiguration {
                    style: NumberingStyle::Numeric,
                    start: 41,
                },
                &preview.expected_file_sha256,
                preview.expected_receipt_generation,
            )
            .unwrap();

        assert_eq!(updated.numbering_style, NumberingStyle::Numeric);
        assert_eq!(updated.numbering_start, 41);
        assert!(!updated.numbering_sync_pending);
        assert!(fs::read_to_string(root.path.join("notes.md"))
            .unwrap()
            .contains("41. Start here"));
        let unchanged_record = store.get_record(&workspace.id, &synced.id).unwrap();
        assert_eq!(unchanged_record.id, synced.id);
        assert_eq!(unchanged_record.logical_order, synced.logical_order);
        assert_eq!(unchanged_record.revision, synced.revision);
    }

    #[test]
    fn stale_numbering_preview_never_queues_or_overwrites() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-stale");
        let queued = store
            .create_record(&workspace.id, Some(&notebook.id), "First note")
            .unwrap();
        coordinator.apply_record(&store, queued).unwrap();
        let preview = coordinator
            .preview_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NumberingStyle::Task,
                1,
            )
            .unwrap();
        let path = root.path.join("notes.md");
        let external = format!("{}\nOutside edit\n", fs::read_to_string(&path).unwrap());
        fs::write(&path, &external).unwrap();

        let error = coordinator
            .change_notebook_numbering(
                &store,
                &workspace.id,
                &notebook.id,
                NotebookNumberingConfiguration {
                    style: NumberingStyle::Task,
                    start: 1,
                },
                &preview.expected_file_sha256,
                preview.expected_receipt_generation,
            )
            .unwrap_err();

        assert!(matches!(error, CoordinatedMutationError::Coordination(_)));
        let unchanged = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        assert_eq!(unchanged.numbering_style, NumberingStyle::Numeric);
        assert!(!unchanged.numbering_sync_pending);
        assert_eq!(fs::read_to_string(path).unwrap(), external);
    }

    #[test]
    fn startup_replay_finishes_a_queued_numbering_start_rewrite() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-replay");
        let queued_record = store
            .create_record(&workspace.id, Some(&notebook.id), "Recover format")
            .unwrap();
        coordinator.apply_record(&store, queued_record).unwrap();
        let current = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        let queued_notebook = store
            .queue_notebook_numbering_configuration(
                &current,
                NumberingStyle::Numeric,
                9,
                receipt.generation,
            )
            .unwrap();
        assert!(queued_notebook.numbering_sync_pending);

        let report = coordinator.replay_pending(&store).unwrap();

        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending_notebooks.is_empty());
        let recovered = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        assert_eq!(recovered.numbering_style, NumberingStyle::Numeric);
        assert_eq!(recovered.numbering_start, 9);
        assert!(!recovered.numbering_sync_pending);
        assert!(fs::read_to_string(root.path.join("notes.md"))
            .unwrap()
            .contains("9. Recover format"));
    }

    #[test]
    fn startup_replay_recognizes_numbering_file_applied_before_receipt() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-applied");
        let queued_record = store
            .create_record(&workspace.id, Some(&notebook.id), "Already formatted")
            .unwrap();
        coordinator.apply_record(&store, queued_record).unwrap();
        let current = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        let queued_notebook = store
            .queue_notebook_numbering_configuration(
                &current,
                NumberingStyle::Bullet,
                current.numbering_start,
                receipt.generation,
            )
            .unwrap();
        let path = root.path.join("notes.md");
        let existing = fs::read_to_string(&path).unwrap();
        let records = store
            .list_records(&workspace.id, Some(&notebook.id), false)
            .unwrap();
        let rendered = synchronize_notebook_for_path(
            &existing,
            &queued_notebook.target_id,
            &records,
            queued_notebook.numbering_style,
            queued_notebook.numbering_start,
            "\n",
            Some(&queued_notebook.relative_path),
        )
        .unwrap();
        fs::write(&path, rendered).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();

        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending_notebooks.is_empty());
        assert!(
            !store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .numbering_sync_pending
        );
        assert!(fs::read_to_string(path)
            .unwrap()
            .contains("- Already formatted"));
    }

    #[test]
    fn numbering_recovery_never_overwrites_an_external_managed_edit() {
        let (root, store, coordinator, workspace, notebook) = setup("numbering-conflict");
        let queued_record = store
            .create_record(&workspace.id, Some(&notebook.id), "Protected text")
            .unwrap();
        coordinator.apply_record(&store, queued_record).unwrap();
        let current = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        store
            .queue_notebook_numbering_configuration(
                &current,
                NumberingStyle::None,
                current.numbering_start,
                receipt.generation,
            )
            .unwrap();
        let path = root.path.join("notes.md");
        let external = fs::read_to_string(&path)
            .unwrap()
            .replace("1. Protected text", "1. External managed edit");
        fs::write(&path, &external).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();

        assert_eq!(report.recovered_operations, 0);
        assert_eq!(report.pending_notebooks.len(), 1);
        assert_eq!(fs::read_to_string(path).unwrap(), external);
        assert!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .numbering_sync_pending
        );
    }

    #[test]
    fn notebook_creation_immediately_creates_and_receipts_an_empty_managed_file() {
        let root = TestRoot::new("notebook-create");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        let notebook = coordinator
            .execute_notebook_creation(&store, &workspace.id, "notes/new.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "New notes",
                    "notes/new.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();

        assert_eq!(
            notebook.target_state,
            crate::domain::NotebookTargetState::Ready
        );
        let markdown = fs::read_to_string(root.path.join("notes/new.md")).unwrap();
        assert!(markdown.contains(&format!("wakegpt:target id=\"{}\"", notebook.target_id)));
        assert!(store.notebook_file_receipt(&notebook.id).unwrap().is_some());
    }

    #[test]
    fn notebook_creation_never_overwrites_an_existing_file() {
        let root = TestRoot::new("notebook-existing");
        fs::write(root.path.join("existing.md"), "# User file\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();

        let error = coordinator
            .execute_notebook_creation(&store, &workspace.id, "existing.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Must not replace",
                    "existing.md",
                    NumberingStyle::None,
                )
            })
            .unwrap_err();

        assert!(matches!(error, CoordinatedMutationError::Coordination(_)));
        assert_eq!(
            fs::read_to_string(root.path.join("existing.md")).unwrap(),
            "# User file\n"
        );
        assert!(store.list_notebooks(&workspace.id).unwrap().is_empty());
    }

    #[test]
    fn binding_preserves_existing_markdown_and_adds_an_empty_managed_region() {
        let root = TestRoot::new("notebook-bind");
        fs::create_dir(root.path.join("notes")).unwrap();
        fs::write(
            root.path.join("notes/existing.md"),
            "# Existing\n\nUser-owned content.\n",
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();

        let notebook = coordinator
            .execute_notebook_binding(
                &store,
                &workspace.id,
                "notes/existing.md",
                NumberingStyle::Numeric,
                1,
                |store| {
                    store.create_notebook(
                        &workspace.id,
                        "Existing",
                        "notes/existing.md",
                        NumberingStyle::Numeric,
                    )
                },
            )
            .unwrap();

        let markdown = fs::read_to_string(root.path.join("notes/existing.md")).unwrap();
        assert!(markdown.starts_with("# Existing\n\nUser-owned content.\n"));
        assert!(markdown.contains(&format!("wakegpt:target id=\"{}\"", notebook.target_id)));
        assert_eq!(
            notebook.target_state,
            crate::domain::NotebookTargetState::Ready
        );
        assert!(store.notebook_file_receipt(&notebook.id).unwrap().is_some());
    }

    #[test]
    fn rebind_accepts_user_edits_outside_the_managed_region_and_resumes_sync() {
        let root = TestRoot::new("notebook-rebind");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        let notebook = coordinator
            .execute_notebook_creation(&store, &workspace.id, "notes.md", |store| {
                store.create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::Numeric)
            })
            .unwrap();
        let created = store
            .create_record(&workspace.id, Some(&notebook.id), "Before unbind")
            .unwrap();
        let synced = coordinator.apply_record(&store, created).unwrap();
        store.unbind_notebook(&workspace.id, &notebook.id).unwrap();
        let path = root.path.join("notes.md");
        let before = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("# User heading\n\n{before}")).unwrap();

        let rebound = coordinator
            .rebind_notebook(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(rebound.target_state, NotebookTargetState::Ready);
        let edited = store
            .update_record(&workspace.id, &synced.id, synced.revision, "After rebind")
            .unwrap();
        coordinator.apply_record(&store, edited).unwrap();
        let after = fs::read_to_string(path).unwrap();
        assert!(after.starts_with("# User heading\n\n"));
        assert!(after.contains("1. After rebind"));
    }

    #[test]
    fn rebind_rejects_changes_inside_the_managed_region() {
        let root = TestRoot::new("notebook-rebind-conflict");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        let notebook = coordinator
            .execute_notebook_creation(&store, &workspace.id, "notes.md", |store| {
                store.create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::Numeric)
            })
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Original")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        store.unbind_notebook(&workspace.id, &notebook.id).unwrap();
        let path = root.path.join("notes.md");
        let before = fs::read_to_string(&path).unwrap();
        fs::write(&path, before.replace("Original", "External change")).unwrap();

        assert!(matches!(
            coordinator.rebind_notebook(&store, &workspace.id, &notebook.id),
            Err(SyncError::Managed(_))
        ));
        assert_eq!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .target_state,
            NotebookTargetState::Unbound
        );
    }

    #[test]
    fn moved_notebook_is_relocated_by_unique_target_identity() {
        let (root, store, coordinator, workspace, notebook) = setup("notebook-relocate");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Move with the file")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        fs::create_dir(root.path.join("moved")).unwrap();
        fs::rename(
            root.path.join("notes.md"),
            root.path.join("moved/renamed.md"),
        )
        .unwrap();

        let document = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        let relocated = store.get_notebook(&workspace.id, &notebook.id).unwrap();

        assert!(document.markdown.contains("1. Move with the file"));
        assert_eq!(relocated.relative_path, "moved/renamed.md");
        assert_eq!(relocated.target_state, NotebookTargetState::Ready);
    }

    #[test]
    fn duplicate_target_identity_fails_closed_without_changing_the_binding() {
        let (root, store, coordinator, workspace, notebook) = setup("notebook-ambiguous");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Duplicate target")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        let bytes = fs::read(root.path.join("notes.md")).unwrap();
        fs::remove_file(root.path.join("notes.md")).unwrap();
        fs::create_dir(root.path.join("copies")).unwrap();
        fs::write(root.path.join("copies/first.md"), &bytes).unwrap();
        fs::write(root.path.join("copies/second.md"), &bytes).unwrap();

        assert!(matches!(
            coordinator.read_notebook_document(&store, &workspace.id, &notebook.id),
            Err(SyncError::Conflict("notebook_target_ambiguous"))
        ));
        assert_eq!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .relative_path,
            "notes.md"
        );
    }

    #[test]
    fn moved_notebook_recovery_respects_exact_configured_ignore_directories() {
        let (root, store, coordinator, workspace, notebook) = setup("configured-ignore");
        let record = store
            .create_record(
                &workspace.id,
                Some(&notebook.id),
                "Move into ignored directory",
            )
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        let mut configured = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        configured.push("archive/private".to_owned());
        store.set_product_settings(Some(30), &configured).unwrap();

        let ignored_directory = root.path.join("archive/private");
        fs::create_dir_all(&ignored_directory).unwrap();
        fs::rename(
            root.path.join(&notebook.relative_path),
            ignored_directory.join("renamed.md"),
        )
        .unwrap();
        assert!(matches!(
            coordinator.recover_notebook_target(&store, &workspace.id, &notebook.id),
            Err(SyncError::Io("notebook_target_missing"))
        ));
        assert_eq!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .relative_path,
            notebook.relative_path
        );

        let mut configured = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        configured.push("archive/other".to_owned());
        store.set_product_settings(Some(30), &configured).unwrap();
        let recovered = coordinator
            .recover_notebook_target(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(recovered.relative_path, "archive/private/renamed.md");
    }

    #[test]
    fn converting_to_plain_markdown_keeps_visible_content_and_local_records() {
        let (root, store, coordinator, workspace, notebook) = setup("notebook-detach");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Keep this visible")
            .unwrap();
        let record = coordinator.apply_record(&store, record).unwrap();
        let path = root.path.join("notes.md");
        let before = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("# User heading\n\n{before}")).unwrap();

        let detached = coordinator
            .convert_notebook_to_plain(&store, &workspace.id, &notebook.id)
            .unwrap();
        let markdown = fs::read_to_string(path).unwrap();
        let retained = store.get_record(&workspace.id, &record.id).unwrap();

        assert_eq!(detached.target_state, NotebookTargetState::Unavailable);
        assert_eq!(
            detached.last_error_code.as_deref(),
            Some("notebook_converted_to_plain")
        );
        assert!(markdown.contains("# User heading"));
        assert!(markdown.contains("1. Keep this visible"));
        assert!(!markdown.contains("<!-- wakegpt:"));
        assert_eq!(retained.notebook_id.as_deref(), Some(notebook.id.as_str()));
        assert!(store
            .update_record(
                &workspace.id,
                &record.id,
                record.revision,
                "Must stay blocked",
            )
            .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_file_moves_to_system_trash_and_reconnects_after_restore() {
        let (root, store, coordinator, workspace, notebook) = setup("notebook-trash");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Trash safely")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        let original = root.path.join("notes.md");

        let (trashed, trashed_path) = coordinator
            .trash_notebook_file(&store, &workspace.id, &notebook.id)
            .unwrap();

        assert!(!original.exists());
        assert!(trashed_path.exists());
        assert_eq!(trashed.target_state, NotebookTargetState::Unavailable);
        assert_eq!(
            trashed.last_error_code.as_deref(),
            Some("notebook_moved_to_trash")
        );
        platform_trash::restore_from_system_trash(&trashed_path, &original).unwrap();
        let recovered = coordinator
            .recover_notebook_target(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(recovered.target_state, NotebookTargetState::Ready);
        assert_eq!(recovered.relative_path, "notes.md");
    }

    #[test]
    fn whole_document_editor_preserves_managed_region_and_uses_file_cas() {
        let (root, store, coordinator, workspace, notebook) = setup("document-editor");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Managed record")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        let original = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        let edited_markdown = format!("# User heading\n\n{}", original.markdown);

        let saved = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &original.file_sha256,
                original.receipt_generation,
                &edited_markdown,
            )
            .unwrap();
        assert!(saved.markdown.starts_with("# User heading\n\n"));
        assert_eq!(saved.receipt_generation, original.receipt_generation + 1);

        let managed_edit = saved.markdown.replace("Managed record", "External edit");
        let error = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &saved.file_sha256,
                saved.receipt_generation,
                &managed_edit,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DocumentSaveError::Unchanged(SyncError::Managed(_))
                | DocumentSaveError::Unchanged(SyncError::Conflict(
                    "managed_region_edit_not_allowed"
                ))
        ));

        fs::write(root.path.join("notes.md"), "# External replacement\n").unwrap();
        let error = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &saved.file_sha256,
                saved.receipt_generation,
                &saved.markdown,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DocumentSaveError::Unchanged(SyncError::Conflict("notebook_document_changed"))
        ));
        assert_eq!(
            fs::read_to_string(root.path.join("notes.md")).unwrap(),
            "# External replacement\n"
        );
    }

    #[test]
    fn whole_document_editor_preserves_crlf_and_utf8_bom() {
        let (root, store, coordinator, workspace, notebook) = setup("document-editor-crlf-bom");
        coordinator.replay_pending(&store).unwrap();
        let original = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        let crlf_markdown = original.markdown.replace('\n', "\r\n");
        let mut original_bytes = Vec::from(UTF8_BOM);
        original_bytes.extend_from_slice(crlf_markdown.as_bytes());
        fs::write(root.path.join("notes.md"), &original_bytes).unwrap();
        let installed =
            read_target(&resolve_notebook_target(&workspace, &notebook).unwrap()).unwrap();
        let (_, installed_text) = decode_markdown(&installed.bytes).unwrap();
        let snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .unwrap()
            .unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        store
            .update_notebook_document_receipt(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: snapshot.managed_sha256,
                    file_sha256: installed.file_sha256,
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
            )
            .unwrap();

        let crlf = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(crlf.line_ending, "\r\n");
        assert!(crlf.had_utf8_bom);
        let edited = format!("# User heading\r\n\r\n{}", crlf.markdown);
        let saved = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &crlf.file_sha256,
                crlf.receipt_generation,
                &edited,
            )
            .unwrap();
        assert_eq!(saved.line_ending, "\r\n");
        assert!(saved.had_utf8_bom);
        let installed = fs::read(root.path.join("notes.md")).unwrap();
        assert!(installed.starts_with(UTF8_BOM));
        let installed_text = std::str::from_utf8(&installed[UTF8_BOM.len()..]).unwrap();
        assert!(!installed_text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn whole_document_editor_preserves_cr_and_utf8_bom() {
        let (root, store, coordinator, workspace, notebook) = setup("document-editor-cr-bom");
        coordinator.replay_pending(&store).unwrap();
        let original = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        let cr_markdown = original.markdown.replace('\n', "\r");
        let mut original_bytes = Vec::from(UTF8_BOM);
        original_bytes.extend_from_slice(cr_markdown.as_bytes());
        fs::write(root.path.join("notes.md"), &original_bytes).unwrap();
        let installed =
            read_target(&resolve_notebook_target(&workspace, &notebook).unwrap()).unwrap();
        let (_, installed_text) = decode_markdown(&installed.bytes).unwrap();
        let snapshot = managed_snapshot(installed_text, &notebook.target_id)
            .unwrap()
            .unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        store
            .update_notebook_document_receipt(
                &notebook,
                receipt.generation,
                &receipt.managed_sha256,
                &NotebookFileReceiptInput {
                    notebook_id: notebook.id.clone(),
                    target_id: notebook.target_id.clone(),
                    marker_schema: MANAGED_SCHEMA_VERSION,
                    managed_sha256: snapshot.managed_sha256,
                    file_sha256: installed.file_sha256,
                    file_modified_ns: installed.modified_ns,
                    file_size_bytes: installed.bytes.len() as u64,
                },
            )
            .unwrap();

        let cr = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(cr.line_ending, "\r");
        assert!(cr.had_utf8_bom);
        let edited = format!("# User heading\r\r{}", cr.markdown);
        let saved = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &cr.file_sha256,
                cr.receipt_generation,
                &edited,
            )
            .unwrap();
        assert_eq!(saved.line_ending, "\r");
        assert!(saved.had_utf8_bom);
        let installed = fs::read(root.path.join("notes.md")).unwrap();
        assert!(installed.starts_with(UTF8_BOM));
        let installed_text = std::str::from_utf8(&installed[UTF8_BOM.len()..]).unwrap();
        assert!(!installed_text.contains('\n'));
        assert!(installed_text.contains("# User heading\r\r"));

        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "CR record")
            .unwrap();
        coordinator.apply_record(&store, record).unwrap();
        let installed = fs::read(root.path.join("notes.md")).unwrap();
        assert!(installed.starts_with(UTF8_BOM));
        let installed_text = std::str::from_utf8(&installed[UTF8_BOM.len()..]).unwrap();
        assert!(!installed_text.contains('\n'));
        assert!(installed_text.contains("1. CR record"));
        let updated = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert_eq!(updated.line_ending, "\r");
        assert!(updated.had_utf8_bom);
    }

    #[test]
    fn whole_document_editor_accepts_the_file_limit_and_rejects_one_byte_over() {
        let (root, store, coordinator, workspace, notebook) = setup("large-document-editor");
        coordinator.replay_pending(&store).unwrap();
        let original = coordinator
            .read_notebook_document(&store, &workspace.id, &notebook.id)
            .unwrap();
        let prefix_length = usize::try_from(MAX_NOTEBOOK_BYTES).unwrap() - original.markdown.len();
        let at_limit = format!("{}{}", "x".repeat(prefix_length), original.markdown);
        assert_eq!(at_limit.len() as u64, MAX_NOTEBOOK_BYTES);

        let saved = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &original.file_sha256,
                original.receipt_generation,
                &at_limit,
            )
            .unwrap();
        assert_eq!(saved.markdown.len() as u64, MAX_NOTEBOOK_BYTES);
        let installed = fs::read(root.path.join("notes.md")).unwrap();
        assert_eq!(installed.len() as u64, MAX_NOTEBOOK_BYTES);

        let over_limit = format!("x{}", saved.markdown);
        let error = coordinator
            .save_notebook_document(
                &store,
                &workspace.id,
                &notebook.id,
                &saved.file_sha256,
                saved.receipt_generation,
                &over_limit,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DocumentSaveError::Unchanged(SyncError::Conflict("notebook_document_too_large"))
        ));
        assert_eq!(fs::read(root.path.join("notes.md")).unwrap(), installed);
    }

    #[test]
    fn startup_replay_initializes_a_notebook_left_unverified() {
        let root = TestRoot::new("notebook-replay");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Recovered",
                "recovered.md",
                NumberingStyle::Bullet,
            )
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();

        assert_eq!(report.initialized_notebooks, 1);
        assert!(report.pending_notebooks.is_empty());
        assert!(fs::read_to_string(root.path.join("recovered.md"))
            .unwrap()
            .contains(&notebook.target_id));
    }

    #[test]
    fn startup_replay_does_not_replace_an_external_notebook_target() {
        let root = TestRoot::new("notebook-replay-conflict");
        fs::write(root.path.join("conflict.md"), "# External\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Conflict",
                "conflict.md",
                NumberingStyle::None,
            )
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();

        assert_eq!(report.initialized_notebooks, 0);
        assert_eq!(report.pending_notebooks.len(), 1);
        assert_eq!(report.pending_notebooks[0].notebook_id, notebook.id);
        assert_eq!(
            fs::read_to_string(root.path.join("conflict.md")).unwrap(),
            "# External\n"
        );
    }

    #[test]
    fn preserves_external_edits_outside_the_managed_region() {
        let (root, store, coordinator, workspace, notebook) = setup("outside-edit");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "First")
            .unwrap();
        let synced = coordinator.apply_record(&store, record).unwrap();
        let path = root.path.join("notes.md");
        let existing = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("# User heading\n\n{existing}")).unwrap();

        let edited = store
            .update_record(&workspace.id, &synced.id, synced.revision, "Second")
            .unwrap();
        coordinator.apply_record(&store, edited).unwrap();
        let markdown = fs::read_to_string(path).unwrap();
        assert!(markdown.starts_with("# User heading\n\n"));
        assert!(markdown.contains("1. Second"));
    }

    #[test]
    fn rejects_external_edits_inside_the_managed_region() {
        let (root, store, coordinator, workspace, notebook) = setup("managed-edit");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "First")
            .unwrap();
        let synced = coordinator.apply_record(&store, record).unwrap();
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        let path = root.path.join("notes.md");
        let existing = fs::read_to_string(&path).unwrap();
        fs::write(&path, existing.replace("First", "External")).unwrap();
        let before_attempt = fs::read(&path).unwrap();
        let (_, before_attempt_text) = decode_markdown(&before_attempt).unwrap();
        let observed_managed_sha256 =
            managed_region_sha256_unchecked(before_attempt_text, &notebook.target_id)
                .unwrap()
                .unwrap();

        let edited = store
            .update_record(&workspace.id, &synced.id, synced.revision, "WakeGPT edit")
            .unwrap();
        let error = coordinator.apply_record(&store, edited).unwrap_err();
        assert_eq!(error.code(), "managed_region_changed");
        assert_eq!(fs::read(path).unwrap(), before_attempt);
        let conflicted = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        assert_eq!(conflicted.target_state, NotebookTargetState::Conflict);
        assert_eq!(
            conflicted.last_error_code.as_deref(),
            Some("managed_region_changed")
        );
        let evidence = store
            .notebook_conflict(&workspace.id, &notebook.id)
            .unwrap()
            .unwrap();
        assert_eq!(evidence.notebook_id, notebook.id);
        assert_eq!(evidence.workspace_id, workspace.id);
        assert_eq!(evidence.target_id, notebook.target_id);
        assert_eq!(evidence.receipt_generation, receipt.generation);
        assert_eq!(evidence.expected_managed_sha256, receipt.managed_sha256);
        assert_eq!(evidence.observed_file_sha256, sha256_hex(&before_attempt));
        assert_eq!(
            evidence.observed_managed_sha256.as_deref(),
            Some(observed_managed_sha256.as_str())
        );
        assert_eq!(evidence.reason_code, "managed_region_changed");
    }

    #[test]
    fn workspace_conflict_check_detects_external_edits_before_the_next_mutation() {
        let (root, store, coordinator, workspace, notebook) = setup("proactive-conflict");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Original")
            .unwrap();
        let synced = coordinator.apply_record(&store, record).unwrap();
        let path = root.path.join("notes.md");
        let existing = fs::read_to_string(&path).unwrap();
        fs::write(&path, existing.replace("Original", "External first")).unwrap();

        assert_eq!(
            coordinator
                .check_workspace_conflicts(&store, &workspace.id)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .target_state,
            NotebookTargetState::Conflict
        );
        assert!(store
            .update_record(&workspace.id, &synced.id, synced.revision, "Blocked edit")
            .is_err());
    }

    fn conflicted_edit(
        label: &str,
    ) -> (
        TestRoot,
        Store,
        SyncCoordinator,
        Workspace,
        Notebook,
        Record,
    ) {
        let (root, store, coordinator, workspace, notebook) = setup(label);
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Original")
            .unwrap();
        let synced = coordinator.apply_record(&store, record).unwrap();
        let path = root.path.join("notes.md");
        let existing = fs::read_to_string(&path).unwrap();
        fs::write(&path, existing.replace("Original", "External version")).unwrap();
        let edited = store
            .update_record(
                &workspace.id,
                &synced.id,
                synced.revision,
                "WakeGPT version",
            )
            .unwrap();
        assert_eq!(
            coordinator
                .apply_record(&store, edited.clone())
                .unwrap_err()
                .code(),
            "managed_region_changed"
        );
        (root, store, coordinator, workspace, notebook, edited)
    }

    #[test]
    fn conflict_resolution_adopts_wakegpt_and_finishes_the_pending_edit() {
        let (root, store, coordinator, workspace, notebook, edited) =
            conflicted_edit("resolve-wakegpt");
        let inspection = coordinator
            .inspect_notebook_conflict(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert!(inspection.adopt_wakegpt.available);
        let resolved = coordinator
            .resolve_notebook_conflict(
                &store,
                &workspace.id,
                &notebook.id,
                &inspection.conflict_token,
                NotebookConflictResolutionAction::AdoptWakegpt,
            )
            .unwrap();
        assert_eq!(resolved.target_state, NotebookTargetState::Ready);
        assert!(store
            .notebook_conflict(&workspace.id, &notebook.id)
            .unwrap()
            .is_none());
        let current = store.get_record(&workspace.id, &edited.id).unwrap();
        assert_eq!(current.body_markdown, "WakeGPT version");
        assert_eq!(current.applied_revision, current.revision);
        assert_eq!(current.sync_state, SyncState::Synced);
        let markdown = fs::read_to_string(root.path.join("notes.md")).unwrap();
        assert!(markdown.contains("1. WakeGPT version"));
        assert!(!markdown.contains("External version"));
    }

    #[test]
    fn conflict_resolution_adopts_file_body_as_a_new_record_revision() {
        let (root, store, coordinator, workspace, notebook, edited) =
            conflicted_edit("resolve-file");
        let inspection = coordinator
            .inspect_notebook_conflict(&store, &workspace.id, &notebook.id)
            .unwrap();
        assert!(inspection.adopt_file.available);
        assert_eq!(
            inspection.record_diffs[0].file_markdown.as_deref(),
            Some("External version")
        );
        let resolved = coordinator
            .resolve_notebook_conflict(
                &store,
                &workspace.id,
                &notebook.id,
                &inspection.conflict_token,
                NotebookConflictResolutionAction::AdoptFile,
            )
            .unwrap();
        assert_eq!(resolved.target_state, NotebookTargetState::Ready);
        let current = store.get_record(&workspace.id, &edited.id).unwrap();
        assert_eq!(current.body_markdown, "External version");
        assert_eq!(current.revision, edited.revision + 1);
        assert_eq!(current.applied_revision, current.revision);
        assert_eq!(current.sync_state, SyncState::Synced);
        let markdown = fs::read_to_string(root.path.join("notes.md")).unwrap();
        assert!(markdown.contains("1. External version"));
        assert!(managed_snapshot(&markdown, &notebook.target_id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn file_adoption_intent_recovers_after_the_file_was_installed() {
        let (root, store, coordinator, workspace, notebook, edited) =
            conflicted_edit("resolve-file-replay");
        let path = root.path.join("notes.md");
        let current_markdown = fs::read_to_string(&path).unwrap();
        let evidence = store
            .notebook_conflict(&workspace.id, &notebook.id)
            .unwrap()
            .unwrap();
        let records = store
            .list_records(&workspace.id, Some(&notebook.id), false)
            .unwrap();
        let adopted = parse_adoptable_file_version(
            &current_markdown,
            &notebook.target_id,
            &records,
            notebook.numbering_style,
            notebook.numbering_start,
            &notebook.relative_path,
        )
        .unwrap();
        let updates = adopted
            .records
            .into_iter()
            .map(|record| NotebookConflictRecordUpdate {
                record_id: record.id,
                expected_revision: record.expected_revision,
                body_markdown: record.body_markdown,
            })
            .collect::<Vec<_>>();
        store
            .begin_notebook_file_version_adoption(&notebook, &evidence, &updates)
            .unwrap();
        let intended_records = store
            .list_records(&workspace.id, Some(&notebook.id), false)
            .unwrap();
        let installed = overwrite_managed_region_for_path(
            &current_markdown,
            &notebook.target_id,
            &intended_records,
            notebook.numbering_style,
            notebook.numbering_start,
            preferred_line_ending(&current_markdown),
            &notebook.relative_path,
        )
        .unwrap();
        fs::write(&path, installed).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending_notebooks.is_empty());
        let resolved = store.get_record(&workspace.id, &edited.id).unwrap();
        assert_eq!(resolved.body_markdown, "External version");
        assert_eq!(resolved.revision, edited.revision + 1);
        assert_eq!(resolved.applied_revision, resolved.revision);
    }

    #[test]
    fn conflict_resolution_unbinds_without_changing_the_file() {
        let (root, store, coordinator, workspace, notebook, edited) =
            conflicted_edit("resolve-unbind");
        let path = root.path.join("notes.md");
        let before = fs::read(&path).unwrap();
        let inspection = coordinator
            .inspect_notebook_conflict(&store, &workspace.id, &notebook.id)
            .unwrap();
        let resolved = coordinator
            .resolve_notebook_conflict(
                &store,
                &workspace.id,
                &notebook.id,
                &inspection.conflict_token,
                NotebookConflictResolutionAction::Unbind,
            )
            .unwrap();
        assert_eq!(resolved.target_state, NotebookTargetState::Unbound);
        assert_eq!(fs::read(path).unwrap(), before);
        let current = store.get_record(&workspace.id, &edited.id).unwrap();
        assert_eq!(current.body_markdown, "WakeGPT version");
        assert_eq!(current.applied_revision, current.revision);
        assert_eq!(current.sync_state, SyncState::Local);
        assert!(store
            .notebook_conflict(&workspace.id, &notebook.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn conflict_resolution_refreshes_stale_evidence_without_writing() {
        let (root, store, coordinator, workspace, notebook, _) = conflicted_edit("resolve-stale");
        let inspection = coordinator
            .inspect_notebook_conflict(&store, &workspace.id, &notebook.id)
            .unwrap();
        let path = root.path.join("notes.md");
        let before = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("# New outside text\n\n{before}")).unwrap();
        let error = coordinator
            .resolve_notebook_conflict(
                &store,
                &workspace.id,
                &notebook.id,
                &inspection.conflict_token,
                NotebookConflictResolutionAction::Unbind,
            )
            .unwrap_err();
        assert_eq!(error.code(), "conflict_evidence_stale");
        assert_eq!(
            store
                .get_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .target_state,
            NotebookTargetState::Conflict
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            format!("# New outside text\n\n{before}")
        );
    }

    #[test]
    fn migration_updates_destination_then_source_without_duplicate_blocks() {
        let root = TestRoot::new("migration");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let source = store
            .create_notebook(&workspace.id, "A", "a.md", NumberingStyle::Numeric)
            .unwrap();
        let destination = store
            .create_notebook(&workspace.id, "B", "b.md", NumberingStyle::Numeric)
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        let record = store
            .create_record(&workspace.id, Some(&source.id), "Move me")
            .unwrap();
        let synced = coordinator.apply_record(&store, record).unwrap();
        let migrated = store
            .migrate_record(
                &workspace.id,
                &synced.id,
                synced.revision,
                Some(&destination.id),
            )
            .unwrap();
        coordinator.apply_record(&store, migrated).unwrap();

        let source_text = fs::read_to_string(root.path.join("a.md")).unwrap();
        let destination_text = fs::read_to_string(root.path.join("b.md")).unwrap();
        assert!(!source_text.contains("Move me"));
        assert_eq!(destination_text.matches("Move me").count(), 1);
    }

    #[test]
    fn startup_replay_finishes_a_staged_operation_and_is_idempotent() {
        let (root, store, coordinator, workspace, notebook) = setup("replay-staged");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Replay me")
            .unwrap();
        let operation = operation_for(&store, &record);
        let _prepared = stage_operation(&coordinator, &store, &operation);

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert!(fs::read_to_string(root.path.join("notes.md"))
            .unwrap()
            .contains("Replay me"));
        let completed = store.get_record(&workspace.id, &record.id).unwrap();
        assert_eq!(completed.sync_state, SyncState::Synced);

        let second = coordinator.replay_pending(&store).unwrap();
        assert_eq!(second.recovered_operations, 0);
        assert!(second.pending.is_empty());
    }

    #[test]
    fn startup_replay_recognizes_an_install_completed_before_its_step_receipt() {
        let (_root, store, coordinator, workspace, notebook) = setup("replay-applying");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Installed first")
            .unwrap();
        let operation = operation_for(&store, &record);
        let prepared = stage_operation(&coordinator, &store, &operation);
        store.mark_recovery_applying(&operation.id).unwrap();
        prepared[0].reconcile_install().unwrap();

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert_eq!(
            store
                .get_record(&workspace.id, &record.id)
                .unwrap()
                .sync_state,
            SyncState::Synced
        );
    }

    #[test]
    fn startup_replay_commits_receipts_after_all_files_were_applied() {
        let (_root, store, coordinator, workspace, notebook) = setup("replay-files-applied");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Receipt pending")
            .unwrap();
        let operation = operation_for(&store, &record);
        let prepared = stage_operation(&coordinator, &store, &operation);
        store.mark_recovery_applying(&operation.id).unwrap();
        prepared[0].reconcile_install().unwrap();
        store.mark_recovery_step_applied(&operation.id, 0).unwrap();
        store.mark_recovery_files_applied(&operation.id).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert!(store.notebook_file_receipt(&notebook.id).unwrap().is_some());
    }

    #[test]
    fn startup_replay_can_resume_a_retry_before_file_steps_were_persisted() {
        let (_root, store, coordinator, workspace, notebook) = setup("replay-needs-staging");
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Retry prepare")
            .unwrap();
        let operation = operation_for(&store, &record);
        store
            .mark_recovery_needed(&operation.id, "interrupted-before-staging")
            .unwrap();

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert_eq!(
            store
                .get_record(&workspace.id, &record.id)
                .unwrap()
                .sync_state,
            SyncState::Synced
        );
    }

    #[test]
    fn startup_replay_finishes_a_two_file_migration_after_the_first_install() {
        let root = TestRoot::new("replay-partial-migration");
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.path.to_str().unwrap())
            .unwrap();
        let source = store
            .create_notebook(&workspace.id, "A", "a.md", NumberingStyle::Numeric)
            .unwrap();
        let destination = store
            .create_notebook(&workspace.id, "B", "b.md", NumberingStyle::Numeric)
            .unwrap();
        let coordinator = SyncCoordinator::new(root.path.join("app-data/recovery")).unwrap();
        let queued = store
            .create_record(&workspace.id, Some(&source.id), "Move after crash")
            .unwrap();
        let synced = coordinator.apply_record(&store, queued).unwrap();
        let migrated = store
            .migrate_record(
                &workspace.id,
                &synced.id,
                synced.revision,
                Some(&destination.id),
            )
            .unwrap();
        let operation = operation_for(&store, &migrated);
        let prepared = stage_operation(&coordinator, &store, &operation);
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0].role, "destination");
        store.mark_recovery_applying(&operation.id).unwrap();
        prepared[0].reconcile_install().unwrap();
        store.mark_recovery_step_applied(&operation.id, 0).unwrap();

        let report = coordinator.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert!(!fs::read_to_string(root.path.join("a.md"))
            .unwrap()
            .contains("Move after crash"));
        assert_eq!(
            fs::read_to_string(root.path.join("b.md"))
                .unwrap()
                .matches("Move after crash")
                .count(),
            1
        );
    }
}
