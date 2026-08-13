use crate::attachments::AttachmentError;
use crate::file_sync::{CoordinatedMutationError, DocumentSaveError, SyncError};
use crate::storage::StoreError;
use serde::Serialize;

pub(crate) type CommandResult<T> = Result<T, CommandError>;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DataState {
    Unchanged,
    SavedLocal,
    RecoveryRequired,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandError {
    code: &'static str,
    message: String,
    retryable: bool,
    data_state: DataState,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entity_id: Option<String>,
}

impl CommandError {
    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn unchanged(
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            data_state: DataState::Unchanged,
            operation_id: None,
            entity_id: None,
        }
    }

    pub(crate) fn storage(error: StoreError) -> Self {
        let (code, message, retryable) = match error {
            StoreError::Domain(error) => ("invalid_request", error.to_string(), false),
            StoreError::Conflict(message) => ("conflict", message, false),
            StoreError::NotFound(entity) => ("not_found", format!("找不到请求的 {entity}"), false),
            StoreError::LockPoisoned => (
                "storage_busy",
                "本地数据暂时不可用，请重试".to_owned(),
                true,
            ),
            StoreError::Sqlite(_) => ("storage_error", "无法完成本地数据库操作".to_owned(), true),
            StoreError::Io(_) => (
                "storage_io_error",
                "无法完成本地数据文件操作".to_owned(),
                true,
            ),
        };
        Self {
            code,
            message,
            retryable,
            data_state: DataState::Unchanged,
            operation_id: None,
            entity_id: None,
        }
    }

    pub(crate) fn sync(error: SyncError, operation_id: Option<String>, entity_id: String) -> Self {
        let (code, message, retryable, data_state) = match error {
            SyncError::Store(_) => (
                "storage_error",
                "记录已保存在本地，但同步回执需要恢复".to_owned(),
                true,
                DataState::RecoveryRequired,
            ),
            SyncError::Managed(message) => (
                "managed_markdown_conflict",
                message,
                false,
                DataState::SavedLocal,
            ),
            SyncError::Io(code) => (
                code,
                "记录已保存在本地，但 Markdown 目标暂时不可用".to_owned(),
                true,
                DataState::SavedLocal,
            ),
            SyncError::Conflict(code) => (
                code,
                "记录已保存在本地，但 Markdown 目标已发生变化".to_owned(),
                false,
                DataState::SavedLocal,
            ),
            SyncError::RecoveryRequired(code) => (
                code,
                "记录已保存在本地，文件操作需要恢复".to_owned(),
                true,
                DataState::RecoveryRequired,
            ),
            SyncError::LockPoisoned => (
                "sync_lock_unavailable",
                "记录已保存在本地，同步队列暂时不可用".to_owned(),
                true,
                DataState::SavedLocal,
            ),
        };
        Self {
            code,
            message,
            retryable,
            data_state,
            operation_id,
            entity_id: Some(entity_id),
        }
    }

    pub(crate) fn coordinated(error: CoordinatedMutationError) -> Self {
        match error {
            CoordinatedMutationError::Storage(error) => Self::storage(error),
            CoordinatedMutationError::Coordination(error) => Self {
                code: error.code(),
                message: error.to_string(),
                retryable: true,
                data_state: DataState::Unchanged,
                operation_id: None,
                entity_id: None,
            },
            CoordinatedMutationError::Saved {
                error,
                operation_id,
                entity_id,
            } => Self::sync(error, operation_id, entity_id),
        }
    }

    pub(crate) fn attachment_saved(error: AttachmentError, entity_id: String) -> Self {
        Self {
            code: error.code(),
            message: "记录已保存，但附件文件操作需要恢复".to_owned(),
            retryable: true,
            data_state: DataState::RecoveryRequired,
            operation_id: None,
            entity_id: Some(entity_id),
        }
    }

    pub(crate) fn notebook_numbering_preview(error: SyncError, notebook_id: String) -> Self {
        let code = error.code();
        let (message, retryable) = match error {
            SyncError::Managed(_) | SyncError::Conflict(_) => (
                "Markdown 受管区或同步回执已变化，编号预览没有生成".to_owned(),
                false,
            ),
            SyncError::Io(_) => ("Markdown 文件暂时不可用，编号预览没有生成".to_owned(), true),
            SyncError::Store(_) | SyncError::LockPoisoned => {
                ("本地速记本状态暂时不可用，请稍后重试".to_owned(), true)
            }
            SyncError::RecoveryRequired(_) => {
                ("请先完成待处理的 Markdown 文件恢复".to_owned(), true)
            }
        };
        Self {
            code,
            message,
            retryable,
            data_state: DataState::Unchanged,
            operation_id: None,
            entity_id: Some(notebook_id),
        }
    }

    pub(crate) fn notebook_numbering_change(
        error: CoordinatedMutationError,
        notebook_id: String,
    ) -> Self {
        match error {
            CoordinatedMutationError::Storage(error) => Self::storage(error),
            CoordinatedMutationError::Coordination(error) => {
                let code = error.code();
                let retryable = !matches!(&error, SyncError::Managed(_) | SyncError::Conflict(_));
                let message = match error {
                    SyncError::Managed(_) | SyncError::Conflict(_) => {
                        "Markdown 文件或同步回执已变化，本次没有修改编号配置".to_owned()
                    }
                    SyncError::Io(_) => "Markdown 文件暂时不可用，本次没有修改编号配置".to_owned(),
                    SyncError::Store(_) | SyncError::LockPoisoned => {
                        "本地速记本状态暂时不可用，请稍后重试".to_owned()
                    }
                    SyncError::RecoveryRequired(_) => {
                        "请先完成待处理的 Markdown 文件恢复".to_owned()
                    }
                };
                Self {
                    code,
                    message,
                    retryable,
                    data_state: DataState::Unchanged,
                    operation_id: None,
                    entity_id: Some(notebook_id),
                }
            }
            CoordinatedMutationError::Saved { error, .. } => {
                let code = error.code();
                let data_state =
                    if matches!(&error, SyncError::Store(_) | SyncError::RecoveryRequired(_)) {
                        DataState::RecoveryRequired
                    } else {
                        DataState::SavedLocal
                    };
                Self {
                    code,
                    message: "编号配置已保存在本地，Markdown 自动重排仍在等待恢复".to_owned(),
                    retryable: true,
                    data_state,
                    operation_id: None,
                    entity_id: Some(notebook_id),
                }
            }
        }
    }

    pub(crate) fn notebook_attachment_directory_preview(
        error: SyncError,
        notebook_id: String,
    ) -> Self {
        let code = error.code();
        let (message, retryable) = match error {
            SyncError::Managed(_) | SyncError::Conflict(_) => (
                "附件目录、Markdown 受管区或同步回执已变化，预览没有生成".to_owned(),
                false,
            ),
            SyncError::Io(_) => ("Markdown 文件暂时不可用，预览没有生成".to_owned(), true),
            SyncError::Store(_) | SyncError::LockPoisoned => {
                ("本地速记本状态暂时不可用，请稍后重试".to_owned(), true)
            }
            SyncError::RecoveryRequired(_) => ("请先完成待处理的记录或附件恢复".to_owned(), true),
        };
        Self {
            code,
            message,
            retryable,
            data_state: DataState::Unchanged,
            operation_id: None,
            entity_id: Some(notebook_id),
        }
    }

    pub(crate) fn notebook_attachment_directory_change(
        error: CoordinatedMutationError,
        notebook_id: String,
    ) -> Self {
        match error {
            CoordinatedMutationError::Storage(error) => Self::storage(error),
            CoordinatedMutationError::Coordination(error) => Self {
                code: error.code(),
                message: "附件目录、Markdown 文件或同步回执已变化，本次没有修改附件目录".to_owned(),
                retryable: !matches!(&error, SyncError::Managed(_) | SyncError::Conflict(_)),
                data_state: DataState::Unchanged,
                operation_id: None,
                entity_id: Some(notebook_id),
            },
            CoordinatedMutationError::Saved { error, .. } => Self {
                code: error.code(),
                message: "附件目录已保存在本地，附件迁移或 Markdown 更新仍在等待恢复".to_owned(),
                retryable: true,
                data_state: DataState::RecoveryRequired,
                operation_id: None,
                entity_id: Some(notebook_id),
            },
        }
    }

    pub(crate) fn document_save(error: DocumentSaveError, notebook_id: String) -> Self {
        match error {
            DocumentSaveError::Unchanged(error) => {
                let code = error.code();
                let retryable = !matches!(&error, SyncError::Managed(_) | SyncError::Conflict(_));
                let message = match error {
                    SyncError::Managed(message) => message,
                    SyncError::Conflict("managed_region_edit_not_allowed") => {
                        "WakeGPT 受管速记区需通过记录卡修改，其他 Markdown 内容可直接编辑"
                            .to_owned()
                    }
                    SyncError::Conflict(_) => {
                        "Markdown 文件已发生变化，本次保存没有覆盖文件".to_owned()
                    }
                    SyncError::Io(_) => "Markdown 文件暂时不可用".to_owned(),
                    SyncError::Store(_) | SyncError::LockPoisoned => {
                        "本地同步状态暂时不可用".to_owned()
                    }
                    SyncError::RecoveryRequired(_) => "Markdown 文件操作需要先完成恢复".to_owned(),
                };
                Self {
                    code,
                    message,
                    retryable,
                    data_state: DataState::Unchanged,
                    operation_id: None,
                    entity_id: Some(notebook_id),
                }
            }
            DocumentSaveError::Saved(error) => Self {
                code: error.code(),
                message: "Markdown 文件已安全写入，但同步回执需要恢复".to_owned(),
                retryable: true,
                data_state: DataState::RecoveryRequired,
                operation_id: None,
                entity_id: Some(notebook_id),
            },
        }
    }

    pub(crate) fn notebook_lifecycle(error: SyncError, notebook_id: String) -> Self {
        let code = error.code();
        let (message, retryable, data_state) = match error {
            SyncError::Managed(_) | SyncError::Conflict(_) => (
                "Markdown 受管区或文件身份已发生变化，本次操作没有继续".to_owned(),
                false,
                DataState::Unchanged,
            ),
            SyncError::Io("notebook_target_missing") => (
                "没有在当前工作区找到带相同目标 ID 的 Markdown 文件".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::Io(_) => (
                "Markdown 文件暂时不可用，本次操作没有继续".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::Store(_) | SyncError::LockPoisoned => (
                "本地速记本状态暂时不可用，请稍后重试".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::RecoveryRequired(_) => (
                "文件操作已经开始，但状态回执需要恢复".to_owned(),
                true,
                DataState::RecoveryRequired,
            ),
        };
        Self {
            code,
            message,
            retryable,
            data_state,
            operation_id: None,
            entity_id: Some(notebook_id),
        }
    }

    pub(crate) fn notebook_conflict(error: SyncError, notebook_id: String) -> Self {
        let code = error.code();
        let (message, retryable, data_state) = match error {
            SyncError::Conflict("conflict_evidence_stale") => (
                "Markdown 文件在预览后再次变化；WakeGPT 已刷新冲突证据，请重新查看差异".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::Conflict("file_version_not_adoptable") => (
                "文件版本改动了记录结构、顺序或附件引用，不能作为正文版本采用".to_owned(),
                false,
                DataState::Unchanged,
            ),
            SyncError::Conflict(_) | SyncError::Managed(_) => (
                "冲突状态或 Markdown 文件已经变化，本次没有继续写入".to_owned(),
                false,
                DataState::Unchanged,
            ),
            SyncError::Io(_) => (
                "Markdown 文件暂时不可用，本次没有继续".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::Store(_) | SyncError::LockPoisoned => (
                "本地冲突状态暂时不可用，请稍后重试".to_owned(),
                true,
                DataState::Unchanged,
            ),
            SyncError::RecoveryRequired(_) => (
                "冲突选择已写入文件，但数据库回执仍需恢复".to_owned(),
                true,
                DataState::RecoveryRequired,
            ),
        };
        Self {
            code,
            message,
            retryable,
            data_state,
            operation_id: None,
            entity_id: Some(notebook_id),
        }
    }
}

impl From<StoreError> for CommandError {
    fn from(value: StoreError) -> Self {
        Self::storage(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_details_are_not_exposed_to_the_webview() {
        let error = CommandError::storage(StoreError::Sqlite(
            rusqlite::Error::InvalidParameterName("private-value".to_owned()),
        ));
        let serialized = serde_json::to_string(&error).unwrap();

        assert!(serialized.contains("storage_error"));
        assert!(!serialized.contains("private-value"));
        assert!(serialized.contains("unchanged"));
    }

    #[test]
    fn sync_failures_report_that_the_record_was_saved() {
        let error = CommandError::sync(
            SyncError::Conflict("managed_region_changed"),
            Some("operation-id".to_owned()),
            "record-id".to_owned(),
        );
        let serialized = serde_json::to_string(&error).unwrap();

        assert!(serialized.contains("savedLocal"));
        assert!(serialized.contains("managed_region_changed"));
        assert!(serialized.contains("operation-id"));
    }

    #[test]
    fn post_save_database_errors_are_redacted_and_require_recovery() {
        let error = CommandError::sync(
            SyncError::Store(StoreError::Sqlite(rusqlite::Error::InvalidParameterName(
                "private-value".to_owned(),
            ))),
            Some("operation-id".to_owned()),
            "record-id".to_owned(),
        );
        let serialized = serde_json::to_string(&error).unwrap();

        assert!(serialized.contains("recoveryRequired"));
        assert!(!serialized.contains("private-value"));
    }

    #[test]
    fn conflict_resolution_distinguishes_stale_preview_from_post_write_recovery() {
        let stale = CommandError::notebook_conflict(
            SyncError::Conflict("conflict_evidence_stale"),
            "notebook-id".to_owned(),
        );
        let stale = serde_json::to_string(&stale).unwrap();
        assert!(stale.contains("conflict_evidence_stale"));
        assert!(stale.contains("unchanged"));

        let recovery = CommandError::notebook_conflict(
            SyncError::RecoveryRequired("conflict_resolution_receipt_pending"),
            "notebook-id".to_owned(),
        );
        let recovery = serde_json::to_string(&recovery).unwrap();
        assert!(recovery.contains("conflict_resolution_receipt_pending"));
        assert!(recovery.contains("recoveryRequired"));
    }
}
