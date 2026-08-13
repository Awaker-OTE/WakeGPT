use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

const DIAGNOSTICS_DIRECTORY: &str = "diagnostics-v1";
const DATABASE_FILE_NAME: &str = "wakegpt-diagnostics.sqlite3";
const EVENTS_FILE_NAME: &str = "wakegpt-diagnostics-v1.jsonl";
const MANIFEST_FILE_NAME: &str = "wakegpt-diagnostics-manifest-v1.json";
const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;
pub(crate) const DEFAULT_RETENTION_DAYS: u32 = 14;
pub(crate) const DEFAULT_MAX_BYTES: u64 = 20 * 1024 * 1024;
const ALLOWED_RETENTION_DAYS: [u32; 4] = [1, 3, 7, 14];
const ALLOWED_MAX_BYTES: [u64; 4] = [
    1024 * 1024,
    5 * 1024 * 1024,
    10 * 1024 * 1024,
    DEFAULT_MAX_BYTES,
];
const MAX_EVENT_CODE_BYTES: usize = 96;
const MAX_CONTEXT_BYTES: usize = 2048;
const MAX_PAGE_SIZE: u32 = 100;

const CREATE_SCHEMA: &str = "CREATE TABLE diagnostic_settings (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        retention_days INTEGER NOT NULL CHECK (retention_days IN (1, 3, 7, 14)),
        max_bytes INTEGER NOT NULL CHECK (
            max_bytes IN (1048576, 5242880, 10485760, 20971520)
        ),
        updated_at_ms INTEGER NOT NULL
     ) STRICT;
     INSERT INTO diagnostic_settings (
        singleton_id, enabled, retention_days, max_bytes, updated_at_ms
     ) VALUES (1, 1, 14, 20971520, 0);
     CREATE TABLE diagnostic_events (
        id INTEGER PRIMARY KEY,
        first_at_ms INTEGER NOT NULL CHECK (first_at_ms >= 0),
        last_at_ms INTEGER NOT NULL CHECK (last_at_ms >= first_at_ms),
        severity TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error')),
        subsystem TEXT NOT NULL CHECK (subsystem IN (
            'app', 'recovery', 'codex', 'card', 'default_identity', 'lifecycle'
        )),
        code TEXT NOT NULL CHECK (
            length(code) BETWEEN 1 AND 96
            AND code NOT GLOB '*[^a-z0-9_]*'
        ),
        context_json TEXT NOT NULL CHECK (length(context_json) <= 2048),
        occurrence_count INTEGER NOT NULL CHECK (occurrence_count > 0)
     ) STRICT;
     CREATE INDEX diagnostic_events_latest
        ON diagnostic_events(last_at_ms DESC, id DESC);";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalDiagnosticsStatus {
    pub available: bool,
    pub enabled: bool,
    pub retention_days: u32,
    pub max_bytes: u64,
    pub event_count: u64,
    pub database_bytes: u64,
    pub earliest_at_ms: Option<i64>,
    pub latest_at_ms: Option<i64>,
    pub last_error_code: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalDiagnosticEvent {
    pub id: i64,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
    pub severity: String,
    pub subsystem: String,
    pub code: String,
    pub context: Value,
    pub occurrence_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalDiagnosticsPage {
    pub events: Vec<LocalDiagnosticEvent>,
    pub next_cursor: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalDiagnosticsExportResult {
    pub folder_name: String,
    pub event_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexDiagnosticCounts {
    pub detected_instances: usize,
    pub connectable_instances: usize,
    pub available_targets: usize,
    pub connected_targets: usize,
    pub failed_targets: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalDiagnosticsError {
    Unavailable,
    InvalidSettings,
    InvalidPage,
    InvalidDestination,
    WriteFailed,
    SerializationFailed,
}

impl LocalDiagnosticsError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "local_diagnostics_unavailable",
            Self::InvalidSettings => "local_diagnostics_settings_invalid",
            Self::InvalidPage => "local_diagnostics_page_invalid",
            Self::InvalidDestination => "local_diagnostics_destination_invalid",
            Self::WriteFailed => "local_diagnostics_export_failed",
            Self::SerializationFailed => "local_diagnostics_serialization_failed",
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "本地诊断存储暂时不可用；WakeGPT 其他功能不受影响",
            Self::InvalidSettings => "诊断保留设置无效；原设置保持不变",
            Self::InvalidPage => "诊断分页请求无效",
            Self::InvalidDestination => "请选择可写的本地文件夹；WakeGPT 未创建诊断导出",
            Self::WriteFailed => "无法安全写入诊断导出；现有诊断没有改变",
            Self::SerializationFailed => "无法生成脱敏诊断导出；现有诊断没有改变",
        }
    }

    pub(crate) const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::WriteFailed)
    }
}

pub(crate) struct LocalDiagnostics {
    directory: PathBuf,
    database_path: PathBuf,
    last_error_code: Mutex<Option<&'static str>>,
    edge_fingerprints: Mutex<HashMap<&'static str, String>>,
}

impl LocalDiagnostics {
    pub(crate) fn new(app_data_dir: &Path) -> Self {
        let directory = app_data_dir.join(DIAGNOSTICS_DIRECTORY);
        let database_path = directory.join(DATABASE_FILE_NAME);
        let diagnostics = Self {
            directory,
            database_path,
            last_error_code: Mutex::new(None),
            edge_fingerprints: Mutex::new(HashMap::new()),
        };
        if diagnostics.initialize().is_err() {
            diagnostics.mark_error(LocalDiagnosticsError::Unavailable.code());
        }
        diagnostics
    }

    pub(crate) fn status(&self) -> LocalDiagnosticsStatus {
        match self.status_result() {
            Ok(status) => {
                self.mark_ready();
                status
            }
            Err(error) => {
                self.mark_error(error.code());
                LocalDiagnosticsStatus {
                    available: false,
                    enabled: false,
                    retention_days: DEFAULT_RETENTION_DAYS,
                    max_bytes: DEFAULT_MAX_BYTES,
                    event_count: 0,
                    database_bytes: database_size(&self.database_path),
                    earliest_at_ms: None,
                    latest_at_ms: None,
                    last_error_code: self.last_error(),
                }
            }
        }
    }

    pub(crate) fn update_settings(
        &self,
        enabled: bool,
        retention_days: u32,
        max_bytes: u64,
    ) -> Result<LocalDiagnosticsStatus, LocalDiagnosticsError> {
        if !ALLOWED_RETENTION_DAYS.contains(&retention_days)
            || !ALLOWED_MAX_BYTES.contains(&max_bytes)
        {
            return Err(LocalDiagnosticsError::InvalidSettings);
        }
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE diagnostic_settings
                 SET enabled = ?1, retention_days = ?2, max_bytes = ?3, updated_at_ms = ?4
                 WHERE singleton_id = 1",
                params![
                    enabled,
                    retention_days,
                    i64::try_from(max_bytes).unwrap_or(i64::MAX),
                    current_time_ms()
                ],
            )
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        compact(&connection, &self.database_path, current_time_ms())?;
        self.status_result()
    }

    pub(crate) fn list(
        &self,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<LocalDiagnosticsPage, LocalDiagnosticsError> {
        if limit == 0 || limit > MAX_PAGE_SIZE || before_id.is_some_and(|id| id <= 0) {
            return Err(LocalDiagnosticsError::InvalidPage);
        }
        let connection = self.connection()?;
        compact(&connection, &self.database_path, current_time_ms())?;
        let query_limit = limit + 1;
        let mut events = if let Some(before_id) = before_id {
            read_events(
                &connection,
                "SELECT id, first_at_ms, last_at_ms, severity, subsystem, code,
                        context_json, occurrence_count
                 FROM diagnostic_events WHERE id < ?1
                 ORDER BY id DESC LIMIT ?2",
                params![before_id, query_limit],
            )?
        } else {
            read_events(
                &connection,
                "SELECT id, first_at_ms, last_at_ms, severity, subsystem, code,
                        context_json, occurrence_count
                 FROM diagnostic_events
                 ORDER BY id DESC LIMIT ?1",
                params![query_limit],
            )?
        };
        let has_more = events.len() > limit as usize;
        if has_more {
            events.truncate(limit as usize);
        }
        let next_cursor = if has_more {
            events.last().map(|event| event.id)
        } else {
            None
        };
        events.shrink_to_fit();
        Ok(LocalDiagnosticsPage {
            events,
            next_cursor,
        })
    }

    pub(crate) fn clear(&self) -> Result<LocalDiagnosticsStatus, LocalDiagnosticsError> {
        let connection = self.connection()?;
        connection
            .pragma_update(None, "secure_delete", "ON")
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        connection
            .execute("DELETE FROM diagnostic_events", [])
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        connection
            .execute_batch("VACUUM;")
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        let remaining: i64 = connection
            .query_row("SELECT COUNT(*) FROM diagnostic_events", [], |row| {
                row.get(0)
            })
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if remaining != 0 {
            return Err(LocalDiagnosticsError::Unavailable);
        }
        if let Ok(mut edges) = self.edge_fingerprints.lock() {
            edges.clear();
        }
        self.status_result()
    }

    pub(crate) fn export_to_parent(
        &self,
        parent: &Path,
    ) -> Result<LocalDiagnosticsExportResult, LocalDiagnosticsError> {
        validate_export_parent(parent)?;
        let exported_at_ms = current_time_ms();
        let identifier = Uuid::now_v7().simple().to_string();
        let folder_name = format!(
            "WakeGPT-diagnostics-{}-{}",
            export_timestamp(exported_at_ms),
            &identifier[..8]
        );
        let final_root = parent.join(&folder_name);
        let staging_root = parent.join(format!(".wakegpt-diagnostics-{identifier}.partial"));
        if final_root.exists() || staging_root.exists() {
            return Err(LocalDiagnosticsError::InvalidDestination);
        }
        create_private_directory(&staging_root)?;
        let result = self.export_contents(&staging_root, folder_name.clone(), exported_at_ms);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(error);
            }
        };
        if fs::rename(&staging_root, &final_root).is_err() {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(LocalDiagnosticsError::WriteFailed);
        }
        let _ = sync_directory(parent);
        Ok(result)
    }

    pub(crate) fn record_startup(
        &self,
        schema_version: u32,
        recovered_operations: usize,
        pending_operations: usize,
    ) {
        self.record(
            "info",
            "app",
            "app_started",
            json!({
                "schemaVersion": schema_version,
                "recoveredOperations": recovered_operations,
                "pendingOperations": pending_operations,
            }),
        );
    }

    pub(crate) fn record_recovery(
        &self,
        recovered_operations: usize,
        pending_operations: usize,
        error_code: Option<&'static str>,
    ) {
        let code = error_code.unwrap_or(if pending_operations == 0 {
            "recovery_completed"
        } else {
            "recovery_pending"
        });
        self.record(
            if error_code.is_some() || pending_operations > 0 {
                "warning"
            } else {
                "info"
            },
            "recovery",
            code,
            json!({
                "recoveredOperations": recovered_operations,
                "pendingOperations": pending_operations,
            }),
        );
    }

    pub(crate) fn record_codex_state(
        &self,
        state: &'static str,
        error_code: Option<&'static str>,
        counts: CodexDiagnosticCounts,
    ) {
        if !matches!(
            state,
            "not_detected"
                | "stale_endpoint"
                | "unmanaged_target_available"
                | "unmanaged_target_incompatible"
                | "connecting"
                | "connected"
                | "partially_connected"
                | "paused"
                | "restart_required"
                | "injection_failed"
        ) {
            return;
        }
        self.record_edge(
            if error_code.is_some() || counts.failed_targets > 0 {
                "warning"
            } else {
                "info"
            },
            "codex",
            error_code.unwrap_or("codex_state_changed"),
            json!({
                "state": state,
                "detectedInstances": counts.detected_instances,
                "connectableInstances": counts.connectable_instances,
                "availableTargets": counts.available_targets,
                "connectedTargets": counts.connected_targets,
                "failedTargets": counts.failed_targets,
            }),
        );
    }

    pub(crate) fn record_card_visibility(
        &self,
        visibility: &'static str,
        layout: Option<&'static str>,
    ) {
        if !matches!(
            visibility,
            "visible"
                | "context_missing"
                | "fullscreen"
                | "modal"
                | "viewport_overlay"
                | "media_lightbox"
                | "context_ambiguous"
        ) || layout
            .is_some_and(|layout| !matches!(layout, "full" | "compact" | "collapsed" | "drawer"))
        {
            return;
        }
        self.record_edge(
            if visibility == "context_ambiguous" {
                "warning"
            } else {
                "info"
            },
            "card",
            "card_visibility_changed",
            json!({ "visibility": visibility, "layout": layout }),
        );
    }

    pub(crate) fn record_default_identity(
        &self,
        state: &'static str,
        error_code: Option<&'static str>,
    ) {
        if !matches!(state, "stopped" | "running" | "occupied" | "unavailable") {
            return;
        }
        self.record_edge(
            if error_code.is_some() {
                "warning"
            } else {
                "info"
            },
            "default_identity",
            error_code.unwrap_or("default_identity_state_changed"),
            json!({ "state": state }),
        );
    }

    pub(crate) fn record_lifecycle_failure(
        &self,
        operation: &'static str,
        error_code: &'static str,
    ) {
        if !matches!(
            operation,
            "hide_main_window"
                | "reopen_main_window"
                | "open_main_window"
                | "open_quick_capture_window"
                | "hide_quick_capture_window"
                | "emit_codex_integration_state"
                | "toggle_codex_integration"
        ) {
            return;
        }
        self.record(
            "error",
            "lifecycle",
            error_code,
            json!({ "operation": operation }),
        );
    }

    fn export_contents(
        &self,
        root: &Path,
        folder_name: String,
        exported_at_ms: i64,
    ) -> Result<LocalDiagnosticsExportResult, LocalDiagnosticsError> {
        let connection = self.connection()?;
        compact(&connection, &self.database_path, exported_at_ms)?;
        let events = read_events(
            &connection,
            "SELECT id, first_at_ms, last_at_ms, severity, subsystem, code,
                    context_json, occurrence_count
             FROM diagnostic_events ORDER BY id",
            [],
        )?;
        let status = status_from_connection(&connection, &self.database_path, self.last_error())?;
        let mut event_bytes = Vec::new();
        for event in &events {
            serde_json::to_writer(&mut event_bytes, event)
                .map_err(|_| LocalDiagnosticsError::SerializationFailed)?;
            event_bytes.push(b'\n');
        }
        write_new_file(&root.join(EVENTS_FILE_NAME), &event_bytes)?;

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Manifest<'a> {
            format_version: u32,
            app_version: &'static str,
            exported_at_ms: i64,
            purpose: &'static str,
            events_file: &'a str,
            event_count: usize,
            retention_days: u32,
            max_bytes: u64,
            excluded_data: [&'static str; 8],
        }
        let manifest = Manifest {
            format_version: 1,
            app_version: env!("CARGO_PKG_VERSION"),
            exported_at_ms,
            purpose: "local-diagnostics-export",
            events_file: EVENTS_FILE_NAME,
            event_count: events.len(),
            retention_days: status.retention_days,
            max_bytes: status.max_bytes,
            excluded_data: [
                "record-bodies",
                "markdown-file-contents",
                "image-bytes",
                "absolute-paths",
                "account-identities",
                "cookies",
                "authentication-tokens",
                "network-addresses",
            ],
        };
        let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|_| LocalDiagnosticsError::SerializationFailed)?;
        manifest_bytes.push(b'\n');
        write_new_file(&root.join(MANIFEST_FILE_NAME), &manifest_bytes)?;
        sync_directory(root)?;
        Ok(LocalDiagnosticsExportResult {
            folder_name,
            event_count: events.len(),
        })
    }

    fn initialize(&self) -> Result<(), LocalDiagnosticsError> {
        ensure_private_directory(&self.directory)?;
        if let Ok(metadata) = fs::symlink_metadata(&self.database_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(LocalDiagnosticsError::Unavailable);
            }
        }
        let connection = open_connection(&self.database_path)?;
        let current: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if current == 0 {
            connection
                .pragma_update(None, "auto_vacuum", "FULL")
                .map_err(|_| LocalDiagnosticsError::Unavailable)?;
            connection
                .execute_batch(CREATE_SCHEMA)
                .map_err(|_| LocalDiagnosticsError::Unavailable)?;
            connection
                .pragma_update(None, "user_version", DIAGNOSTICS_SCHEMA_VERSION)
                .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        } else if current != DIAGNOSTICS_SCHEMA_VERSION {
            return Err(LocalDiagnosticsError::Unavailable);
        }
        restrict_file_permissions(&self.database_path)?;
        let quick_check: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if quick_check != "ok" {
            return Err(LocalDiagnosticsError::Unavailable);
        }
        compact(&connection, &self.database_path, current_time_ms())
    }

    fn connection(&self) -> Result<Connection, LocalDiagnosticsError> {
        if let Ok(metadata) = fs::symlink_metadata(&self.database_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(LocalDiagnosticsError::Unavailable);
            }
        }
        let connection = open_connection(&self.database_path)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if version != DIAGNOSTICS_SCHEMA_VERSION {
            return Err(LocalDiagnosticsError::Unavailable);
        }
        Ok(connection)
    }

    fn status_result(&self) -> Result<LocalDiagnosticsStatus, LocalDiagnosticsError> {
        let connection = self.connection()?;
        compact(&connection, &self.database_path, current_time_ms())?;
        status_from_connection(&connection, &self.database_path, self.last_error())
    }

    fn record(
        &self,
        severity: &'static str,
        subsystem: &'static str,
        code: &'static str,
        context: Value,
    ) -> bool {
        if !valid_code(code) {
            return false;
        }
        let context_json = match serde_json::to_string(&context) {
            Ok(context_json) if context_json.len() <= MAX_CONTEXT_BYTES => context_json,
            _ => return false,
        };
        let result = self.record_at(current_time_ms(), severity, subsystem, code, &context_json);
        match result {
            Ok(written) => {
                self.mark_ready();
                written
            }
            Err(error) => {
                self.mark_error(error.code());
                false
            }
        }
    }

    fn record_edge(
        &self,
        severity: &'static str,
        subsystem: &'static str,
        code: &'static str,
        context: Value,
    ) {
        let context_json = match serde_json::to_string(&context) {
            Ok(context_json) if context_json.len() <= MAX_CONTEXT_BYTES => context_json,
            _ => return,
        };
        let fingerprint = format!("{severity}\n{code}\n{context_json}");
        if self
            .edge_fingerprints
            .lock()
            .ok()
            .and_then(|edges| edges.get(subsystem).cloned())
            .as_deref()
            == Some(fingerprint.as_str())
        {
            return;
        }
        if self.record(severity, subsystem, code, context) {
            if let Ok(mut edges) = self.edge_fingerprints.lock() {
                edges.insert(subsystem, fingerprint);
            }
        }
    }

    fn record_at(
        &self,
        timestamp_ms: i64,
        severity: &str,
        subsystem: &str,
        code: &str,
        context_json: &str,
    ) -> Result<bool, LocalDiagnosticsError> {
        if timestamp_ms < 0
            || !matches!(severity, "info" | "warning" | "error")
            || !matches!(
                subsystem,
                "app" | "recovery" | "codex" | "card" | "default_identity" | "lifecycle"
            )
            || !valid_code(code)
            || context_json.len() > MAX_CONTEXT_BYTES
        {
            return Ok(false);
        }
        let connection = self.connection()?;
        let (enabled, _, _): (bool, u32, u64) = diagnostic_settings(&connection)?;
        if !enabled {
            return Ok(false);
        }
        compact(&connection, &self.database_path, timestamp_ms)?;
        let latest: Option<(i64, String, String, String, String)> = connection
            .query_row(
                "SELECT id, severity, subsystem, code, context_json
                 FROM diagnostic_events ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if let Some((id, old_severity, old_subsystem, old_code, old_context)) = latest {
            if old_severity == severity
                && old_subsystem == subsystem
                && old_code == code
                && old_context == context_json
            {
                connection
                    .execute(
                        "UPDATE diagnostic_events
                         SET last_at_ms = MAX(last_at_ms, ?2),
                             occurrence_count = occurrence_count + 1
                         WHERE id = ?1",
                        params![id, timestamp_ms],
                    )
                    .map_err(|_| LocalDiagnosticsError::Unavailable)?;
                compact(&connection, &self.database_path, timestamp_ms)?;
                return Ok(true);
            }
        }
        connection
            .execute(
                "INSERT INTO diagnostic_events (
                    first_at_ms, last_at_ms, severity, subsystem, code,
                    context_json, occurrence_count
                 ) VALUES (?1, ?1, ?2, ?3, ?4, ?5, 1)",
                params![timestamp_ms, severity, subsystem, code, context_json],
            )
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        compact(&connection, &self.database_path, timestamp_ms)?;
        Ok(true)
    }

    fn mark_error(&self, code: &'static str) {
        if let Ok(mut current) = self.last_error_code.lock() {
            *current = Some(code);
        }
    }

    fn mark_ready(&self) {
        if let Ok(mut current) = self.last_error_code.lock() {
            *current = None;
        }
    }

    fn last_error(&self) -> Option<&'static str> {
        self.last_error_code
            .lock()
            .ok()
            .and_then(|current| *current)
    }
}

fn open_connection(path: &Path) -> Result<Connection, LocalDiagnosticsError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(1000))
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    connection
        .pragma_update(None, "secure_delete", "ON")
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    connection
        .pragma_update(None, "trusted_schema", "OFF")
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    Ok(connection)
}

fn diagnostic_settings(connection: &Connection) -> Result<(bool, u32, u64), LocalDiagnosticsError> {
    let (enabled, retention_days, max_bytes): (bool, u32, i64) = connection
        .query_row(
            "SELECT enabled, retention_days, max_bytes
             FROM diagnostic_settings WHERE singleton_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    let max_bytes = u64::try_from(max_bytes).map_err(|_| LocalDiagnosticsError::Unavailable)?;
    Ok((enabled, retention_days, max_bytes))
}

fn status_from_connection(
    connection: &Connection,
    database_path: &Path,
    last_error_code: Option<&'static str>,
) -> Result<LocalDiagnosticsStatus, LocalDiagnosticsError> {
    let (enabled, retention_days, max_bytes) = diagnostic_settings(connection)?;
    let (event_count, earliest_at_ms, latest_at_ms): (i64, Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT COUNT(*), MIN(first_at_ms), MAX(last_at_ms) FROM diagnostic_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    Ok(LocalDiagnosticsStatus {
        available: true,
        enabled,
        retention_days,
        max_bytes,
        event_count: u64::try_from(event_count).unwrap_or(0),
        database_bytes: database_size(database_path),
        earliest_at_ms,
        latest_at_ms,
        last_error_code,
    })
}

fn compact(
    connection: &Connection,
    database_path: &Path,
    now_ms: i64,
) -> Result<(), LocalDiagnosticsError> {
    let (_, retention_days, max_bytes) = diagnostic_settings(connection)?;
    let retention_ms = i64::from(retention_days).saturating_mul(24 * 60 * 60 * 1000);
    let cutoff = now_ms.saturating_sub(retention_ms);
    connection
        .execute(
            "DELETE FROM diagnostic_events WHERE last_at_ms < ?1",
            [cutoff],
        )
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    for _ in 0..4096 {
        if database_size(database_path) <= max_bytes {
            return Ok(());
        }
        let deleted = connection
            .execute(
                "DELETE FROM diagnostic_events
                 WHERE id IN (SELECT id FROM diagnostic_events ORDER BY id LIMIT 64)",
                [],
            )
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
        if deleted == 0 {
            break;
        }
    }
    if database_size(database_path) > max_bytes {
        connection
            .execute_batch("VACUUM;")
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    }
    if database_size(database_path) > max_bytes {
        return Err(LocalDiagnosticsError::Unavailable);
    }
    Ok(())
}

fn read_events<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<LocalDiagnosticEvent>, LocalDiagnosticsError>
where
    P: rusqlite::Params,
{
    let mut statement = connection
        .prepare(sql)
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    let rows = statement
        .query_map(parameters, |row| {
            let context_json: String = row.get(6)?;
            let context = serde_json::from_str(&context_json).unwrap_or(Value::Null);
            let occurrence_count: i64 = row.get(7)?;
            Ok(LocalDiagnosticEvent {
                id: row.get(0)?,
                first_at_ms: row.get(1)?,
                last_at_ms: row.get(2)?,
                severity: row.get(3)?,
                subsystem: row.get(4)?,
                code: row.get(5)?,
                context,
                occurrence_count: u64::try_from(occurrence_count).unwrap_or(0),
            })
        })
        .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| LocalDiagnosticsError::Unavailable)
}

fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_EVENT_CODE_BYTES
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn database_size(database_path: &Path) -> u64 {
    [
        database_path.to_path_buf(),
        sidecar_path(database_path, "-journal"),
        sidecar_path(database_path, "-wal"),
        sidecar_path(database_path, "-shm"),
    ]
    .iter()
    .filter_map(|path| fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn ensure_private_directory(path: &Path) -> Result<(), LocalDiagnosticsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(LocalDiagnosticsError::Unavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder
                    .create(path)
                    .map_err(|_| LocalDiagnosticsError::Unavailable)?;
            }
            #[cfg(not(unix))]
            fs::create_dir(path).map_err(|_| LocalDiagnosticsError::Unavailable)?;
        }
        Err(_) => return Err(LocalDiagnosticsError::Unavailable),
    }
    restrict_directory_permissions(path)
}

fn create_private_directory(path: &Path) -> Result<(), LocalDiagnosticsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(path)
            .map_err(|_| LocalDiagnosticsError::WriteFailed)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(|_| LocalDiagnosticsError::WriteFailed)?;
    restrict_directory_permissions(path).map_err(|_| LocalDiagnosticsError::WriteFailed)
}

fn restrict_directory_permissions(path: &Path) -> Result<(), LocalDiagnosticsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    }
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> Result<(), LocalDiagnosticsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| LocalDiagnosticsError::Unavailable)?;
    }
    Ok(())
}

fn validate_export_parent(parent: &Path) -> Result<(), LocalDiagnosticsError> {
    if !parent.is_absolute() {
        return Err(LocalDiagnosticsError::InvalidDestination);
    }
    let metadata =
        fs::symlink_metadata(parent).map_err(|_| LocalDiagnosticsError::InvalidDestination)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalDiagnosticsError::InvalidDestination);
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), LocalDiagnosticsError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| LocalDiagnosticsError::WriteFailed)?;
    file.write_all(bytes)
        .map_err(|_| LocalDiagnosticsError::WriteFailed)?;
    file.sync_all()
        .map_err(|_| LocalDiagnosticsError::WriteFailed)?;
    restrict_file_permissions(path).map_err(|_| LocalDiagnosticsError::WriteFailed)
}

fn sync_directory(path: &Path) -> Result<(), LocalDiagnosticsError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| LocalDiagnosticsError::WriteFailed)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wakegpt-diagnostics-test-{}",
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
    fn diagnostics_are_private_bounded_and_merge_identical_edges() {
        let root = TestRoot::new();
        let diagnostics = LocalDiagnostics::new(&root.0);
        assert!(diagnostics.status().available);
        diagnostics.update_settings(true, 14, 1024 * 1024).unwrap();
        let now = current_time_ms();
        diagnostics
            .record_at(now, "info", "app", "app_started", "{}")
            .unwrap();
        diagnostics
            .record_at(now + 1, "info", "app", "app_started", "{}")
            .unwrap();
        let page = diagnostics.list(None, 20).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].occurrence_count, 2);
        assert_eq!(page.events[0].first_at_ms, now);
        assert_eq!(page.events[0].last_at_ms, now + 1);
        assert!(diagnostics.status().database_bytes <= 1024 * 1024);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(root.0.join(DIAGNOSTICS_DIRECTORY))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&diagnostics.database_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn retention_settings_prune_old_events_and_disabled_logging_stays_quiet() {
        let root = TestRoot::new();
        let diagnostics = LocalDiagnostics::new(&root.0);
        let now = current_time_ms();
        let day_ms = 24 * 60 * 60 * 1000;
        diagnostics
            .record_at(
                now - 2 * day_ms,
                "warning",
                "recovery",
                "recovery_pending",
                "{}",
            )
            .unwrap();
        diagnostics
            .record_at(now, "info", "recovery", "recovery_completed", "{}")
            .unwrap();
        diagnostics
            .update_settings(true, 1, DEFAULT_MAX_BYTES)
            .unwrap();
        diagnostics
            .record_at(now + 1, "info", "app", "settings_changed", "{}")
            .unwrap();
        assert_eq!(diagnostics.status().event_count, 2);
        diagnostics
            .update_settings(false, 1, DEFAULT_MAX_BYTES)
            .unwrap();
        diagnostics.record_startup(17, 0, 0);
        assert_eq!(diagnostics.status().event_count, 2);
    }

    #[test]
    fn settings_and_page_boundaries_fail_closed() {
        let root = TestRoot::new();
        let diagnostics = LocalDiagnostics::new(&root.0);
        assert!(matches!(
            diagnostics.update_settings(true, 30, DEFAULT_MAX_BYTES),
            Err(LocalDiagnosticsError::InvalidSettings)
        ));
        assert!(matches!(
            diagnostics.update_settings(true, 14, 2 * 1024 * 1024),
            Err(LocalDiagnosticsError::InvalidSettings)
        ));
        assert!(matches!(
            diagnostics.list(None, 0),
            Err(LocalDiagnosticsError::InvalidPage)
        ));
        assert!(matches!(
            diagnostics.list(Some(0), 20),
            Err(LocalDiagnosticsError::InvalidPage)
        ));
        assert!(matches!(
            diagnostics.list(None, MAX_PAGE_SIZE + 1),
            Err(LocalDiagnosticsError::InvalidPage)
        ));

        let now = current_time_ms();
        diagnostics
            .record_at(now, "info", "app", "first_event", "{}")
            .unwrap();
        diagnostics
            .record_at(now + 1, "info", "app", "second_event", "{}")
            .unwrap();
        let exact_page = diagnostics.list(None, 2).unwrap();
        assert_eq!(exact_page.events.len(), 2);
        assert_eq!(exact_page.next_cursor, None);
        diagnostics
            .record_at(now + 2, "info", "app", "third_event", "{}")
            .unwrap();
        let bounded_page = diagnostics.list(None, 2).unwrap();
        assert_eq!(bounded_page.events.len(), 2);
        assert_eq!(
            bounded_page.next_cursor,
            bounded_page.events.last().map(|event| event.id)
        );
    }

    #[test]
    fn untrusted_canaries_are_rejected_and_never_enter_the_database() {
        let root = TestRoot::new();
        let diagnostics = LocalDiagnostics::new(&root.0);
        let email_canary = ["secret", "example.invalid"].join("@");
        diagnostics
            .record_at(
                100,
                "error",
                "lifecycle",
                &format!("contains_{email_canary}_/example/private/Cookie_token"),
                r#"{"body":"private markdown","path":"/example/private/file.md"}"#,
            )
            .unwrap();
        assert_eq!(diagnostics.status().event_count, 0);
        let bytes = fs::read(&diagnostics.database_path).unwrap();
        for canary in [
            b"private markdown".as_slice(),
            b"/example/private".as_slice(),
            email_canary.as_bytes(),
            b"Cookie_token".as_slice(),
        ] {
            assert!(!bytes.windows(canary.len()).any(|window| window == canary));
        }
    }

    #[test]
    fn clear_scrubs_deleted_event_bytes_and_preserves_settings() {
        let root = TestRoot::new();
        let diagnostics = LocalDiagnostics::new(&root.0);
        let connection = diagnostics.connection().unwrap();
        connection
            .execute(
                "INSERT INTO diagnostic_events (
                    first_at_ms, last_at_ms, severity, subsystem, code,
                    context_json, occurrence_count
                 ) VALUES (1, 1, 'error', 'app', 'synthetic_canary', ?1, 1)",
                [r#"{"canary":"WAKEGPT_DIAGNOSTIC_PRIVATE_CANARY"}"#],
            )
            .unwrap();
        drop(connection);
        diagnostics.record_card_visibility("media_lightbox", None);
        diagnostics
            .update_settings(false, 3, 5 * 1024 * 1024)
            .unwrap();
        let status = diagnostics.clear().unwrap();
        assert_eq!(status.event_count, 0);
        assert!(!status.enabled);
        assert_eq!(status.retention_days, 3);
        assert_eq!(status.max_bytes, 5 * 1024 * 1024);
        diagnostics.record_card_visibility("media_lightbox", None);
        assert_eq!(diagnostics.status().event_count, 0);
        diagnostics
            .update_settings(true, 3, 5 * 1024 * 1024)
            .unwrap();
        diagnostics.record_card_visibility("media_lightbox", None);
        assert_eq!(diagnostics.status().event_count, 1);
        for path in [
            diagnostics.database_path.clone(),
            sidecar_path(&diagnostics.database_path, "-journal"),
            sidecar_path(&diagnostics.database_path, "-wal"),
            sidecar_path(&diagnostics.database_path, "-shm"),
        ] {
            let Ok(bytes) = fs::read(path) else { continue };
            let canary = b"WAKEGPT_DIAGNOSTIC_PRIVATE_CANARY";
            assert!(!bytes.windows(canary.len()).any(|window| window == canary));
        }
    }

    #[test]
    fn export_contains_only_logical_events_and_a_redaction_manifest() {
        let root = TestRoot::new();
        let exports = root.0.join("exports");
        fs::create_dir(&exports).unwrap();
        let diagnostics = LocalDiagnostics::new(&root.0);
        diagnostics.record_card_visibility("media_lightbox", None);
        let result = diagnostics.export_to_parent(&exports).unwrap();
        assert_eq!(result.event_count, 1);
        let package = exports.join(result.folder_name);
        assert!(package.join(EVENTS_FILE_NAME).is_file());
        assert!(!package.join(DATABASE_FILE_NAME).exists());
        let manifest: Value =
            serde_json::from_slice(&fs::read(package.join(MANIFEST_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(manifest["purpose"], "local-diagnostics-export");
        assert_eq!(manifest["eventCount"], 1);
        assert!(manifest["excludedData"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "record-bodies"));
        assert!(!fs::read_dir(&exports).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&package).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(package.join(EVENTS_FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn broken_storage_never_blocks_construction() {
        let root = TestRoot::new();
        fs::write(root.0.join(DIAGNOSTICS_DIRECTORY), b"not a directory").unwrap();
        let diagnostics = LocalDiagnostics::new(&root.0);
        let status = diagnostics.status();
        assert!(!status.available);
        assert_eq!(
            status.last_error_code,
            Some("local_diagnostics_unavailable")
        );
        diagnostics.record_startup(17, 0, 0);
    }
}
