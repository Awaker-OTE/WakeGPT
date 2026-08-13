use crate::anchored_fs::{AnchoredFsErrorCode, AnchoredRoot};
use crate::domain::{
    managed_attachment_relative_path, new_id, validate_id,
    validate_managed_attachment_relative_path as validate_domain_attachment_path,
    validate_notebook_relative_path, Attachment, AttachmentFileState, AttachmentRelocationState,
    Notebook, Record, RecordState, Workspace, INBOX_ATTACHMENT_DIRECTORY,
};
use crate::file_sync::{open_workspace_root, sha256_hex, SyncError};
use crate::platform_trash::{self, PlatformTrashError};
use crate::storage::{
    AttachmentFileOperation, AttachmentRelocationOperation, DraftAttachment, NewAttachment, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub const MAX_ATTACHMENTS_PER_RECORD: usize = 10;
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_RECORD_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_MARKDOWN_IMAGE_SOURCE_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingAttachment {
    pub token: String,
    pub media_type: String,
    pub byte_size: u64,
    pub content_sha256: String,
    pub display_name: String,
}

pub struct AttachmentCoordinator {
    pending_root: PathBuf,
    recovery_root: PathBuf,
    gate: Mutex<()>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecoveryReport {
    pub recovered_operations: usize,
    pub pending: Vec<AttachmentRecoveryIssue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecoveryIssue {
    pub attachment_id: String,
    pub record_id: String,
    pub code: &'static str,
    pub attempt_count: u64,
    pub previous_error_code: Option<String>,
}

impl AttachmentCoordinator {
    pub fn new(pending_root: PathBuf, recovery_root: PathBuf) -> Result<Self, AttachmentError> {
        fs::create_dir_all(&pending_root)
            .map_err(|_| AttachmentError::Io("attachment_pending_dir_create"))?;
        fs::create_dir_all(&recovery_root)
            .map_err(|_| AttachmentError::Io("attachment_recovery_dir_create"))?;
        validate_private_directory(&pending_root, "attachment_pending_dir_invalid")?;
        validate_private_directory(&recovery_root, "attachment_recovery_dir_invalid")?;
        Ok(Self {
            pending_root,
            recovery_root,
            gate: Mutex::new(()),
        })
    }

    pub fn stage_selected(
        &self,
        paths: &[PathBuf],
    ) -> Result<Vec<PendingAttachment>, AttachmentError> {
        let _guard = self.lock()?;
        self.stage_selected_locked(paths)
    }

    fn stage_selected_locked(
        &self,
        paths: &[PathBuf],
    ) -> Result<Vec<PendingAttachment>, AttachmentError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > MAX_ATTACHMENTS_PER_RECORD {
            return Err(AttachmentError::Invalid("too_many_attachments"));
        }

        let mut total = 0_u64;
        let mut staged = Vec::with_capacity(paths.len());
        let mut digests = HashSet::new();
        for path in paths {
            let (bytes, kind) = read_selected_image(path)?;
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or(AttachmentError::Invalid("attachments_total_too_large"))?;
            if total > MAX_RECORD_ATTACHMENT_BYTES {
                return Err(AttachmentError::Invalid("attachments_total_too_large"));
            }
            let digest = sha256_hex(&bytes);
            if !digests.insert(digest.clone()) {
                continue;
            }
            let display_name = path
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("图片")
                .chars()
                .take(160)
                .collect();
            staged.push(self.write_pending_image(&bytes, kind, digest, display_name)?);
        }
        sync_directory(&self.pending_root)?;
        Ok(staged)
    }

    pub fn stage_uploaded(&self, bytes: &[u8]) -> Result<PendingAttachment, AttachmentError> {
        let _guard = self.lock()?;
        self.stage_uploaded_locked(bytes)
    }

    fn stage_uploaded_locked(&self, bytes: &[u8]) -> Result<PendingAttachment, AttachmentError> {
        if bytes.is_empty() {
            return Err(AttachmentError::Invalid("attachment_file_invalid"));
        }
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::Invalid("attachment_too_large"));
        }
        let kind = detect_image_kind(bytes)
            .ok_or(AttachmentError::Invalid("attachment_type_unsupported"))?;
        let pending = self.write_pending_image(
            bytes,
            kind,
            sha256_hex(bytes),
            format!("图片.{}", kind.extension),
        )?;
        sync_directory(&self.pending_root)?;
        Ok(pending)
    }

    pub fn discard_pending(&self, token: &str) -> Result<(), AttachmentError> {
        let _guard = self.lock()?;
        self.discard_pending_locked(token)
    }

    fn discard_pending_locked(&self, token: &str) -> Result<(), AttachmentError> {
        let path = self.pending_path(token)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(AttachmentError::Invalid("attachment_pending_invalid"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(AttachmentError::Io("attachment_pending_metadata")),
        }
        fs::remove_file(path).map_err(|_| AttachmentError::Io("attachment_pending_remove"))?;
        sync_directory(&self.pending_root)
    }

    pub(crate) fn read_pending_preview(
        &self,
        token: &str,
    ) -> Result<(Vec<u8>, &'static str), AttachmentError> {
        let path = self.pending_path(token)?;
        let (bytes, kind) = read_pending_image(&path)?;
        Ok((bytes, kind.media_type))
    }

    pub(crate) fn read_record_preview(
        &self,
        workspace: &Workspace,
        attachment: &Attachment,
    ) -> Result<Vec<u8>, AttachmentError> {
        if attachment.relocation_state != AttachmentRelocationState::Ready
            || !matches!(
                attachment.file_state,
                AttachmentFileState::Ready | AttachmentFileState::PreservedShared
            )
        {
            return Err(AttachmentError::Invalid("attachment_preview_unavailable"));
        }
        validate_existing_attachment_relative_path(
            &attachment.managed_relative_path,
            &attachment.content_sha256,
            &attachment.media_type,
        )?;
        let root = open_workspace_root(workspace).map_err(AttachmentError::Sync)?;
        let bytes = match observe_expected_attachment(
            &root,
            Path::new(&attachment.managed_relative_path),
            &attachment.content_sha256,
            attachment.byte_size,
        )? {
            ManagedAttachmentObservation::Ready(bytes) => bytes,
            ManagedAttachmentObservation::Missing => {
                return Err(AttachmentError::Io("attachment_preview_missing"));
            }
            ManagedAttachmentObservation::Changed => {
                return Err(AttachmentError::Conflict("attachment_preview_changed"));
            }
        };
        let kind = detect_image_kind(&bytes)
            .ok_or(AttachmentError::Invalid("attachment_type_unsupported"))?;
        if kind.media_type != attachment.media_type {
            return Err(AttachmentError::Conflict(
                "attachment_preview_media_type_changed",
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn read_markdown_preview(
        &self,
        workspace: &Workspace,
        notebook: &Notebook,
        source: &str,
    ) -> Result<(Vec<u8>, &'static str), AttachmentError> {
        let relative = markdown_image_relative_path(notebook, source)?;
        let root = open_workspace_root(workspace).map_err(AttachmentError::Sync)?;
        let file = root
            .open_regular_file_read(&relative)
            .map_err(markdown_image_open_error)?;
        let (bytes, kind) = read_image(file, "markdown_image_preview_read")?;
        Ok((bytes, kind.media_type))
    }

    #[cfg(test)]
    pub(crate) fn materialize(
        &self,
        workspace: &Workspace,
        attachment_directory: &str,
        tokens: &[String],
    ) -> Result<Vec<NewAttachment>, AttachmentError> {
        let _guard = self.lock()?;
        self.materialize_locked(workspace, attachment_directory, tokens)
    }

    pub(crate) fn materialize_locked(
        &self,
        workspace: &Workspace,
        attachment_directory: &str,
        tokens: &[String],
    ) -> Result<Vec<NewAttachment>, AttachmentError> {
        if tokens.len() > MAX_ATTACHMENTS_PER_RECORD {
            return Err(AttachmentError::Invalid("too_many_attachments"));
        }
        let root = open_workspace_root(workspace).map_err(AttachmentError::Sync)?;
        let mut total = 0_u64;
        let mut digests = HashSet::new();
        let mut attachments = Vec::with_capacity(tokens.len());
        for token in tokens {
            validate_id(token, "attachment token")
                .map_err(|_| AttachmentError::Invalid("attachment_token_invalid"))?;
            let path = self.pending_path(token)?;
            let (bytes, kind) = read_pending_image(&path)?;
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or(AttachmentError::Invalid("attachments_total_too_large"))?;
            if total > MAX_RECORD_ATTACHMENT_BYTES {
                return Err(AttachmentError::Invalid("attachments_total_too_large"));
            }
            let digest = sha256_hex(&bytes);
            if !digests.insert(digest.clone()) {
                continue;
            }
            let relative_path =
                managed_attachment_relative_path(attachment_directory, &digest, kind.media_type)
                    .map_err(|_| AttachmentError::Invalid("attachment_path_invalid"))?;
            install_content_addressed(&root, &relative_path, token, &bytes, &digest)?;
            attachments.push(NewAttachment {
                media_type: kind.media_type.to_owned(),
                managed_relative_path: relative_path,
                previous_managed_relative_path: None,
                content_sha256: digest,
                byte_size: bytes.len() as u64,
            });
        }
        Ok(attachments)
    }

    pub(crate) fn restore_pending_draft(
        &self,
        attachments: &[DraftAttachment],
    ) -> Vec<PendingAttachment> {
        attachments
            .iter()
            .filter_map(|attachment| {
                let path = self.pending_path(&attachment.token).ok()?;
                let (bytes, kind) = read_pending_image(&path).ok()?;
                let digest = sha256_hex(&bytes);
                if kind.media_type != attachment.media_type
                    || bytes.len() as u64 != attachment.byte_size
                    || digest != attachment.content_sha256
                {
                    return None;
                }
                Some(PendingAttachment {
                    token: attachment.token.clone(),
                    media_type: attachment.media_type.clone(),
                    byte_size: attachment.byte_size,
                    content_sha256: attachment.content_sha256.clone(),
                    display_name: attachment.display_name.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn apply_record_lifecycle(
        &self,
        store: &Store,
        record: Record,
    ) -> Result<Record, AttachmentError> {
        let _guard = self.lock()?;
        let operations =
            store.attachment_file_operations_for_record_revision(&record.id, record.revision)?;
        for operation in operations {
            if let Err(error) = self.reconcile_operation(store, &operation) {
                let _ = store.mark_attachment_file_recovery_needed(&operation, error.code());
                return Err(error);
            }
        }
        store
            .get_record(&record.workspace_id, &record.id)
            .map_err(AttachmentError::from)
    }

    pub(crate) fn replay_pending(
        &self,
        store: &Store,
    ) -> Result<AttachmentRecoveryReport, AttachmentError> {
        let _guard = self.lock()?;
        let operations = store.pending_attachment_file_operations()?;
        let mut report = AttachmentRecoveryReport {
            recovered_operations: 0,
            pending: Vec::new(),
        };
        for operation in operations {
            match self.reconcile_operation(store, &operation) {
                Ok(()) => report.recovered_operations += 1,
                Err(error) => {
                    let _ = store.mark_attachment_file_recovery_needed(&operation, error.code());
                    report.pending.push(AttachmentRecoveryIssue {
                        attachment_id: operation.attachment_id,
                        record_id: operation.record_id,
                        code: error.code(),
                        attempt_count: operation.attempt_count,
                        previous_error_code: operation.last_error_code,
                    });
                }
            }
        }
        Ok(report)
    }

    pub(crate) fn prepare_pending_relocations(
        &self,
        store: &Store,
    ) -> Result<AttachmentRecoveryReport, AttachmentError> {
        let _guard = self.lock()?;
        let operations = store.pending_attachment_relocations()?;
        let mut report = AttachmentRecoveryReport {
            recovered_operations: 0,
            pending: Vec::new(),
        };
        for operation in operations {
            if operation.state == AttachmentRelocationState::CleanupPending {
                continue;
            }
            match self.prepare_relocation(store, &operation) {
                Ok(()) => report.recovered_operations += 1,
                Err(error) => {
                    let _ = store.mark_attachment_relocation_state(
                        &operation,
                        AttachmentRelocationState::Conflict,
                        Some(error.code()),
                    );
                    report.pending.push(AttachmentRecoveryIssue {
                        attachment_id: operation.attachment_id,
                        record_id: operation.record_id,
                        code: error.code(),
                        attempt_count: 0,
                        previous_error_code: operation.error_code,
                    });
                }
            }
        }
        Ok(report)
    }

    pub(crate) fn prepare_record_relocations(
        &self,
        store: &Store,
        record_id: &str,
    ) -> Result<(), AttachmentError> {
        validate_id(record_id, "record id")
            .map_err(|_| AttachmentError::Invalid("attachment_record_id_invalid"))?;
        let _guard = self.lock()?;
        let operations = store.pending_attachment_relocations()?;
        for operation in operations
            .into_iter()
            .filter(|operation| operation.record_id == record_id)
        {
            if operation.state == AttachmentRelocationState::CleanupPending {
                continue;
            }
            if let Err(error) = self.prepare_relocation(store, &operation) {
                let _ = store.mark_attachment_relocation_state(
                    &operation,
                    AttachmentRelocationState::Conflict,
                    Some(error.code()),
                );
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_notebook_relocations(
        &self,
        store: &Store,
        notebook_id: &str,
    ) -> Result<(), AttachmentError> {
        validate_id(notebook_id, "notebook id")
            .map_err(|_| AttachmentError::Invalid("attachment_notebook_id_invalid"))?;
        let _guard = self.lock()?;
        let operations = store.pending_attachment_relocations()?;
        for operation in operations
            .into_iter()
            .filter(|operation| operation.notebook_id.as_deref() == Some(notebook_id))
        {
            if operation.state == AttachmentRelocationState::CleanupPending {
                continue;
            }
            if let Err(error) = self.prepare_relocation(store, &operation) {
                let _ = store.mark_attachment_relocation_state(
                    &operation,
                    AttachmentRelocationState::Conflict,
                    Some(error.code()),
                );
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn finish_pending_relocations(
        &self,
        store: &Store,
    ) -> Result<AttachmentRecoveryReport, AttachmentError> {
        let _guard = self.lock()?;
        let mut groups = BTreeMap::<(String, String), Vec<AttachmentRelocationOperation>>::new();
        for operation in store.pending_attachment_relocations()? {
            if operation.state == AttachmentRelocationState::CleanupPending {
                groups
                    .entry((
                        operation.workspace_id.clone(),
                        operation.previous_managed_relative_path.clone(),
                    ))
                    .or_default()
                    .push(operation);
            }
        }
        let mut report = AttachmentRecoveryReport {
            recovered_operations: 0,
            pending: Vec::new(),
        };
        for operations in groups.into_values() {
            if operations.iter().any(|operation| {
                operation.record_revision != operation.applied_revision
                    || operation.notebook_directory_sync_pending
            }) {
                for operation in operations {
                    report.pending.push(AttachmentRecoveryIssue {
                        attachment_id: operation.attachment_id,
                        record_id: operation.record_id,
                        code: "attachment_waiting_for_markdown_sync",
                        attempt_count: 0,
                        previous_error_code: operation.error_code,
                    });
                }
                continue;
            }
            match self.finish_relocation_group(store, &operations) {
                Ok(()) => report.recovered_operations += operations.len(),
                Err(error) => {
                    for operation in operations {
                        let _ = store.mark_attachment_relocation_state(
                            &operation,
                            AttachmentRelocationState::CleanupPending,
                            Some(error.code()),
                        );
                        report.pending.push(AttachmentRecoveryIssue {
                            attachment_id: operation.attachment_id,
                            record_id: operation.record_id,
                            code: error.code(),
                            attempt_count: 0,
                            previous_error_code: operation.error_code,
                        });
                    }
                }
            }
        }
        Ok(report)
    }

    fn prepare_relocation(
        &self,
        store: &Store,
        operation: &AttachmentRelocationOperation,
    ) -> Result<(), AttachmentError> {
        validate_relocation_operation(operation)?;
        let workspace = store.get_workspace(&operation.workspace_id)?;
        let root = open_workspace_root(&workspace).map_err(AttachmentError::Sync)?;
        let destination = Path::new(&operation.managed_relative_path);
        match observe_expected_attachment(
            &root,
            destination,
            &operation.content_sha256,
            operation.byte_size,
        )? {
            ManagedAttachmentObservation::Ready(_) => {}
            ManagedAttachmentObservation::Changed => {
                return Err(AttachmentError::Conflict(
                    "attachment_relocation_destination_conflict",
                ));
            }
            ManagedAttachmentObservation::Missing => {
                let source = Path::new(&operation.previous_managed_relative_path);
                let bytes = match observe_expected_attachment(
                    &root,
                    source,
                    &operation.content_sha256,
                    operation.byte_size,
                )? {
                    ManagedAttachmentObservation::Ready(bytes) => bytes,
                    ManagedAttachmentObservation::Missing => {
                        return Err(AttachmentError::Conflict(
                            "attachment_relocation_source_missing",
                        ));
                    }
                    ManagedAttachmentObservation::Changed => {
                        return Err(AttachmentError::Conflict(
                            "attachment_relocation_source_changed",
                        ));
                    }
                };
                install_content_addressed(
                    &root,
                    &operation.managed_relative_path,
                    &format!("relocate-{}", operation.attachment_id),
                    &bytes,
                    &operation.content_sha256,
                )?;
            }
        }
        match observe_expected_attachment(
            &root,
            destination,
            &operation.content_sha256,
            operation.byte_size,
        )? {
            ManagedAttachmentObservation::Ready(_) => store
                .mark_attachment_relocation_state(
                    operation,
                    AttachmentRelocationState::CleanupPending,
                    None,
                )
                .map_err(AttachmentError::from),
            ManagedAttachmentObservation::Missing => Err(AttachmentError::Io(
                "attachment_relocation_destination_missing",
            )),
            ManagedAttachmentObservation::Changed => Err(AttachmentError::Conflict(
                "attachment_relocation_destination_changed",
            )),
        }
    }

    fn finish_relocation_group(
        &self,
        store: &Store,
        operations: &[AttachmentRelocationOperation],
    ) -> Result<(), AttachmentError> {
        let first = operations.first().ok_or(AttachmentError::Invalid(
            "attachment_relocation_group_empty",
        ))?;
        for operation in operations {
            validate_relocation_operation(operation)?;
            if operation.workspace_id != first.workspace_id
                || operation.previous_managed_relative_path != first.previous_managed_relative_path
                || operation.content_sha256 != first.content_sha256
                || operation.media_type != first.media_type
                || operation.byte_size != first.byte_size
            {
                return Err(AttachmentError::Conflict(
                    "attachment_relocation_group_mismatch",
                ));
            }
        }
        let workspace = store.get_workspace(&first.workspace_id)?;
        let root = open_workspace_root(&workspace).map_err(AttachmentError::Sync)?;
        for operation in operations {
            match observe_expected_attachment(
                &root,
                Path::new(&operation.managed_relative_path),
                &operation.content_sha256,
                operation.byte_size,
            )? {
                ManagedAttachmentObservation::Ready(_) => {}
                ManagedAttachmentObservation::Missing => {
                    return Err(AttachmentError::Io(
                        "attachment_relocation_destination_missing",
                    ));
                }
                ManagedAttachmentObservation::Changed => {
                    return Err(AttachmentError::Conflict(
                        "attachment_relocation_destination_changed",
                    ));
                }
            }
        }
        if store.attachment_path_live_reference_count(
            &first.workspace_id,
            &first.previous_managed_relative_path,
        )? > 0
        {
            return store
                .complete_attachment_relocation_group(
                    operations,
                    Some("attachment_relocation_shared_preserved"),
                )
                .map_err(AttachmentError::from);
        }

        let previous = Path::new(&first.previous_managed_relative_path);
        let terminal_code = match observe_expected_attachment(
            &root,
            previous,
            &first.content_sha256,
            first.byte_size,
        )? {
            ManagedAttachmentObservation::Missing => Some("attachment_relocation_old_copy_missing"),
            ManagedAttachmentObservation::Changed => {
                Some("attachment_relocation_old_copy_changed_preserved")
            }
            ManagedAttachmentObservation::Ready(_) => {
                let absolute = absolute_workspace_path(&workspace, previous)?;
                verify_absolute_expected(&absolute, &first.content_sha256, first.byte_size)?;
                let trashed = platform_trash::move_to_system_trash(&absolute)?;
                verify_absolute_expected(&trashed, &first.content_sha256, first.byte_size)?;
                None
            }
        };
        store
            .complete_attachment_relocation_group(operations, terminal_code)
            .map_err(AttachmentError::from)
    }

    fn reconcile_operation(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
    ) -> Result<(), AttachmentError> {
        if !matches!(
            operation.phase.as_str(),
            "queued" | "prepared" | "needs_recovery"
        ) {
            return self.cleanup_backup(store, operation);
        }

        let record = store.get_record(&operation.workspace_id, &operation.record_id)?;
        let expected_state = match operation.action.as_str() {
            "trash" => RecordState::Trashed,
            "restore" | "detach" => RecordState::Active,
            _ => {
                return Err(AttachmentError::Conflict(
                    "attachment_operation_action_invalid",
                ));
            }
        };
        if record.revision != operation.record_revision || record.state != expected_state {
            return Err(AttachmentError::Conflict(
                "attachment_operation_record_changed",
            ));
        }
        if record.notebook_id.is_some() && record.applied_revision != record.revision {
            return Err(AttachmentError::Conflict(
                "attachment_waiting_for_markdown_sync",
            ));
        }
        let workspace = store.get_workspace(&operation.workspace_id)?;
        let root = open_workspace_root(&workspace).map_err(AttachmentError::Sync)?;
        validate_existing_attachment_relative_path(
            &operation.managed_relative_path,
            &operation.expected_sha256,
            &operation.media_type,
        )?;

        if matches!(operation.action.as_str(), "trash" | "detach") {
            self.trash_attachment(store, operation, &workspace, &root)
        } else {
            self.restore_attachment(store, operation, &workspace, &root)
        }
    }

    fn trash_attachment(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
        workspace: &Workspace,
        root: &AnchoredRoot,
    ) -> Result<(), AttachmentError> {
        if store.attachment_path_reference_count(
            &operation.workspace_id,
            &operation.managed_relative_path,
        )? > 1
        {
            store.mark_attachment_file_terminal(
                operation,
                "preserved_shared",
                Some("attachment_shared_preserved"),
            )?;
            return self.cleanup_backup(store, operation);
        }

        let relative = Path::new(&operation.managed_relative_path);
        let mut observation = observe_managed_attachment(root, relative, operation)?;
        if matches!(observation, ManagedAttachmentObservation::Missing)
            && operation.phase == "prepared"
            && operation.backup_app_data_relative_path.is_some()
        {
            self.restore_backup_to_workspace(root, operation)?;
            observation = observe_managed_attachment(root, relative, operation)?;
        }
        let bytes = match observation {
            ManagedAttachmentObservation::Ready(bytes) => bytes,
            ManagedAttachmentObservation::Missing => {
                store.mark_attachment_file_terminal(
                    operation,
                    "missing",
                    Some("attachment_source_missing"),
                )?;
                return self.cleanup_backup(store, operation);
            }
            ManagedAttachmentObservation::Changed => {
                store.mark_attachment_file_terminal(
                    operation,
                    "preserved_changed",
                    Some("attachment_changed_preserved"),
                )?;
                return self.cleanup_backup(store, operation);
            }
        };

        let backup_relative = operation
            .backup_app_data_relative_path
            .clone()
            .unwrap_or_else(|| {
                format!(
                    "{}/{}.before",
                    operation.attachment_id, operation.record_revision
                )
            });
        self.write_or_verify_backup(&backup_relative, &bytes, operation)?;
        store.mark_attachment_file_prepared(operation, &backup_relative)?;

        let absolute = absolute_workspace_attachment(workspace, operation)?;
        verify_absolute_attachment(&absolute, operation)?;
        let trashed = platform_trash::move_to_system_trash(&absolute)?;
        verify_absolute_attachment(&trashed, operation)?;
        let trash_path = trashed
            .to_str()
            .ok_or(AttachmentError::Invalid("attachment_trash_path_invalid"))?;
        store.mark_attachment_file_trashed(operation, trash_path)?;
        let mut completed = operation.clone();
        completed.phase = "trashed".to_owned();
        completed.backup_app_data_relative_path = Some(backup_relative);
        self.cleanup_backup(store, &completed)
    }

    fn restore_attachment(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
        workspace: &Workspace,
        root: &AnchoredRoot,
    ) -> Result<(), AttachmentError> {
        let relative = Path::new(&operation.managed_relative_path);
        match observe_managed_attachment(root, relative, operation)? {
            ManagedAttachmentObservation::Ready(_) => {
                store.mark_attachment_file_terminal(operation, "ready", None)?;
                return self.cleanup_backup(store, operation);
            }
            ManagedAttachmentObservation::Changed => {
                store.mark_attachment_file_terminal(
                    operation,
                    "preserved_changed",
                    Some("attachment_restore_collision_preserved"),
                )?;
                return self.cleanup_backup(store, operation);
            }
            ManagedAttachmentObservation::Missing => {}
        }

        let absolute = absolute_workspace_attachment(workspace, operation)?;
        if let Some(trash_path) = operation.trash_path.as_deref() {
            let trashed = Path::new(trash_path);
            match verify_absolute_attachment(trashed, operation) {
                Ok(()) => match platform_trash::restore_from_system_trash(trashed, &absolute) {
                    Ok(()) => {}
                    Err(PlatformTrashError::SourceUnavailable) => {
                        return self.restore_missing_attachment(store, operation, root);
                    }
                    Err(PlatformTrashError::DestinationCollision) => {
                        return self.finish_restore_collision(store, operation, root, relative);
                    }
                    Err(error) => return Err(error.into()),
                },
                Err(AttachmentError::Io("attachment_absolute_missing")) => {
                    return self.restore_missing_attachment(store, operation, root);
                }
                Err(AttachmentError::Conflict(_)) => {
                    store.mark_attachment_file_terminal(
                        operation,
                        "missing",
                        Some("attachment_trash_changed_preserved"),
                    )?;
                    return self.cleanup_backup(store, operation);
                }
                Err(error) => return Err(error),
            }
        } else {
            return self.restore_missing_attachment(store, operation, root);
        }

        match observe_managed_attachment(root, relative, operation)? {
            ManagedAttachmentObservation::Ready(_) => {
                store.mark_attachment_file_terminal(operation, "ready", None)?;
            }
            ManagedAttachmentObservation::Missing => {
                return Err(AttachmentError::Io("attachment_restore_result_missing"));
            }
            ManagedAttachmentObservation::Changed => {
                store.mark_attachment_file_terminal(
                    operation,
                    "preserved_changed",
                    Some("attachment_restore_digest_changed"),
                )?;
            }
        }
        self.cleanup_backup(store, operation)
    }

    fn restore_missing_attachment(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
        root: &AnchoredRoot,
    ) -> Result<(), AttachmentError> {
        if operation.backup_app_data_relative_path.is_some() {
            self.restore_backup_to_workspace(root, operation)?;
            store.mark_attachment_file_terminal(operation, "ready", None)?;
        } else {
            store.mark_attachment_file_terminal(
                operation,
                "missing",
                Some("attachment_trash_item_missing"),
            )?;
        }
        self.cleanup_backup(store, operation)
    }

    fn finish_restore_collision(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
        root: &AnchoredRoot,
        relative: &Path,
    ) -> Result<(), AttachmentError> {
        let (phase, code) = match observe_managed_attachment(root, relative, operation)? {
            ManagedAttachmentObservation::Ready(_) => ("ready", None),
            ManagedAttachmentObservation::Changed => (
                "preserved_changed",
                Some("attachment_restore_collision_preserved"),
            ),
            ManagedAttachmentObservation::Missing => {
                return Err(AttachmentError::Conflict(
                    "attachment_restore_collision_unresolved",
                ));
            }
        };
        store.mark_attachment_file_terminal(operation, phase, code)?;
        self.cleanup_backup(store, operation)
    }

    fn restore_backup_to_workspace(
        &self,
        root: &AnchoredRoot,
        operation: &AttachmentFileOperation,
    ) -> Result<(), AttachmentError> {
        let backup_relative = operation
            .backup_app_data_relative_path
            .as_deref()
            .ok_or(AttachmentError::Io("attachment_backup_missing"))?;
        let backup_path = self.backup_path(backup_relative)?;
        let bytes = read_verified_absolute(&backup_path, operation)
            .map_err(|_| AttachmentError::Io("attachment_backup_unavailable"))?;
        install_content_addressed(
            root,
            &operation.managed_relative_path,
            &format!("restore-{}", operation.attachment_id),
            &bytes,
            &operation.expected_sha256,
        )
    }

    fn write_or_verify_backup(
        &self,
        backup_relative: &str,
        bytes: &[u8],
        operation: &AttachmentFileOperation,
    ) -> Result<(), AttachmentError> {
        let backup_path = self.backup_path(backup_relative)?;
        let parent = backup_path
            .parent()
            .ok_or(AttachmentError::Invalid("attachment_backup_path_invalid"))?;
        if !parent.exists() {
            fs::create_dir(parent)
                .map_err(|_| AttachmentError::Io("attachment_backup_dir_create"))?;
        }
        validate_private_directory(parent, "attachment_backup_dir_invalid")?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup_path)
        {
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|_| AttachmentError::Io("attachment_backup_write"))?;
                file.sync_all()
                    .map_err(|_| AttachmentError::Io("attachment_backup_sync"))?;
                sync_directory(parent)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                read_verified_absolute(&backup_path, operation)
                    .map_err(|_| AttachmentError::Conflict("attachment_backup_changed"))?;
            }
            Err(_) => return Err(AttachmentError::Io("attachment_backup_create")),
        }
        Ok(())
    }

    fn cleanup_backup(
        &self,
        store: &Store,
        operation: &AttachmentFileOperation,
    ) -> Result<(), AttachmentError> {
        let Some(relative) = operation.backup_app_data_relative_path.as_deref() else {
            return Ok(());
        };
        let path = self.backup_path(relative)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                    let _ = fs::remove_dir(parent);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AttachmentError::Io("attachment_backup_cleanup")),
        }
        store.clear_attachment_file_backup(operation)?;
        Ok(())
    }

    fn backup_path(&self, relative: &str) -> Result<PathBuf, AttachmentError> {
        validate_backup_relative_path(relative)?;
        let path = self.recovery_root.join(relative);
        if !path.starts_with(&self.recovery_root) {
            return Err(AttachmentError::Invalid("attachment_backup_path_invalid"));
        }
        Ok(path)
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, ()>, AttachmentError> {
        self.gate.lock().map_err(|_| AttachmentError::LockPoisoned)
    }

    pub(crate) fn freeze_for_update(&self) -> Result<MutexGuard<'_, ()>, AttachmentError> {
        self.lock()
    }

    fn pending_path(&self, token: &str) -> Result<PathBuf, AttachmentError> {
        validate_id(token, "attachment token")
            .map_err(|_| AttachmentError::Invalid("attachment_token_invalid"))?;
        Ok(self.pending_root.join(format!("{token}.pending")))
    }

    fn write_pending_image(
        &self,
        bytes: &[u8],
        kind: ImageKind,
        content_sha256: String,
        display_name: String,
    ) -> Result<PendingAttachment, AttachmentError> {
        let token = new_id();
        let pending_path = self.pending_path(&token)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending_path)
            .map_err(|_| AttachmentError::Io("attachment_pending_create"))?;
        output
            .write_all(bytes)
            .map_err(|_| AttachmentError::Io("attachment_pending_write"))?;
        output
            .sync_all()
            .map_err(|_| AttachmentError::Io("attachment_pending_sync"))?;
        Ok(PendingAttachment {
            token,
            media_type: kind.media_type.to_owned(),
            byte_size: bytes.len() as u64,
            content_sha256,
            display_name,
        })
    }
}

#[derive(Debug)]
pub enum AttachmentError {
    Invalid(&'static str),
    Conflict(&'static str),
    Io(&'static str),
    Sync(SyncError),
    Store(StoreError),
    Platform(PlatformTrashError),
    LockPoisoned,
}

impl AttachmentError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(code) | Self::Conflict(code) | Self::Io(code) => code,
            Self::Sync(error) => error.code(),
            Self::Store(_) => "attachment_storage_error",
            Self::Platform(error) => error.code(),
            Self::LockPoisoned => "attachment_lock_unavailable",
        }
    }
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "attachment storage error: {error}"),
            _ => formatter.write_str(self.code()),
        }
    }
}

impl std::error::Error for AttachmentError {}

impl From<StoreError> for AttachmentError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<PlatformTrashError> for AttachmentError {
    fn from(value: PlatformTrashError) -> Self {
        Self::Platform(value)
    }
}

enum ManagedAttachmentObservation {
    Missing,
    Ready(Vec<u8>),
    Changed,
}

fn observe_managed_attachment(
    root: &AnchoredRoot,
    relative: &Path,
    operation: &AttachmentFileOperation,
) -> Result<ManagedAttachmentObservation, AttachmentError> {
    observe_expected_attachment(
        root,
        relative,
        &operation.expected_sha256,
        operation.expected_byte_size,
    )
}

fn observe_expected_attachment(
    root: &AnchoredRoot,
    relative: &Path,
    expected_sha256: &str,
    expected_byte_size: u64,
) -> Result<ManagedAttachmentObservation, AttachmentError> {
    let mut file = match root.open_regular_file_read(relative) {
        Ok(file) => file,
        Err(error) if error.code() == AnchoredFsErrorCode::EntryNotFound => {
            return Ok(ManagedAttachmentObservation::Missing);
        }
        Err(error) => return Err(AttachmentError::Conflict(error.code_str())),
    };
    let metadata = file
        .metadata()
        .map_err(|_| AttachmentError::Io("attachment_managed_metadata"))?;
    if metadata.len() != expected_byte_size
        || metadata.len() > MAX_ATTACHMENT_BYTES
        || metadata.len() == 0
    {
        return Ok(ManagedAttachmentObservation::Changed);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AttachmentError::Io("attachment_managed_read"))?;
    if bytes.len() as u64 != expected_byte_size || sha256_hex(&bytes) != expected_sha256 {
        return Ok(ManagedAttachmentObservation::Changed);
    }
    Ok(ManagedAttachmentObservation::Ready(bytes))
}

fn validate_relocation_operation(
    operation: &AttachmentRelocationOperation,
) -> Result<(), AttachmentError> {
    if operation.managed_relative_path == operation.previous_managed_relative_path {
        return Err(AttachmentError::Invalid(
            "attachment_relocation_path_unchanged",
        ));
    }
    validate_managed_attachment_relative_path(
        &operation.managed_relative_path,
        &operation.content_sha256,
        &operation.media_type,
    )?;
    validate_existing_attachment_relative_path(
        &operation.previous_managed_relative_path,
        &operation.content_sha256,
        &operation.media_type,
    )
}

fn validate_existing_attachment_relative_path(
    value: &str,
    content_sha256: &str,
    media_type: &str,
) -> Result<(), AttachmentError> {
    if validate_managed_attachment_relative_path(value, content_sha256, media_type).is_ok() {
        return Ok(());
    }
    let inbox_path =
        managed_attachment_relative_path(INBOX_ATTACHMENT_DIRECTORY, content_sha256, media_type)
            .map_err(|_| AttachmentError::Invalid("attachment_path_invalid"))?;
    let file_name = Path::new(&inbox_path)
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(AttachmentError::Invalid("attachment_path_invalid"))?;
    let legacy = format!(".wakegpt/attachments/{}/{file_name}", &content_sha256[..2]);
    if value == legacy {
        Ok(())
    } else {
        Err(AttachmentError::Invalid("attachment_path_invalid"))
    }
}

fn absolute_workspace_attachment(
    workspace: &Workspace,
    operation: &AttachmentFileOperation,
) -> Result<PathBuf, AttachmentError> {
    validate_existing_attachment_relative_path(
        &operation.managed_relative_path,
        &operation.expected_sha256,
        &operation.media_type,
    )?;
    let relative = Path::new(&operation.managed_relative_path);
    let root = Path::new(&workspace.root_path);
    if !root.is_absolute() {
        return Err(AttachmentError::Invalid(
            "attachment_workspace_path_invalid",
        ));
    }
    let path = root.join(relative);
    if !path.starts_with(root) {
        return Err(AttachmentError::Invalid("attachment_path_invalid"));
    }
    Ok(path)
}

fn absolute_workspace_path(
    workspace: &Workspace,
    relative: &Path,
) -> Result<PathBuf, AttachmentError> {
    let root = Path::new(&workspace.root_path);
    if !root.is_absolute()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AttachmentError::Invalid("attachment_path_invalid"));
    }
    let path = root.join(relative);
    if !path.starts_with(root) {
        return Err(AttachmentError::Invalid("attachment_path_invalid"));
    }
    Ok(path)
}

fn validate_managed_attachment_relative_path(
    value: &str,
    content_sha256: &str,
    media_type: &str,
) -> Result<(), AttachmentError> {
    validate_domain_attachment_path(value, content_sha256, media_type)
        .map(|_| ())
        .map_err(|_| AttachmentError::Invalid("attachment_path_invalid"))
}

fn validate_backup_relative_path(value: &str) -> Result<(), AttachmentError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AttachmentError::Invalid("attachment_backup_path_invalid"));
    }
    Ok(())
}

fn validate_private_directory(
    path: &Path,
    invalid_code: &'static str,
) -> Result<(), AttachmentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| AttachmentError::Io("attachment_directory_metadata"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AttachmentError::Conflict(invalid_code));
    }
    Ok(())
}

fn verify_absolute_attachment(
    path: &Path,
    operation: &AttachmentFileOperation,
) -> Result<(), AttachmentError> {
    read_verified_absolute(path, operation).map(|_| ())
}

fn verify_absolute_expected(
    path: &Path,
    expected_sha256: &str,
    expected_byte_size: u64,
) -> Result<(), AttachmentError> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(AttachmentError::Invalid("attachment_absolute_path_invalid"));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| AttachmentError::Io("attachment_absolute_missing"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttachmentError::Conflict("attachment_absolute_not_regular"));
    }
    if metadata.len() != expected_byte_size
        || metadata.len() == 0
        || metadata.len() > MAX_ATTACHMENT_BYTES
    {
        return Err(AttachmentError::Conflict(
            "attachment_absolute_size_changed",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AttachmentError::Io("attachment_absolute_open"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AttachmentError::Io("attachment_absolute_read"))?;
    if bytes.len() as u64 != expected_byte_size || sha256_hex(&bytes) != expected_sha256 {
        return Err(AttachmentError::Conflict(
            "attachment_absolute_digest_changed",
        ));
    }
    Ok(())
}

fn read_verified_absolute(
    path: &Path,
    operation: &AttachmentFileOperation,
) -> Result<Vec<u8>, AttachmentError> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(AttachmentError::Invalid("attachment_absolute_path_invalid"));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AttachmentError::Io("attachment_absolute_missing"));
        }
        Err(_) => return Err(AttachmentError::Io("attachment_absolute_metadata")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttachmentError::Conflict("attachment_absolute_not_regular"));
    }
    if metadata.len() != operation.expected_byte_size
        || metadata.len() == 0
        || metadata.len() > MAX_ATTACHMENT_BYTES
    {
        return Err(AttachmentError::Conflict(
            "attachment_absolute_size_changed",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AttachmentError::Io("attachment_absolute_open"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AttachmentError::Io("attachment_absolute_read"))?;
    if bytes.len() as u64 != operation.expected_byte_size
        || sha256_hex(&bytes) != operation.expected_sha256
    {
        return Err(AttachmentError::Conflict(
            "attachment_absolute_digest_changed",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy)]
struct ImageKind {
    media_type: &'static str,
    extension: &'static str,
}

fn read_selected_image(path: &Path) -> Result<(Vec<u8>, ImageKind), AttachmentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| AttachmentError::Io("attachment_source_metadata"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttachmentError::Invalid("attachment_source_invalid"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AttachmentError::Io("attachment_source_open"))?;
    read_image(file, "attachment_source_read")
}

fn read_pending_image(path: &Path) -> Result<(Vec<u8>, ImageKind), AttachmentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| AttachmentError::Invalid("attachment_token_expired"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttachmentError::Invalid("attachment_pending_invalid"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AttachmentError::Invalid("attachment_token_expired"))?;
    read_image(file, "attachment_pending_read")
}

fn read_image(
    file: File,
    read_code: &'static str,
) -> Result<(Vec<u8>, ImageKind), AttachmentError> {
    let before = file
        .metadata()
        .map_err(|_| AttachmentError::Io("attachment_file_metadata"))?;
    if !before.is_file() || before.len() == 0 {
        return Err(AttachmentError::Invalid("attachment_file_invalid"));
    }
    if before.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::Invalid("attachment_too_large"));
    }
    let before_modified = before.modified().ok();
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut limited = file.take(MAX_ATTACHMENT_BYTES + 1);
    limited
        .read_to_end(&mut bytes)
        .map_err(|_| AttachmentError::Io(read_code))?;
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::Invalid("attachment_too_large"));
    }
    let after = limited
        .into_inner()
        .metadata()
        .map_err(|_| AttachmentError::Io("attachment_file_metadata"))?;
    if after.len() != before.len()
        || (before_modified.is_some() && after.modified().ok() != before_modified)
    {
        return Err(AttachmentError::Conflict(
            "attachment_file_changed_during_read",
        ));
    }
    let kind =
        detect_image_kind(&bytes).ok_or(AttachmentError::Invalid("attachment_type_unsupported"))?;
    Ok((bytes, kind))
}

fn markdown_image_relative_path(
    notebook: &Notebook,
    source: &str,
) -> Result<PathBuf, AttachmentError> {
    if source.is_empty()
        || source.len() > MAX_MARKDOWN_IMAGE_SOURCE_BYTES
        || source != source.trim()
        || source.contains(['\\', ':', '?', '#', '%'])
        || source.chars().any(char::is_control)
        || source
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(AttachmentError::Invalid("markdown_image_path_invalid"));
    }
    let source_path = Path::new(source);
    if source_path.is_absolute()
        || source_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AttachmentError::Invalid("markdown_image_path_invalid"));
    }

    let notebook_relative = validate_notebook_relative_path(&notebook.relative_path)
        .map_err(|_| AttachmentError::Conflict("stored_notebook_path_invalid"))?;
    let parent = Path::new(&notebook_relative)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    Ok(parent.join(source_path))
}

fn markdown_image_open_error(error: crate::anchored_fs::AnchoredFsError) -> AttachmentError {
    match error.code() {
        AnchoredFsErrorCode::EntryNotFound => AttachmentError::Io("markdown_image_missing"),
        AnchoredFsErrorCode::SymlinkEncountered => {
            AttachmentError::Conflict("markdown_image_symlink_rejected")
        }
        AnchoredFsErrorCode::NotRegularFile | AnchoredFsErrorCode::NotDirectory => {
            AttachmentError::Invalid("markdown_image_not_regular")
        }
        AnchoredFsErrorCode::InvalidRelativePath | AnchoredFsErrorCode::NameContainsNul => {
            AttachmentError::Invalid("markdown_image_path_invalid")
        }
        AnchoredFsErrorCode::PermissionDenied => AttachmentError::Io("markdown_image_unavailable"),
        _ => AttachmentError::Io("markdown_image_unavailable"),
    }
}

fn detect_image_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageKind {
            media_type: "image/png",
            extension: "png",
        });
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageKind {
            media_type: "image/jpeg",
            extension: "jpg",
        });
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageKind {
            media_type: "image/gif",
            extension: "gif",
        });
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind {
            media_type: "image/webp",
            extension: "webp",
        });
    }
    None
}

fn install_content_addressed(
    root: &AnchoredRoot,
    relative_path: &str,
    token: &str,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<(), AttachmentError> {
    let relative = Path::new(relative_path);
    match root.open_regular_file_read(relative) {
        Ok(existing) => {
            let mut existing_bytes = Vec::new();
            existing
                .take(MAX_ATTACHMENT_BYTES + 1)
                .read_to_end(&mut existing_bytes)
                .map_err(|_| AttachmentError::Io("attachment_existing_read"))?;
            if existing_bytes.len() as u64 > MAX_ATTACHMENT_BYTES
                || sha256_hex(&existing_bytes) != expected_sha256
            {
                return Err(AttachmentError::Conflict("attachment_blob_conflict"));
            }
            Ok(())
        }
        Err(error) if error.code() == AnchoredFsErrorCode::EntryNotFound => {
            let stage_name = format!(".wakegpt-attachment-{token}.stage");
            let mut stage = root
                .create_exclusive_stage(relative, OsStr::new(&stage_name), true)
                .map_err(|error| AttachmentError::Conflict(error.code_str()))?;
            stage
                .file_mut()
                .write_all(bytes)
                .map_err(|_| AttachmentError::Io("attachment_stage_write"))?;
            stage
                .install_new()
                .map_err(|error| AttachmentError::Conflict(error.code_str()))
        }
        Err(error) => Err(AttachmentError::Conflict(error.code_str())),
    }
}

fn sync_directory(path: &Path) -> Result<(), AttachmentError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| AttachmentError::Io("attachment_pending_dir_open"))?;
    directory
        .sync_all()
        .map_err(|_| AttachmentError::Io("attachment_pending_dir_sync"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NumberingStyle, Record, Workspace};
    use crate::file_sync::SyncCoordinator;
    use crate::storage::{RecordAttachmentSetRevision, Store, MUTATION_SCHEMA_VERSION};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("wakegpt-attachments-{}", crate::domain::new_id()));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn local_attachment_fixture(
        root: &TestRoot,
    ) -> (
        AttachmentCoordinator,
        Store,
        Workspace,
        Record,
        PathBuf,
        PathBuf,
    ) {
        let source = root.0.join("selected.png");
        fs::write(&source, b"\x89PNG\r\n\x1a\nattachment-lifecycle").unwrap();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending-lifecycle"),
            root.0.join("app-data/recovery-lifecycle"),
        )
        .unwrap();
        let pending = coordinator
            .stage_selected(std::slice::from_ref(&source))
            .unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Lifecycle", root.0.to_str().unwrap())
            .unwrap();
        let prepared = coordinator
            .materialize(
                &workspace,
                crate::domain::INBOX_ATTACHMENT_DIRECTORY,
                &[pending[0].token.clone()],
            )
            .unwrap();
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Image lifecycle",
                &prepared,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let blob = root.0.join(&record.attachments[0].managed_relative_path);
        (coordinator, store, workspace, record, source, blob)
    }

    #[test]
    fn detects_only_supported_image_signatures() {
        assert_eq!(
            detect_image_kind(b"\x89PNG\r\n\x1a\nrest")
                .unwrap()
                .media_type,
            "image/png"
        );
        assert_eq!(
            detect_image_kind(b"\xff\xd8\xffrest").unwrap().media_type,
            "image/jpeg"
        );
        assert_eq!(
            detect_image_kind(b"GIF89arest").unwrap().media_type,
            "image/gif"
        );
        assert_eq!(
            detect_image_kind(b"RIFF0000WEBPrest").unwrap().media_type,
            "image/webp"
        );
        assert!(detect_image_kind(b"<svg></svg>").is_none());
        assert!(detect_image_kind(b"not an image").is_none());
    }

    #[test]
    fn uploaded_image_is_staged_and_can_be_discarded_idempotently() {
        let root = TestRoot::new();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending-upload"),
            root.0.join("app-data/recovery-upload"),
        )
        .unwrap();
        let png = b"\x89PNG\r\n\x1a\nraw-webview-upload";
        let pending = coordinator.stage_uploaded(png).unwrap();
        assert_eq!(pending.media_type, "image/png");
        assert_eq!(pending.display_name, "图片.png");
        assert_eq!(
            fs::read(coordinator.pending_path(&pending.token).unwrap()).unwrap(),
            png
        );

        coordinator.discard_pending(&pending.token).unwrap();
        coordinator.discard_pending(&pending.token).unwrap();
        assert!(!coordinator.pending_path(&pending.token).unwrap().exists());
        assert_eq!(
            coordinator
                .stage_uploaded(b"<svg></svg>")
                .unwrap_err()
                .code(),
            "attachment_type_unsupported"
        );
    }

    #[test]
    fn update_freeze_blocks_attachment_staging_until_the_guard_is_released() {
        let root = TestRoot::new();
        let coordinator = Arc::new(
            AttachmentCoordinator::new(
                root.0.join("app-data/pending-update-freeze"),
                root.0.join("app-data/recovery-update-freeze"),
            )
            .unwrap(),
        );
        let guard = coordinator.freeze_for_update().unwrap();
        let worker_coordinator = Arc::clone(&coordinator);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(
                    worker_coordinator
                        .stage_uploaded(b"\x89PNG\r\n\x1a\nblocked-by-update")
                        .unwrap()
                        .token,
                )
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(guard);
        assert!(!receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_empty());
        worker.join().unwrap();
    }

    #[test]
    fn previews_read_only_verified_pending_and_managed_images() {
        let root = TestRoot::new();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending-preview"),
            root.0.join("app-data/recovery-preview"),
        )
        .unwrap();
        let png = b"\x89PNG\r\n\x1a\nverified-preview";
        let pending = coordinator.stage_uploaded(png).unwrap();
        let (pending_bytes, pending_media_type) =
            coordinator.read_pending_preview(&pending.token).unwrap();
        assert_eq!(pending_bytes, png);
        assert_eq!(pending_media_type, "image/png");

        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Preview", root.0.to_str().unwrap())
            .unwrap();
        let prepared = coordinator
            .materialize(
                &workspace,
                crate::domain::INBOX_ATTACHMENT_DIRECTORY,
                std::slice::from_ref(&pending.token),
            )
            .unwrap();
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Preview",
                &prepared,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let attachment = &record.attachments[0];
        assert_eq!(
            coordinator
                .read_record_preview(&workspace, attachment)
                .unwrap(),
            png
        );

        let blob = root.0.join(&attachment.managed_relative_path);
        let mut changed = png.to_vec();
        *changed.last_mut().unwrap() ^= 1;
        fs::write(blob, changed).unwrap();
        assert_eq!(
            coordinator
                .read_record_preview(&workspace, attachment)
                .unwrap_err()
                .code(),
            "attachment_preview_changed"
        );
    }

    #[test]
    fn markdown_preview_reads_only_safe_relative_images_for_the_notebook() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new();
        fs::create_dir_all(root.0.join("notes/assets")).unwrap();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending-markdown-preview"),
            root.0.join("app-data/recovery-markdown-preview"),
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Markdown preview", root.0.to_str().unwrap())
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Product",
                "notes/product.md",
                NumberingStyle::None,
            )
            .unwrap();
        let png = b"\x89PNG\r\n\x1a\nrelative-markdown-preview";
        fs::write(root.0.join("notes/assets/valid.png"), png).unwrap();

        let (bytes, media_type) = coordinator
            .read_markdown_preview(&workspace, &notebook, "assets/valid.png")
            .unwrap();
        assert_eq!(bytes, png);
        assert_eq!(media_type, "image/png");

        for source in [
            "../outside.png",
            "/tmp/outside.png",
            "https://example.invalid/tracker.png",
            "%2e%2e/outside.png",
            "assets/../outside.png",
        ] {
            assert_eq!(
                coordinator
                    .read_markdown_preview(&workspace, &notebook, source)
                    .unwrap_err()
                    .code(),
                "markdown_image_path_invalid",
                "source should be rejected: {source}",
            );
        }

        assert_eq!(
            coordinator
                .read_markdown_preview(&workspace, &notebook, "assets/missing.png")
                .unwrap_err()
                .code(),
            "markdown_image_missing"
        );

        fs::write(root.0.join("notes/assets/fake.png"), b"not an image").unwrap();
        assert_eq!(
            coordinator
                .read_markdown_preview(&workspace, &notebook, "assets/fake.png")
                .unwrap_err()
                .code(),
            "attachment_type_unsupported"
        );

        symlink("valid.png", root.0.join("notes/assets/link.png")).unwrap();
        assert_eq!(
            coordinator
                .read_markdown_preview(&workspace, &notebook, "assets/link.png")
                .unwrap_err()
                .code(),
            "markdown_image_symlink_rejected"
        );
    }

    #[test]
    fn image_capture_materializes_a_real_blob_and_syncs_a_relative_markdown_link() {
        let root = TestRoot::new();
        let source = root.0.join("selected.png");
        let png = b"\x89PNG\r\n\x1a\nsynthetic-regression-image";
        fs::write(&source, png).unwrap();
        let attachment_coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending"),
            root.0.join("app-data/recovery-attachments"),
        )
        .unwrap();
        let pending = attachment_coordinator
            .stage_selected(std::slice::from_ref(&source))
            .unwrap();
        assert_eq!(pending.len(), 1);

        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Workspace", root.0.to_str().unwrap())
            .unwrap();
        let sync = SyncCoordinator::new(root.0.join("app-data/recovery")).unwrap();
        let notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "notes/product.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Product",
                    "notes/product.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();
        let prepared = attachment_coordinator
            .materialize(
                &workspace,
                &notebook.attachment_directory,
                &[pending.first().unwrap().token.clone()],
            )
            .unwrap();
        let record = sync
            .execute_record_mutation(&store, |store| {
                store.create_record_with_attachments_idempotent(
                    &workspace.id,
                    Some(&notebook.id),
                    "Screenshot",
                    &prepared,
                    &crate::domain::new_id(),
                    MUTATION_SCHEMA_VERSION,
                )
            })
            .unwrap();

        assert_eq!(record.attachments.len(), 1);
        let blob = root.0.join(&record.attachments[0].managed_relative_path);
        assert_eq!(fs::read(blob).unwrap(), png);
        let markdown = fs::read_to_string(root.0.join("notes/product.md")).unwrap();
        assert!(markdown.contains("![图片](attachments/product/"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn attachment_detach_waits_for_markdown_then_moves_the_exclusive_copy_to_trash() {
        let root = TestRoot::new();
        let source = root.0.join("selected.png");
        fs::write(&source, b"\x89PNG\r\n\x1a\ndetach-after-markdown").unwrap();
        let attachments = AttachmentCoordinator::new(
            root.0.join("app-data/pending-detach"),
            root.0.join("app-data/recovery-attachments-detach"),
        )
        .unwrap();
        let pending = attachments
            .stage_selected(std::slice::from_ref(&source))
            .unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Detach", root.0.to_str().unwrap())
            .unwrap();
        let sync = SyncCoordinator::new(root.0.join("app-data/recovery-detach")).unwrap();
        let notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "notes/detach.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Detach",
                    "notes/detach.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();
        let prepared = attachments
            .materialize(
                &workspace,
                &notebook.attachment_directory,
                &[pending[0].token.clone()],
            )
            .unwrap();
        let record = sync
            .execute_record_mutation(&store, |store| {
                store.create_record_with_attachments_idempotent(
                    &workspace.id,
                    Some(&notebook.id),
                    "Keep text",
                    &prepared,
                    &crate::domain::new_id(),
                    MUTATION_SCHEMA_VERSION,
                )
            })
            .unwrap();
        let blob = root.0.join(&record.attachments[0].managed_relative_path);
        let queued = store
            .revise_record_attachment_set_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                RecordAttachmentSetRevision {
                    body_markdown: "Keep revised text",
                    retained_attachment_ids: &[],
                    new_attachments: &[],
                    mutation_id: &crate::domain::new_id(),
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        assert_eq!(
            attachments
                .apply_record_lifecycle(&store, queued)
                .unwrap_err()
                .code(),
            "attachment_waiting_for_markdown_sync"
        );
        assert!(blob.exists());

        sync.replay_pending(&store).unwrap();
        let report = attachments.replay_pending(&store).unwrap();
        assert_eq!(report.recovered_operations, 1);
        assert!(report.pending.is_empty());
        assert!(!blob.exists());
        let revised = store.get_record(&workspace.id, &record.id).unwrap();
        assert!(revised.attachments.is_empty());
        assert_eq!(revised.applied_revision, revised.revision);
        let markdown = fs::read_to_string(root.0.join("notes/detach.md")).unwrap();
        assert!(markdown.contains("Keep revised text"));
        assert!(!markdown.contains("![图片]"));
        let operation = store
            .attachment_file_operations_for_record_revision(&record.id, revised.revision)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(operation.action, "detach");
        assert_eq!(operation.phase, "trashed");
    }

    #[test]
    fn detached_shared_attachment_is_removed_from_the_record_but_preserved_on_disk() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, _source, blob) =
            local_attachment_fixture(&root);
        let original_attachment = record.attachments[0].clone();
        store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Second reference",
                &[NewAttachment {
                    media_type: original_attachment.media_type.clone(),
                    managed_relative_path: original_attachment.managed_relative_path.clone(),
                    previous_managed_relative_path: None,
                    content_sha256: original_attachment.content_sha256.clone(),
                    byte_size: original_attachment.byte_size,
                }],
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let revised = store
            .revise_record_attachment_set_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                RecordAttachmentSetRevision {
                    body_markdown: "Image removed",
                    retained_attachment_ids: &[],
                    new_attachments: &[],
                    mutation_id: &crate::domain::new_id(),
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        let revised = coordinator.apply_record_lifecycle(&store, revised).unwrap();
        assert!(revised.attachments.is_empty());
        assert!(blob.exists());
        let operation = store
            .attachment_file_operations_for_record_revision(&record.id, revised.revision)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(operation.phase, "preserved_shared");
        assert_eq!(
            operation.last_error_code.as_deref(),
            Some("attachment_shared_preserved")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn record_migration_copies_before_markdown_and_trashes_the_unreferenced_old_copy() {
        let root = TestRoot::new();
        let source_image = root.0.join("selected.png");
        let bytes = b"\x89PNG\r\n\x1a\nrecord-relocation";
        fs::write(&source_image, bytes).unwrap();
        let attachments = AttachmentCoordinator::new(
            root.0.join("app-data/pending-migrate"),
            root.0.join("app-data/recovery-attachments-migrate"),
        )
        .unwrap();
        let sync = SyncCoordinator::new(root.0.join("app-data/recovery-migrate")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Move attachment", root.0.to_str().unwrap())
            .unwrap();
        let source_notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "notes/source.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Source",
                    "notes/source.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();
        let destination_notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "notes/destination.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Destination",
                    "notes/destination.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();
        let staged = attachments
            .stage_selected(std::slice::from_ref(&source_image))
            .unwrap();
        let prepared = attachments
            .materialize(
                &workspace,
                &source_notebook.attachment_directory,
                &[staged[0].token.clone()],
            )
            .unwrap();
        let record = sync
            .execute_record_mutation(&store, |store| {
                store.create_record_with_attachments_idempotent(
                    &workspace.id,
                    Some(&source_notebook.id),
                    "Move this image",
                    &prepared,
                    &crate::domain::new_id(),
                    MUTATION_SCHEMA_VERSION,
                )
            })
            .unwrap();
        let old_relative = record.attachments[0].managed_relative_path.clone();
        let old_absolute = root.0.join(&old_relative);
        assert!(old_absolute.exists());

        let migrated = sync
            .execute_record_mutation_with_prepare(
                &store,
                |store| {
                    store.migrate_record_idempotent(
                        &workspace.id,
                        &record.id,
                        record.revision,
                        Some(&destination_notebook.id),
                        &crate::domain::new_id(),
                        MUTATION_SCHEMA_VERSION,
                    )
                },
                |store, record| {
                    attachments
                        .prepare_record_relocations(store, &record.id)
                        .map_err(|error| SyncError::RecoveryRequired(error.code()))
                },
            )
            .unwrap();
        let new_relative = migrated.attachments[0].managed_relative_path.clone();
        assert!(new_relative.starts_with("notes/attachments/destination/"));
        assert_eq!(fs::read(root.0.join(&new_relative)).unwrap(), bytes);
        assert!(
            old_absolute.exists(),
            "old copy remains until Markdown receipts complete"
        );

        let finish = attachments.finish_pending_relocations(&store).unwrap();
        assert!(finish.pending.is_empty());
        assert_eq!(finish.recovered_operations, 1);
        assert!(!old_absolute.exists());
        let completed = store.get_record(&workspace.id, &record.id).unwrap();
        assert_eq!(
            completed.attachments[0].relocation_state,
            AttachmentRelocationState::Ready
        );
        assert!(completed.attachments[0]
            .previous_managed_relative_path
            .is_none());
        let destination_markdown = fs::read_to_string(root.0.join("notes/destination.md")).unwrap();
        assert!(destination_markdown.contains("attachments/destination/"));
        let source_markdown = fs::read_to_string(root.0.join("notes/source.md")).unwrap();
        assert!(!source_markdown.contains(&record.id));
    }

    #[test]
    fn record_copy_uses_the_destination_home_and_preserves_the_shared_source_copy() {
        let root = TestRoot::new();
        let source_image = root.0.join("copy.png");
        let bytes = b"\x89PNG\r\n\x1a\nrecord-copy";
        fs::write(&source_image, bytes).unwrap();
        let attachments = AttachmentCoordinator::new(
            root.0.join("app-data/pending-copy"),
            root.0.join("app-data/recovery-attachments-copy"),
        )
        .unwrap();
        let sync = SyncCoordinator::new(root.0.join("app-data/recovery-copy")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Copy attachment", root.0.to_str().unwrap())
            .unwrap();
        let source_notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "source.md", |store| {
                store.create_notebook(&workspace.id, "Source", "source.md", NumberingStyle::None)
            })
            .unwrap();
        let destination_notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "destination.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Destination",
                    "destination.md",
                    NumberingStyle::None,
                )
            })
            .unwrap();
        let staged = attachments
            .stage_selected(std::slice::from_ref(&source_image))
            .unwrap();
        let prepared = attachments
            .materialize(
                &workspace,
                &source_notebook.attachment_directory,
                &[staged[0].token.clone()],
            )
            .unwrap();
        let original = sync
            .execute_record_mutation(&store, |store| {
                store.create_record_with_attachments_idempotent(
                    &workspace.id,
                    Some(&source_notebook.id),
                    "Copy this image",
                    &prepared,
                    &crate::domain::new_id(),
                    MUTATION_SCHEMA_VERSION,
                )
            })
            .unwrap();
        let source_relative = original.attachments[0].managed_relative_path.clone();
        let destination_relative = managed_attachment_relative_path(
            &destination_notebook.attachment_directory,
            &original.attachments[0].content_sha256,
            &original.attachments[0].media_type,
        )
        .unwrap();
        let copied_input = NewAttachment {
            media_type: original.attachments[0].media_type.clone(),
            managed_relative_path: destination_relative.clone(),
            previous_managed_relative_path: Some(source_relative.clone()),
            content_sha256: original.attachments[0].content_sha256.clone(),
            byte_size: original.attachments[0].byte_size,
        };
        let copied = sync
            .execute_record_mutation_with_prepare(
                &store,
                |store| {
                    store.create_record_with_attachments_idempotent(
                        &workspace.id,
                        Some(&destination_notebook.id),
                        &original.body_markdown,
                        &[copied_input],
                        &crate::domain::new_id(),
                        MUTATION_SCHEMA_VERSION,
                    )
                },
                |store, record| {
                    attachments
                        .prepare_record_relocations(store, &record.id)
                        .map_err(|error| SyncError::RecoveryRequired(error.code()))
                },
            )
            .unwrap();
        attachments.finish_pending_relocations(&store).unwrap();

        assert!(root.0.join(&source_relative).exists());
        assert_eq!(fs::read(root.0.join(&destination_relative)).unwrap(), bytes);
        let copied = store.get_record(&workspace.id, &copied.id).unwrap();
        assert_eq!(
            copied.attachments[0].relocation_state,
            AttachmentRelocationState::Ready
        );
        assert_eq!(
            copied.attachments[0].relocation_error_code.as_deref(),
            Some("attachment_relocation_shared_preserved")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_attachment_directory_change_is_previewed_recoverable_and_idempotent() {
        let root = TestRoot::new();
        let source_image = root.0.join("directory.png");
        let bytes = b"\x89PNG\r\n\x1a\nnotebook-directory";
        fs::write(&source_image, bytes).unwrap();
        let attachments = AttachmentCoordinator::new(
            root.0.join("app-data/pending-directory"),
            root.0.join("app-data/recovery-attachments-directory"),
        )
        .unwrap();
        let sync = SyncCoordinator::new(root.0.join("app-data/recovery-directory")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Directory", root.0.to_str().unwrap())
            .unwrap();
        let notebook = sync
            .execute_notebook_creation(&store, &workspace.id, "notes/product.md", |store| {
                store.create_notebook(
                    &workspace.id,
                    "Product",
                    "notes/product.md",
                    NumberingStyle::Numeric,
                )
            })
            .unwrap();
        let staged = attachments
            .stage_selected(std::slice::from_ref(&source_image))
            .unwrap();
        let prepared = attachments
            .materialize(
                &workspace,
                &notebook.attachment_directory,
                &[staged[0].token.clone()],
            )
            .unwrap();
        let record = sync
            .execute_record_mutation(&store, |store| {
                store.create_record_with_attachments_idempotent(
                    &workspace.id,
                    Some(&notebook.id),
                    "Directory image",
                    &prepared,
                    &crate::domain::new_id(),
                    MUTATION_SCHEMA_VERSION,
                )
            })
            .unwrap();
        let old_relative = record.attachments[0].managed_relative_path.clone();
        let preview = sync
            .preview_notebook_attachment_directory(
                &store,
                &workspace.id,
                &notebook.id,
                "assets/product-images",
            )
            .unwrap();
        assert_eq!(preview.attachment_count, 1);
        assert!(preview.markdown.contains("../assets/product-images/"));

        let queued = store
            .queue_notebook_attachment_directory(
                &notebook,
                &preview.next_attachment_directory,
                preview.expected_receipt_generation,
            )
            .unwrap();
        attachments
            .prepare_notebook_relocations(&store, &notebook.id)
            .unwrap();
        assert!(queued.attachment_directory_sync_pending);
        assert!(root.0.join(&old_relative).exists());
        let recovery = sync.replay_pending(&store).unwrap();
        assert!(recovery.pending_notebooks.is_empty());
        let changed = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        assert_eq!(changed.attachment_directory, "assets/product-images");
        assert!(!changed.attachment_directory_sync_pending);
        assert!(root
            .0
            .join(&record.attachments[0].managed_relative_path)
            .exists());
        let before_finish = store.get_record(&workspace.id, &record.id).unwrap();
        assert_eq!(
            before_finish.attachments[0].relocation_state,
            AttachmentRelocationState::CleanupPending
        );

        let first_finish = attachments.finish_pending_relocations(&store).unwrap();
        assert_eq!(first_finish.recovered_operations, 1);
        assert!(first_finish.pending.is_empty());
        assert!(!root.0.join(&old_relative).exists());
        let completed = store.get_record(&workspace.id, &record.id).unwrap();
        assert!(completed.attachments[0]
            .managed_relative_path
            .starts_with("assets/product-images/"));
        assert_eq!(
            completed.attachments[0].relocation_state,
            AttachmentRelocationState::Ready
        );
        let markdown = fs::read_to_string(root.0.join("notes/product.md")).unwrap();
        assert!(markdown.contains("../assets/product-images/"));

        let second_finish = attachments.finish_pending_relocations(&store).unwrap();
        assert_eq!(second_finish.recovered_operations, 0);
        assert!(second_finish.pending.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exclusive_attachment_moves_to_system_trash_and_restores_from_its_receipt() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, source, blob) = local_attachment_fixture(&root);

        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trashed = coordinator.apply_record_lifecycle(&store, trashed).unwrap();
        assert_eq!(
            trashed.attachments[0].file_state,
            AttachmentFileState::Trashed
        );
        assert!(!blob.exists());
        assert!(
            source.exists(),
            "the user-selected original must remain untouched"
        );

        let restored = store
            .restore_record_idempotent(
                &workspace.id,
                &record.id,
                trashed.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let restored = coordinator
            .apply_record_lifecycle(&store, restored)
            .unwrap();
        assert_eq!(
            restored.attachments[0].file_state,
            AttachmentFileState::Ready
        );
        assert_eq!(
            fs::read(blob).unwrap(),
            b"\x89PNG\r\n\x1a\nattachment-lifecycle"
        );
    }

    #[test]
    fn shared_attachment_blob_is_preserved_when_one_record_is_trashed() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, _source, blob) =
            local_attachment_fixture(&root);
        let attachment = &record.attachments[0];
        store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Second reference",
                &[NewAttachment {
                    media_type: attachment.media_type.clone(),
                    managed_relative_path: attachment.managed_relative_path.clone(),
                    previous_managed_relative_path: None,
                    content_sha256: attachment.content_sha256.clone(),
                    byte_size: attachment.byte_size,
                }],
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trashed = coordinator.apply_record_lifecycle(&store, trashed).unwrap();

        assert_eq!(
            trashed.attachments[0].file_state,
            AttachmentFileState::PreservedShared
        );
        assert!(blob.exists());
    }

    #[test]
    fn externally_changed_attachment_is_never_moved_or_overwritten() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, _source, blob) =
            local_attachment_fixture(&root);
        fs::write(&blob, b"externally changed").unwrap();

        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trashed = coordinator.apply_record_lifecycle(&store, trashed).unwrap();

        assert_eq!(
            trashed.attachments[0].file_state,
            AttachmentFileState::Modified
        );
        assert_eq!(fs::read(blob).unwrap(), b"externally changed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn restore_keeps_text_active_and_marks_image_missing_after_trash_was_emptied() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, _source, blob) =
            local_attachment_fixture(&root);
        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trashed = coordinator.apply_record_lifecycle(&store, trashed).unwrap();
        let operation = store
            .attachment_file_operations_for_record_revision(&record.id, trashed.revision)
            .unwrap()
            .remove(0);
        fs::remove_file(operation.trash_path.unwrap()).unwrap();

        let restored = store
            .restore_record_idempotent(
                &workspace.id,
                &record.id,
                trashed.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let restored = coordinator
            .apply_record_lifecycle(&store, restored)
            .unwrap();

        assert_eq!(restored.state, RecordState::Active);
        assert_eq!(
            restored.attachments[0].file_state,
            AttachmentFileState::Missing
        );
        assert!(!blob.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn startup_replay_recovers_a_crash_after_the_source_left_the_workspace() {
        let root = TestRoot::new();
        let (coordinator, store, workspace, record, _source, blob) =
            local_attachment_fixture(&root);
        let queued = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let operation = store
            .attachment_file_operations_for_record_revision(&record.id, queued.revision)
            .unwrap()
            .remove(0);
        let bytes = fs::read(&blob).unwrap();
        let backup_relative = format!(
            "{}/{}.before",
            operation.attachment_id, operation.record_revision
        );
        coordinator
            .write_or_verify_backup(&backup_relative, &bytes, &operation)
            .unwrap();
        store
            .mark_attachment_file_prepared(&operation, &backup_relative)
            .unwrap();
        fs::remove_file(&blob).unwrap();

        let first = coordinator.replay_pending(&store).unwrap();
        assert_eq!(first.recovered_operations, 1);
        assert!(first.pending.is_empty());
        let after_replay = store.get_record(&workspace.id, &record.id).unwrap();
        assert_eq!(
            after_replay.attachments[0].file_state,
            AttachmentFileState::Trashed
        );
        assert!(!blob.exists());

        let second = coordinator.replay_pending(&store).unwrap();
        assert_eq!(second.recovered_operations, 0);
        assert!(second.pending.is_empty());

        let restored = store
            .restore_record_idempotent(
                &workspace.id,
                &record.id,
                queued.revision,
                &crate::domain::new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        coordinator
            .apply_record_lifecycle(&store, restored)
            .unwrap();
        assert!(blob.exists());
    }
}
