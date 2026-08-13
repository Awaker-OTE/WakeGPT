use crate::attachments::AttachmentCoordinator;
use crate::file_sync::sha256_hex;
use crate::storage::{DataExportInventory, Store, SCHEMA_VERSION};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

const DATABASE_FILE_NAME: &str = "wakegpt.sqlite3";
const MANIFEST_FILE_NAME: &str = "wakegpt-export-v1.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataExportResult {
    pub folder_name: String,
    pub database_file_name: &'static str,
    pub workspace_count: usize,
    pub notebook_count: usize,
    pub record_count: usize,
    pub exported_attachment_files: usize,
    pub unavailable_attachments: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DataExportError {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl DataExportError {
    pub(crate) fn code(self) -> &'static str {
        self.code
    }

    pub(crate) fn message(self) -> &'static str {
        self.message
    }

    pub(crate) fn retryable(self) -> bool {
        self.retryable
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DataExportManifest {
    format_version: u32,
    app_version: &'static str,
    database_schema_version: u32,
    exported_at_ms: i64,
    database_file: &'static str,
    purpose: &'static str,
    excluded_data: [&'static str; 4],
    summary: DataExportSummary,
    attachments: Vec<DataExportAttachmentEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DataExportSummary {
    workspace_count: usize,
    notebook_count: usize,
    record_count: usize,
    attachment_reference_count: usize,
    exported_attachment_files: usize,
    unavailable_attachments: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DataExportAttachmentEntry {
    source_kind: &'static str,
    reference_id: String,
    workspace_id: String,
    record_id: Option<String>,
    tab_key: Option<String>,
    display_name: Option<String>,
    media_type: String,
    byte_size: u64,
    content_sha256: String,
    state: &'static str,
    export_relative_path: Option<String>,
    error_code: Option<&'static str>,
}

pub(crate) fn export_to_parent(
    store: &Store,
    attachments: &AttachmentCoordinator,
    parent: &Path,
) -> Result<DataExportResult, DataExportError> {
    validate_export_parent(parent)?;
    let exported_at_ms = current_time_ms();
    let identifier = Uuid::now_v7().simple().to_string();
    let folder_name = format!(
        "WakeGPT-export-{}-{}",
        export_timestamp(exported_at_ms),
        &identifier[..8]
    );
    let final_root = parent.join(&folder_name);
    let staging_root = parent.join(format!(".wakegpt-export-{identifier}.partial"));
    if final_root.exists() || staging_root.exists() {
        return Err(export_error(
            "data_export_destination_collision",
            "导出目录已存在，请重新选择位置",
            false,
        ));
    }
    create_private_directory(&staging_root)?;

    let result = export_package_contents(
        store,
        attachments,
        &staging_root,
        folder_name.clone(),
        exported_at_ms,
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    if fs::rename(&staging_root, &final_root).is_err() {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(write_error());
    }
    let _ = sync_directory(parent);
    Ok(result)
}

fn export_package_contents(
    store: &Store,
    attachments: &AttachmentCoordinator,
    root: &Path,
    folder_name: String,
    exported_at_ms: i64,
) -> Result<DataExportResult, DataExportError> {
    let database_path = root.join(DATABASE_FILE_NAME);
    let inventory = store
        .prepare_data_export(&database_path)
        .map_err(|_| database_error())?;
    File::open(&database_path)
        .and_then(|file| file.sync_all())
        .map_err(|_| database_error())?;

    let attachments_root = root.join("attachments");
    create_private_directory(&attachments_root)?;
    let mut exported_paths = HashSet::new();
    let mut entries = Vec::new();
    export_record_attachments(
        &inventory,
        attachments,
        &attachments_root,
        &mut exported_paths,
        &mut entries,
    )?;
    export_draft_attachments(
        &inventory,
        attachments,
        &attachments_root,
        &mut exported_paths,
        &mut entries,
    )?;
    sync_directory(&attachments_root)?;

    let unavailable_attachments = entries
        .iter()
        .filter(|entry| entry.state == "unavailable")
        .count();
    let manifest = DataExportManifest {
        format_version: 1,
        app_version: env!("CARGO_PKG_VERSION"),
        database_schema_version: SCHEMA_VERSION,
        exported_at_ms,
        database_file: DATABASE_FILE_NAME,
        purpose: "portable-local-data-export-not-restore-backup",
        excluded_data: [
            "protected-chatgpt-profile",
            "cookies",
            "authentication-tokens",
            "local-diagnostics",
        ],
        summary: DataExportSummary {
            workspace_count: inventory.workspaces.len(),
            notebook_count: inventory.notebooks.len(),
            record_count: inventory.records.len(),
            attachment_reference_count: entries.len(),
            exported_attachment_files: exported_paths.len(),
            unavailable_attachments,
        },
        attachments: entries,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|_| {
        export_error(
            "data_export_serialization_failed",
            "无法生成导出清单；WakeGPT 现有数据未改变",
            false,
        )
    })?;
    manifest_bytes.push(b'\n');
    write_new_file(&root.join(MANIFEST_FILE_NAME), &manifest_bytes)?;
    sync_directory(root)?;

    Ok(DataExportResult {
        folder_name,
        database_file_name: DATABASE_FILE_NAME,
        workspace_count: inventory.workspaces.len(),
        notebook_count: inventory.notebooks.len(),
        record_count: inventory.records.len(),
        exported_attachment_files: exported_paths.len(),
        unavailable_attachments,
    })
}

fn export_record_attachments(
    inventory: &DataExportInventory,
    attachments: &AttachmentCoordinator,
    attachments_root: &Path,
    exported_paths: &mut HashSet<String>,
    entries: &mut Vec<DataExportAttachmentEntry>,
) -> Result<(), DataExportError> {
    let workspaces: HashMap<&str, _> = inventory
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), workspace))
        .collect();
    for record in &inventory.records {
        for attachment in &record.attachments {
            let outcome = workspaces
                .get(record.workspace_id.as_str())
                .ok_or("data_export_workspace_missing")
                .and_then(|workspace| {
                    attachments
                        .read_record_preview(workspace, attachment)
                        .map_err(|error| error.code())
                });
            let (state, relative_path, error_code) = export_attachment_bytes(
                outcome,
                &attachment.media_type,
                attachment.byte_size,
                &attachment.content_sha256,
                attachments_root,
                exported_paths,
            )?;
            entries.push(DataExportAttachmentEntry {
                source_kind: "record",
                reference_id: attachment.id.clone(),
                workspace_id: record.workspace_id.clone(),
                record_id: Some(record.id.clone()),
                tab_key: None,
                display_name: None,
                media_type: attachment.media_type.clone(),
                byte_size: attachment.byte_size,
                content_sha256: attachment.content_sha256.clone(),
                state,
                export_relative_path: relative_path,
                error_code,
            });
        }
    }
    Ok(())
}

fn export_draft_attachments(
    inventory: &DataExportInventory,
    attachments: &AttachmentCoordinator,
    attachments_root: &Path,
    exported_paths: &mut HashSet<String>,
    entries: &mut Vec<DataExportAttachmentEntry>,
) -> Result<(), DataExportError> {
    for draft in &inventory.draft_attachments {
        let outcome = attachments
            .read_pending_preview(&draft.attachment.token)
            .map_err(|error| error.code())
            .and_then(|(bytes, media_type)| {
                if media_type != draft.attachment.media_type {
                    Err("data_export_draft_attachment_changed")
                } else {
                    Ok(bytes)
                }
            });
        let (state, relative_path, error_code) = export_attachment_bytes(
            outcome,
            &draft.attachment.media_type,
            draft.attachment.byte_size,
            &draft.attachment.content_sha256,
            attachments_root,
            exported_paths,
        )?;
        entries.push(DataExportAttachmentEntry {
            source_kind: "draft",
            reference_id: draft.attachment.token.clone(),
            workspace_id: draft.workspace_id.clone(),
            record_id: None,
            tab_key: Some(draft.tab_key.clone()),
            display_name: Some(draft.attachment.display_name.clone()),
            media_type: draft.attachment.media_type.clone(),
            byte_size: draft.attachment.byte_size,
            content_sha256: draft.attachment.content_sha256.clone(),
            state,
            export_relative_path: relative_path,
            error_code,
        });
    }
    Ok(())
}

fn export_attachment_bytes(
    outcome: Result<Vec<u8>, &'static str>,
    media_type: &str,
    expected_byte_size: u64,
    expected_sha256: &str,
    attachments_root: &Path,
    exported_paths: &mut HashSet<String>,
) -> Result<(&'static str, Option<String>, Option<&'static str>), DataExportError> {
    let bytes = match outcome {
        Ok(bytes) => bytes,
        Err(code) => return Ok(("unavailable", None, Some(code))),
    };
    let extension = match attachment_extension(media_type) {
        Some(extension) => extension,
        None => return Ok(("unavailable", None, Some("attachment_type_unsupported"))),
    };
    if !valid_sha256(expected_sha256)
        || bytes.len() as u64 != expected_byte_size
        || sha256_hex(&bytes) != expected_sha256
    {
        return Ok(("unavailable", None, Some("data_export_attachment_changed")));
    }
    let relative_path = format!("attachments/{expected_sha256}.{extension}");
    if exported_paths.insert(relative_path.clone()) {
        write_new_file(
            &attachments_root.join(format!("{expected_sha256}.{extension}")),
            &bytes,
        )?;
    }
    Ok(("exported", Some(relative_path), None))
}

fn attachment_extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_export_parent(parent: &Path) -> Result<(), DataExportError> {
    if !parent.is_absolute() {
        return Err(destination_error());
    }
    let metadata = fs::symlink_metadata(parent).map_err(|_| destination_error())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(destination_error());
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), DataExportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| write_error())?;
    file.write_all(bytes).map_err(|_| write_error())?;
    file.sync_all().map_err(|_| write_error())
}

fn create_private_directory(path: &Path) -> Result<(), DataExportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(|_| write_error())?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(|_| write_error())?;

    if let Err(error) = restrict_directory_permissions(path) {
        let _ = fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

fn restrict_directory_permissions(path: &Path) -> Result<(), DataExportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| write_error())?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), DataExportError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| write_error())?;
    }
    Ok(())
}

fn current_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn export_timestamp(timestamp_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp(timestamp_ms.div_euclid(1000))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(UtcOffset::UTC)
        .format(format_description!(
            "[year][month][day]-[hour][minute][second]Z"
        ))
        .unwrap_or_else(|_| "19700101-000000Z".to_owned())
}

fn export_error(code: &'static str, message: &'static str, retryable: bool) -> DataExportError {
    DataExportError {
        code,
        message,
        retryable,
    }
}

fn destination_error() -> DataExportError {
    export_error(
        "data_export_destination_invalid",
        "请选择可写的本地文件夹；WakeGPT 未创建导出",
        false,
    )
}

fn database_error() -> DataExportError {
    export_error(
        "data_export_database_failed",
        "无法创建并验证一致的数据库快照；WakeGPT 现有数据未改变",
        true,
    )
}

fn write_error() -> DataExportError {
    export_error(
        "data_export_write_failed",
        "无法安全写入所选位置；WakeGPT 现有数据未改变",
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::INBOX_ATTACHMENT_DIRECTORY;
    use crate::storage::{DraftAttachment, MUTATION_SCHEMA_VERSION};
    use rusqlite::{Connection, OpenFlags};
    use std::path::PathBuf;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wakegpt-data-export-test-{}",
                Uuid::now_v7().simple()
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn export_contains_a_verified_database_and_only_verified_image_copies() {
        let root = TestRoot::new();
        let workspace_root = root.0.join("workspace");
        let exports_root = root.0.join("exports");
        let diagnostics_root = root.0.join("app-data/diagnostics-v1");
        let diagnostics_database = diagnostics_root.join("wakegpt-diagnostics.sqlite3");
        fs::create_dir(&workspace_root).unwrap();
        fs::create_dir(&exports_root).unwrap();
        fs::create_dir_all(&diagnostics_root).unwrap();
        fs::write(&diagnostics_database, b"private-local-diagnostics").unwrap();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending"),
            root.0.join("app-data/recovery"),
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Export", workspace_root.to_str().unwrap())
            .unwrap();

        let record_png = b"\x89PNG\r\n\x1a\nrecord-export";
        let staged_record = coordinator.stage_uploaded(record_png).unwrap();
        let prepared = coordinator
            .materialize(
                &workspace,
                INBOX_ATTACHMENT_DIRECTORY,
                std::slice::from_ref(&staged_record.token),
            )
            .unwrap();
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "portable record",
                &prepared,
                &Uuid::now_v7().to_string(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        let draft_gif = b"GIF89adraft-export";
        let staged_draft = coordinator.stage_uploaded(draft_gif).unwrap();
        let missing_draft = coordinator
            .stage_uploaded(b"RIFFxxxxWEBPmissing-draft")
            .unwrap();
        store
            .save_draft(
                &workspace.id,
                None,
                "portable draft",
                &[
                    DraftAttachment {
                        token: staged_draft.token.clone(),
                        media_type: staged_draft.media_type.clone(),
                        byte_size: staged_draft.byte_size,
                        content_sha256: staged_draft.content_sha256.clone(),
                        display_name: staged_draft.display_name.clone(),
                    },
                    DraftAttachment {
                        token: missing_draft.token.clone(),
                        media_type: missing_draft.media_type.clone(),
                        byte_size: missing_draft.byte_size,
                        content_sha256: missing_draft.content_sha256.clone(),
                        display_name: missing_draft.display_name.clone(),
                    },
                ],
            )
            .unwrap();
        coordinator.discard_pending(&missing_draft.token).unwrap();
        store.disconnect_workspace(&workspace.id).unwrap();

        let result = export_to_parent(&store, &coordinator, &exports_root).unwrap();
        assert_eq!(result.workspace_count, 1);
        assert_eq!(result.notebook_count, 0);
        assert_eq!(result.record_count, 1);
        assert_eq!(result.exported_attachment_files, 2);
        assert_eq!(result.unavailable_attachments, 1);

        let package = exports_root.join(&result.folder_name);
        assert!(package.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&package).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(package.join("attachments"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(!fs::read_dir(&exports_root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")));
        let exported_database = package.join(DATABASE_FILE_NAME);
        let connection = Connection::open_with_flags(
            exported_database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        assert_eq!(
            connection
                .query_row::<String, _, _>("PRAGMA quick_check", [], |row| row.get(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            connection
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM records", [], |row| row.get(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row::<i64, _, _>(
                    "SELECT is_connected FROM workspaces WHERE id = ?1",
                    [&workspace.id],
                    |row| row.get(0),
                )
                .unwrap(),
            0
        );

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(package.join(MANIFEST_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(manifest["formatVersion"], 1);
        assert_eq!(manifest["summary"]["attachmentReferenceCount"], 3);
        assert_eq!(manifest["summary"]["exportedAttachmentFiles"], 2);
        assert_eq!(manifest["summary"]["unavailableAttachments"], 1);
        assert_eq!(
            manifest["excludedData"],
            serde_json::json!([
                "protected-chatgpt-profile",
                "cookies",
                "authentication-tokens",
                "local-diagnostics"
            ])
        );
        assert!(diagnostics_database.is_file());
        assert!(!package.join("diagnostics-v1").exists());
        assert!(!package.join("wakegpt-diagnostics.sqlite3").exists());
        assert_eq!(
            fs::read(package.join(format!(
                "attachments/{}.png",
                record.attachments[0].content_sha256
            )))
            .unwrap(),
            record_png
        );
        assert_eq!(
            fs::read(package.join(format!("attachments/{}.gif", staged_draft.content_sha256)))
                .unwrap(),
            draft_gif
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_a_symlink_destination_without_creating_files() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new();
        let real_parent = root.0.join("real-parent");
        let alias = root.0.join("alias");
        fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &alias).unwrap();
        let store = Store::open_in_memory().unwrap();
        let coordinator = AttachmentCoordinator::new(
            root.0.join("app-data/pending"),
            root.0.join("app-data/recovery"),
        )
        .unwrap();

        let error = export_to_parent(&store, &coordinator, &alias).unwrap_err();
        assert_eq!(error.code(), "data_export_destination_invalid");
        assert_eq!(fs::read_dir(&real_parent).unwrap().count(), 0);
    }
}
