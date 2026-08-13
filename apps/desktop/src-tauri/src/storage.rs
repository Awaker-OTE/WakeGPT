use crate::domain::{
    default_attachment_directory, managed_attachment_relative_path, new_id,
    validate_attachment_directory, validate_display_name, validate_id,
    validate_managed_attachment_relative_path, validate_notebook_relative_path,
    validate_notebook_scan_ignore_directories, validate_numbering_start, validate_record_markdown,
    validate_record_trash_retention_days, validate_workspace_root, Attachment, AttachmentFileState,
    AttachmentRelocationState, DefaultIdentitySlot, DomainError, MarkdownLayout, Notebook,
    NotebookTargetState, NumberingStyle, ProductSettings, Record, RecordState, SubmitShortcut,
    SyncState, ThemePreference, UiPreferences, UpdateSettings, Workspace, WorkspaceOpenPreference,
    DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES, INBOX_ATTACHMENT_DIRECTORY,
};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::ops::{Deref, DerefMut};
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: u32 = 21;
pub const MUTATION_SCHEMA_VERSION: u32 = 1;
const LEGACY_SCHEMA_VERSION: u32 = 11;
const DEFAULT_IDENTITY_SCHEMA_VERSION: u32 = 12;
const WORKSPACE_CONNECTION_SCHEMA_VERSION: u32 = 13;
const DRAFT_CREATE_ATTEMPT_SCHEMA_VERSION: u32 = 14;
const NOTEBOOK_CONFLICT_SCHEMA_VERSION: u32 = 15;
const ATTACHMENT_SET_REVISION_SCHEMA_VERSION: u32 = 16;
const COMPOSER_RECEIPT_LATEST_SCHEMA_VERSION: u32 = 17;
const UPDATE_SETTINGS_SCHEMA_VERSION: u32 = 18;
const UPDATE_NETWORK_CONTROLS_SCHEMA_VERSION: u32 = 19;
const PRODUCT_SETTINGS_SCHEMA_VERSION: u32 = 20;
pub(crate) const QUICK_CAPTURE_DRAFT_SURFACE: &str = "quick_capture";

const CREATE_WORKSPACE_OPEN_PREFERENCES: &str =
    "CREATE TABLE IF NOT EXISTS workspace_open_preferences (
        workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
        default_notebook_id TEXT REFERENCES notebooks(id) ON DELETE SET NULL,
        use_last_selection INTEGER NOT NULL DEFAULT 1
            CHECK (use_last_selection IN (0, 1)),
        updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
        CHECK (use_last_selection = 0 OR default_notebook_id IS NULL)
     ) STRICT;";

const CREATE_PRODUCT_SETTINGS_FRESH: &str = "CREATE TABLE IF NOT EXISTS product_settings (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        record_trash_retention_days INTEGER CHECK (
            record_trash_retention_days IS NULL
            OR record_trash_retention_days BETWEEN 1 AND 3650
        ),
        notebook_scan_ignore_directories_json TEXT NOT NULL CHECK (
            length(notebook_scan_ignore_directories_json) BETWEEN 2 AND 65536
        ),
        codex_integration_paused INTEGER NOT NULL DEFAULT 0
            CHECK (codex_integration_paused IN (0, 1)),
        updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
     ) STRICT;
     INSERT OR IGNORE INTO product_settings (
        singleton_id, record_trash_retention_days,
        notebook_scan_ignore_directories_json, codex_integration_paused, updated_at_ms
     ) VALUES (1, 30, '[\".git\",\".hg\",\".svn\",\".wakegpt\",\".next\",\"node_modules\",\"target\",\"dist\",\"build\"]', 0, 0);";

const CREATE_PRODUCT_SETTINGS_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS product_settings (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        record_trash_retention_days INTEGER CHECK (
            record_trash_retention_days IS NULL
            OR record_trash_retention_days BETWEEN 1 AND 3650
        ),
        notebook_scan_ignore_directories_json TEXT NOT NULL CHECK (
            length(notebook_scan_ignore_directories_json) BETWEEN 2 AND 65536
        ),
        codex_integration_paused INTEGER NOT NULL DEFAULT 0
            CHECK (codex_integration_paused IN (0, 1)),
        updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
     ) STRICT;
     INSERT OR IGNORE INTO product_settings (
        singleton_id, record_trash_retention_days,
        notebook_scan_ignore_directories_json, codex_integration_paused, updated_at_ms
     ) VALUES (1, NULL, '[\".git\",\".hg\",\".svn\",\".wakegpt\",\".next\",\"node_modules\",\"target\",\"dist\",\"build\"]', 0, 0);";

const CREATE_COMPOSER_RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS composer_receipts (
        id TEXT PRIMARY KEY,
        record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
        host_kind TEXT NOT NULL,
        state TEXT NOT NULL,
        detail_json TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
     ) STRICT;";

const CREATE_COMPOSER_RECEIPT_LATEST_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS composer_receipts_by_host_record_latest
        ON composer_receipts(host_kind, record_id, created_at_ms DESC, id DESC);";

const ADD_ATTACHMENT_SET_REVISION_STATE: &str = "ALTER TABLE attachments
        ADD COLUMN ordinal INTEGER NOT NULL DEFAULT 0 CHECK (ordinal >= 0);
     ALTER TABLE attachments
        ADD COLUMN membership_state TEXT NOT NULL DEFAULT 'active'
        CHECK (membership_state IN ('active', 'detaching', 'detached'));
     UPDATE attachments AS current
     SET ordinal = (
        SELECT COUNT(*) - 1
        FROM attachments AS earlier
        WHERE earlier.record_id = current.record_id
          AND (
            earlier.created_at_ms < current.created_at_ms
            OR (
                earlier.created_at_ms = current.created_at_ms
                AND earlier.id <= current.id
            )
          )
     );";

const ADD_WORKSPACE_CONNECTION_STATE: &str = "ALTER TABLE workspaces
        ADD COLUMN is_connected INTEGER NOT NULL DEFAULT 1
        CHECK (is_connected IN (0, 1));";

const CREATE_DEFAULT_IDENTITY_SLOT: &str = "CREATE TABLE IF NOT EXISTS default_identity_slot (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        alias TEXT NOT NULL CHECK (
            length(alias) BETWEEN 1 AND 120
            AND alias = trim(alias)
        ),
        locked INTEGER NOT NULL CHECK (locked IN (0, 1)),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
     ) STRICT;";

const CREATE_DRAFT_CREATE_ATTEMPTS: &str = "CREATE TABLE IF NOT EXISTS draft_create_attempts (
        workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        tab_key TEXT NOT NULL,
        mutation_id TEXT NOT NULL UNIQUE CHECK (length(mutation_id) = 36),
        draft_fingerprint TEXT NOT NULL CHECK (
            length(draft_fingerprint) = 64
            AND draft_fingerprint NOT GLOB '*[^0-9a-f]*'
        ),
        request_sha256 TEXT NOT NULL CHECK (
            length(request_sha256) = 64
            AND request_sha256 NOT GLOB '*[^0-9a-f]*'
        ),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (workspace_id, tab_key)
     ) STRICT;";

const CREATE_NOTEBOOK_CONFLICTS: &str = "CREATE TABLE IF NOT EXISTS notebook_conflicts (
        notebook_id TEXT PRIMARY KEY REFERENCES notebooks(id) ON DELETE CASCADE,
        workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        target_id TEXT NOT NULL,
        receipt_generation INTEGER NOT NULL CHECK (receipt_generation > 0),
        expected_managed_sha256 TEXT NOT NULL CHECK (
            length(expected_managed_sha256) = 64
            AND expected_managed_sha256 NOT GLOB '*[^0-9a-f]*'
        ),
        observed_file_sha256 TEXT NOT NULL CHECK (
            length(observed_file_sha256) = 64
            AND observed_file_sha256 NOT GLOB '*[^0-9a-f]*'
        ),
        observed_managed_sha256 TEXT CHECK (
            observed_managed_sha256 IS NULL OR (
                length(observed_managed_sha256) = 64
                AND observed_managed_sha256 NOT GLOB '*[^0-9a-f]*'
            )
        ),
        reason_code TEXT NOT NULL CHECK (length(reason_code) BETWEEN 1 AND 128),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        UNIQUE (workspace_id, target_id)
     ) STRICT;";

const CREATE_ATTACHMENT_FILE_OPERATIONS: &str =
    "CREATE TABLE IF NOT EXISTS attachment_file_operations (
        attachment_id TEXT PRIMARY KEY REFERENCES attachments(id) ON DELETE CASCADE,
        action TEXT NOT NULL CHECK (action IN ('trash', 'restore')),
        phase TEXT NOT NULL CHECK (phase IN (
            'queued', 'prepared', 'trashed', 'ready', 'preserved_shared',
            'preserved_changed', 'missing', 'needs_recovery'
        )),
        record_revision INTEGER NOT NULL CHECK (
            record_revision > 0 AND record_revision <= 9007199254740991
        ),
        trash_path TEXT,
        backup_app_data_relative_path TEXT,
        attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
        last_error_code TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        completed_at_ms INTEGER,
        CHECK (
            completed_at_ms IS NULL
            OR phase IN (
                'trashed', 'ready', 'preserved_shared', 'preserved_changed', 'missing'
            )
        )
     ) STRICT;
     CREATE INDEX IF NOT EXISTS pending_attachment_file_operations
        ON attachment_file_operations(phase, updated_at_ms)
        WHERE phase IN ('queued', 'prepared', 'needs_recovery');";

const REBUILD_ATTACHMENT_FILE_OPERATIONS_FOR_DETACH: &str =
    "DROP INDEX IF EXISTS pending_attachment_file_operations;
     ALTER TABLE attachment_file_operations RENAME TO attachment_file_operations_v15;
     CREATE TABLE attachment_file_operations (
        attachment_id TEXT PRIMARY KEY REFERENCES attachments(id) ON DELETE CASCADE,
        action TEXT NOT NULL CHECK (action IN ('trash', 'restore', 'detach')),
        phase TEXT NOT NULL CHECK (phase IN (
            'queued', 'prepared', 'trashed', 'ready', 'preserved_shared',
            'preserved_changed', 'missing', 'needs_recovery'
        )),
        record_revision INTEGER NOT NULL CHECK (
            record_revision > 0 AND record_revision <= 9007199254740991
        ),
        trash_path TEXT,
        backup_app_data_relative_path TEXT,
        attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
        last_error_code TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        completed_at_ms INTEGER,
        CHECK (
            completed_at_ms IS NULL
            OR phase IN (
                'trashed', 'ready', 'preserved_shared', 'preserved_changed', 'missing'
            )
        )
     ) STRICT;
     INSERT INTO attachment_file_operations (
        attachment_id, action, phase, record_revision, trash_path,
        backup_app_data_relative_path, attempt_count, last_error_code,
        created_at_ms, updated_at_ms, completed_at_ms
     )
     SELECT attachment_id, action, phase, record_revision, trash_path,
            backup_app_data_relative_path, attempt_count, last_error_code,
            created_at_ms, updated_at_ms, completed_at_ms
     FROM attachment_file_operations_v15;
     DROP TABLE attachment_file_operations_v15;
     CREATE INDEX pending_attachment_file_operations
        ON attachment_file_operations(phase, updated_at_ms)
        WHERE phase IN ('queued', 'prepared', 'needs_recovery');";

const CREATE_RECORD_PINS: &str = "CREATE TABLE IF NOT EXISTS record_pins (
        record_id TEXT PRIMARY KEY REFERENCES records(id) ON DELETE CASCADE,
        pinned_at_ms INTEGER NOT NULL
     ) STRICT;";

const CREATE_NOTEBOOK_TAB_STATE: &str = "CREATE TABLE IF NOT EXISTS notebook_tab_state (
        notebook_id TEXT PRIMARY KEY REFERENCES notebooks(id) ON DELETE CASCADE,
        is_pinned INTEGER NOT NULL CHECK (is_pinned IN (0, 1)),
        updated_at_ms INTEGER NOT NULL
     ) STRICT;";

const CREATE_UI_STATE: &str = "CREATE TABLE IF NOT EXISTS app_ui_state (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        active_workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
        theme TEXT NOT NULL CHECK (theme IN ('system', 'light', 'dark')),
        submit_shortcut TEXT NOT NULL CHECK (submit_shortcut IN ('enter', 'command_enter')),
        markdown_layout TEXT NOT NULL CHECK (markdown_layout IN ('edit', 'preview', 'split')),
        updated_at_ms INTEGER NOT NULL
     ) STRICT;
     INSERT OR IGNORE INTO app_ui_state (
        singleton_id, active_workspace_id, theme, submit_shortcut, markdown_layout, updated_at_ms
     ) VALUES (1, NULL, 'system', 'enter', 'split', 0);
     CREATE TABLE IF NOT EXISTS workspace_ui_state (
        workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
        selected_notebook_id TEXT REFERENCES notebooks(id) ON DELETE SET NULL,
        updated_at_ms INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE IF NOT EXISTS draft_attachments (
        workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        tab_key TEXT NOT NULL,
        token TEXT NOT NULL,
        media_type TEXT NOT NULL,
        byte_size INTEGER NOT NULL CHECK (byte_size > 0),
        content_sha256 TEXT NOT NULL,
        display_name TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
        PRIMARY KEY (workspace_id, tab_key, token),
        UNIQUE (workspace_id, tab_key, ordinal)
     ) STRICT;";

const ADD_EDITOR_PREFERENCES: &str = "ALTER TABLE app_ui_state
        ADD COLUMN submit_shortcut TEXT NOT NULL DEFAULT 'enter'
        CHECK (submit_shortcut IN ('enter', 'command_enter'));
     ALTER TABLE app_ui_state
        ADD COLUMN markdown_layout TEXT NOT NULL DEFAULT 'split'
        CHECK (markdown_layout IN ('edit', 'preview', 'split'));";

const CREATE_UPDATE_SETTINGS: &str = "CREATE TABLE IF NOT EXISTS update_settings (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        automatic_checks_enabled INTEGER NOT NULL DEFAULT 1
            CHECK (automatic_checks_enabled IN (0, 1)),
        check_interval_hours INTEGER NOT NULL DEFAULT 24
            CHECK (check_interval_hours IN (6, 12, 24, 48)),
        skipped_version TEXT CHECK (
            skipped_version IS NULL
            OR (length(skipped_version) BETWEEN 1 AND 64)
        ),
        last_checked_at_ms INTEGER CHECK (
            last_checked_at_ms IS NULL OR last_checked_at_ms >= 0
        ),
        last_error_code TEXT CHECK (
            last_error_code IS NULL
            OR (length(last_error_code) BETWEEN 1 AND 64)
        ),
        updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
     ) STRICT;
     INSERT OR IGNORE INTO update_settings (
        singleton_id, automatic_checks_enabled, check_interval_hours,
        skipped_version, last_checked_at_ms, last_error_code, updated_at_ms
     ) VALUES (1, 1, 24, NULL, NULL, NULL, 0);";

const ADD_EXTERNAL_NETWORK_ENABLED: &str = "ALTER TABLE update_settings
        ADD COLUMN external_network_enabled INTEGER NOT NULL DEFAULT 1
        CHECK (external_network_enabled IN (0, 1));";

const ADD_AUTOMATIC_DOWNLOADS_ENABLED: &str = "ALTER TABLE update_settings
        ADD COLUMN automatic_downloads_enabled INTEGER NOT NULL DEFAULT 0
        CHECK (automatic_downloads_enabled IN (0, 1));";

const ADD_NOTEBOOK_NUMBERING_SYNC_PENDING: &str = "ALTER TABLE notebooks
        ADD COLUMN numbering_sync_pending INTEGER NOT NULL DEFAULT 0
        CHECK (numbering_sync_pending IN (0, 1));";

const ADD_NOTEBOOK_NUMBERING_START: &str = "ALTER TABLE notebooks
        ADD COLUMN numbering_start INTEGER NOT NULL DEFAULT 1
        CHECK (numbering_start BETWEEN 1 AND 1000000000);";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Domain(DomainError),
    Conflict(String),
    NotFound(&'static str),
    LockPoisoned,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "database error: {error}"),
            Self::Io(error) => write!(formatter, "file error: {error}"),
            Self::Domain(error) => error.fmt(formatter),
            Self::Conflict(message) => formatter.write_str(message),
            Self::NotFound(entity) => write!(formatter, "{entity} was not found"),
            Self::LockPoisoned => formatter.write_str("database lock is unavailable"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<DomainError> for StoreError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value)
    }
}

pub struct Store {
    connection: Mutex<Connection>,
    update_gate: RwLock<()>,
}

struct StoreConnectionGuard<'a> {
    connection: MutexGuard<'a, Connection>,
    _update_gate: RwLockReadGuard<'a, ()>,
}

pub(crate) struct StoreUpdateGuard<'a> {
    connection: MutexGuard<'a, Connection>,
    _update_gate: RwLockWriteGuard<'a, ()>,
}

impl Deref for StoreConnectionGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for StoreConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryOperation {
    pub id: String,
    pub workspace_id: String,
    pub record_id: String,
    pub operation_kind: String,
    pub phase: String,
    pub expected_revision: u64,
    pub desired_revision: u64,
    pub desired_record_state: RecordState,
    pub source_notebook_id: Option<String>,
    pub destination_notebook_id: Option<String>,
    pub source_logical_order: Option<i64>,
    pub destination_logical_order: Option<i64>,
    pub attempt_count: u64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NotebookFileReceipt {
    pub managed_sha256: String,
    pub file_sha256: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotebookConflictEvidence {
    pub notebook_id: String,
    pub workspace_id: String,
    pub target_id: String,
    pub receipt_generation: u64,
    pub expected_managed_sha256: String,
    pub observed_file_sha256: String,
    pub observed_managed_sha256: Option<String>,
    pub reason_code: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotebookConflictRecordUpdate {
    pub record_id: String,
    pub expected_revision: u64,
    pub body_markdown: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryFileStepInput {
    pub step_index: usize,
    pub notebook_id: String,
    pub role: &'static str,
    pub effect: &'static str,
    pub target_workspace_relative_path: String,
    pub staged_sibling_name: String,
    pub backup_app_data_relative_path: String,
    pub expected_target_id: String,
    pub before_file_sha256: String,
    pub after_file_sha256: String,
    pub before_managed_sha256: Option<String>,
    pub after_managed_sha256: String,
    pub before_modified_ns: u64,
    pub target_existed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryFileStep {
    pub operation_id: String,
    pub step_index: usize,
    pub notebook_id: String,
    pub role: String,
    pub effect: String,
    pub state: String,
    pub target_workspace_relative_path: String,
    pub staged_sibling_name: String,
    pub backup_app_data_relative_path: String,
    pub expected_target_id: String,
    pub before_file_sha256: String,
    pub after_file_sha256: String,
    pub before_managed_sha256: Option<String>,
    pub after_managed_sha256: String,
    pub before_modified_ns: u64,
    pub target_existed: bool,
    pub applied_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct NotebookFileReceiptInput {
    pub notebook_id: String,
    pub target_id: String,
    pub marker_schema: u32,
    pub managed_sha256: String,
    pub file_sha256: String,
    pub file_modified_ns: u64,
    pub file_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewAttachment {
    pub media_type: String,
    pub managed_relative_path: String,
    pub previous_managed_relative_path: Option<String>,
    pub content_sha256: String,
    pub byte_size: u64,
}

pub(crate) struct RecordAttachmentSetRevision<'a> {
    pub body_markdown: &'a str,
    pub retained_attachment_ids: &'a [String],
    pub new_attachments: &'a [NewAttachment],
    pub mutation_id: &'a str,
    pub mutation_schema_version: u32,
}

pub(crate) struct DraftRecordCreate<'a> {
    pub workspace_id: &'a str,
    pub notebook_id: Option<&'a str>,
    pub body_markdown: &'a str,
    pub attachments: &'a [NewAttachment],
    pub proposed_mutation_id: &'a str,
    pub mutation_schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentFileOperation {
    pub attachment_id: String,
    pub record_id: String,
    pub workspace_id: String,
    pub action: String,
    pub phase: String,
    pub record_revision: u64,
    pub managed_relative_path: String,
    pub media_type: String,
    pub expected_sha256: String,
    pub expected_byte_size: u64,
    pub trash_path: Option<String>,
    pub backup_app_data_relative_path: Option<String>,
    pub attempt_count: u64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentRelocationOperation {
    pub attachment_id: String,
    pub record_id: String,
    pub workspace_id: String,
    pub notebook_id: Option<String>,
    pub record_revision: u64,
    pub applied_revision: u64,
    pub managed_relative_path: String,
    pub previous_managed_relative_path: String,
    pub media_type: String,
    pub content_sha256: String,
    pub byte_size: u64,
    pub state: AttachmentRelocationState,
    pub error_code: Option<String>,
    pub notebook_directory_sync_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposerReceipt {
    pub id: String,
    pub state: String,
    pub detail_json: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftAttachment {
    pub token: String,
    pub media_type: String,
    pub byte_size: u64,
    pub content_sha256: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedDraft {
    pub body_markdown: String,
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Debug, Clone)]
pub(crate) struct DataExportDraftAttachment {
    pub workspace_id: String,
    pub tab_key: String,
    pub attachment: DraftAttachment,
}

#[derive(Debug, Clone)]
pub(crate) struct DataExportInventory {
    pub workspaces: Vec<Workspace>,
    pub notebooks: Vec<Notebook>,
    pub records: Vec<Record>,
    pub draft_attachments: Vec<DataExportDraftAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalDataResetInventory {
    pub workspace_count: u64,
    pub notebook_count: u64,
    pub record_count: u64,
    pub attachment_count: u64,
    pub draft_count: u64,
    pub draft_attachment_count: u64,
    pub composer_receipt_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateDatabaseBackup {
    pub sha256: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordTrashCleanupCandidate {
    pub record_id: String,
    pub workspace_id: String,
    pub notebook_id: Option<String>,
    pub trashed_at_ms: i64,
    pub attachment_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordTrashCleanupPreview {
    pub retention_days: Option<u32>,
    pub cutoff_at_ms: Option<i64>,
    pub eligible: Vec<RecordTrashCleanupCandidate>,
    pub blocked_count: u64,
    pub preview_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordTrashCleanupResult {
    pub deleted_record_count: u64,
    pub deleted_attachment_count: u64,
}

struct RecordTrashCleanupCurrentState {
    state: String,
    notebook_id: Option<String>,
    trashed_at_ms: Option<i64>,
    revision: i64,
    applied_revision: i64,
    sync_state: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "EXTRA")?;
        #[cfg(target_os = "macos")]
        connection.pragma_update(None, "fullfsync", "ON")?;
        migrate(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            update_gate: RwLock::new(()),
        })
    }

    fn connection(&self) -> Result<StoreConnectionGuard<'_>, StoreError> {
        let update_gate = self
            .update_gate
            .read()
            .map_err(|_| StoreError::LockPoisoned)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(StoreConnectionGuard {
            connection,
            _update_gate: update_gate,
        })
    }

    pub(crate) fn freeze_for_update(&self) -> Result<StoreUpdateGuard<'_>, StoreError> {
        let update_gate = self
            .update_gate
            .write()
            .map_err(|_| StoreError::LockPoisoned)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(StoreUpdateGuard {
            connection,
            _update_gate: update_gate,
        })
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        Ok(version)
    }

    pub(crate) fn verify_update_health(&self) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let quick_check: String =
            connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let foreign_key_violations: i64 =
            connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        let schema_version: u32 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if quick_check != "ok" || foreign_key_violations != 0 || schema_version != SCHEMA_VERSION {
            return Err(StoreError::Conflict(
                "update database health verification failed".to_owned(),
            ));
        }
        Ok(schema_version)
    }

    pub fn create_workspace(
        &self,
        display_name: &str,
        root_path: &str,
    ) -> Result<Workspace, StoreError> {
        let display_name = validate_display_name(display_name, "workspace name")?;
        let root_path = validate_workspace_root(root_path)?;
        let id = new_id();
        let now = now_ms();
        let connection = self.connection()?;

        let existing: Option<(Workspace, bool)> = connection
            .query_row(
                "SELECT id, display_name, root_path, created_at_ms, updated_at_ms, is_connected
                 FROM workspaces WHERE root_path = ?1",
                [&root_path],
                |row| Ok((workspace_from_row(row)?, row.get(5)?)),
            )
            .optional()?;
        if let Some((mut workspace, is_connected)) = existing {
            if is_connected {
                return Err(StoreError::Conflict(
                    "that workspace root is already authorized".to_owned(),
                ));
            }
            let changed = connection.execute(
                "UPDATE workspaces
                 SET display_name = ?1, is_connected = 1, updated_at_ms = ?2
                 WHERE id = ?3 AND is_connected = 0",
                params![display_name, now, workspace.id],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "workspace connection changed before reconnection".to_owned(),
                ));
            }
            workspace.display_name = display_name;
            workspace.updated_at_ms = now;
            return Ok(workspace);
        }

        connection.execute(
            "INSERT INTO workspaces (id, display_name, root_path, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, display_name, root_path, now],
        )?;

        Ok(Workspace {
            id,
            display_name,
            root_path,
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, display_name, root_path, created_at_ms, updated_at_ms
             FROM workspaces WHERE is_connected = 1
             ORDER BY updated_at_ms DESC, display_name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], workspace_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn prepare_data_export(
        &self,
        database_destination: &Path,
    ) -> Result<DataExportInventory, StoreError> {
        if !database_destination.is_absolute() || database_destination.exists() {
            return Err(StoreError::Conflict(
                "data export database destination is invalid".to_owned(),
            ));
        }
        let destination = database_destination.to_str().ok_or_else(|| {
            StoreError::Conflict("data export database destination is invalid".to_owned())
        })?;
        let connection = self.connection()?;

        let workspaces = {
            let mut statement = connection.prepare(
                "SELECT id, display_name, root_path, created_at_ms, updated_at_ms
                 FROM workspaces ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map([], workspace_from_row)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let notebooks = {
            let mut statement = connection.prepare(
                "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                        numbering_style, numbering_start, numbering_sync_pending, target_state,
                        last_error_code, created_at_ms, updated_at_ms, attachment_directory,
                        previous_attachment_directory, attachment_directory_sync_pending,
                        COALESCE((SELECT is_pinned FROM notebook_tab_state
                                  WHERE notebook_id = notebooks.id), 1)
                 FROM notebooks ORDER BY workspace_id, ordinal, created_at_ms, id",
            )?;
            let rows = statement.query_map([], notebook_from_row)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut records = {
            let mut statement = connection.prepare(
                "SELECT id, workspace_id, notebook_id, body_markdown, created_at_ms, updated_at_ms,
                        logical_order, state, sync_state, revision, applied_revision, trashed_at_ms
                 FROM records
                 ORDER BY workspace_id, notebook_id, logical_order, created_at_ms, id",
            )?;
            let rows = statement.query_map([], record_from_row)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for record in &mut records {
            record.attachments = attachments_for_record(&connection, &record.id)?;
            record.is_pinned = record_is_pinned(&connection, &record.id)?;
        }
        let draft_attachments = {
            let mut statement = connection.prepare(
                "SELECT workspace_id, tab_key, token, media_type, byte_size,
                        content_sha256, display_name
                 FROM draft_attachments
                 ORDER BY workspace_id, tab_key, ordinal, token",
            )?;
            let rows = statement.query_map([], |row| {
                let byte_size: i64 = row.get(4)?;
                let byte_size = u64::try_from(byte_size).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(4, Type::Integer, Box::new(error))
                })?;
                Ok(DataExportDraftAttachment {
                    workspace_id: row.get(0)?,
                    tab_key: row.get(1)?,
                    attachment: DraftAttachment {
                        token: row.get(2)?,
                        media_type: row.get(3)?,
                        byte_size,
                        content_sha256: row.get(5)?,
                        display_name: row.get(6)?,
                    },
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        connection.execute("VACUUM INTO ?1", [destination])?;
        let snapshot = Connection::open_with_flags(
            database_destination,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let quick_check: String = snapshot.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let schema_version: u32 =
            snapshot.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if quick_check != "ok" || schema_version != SCHEMA_VERSION {
            return Err(StoreError::Conflict(
                "data export database verification failed".to_owned(),
            ));
        }

        Ok(DataExportInventory {
            workspaces,
            notebooks,
            records,
            draft_attachments,
        })
    }

    pub(crate) fn backup_database_for_update(
        &self,
        update_guard: &mut StoreUpdateGuard<'_>,
        destination: &Path,
    ) -> Result<UpdateDatabaseBackup, StoreError> {
        if !destination.is_absolute() || destination.exists() {
            return Err(StoreError::Conflict(
                "update database backup destination is invalid".to_owned(),
            ));
        }
        let parent = destination.parent().ok_or_else(|| {
            StoreError::Conflict("update database backup destination is invalid".to_owned())
        })?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(StoreError::Conflict(
                "update database backup parent is invalid".to_owned(),
            ));
        }
        let destination_text = destination.to_str().ok_or_else(|| {
            StoreError::Conflict("update database backup destination is invalid".to_owned())
        })?;

        let result = (|| {
            let pending_operations: i64 = update_guard.connection.query_row(
                "SELECT
                    (SELECT COUNT(*) FROM recovery_operations
                     WHERE phase NOT IN ('completed', 'superseded'))
                  + (SELECT COUNT(*) FROM attachment_file_operations
                     WHERE phase IN ('queued', 'prepared', 'needs_recovery')
                        OR backup_app_data_relative_path IS NOT NULL)
                  + (SELECT COUNT(*) FROM attachments
                     WHERE membership_state = 'active' AND relocation_state <> 'ready')
                  + (SELECT COUNT(*) FROM notebooks
                     WHERE numbering_sync_pending = 1
                        OR attachment_directory_sync_pending = 1)
                  + (SELECT COUNT(*) FROM notebook_conflicts
                     WHERE reason_code = 'file_version_adoption_pending')",
                [],
                |row| row.get(0),
            )?;
            if pending_operations != 0 {
                return Err(StoreError::Conflict(
                    "update recovery operations are pending".to_owned(),
                ));
            }
            update_guard
                .connection
                .execute("VACUUM INTO ?1", [destination_text])?;
            #[cfg(unix)]
            fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
            let mut file = File::open(destination)?;
            file.sync_all()?;
            #[cfg(target_os = "macos")]
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
                return Err(StoreError::Io(std::io::Error::last_os_error()));
            }

            let snapshot = Connection::open_with_flags(
                destination,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let quick_check: String =
                snapshot.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            let schema_version: u32 =
                snapshot.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            let foreign_key_violations: i64 =
                snapshot.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })?;
            let foreign_key_violations = u64::try_from(foreign_key_violations).map_err(|_| {
                StoreError::Conflict("update database backup verification failed".to_owned())
            })?;
            if quick_check != "ok"
                || schema_version != SCHEMA_VERSION
                || foreign_key_violations != 0
            {
                return Err(StoreError::Conflict(
                    "update database backup verification failed".to_owned(),
                ));
            }

            let byte_size = file.metadata()?.len();
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            let digest = hasher.finalize();
            let mut sha256 = String::with_capacity(digest.len() * 2);
            for byte in digest {
                use std::fmt::Write as _;
                let _ = write!(sha256, "{byte:02x}");
            }
            Ok(UpdateDatabaseBackup { sha256, byte_size })
        })();
        if result.is_err() {
            let _ = fs::remove_file(destination);
        }
        result
    }

    pub(crate) fn local_data_reset_inventory(&self) -> Result<LocalDataResetInventory, StoreError> {
        let connection = self.connection()?;
        let counts: (i64, i64, i64, i64, i64, i64, i64) = connection.query_row(
            "SELECT
                (SELECT COUNT(*) FROM workspaces),
                (SELECT COUNT(*) FROM notebooks),
                (SELECT COUNT(*) FROM records),
                (SELECT COUNT(*) FROM attachments),
                (SELECT COUNT(*) FROM drafts),
                (SELECT COUNT(*) FROM draft_attachments),
                (SELECT COUNT(*) FROM composer_receipts)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        let convert = |value: i64| {
            u64::try_from(value).map_err(|_| {
                StoreError::Conflict("local data reset inventory is invalid".to_owned())
            })
        };
        Ok(LocalDataResetInventory {
            workspace_count: convert(counts.0)?,
            notebook_count: convert(counts.1)?,
            record_count: convert(counts.2)?,
            attachment_count: convert(counts.3)?,
            draft_count: convert(counts.4)?,
            draft_attachment_count: convert(counts.5)?,
            composer_receipt_count: convert(counts.6)?,
        })
    }

    pub fn disconnect_workspace(&self, workspace_id: &str) -> Result<UiPreferences, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let is_connected: Option<bool> = transaction
            .query_row(
                "SELECT is_connected FROM workspaces WHERE id = ?1",
                [workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        match is_connected {
            None => return Err(StoreError::NotFound("workspace")),
            Some(false) => {
                return Err(StoreError::Conflict(
                    "workspace is already disconnected".to_owned(),
                ));
            }
            Some(true) => {}
        }

        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE workspaces
             SET is_connected = 0, updated_at_ms = ?2
             WHERE id = ?1 AND is_connected = 1",
            params![workspace_id, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "workspace connection changed before disconnection".to_owned(),
            ));
        }

        let active_workspace_id: Option<String> = transaction.query_row(
            "SELECT active_workspace_id FROM app_ui_state WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )?;
        if active_workspace_id.as_deref() == Some(workspace_id) {
            let next_workspace_id: Option<String> = transaction
                .query_row(
                    "SELECT id FROM workspaces
                     WHERE is_connected = 1
                     ORDER BY updated_at_ms DESC, display_name COLLATE NOCASE
                     LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            transaction.execute(
                "UPDATE app_ui_state
                 SET active_workspace_id = ?1, updated_at_ms = ?2
                 WHERE singleton_id = 1",
                params![next_workspace_id, now],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.ui_preferences()
    }

    pub fn get_workspace(&self, workspace_id: &str) -> Result<Workspace, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, display_name, root_path, created_at_ms, updated_at_ms
                 FROM workspaces WHERE id = ?1",
                [workspace_id],
                workspace_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("workspace"))
    }

    pub(crate) fn get_connected_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Workspace, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, display_name, root_path, created_at_ms, updated_at_ms
                 FROM workspaces WHERE id = ?1 AND is_connected = 1",
                [workspace_id],
                workspace_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("connected workspace"))
    }

    pub fn ui_preferences(&self) -> Result<UiPreferences, StoreError> {
        let connection = self.connection()?;
        let (active_workspace_id, theme, submit_shortcut, markdown_layout): (
            Option<String>,
            String,
            String,
            String,
        ) = connection.query_row(
            "SELECT active_workspace_id, theme, submit_shortcut, markdown_layout
             FROM app_ui_state WHERE singleton_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let selected_notebook_id = active_workspace_id
            .as_deref()
            .map(|workspace_id| workspace_open_notebook_id(&connection, workspace_id))
            .transpose()?
            .flatten();
        Ok(UiPreferences {
            active_workspace_id,
            selected_notebook_id,
            theme: ThemePreference::from_db_value(&theme)?,
            submit_shortcut: SubmitShortcut::from_db_value(&submit_shortcut)?,
            markdown_layout: MarkdownLayout::from_db_value(&markdown_layout)?,
        })
    }

    pub fn update_settings(&self) -> Result<UpdateSettings, StoreError> {
        let connection = self.connection()?;
        struct StoredUpdateSettings {
            external_network_enabled: i64,
            automatic_checks_enabled: i64,
            automatic_downloads_enabled: i64,
            check_interval_hours: i64,
            skipped_version: Option<String>,
            last_checked_at_ms: Option<i64>,
            last_error_code: Option<String>,
            updated_at_ms: i64,
        }
        let stored = connection.query_row(
            "SELECT external_network_enabled, automatic_checks_enabled,
                        automatic_downloads_enabled, check_interval_hours, skipped_version,
                        last_checked_at_ms, last_error_code, updated_at_ms
                 FROM update_settings WHERE singleton_id = 1",
            [],
            |row| {
                Ok(StoredUpdateSettings {
                    external_network_enabled: row.get(0)?,
                    automatic_checks_enabled: row.get(1)?,
                    automatic_downloads_enabled: row.get(2)?,
                    check_interval_hours: row.get(3)?,
                    skipped_version: row.get(4)?,
                    last_checked_at_ms: row.get(5)?,
                    last_error_code: row.get(6)?,
                    updated_at_ms: row.get(7)?,
                })
            },
        )?;
        if !matches!(stored.external_network_enabled, 0 | 1)
            || !matches!(stored.automatic_checks_enabled, 0 | 1)
            || !matches!(stored.automatic_downloads_enabled, 0 | 1)
        {
            return Err(StoreError::Conflict(
                "update settings contain an invalid boolean value".to_owned(),
            ));
        }
        let check_interval_hours = u32::try_from(stored.check_interval_hours).map_err(|_| {
            StoreError::Conflict("update settings contain an invalid interval".to_owned())
        })?;
        validate_update_interval(check_interval_hours)?;
        if let Some(version) = stored.skipped_version.as_deref() {
            validate_update_version(version)?;
        }
        if let Some(code) = stored.last_error_code.as_deref() {
            validate_update_error_code(code)?;
        }
        Ok(UpdateSettings {
            external_network_enabled: stored.external_network_enabled == 1,
            automatic_checks_enabled: stored.automatic_checks_enabled == 1,
            automatic_downloads_enabled: stored.automatic_downloads_enabled == 1,
            check_interval_hours,
            skipped_version: stored.skipped_version,
            last_checked_at_ms: stored.last_checked_at_ms,
            last_error_code: stored.last_error_code,
            updated_at_ms: stored.updated_at_ms,
        })
    }

    pub fn set_update_settings(
        &self,
        external_network_enabled: bool,
        automatic_checks_enabled: bool,
        automatic_downloads_enabled: bool,
        check_interval_hours: u32,
    ) -> Result<UpdateSettings, StoreError> {
        validate_update_interval(check_interval_hours)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE update_settings
             SET external_network_enabled = ?1, automatic_checks_enabled = ?2,
                 automatic_downloads_enabled = ?3, check_interval_hours = ?4, updated_at_ms = ?5
             WHERE singleton_id = 1",
            params![
                i64::from(external_network_enabled),
                i64::from(automatic_checks_enabled),
                i64::from(automatic_downloads_enabled),
                i64::from(check_interval_hours),
                now_ms(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("update settings"));
        }
        drop(connection);
        self.update_settings()
    }

    pub fn set_skipped_update_version(
        &self,
        skipped_version: Option<&str>,
    ) -> Result<UpdateSettings, StoreError> {
        if let Some(version) = skipped_version {
            validate_update_version(version)?;
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE update_settings
             SET skipped_version = ?1, updated_at_ms = ?2
             WHERE singleton_id = 1",
            params![skipped_version, now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("update settings"));
        }
        drop(connection);
        self.update_settings()
    }

    pub fn product_settings(&self) -> Result<ProductSettings, StoreError> {
        let connection = self.connection()?;
        let (retention_days, ignore_json, codex_paused, updated_at_ms): (
            Option<i64>,
            String,
            i64,
            i64,
        ) = connection.query_row(
            "SELECT record_trash_retention_days,
                    notebook_scan_ignore_directories_json,
                    codex_integration_paused, updated_at_ms
             FROM product_settings WHERE singleton_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let retention_days = retention_days
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    StoreError::Conflict(
                        "product settings contain an invalid trash retention".to_owned(),
                    )
                })
            })
            .transpose()?;
        let retention_days = validate_record_trash_retention_days(retention_days)?;
        let ignore_directories: Vec<String> = serde_json::from_str(&ignore_json).map_err(|_| {
            StoreError::Conflict(
                "product settings contain invalid scan ignore directories".to_owned(),
            )
        })?;
        let ignore_directories = validate_notebook_scan_ignore_directories(&ignore_directories)?;
        if !matches!(codex_paused, 0 | 1) {
            return Err(StoreError::Conflict(
                "product settings contain an invalid Codex integration state".to_owned(),
            ));
        }
        Ok(ProductSettings {
            record_trash_retention_days: retention_days,
            notebook_scan_ignore_directories: ignore_directories,
            protected_notebook_scan_ignore_directories: DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            codex_integration_paused: codex_paused == 1,
            updated_at_ms,
        })
    }

    pub fn set_product_settings(
        &self,
        record_trash_retention_days: Option<u32>,
        notebook_scan_ignore_directories: &[String],
    ) -> Result<ProductSettings, StoreError> {
        let record_trash_retention_days =
            validate_record_trash_retention_days(record_trash_retention_days)?;
        let notebook_scan_ignore_directories =
            validate_notebook_scan_ignore_directories(notebook_scan_ignore_directories)?;
        let ignore_json =
            serde_json::to_string(&notebook_scan_ignore_directories).map_err(|_| {
                StoreError::Conflict("product settings could not be encoded".to_owned())
            })?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE product_settings
             SET record_trash_retention_days = ?1,
                 notebook_scan_ignore_directories_json = ?2, updated_at_ms = ?3
             WHERE singleton_id = 1",
            params![record_trash_retention_days, ignore_json, now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("product settings"));
        }
        drop(connection);
        self.product_settings()
    }

    pub(crate) fn set_codex_integration_paused(
        &self,
        paused: bool,
    ) -> Result<ProductSettings, StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE product_settings
             SET codex_integration_paused = ?1, updated_at_ms = ?2
             WHERE singleton_id = 1",
            params![i64::from(paused), now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("product settings"));
        }
        drop(connection);
        self.product_settings()
    }

    pub(crate) fn notebook_scan_ignore_directories(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.product_settings()?.notebook_scan_ignore_directories)
    }

    pub(crate) fn preview_record_trash_cleanup(
        &self,
    ) -> Result<RecordTrashCleanupPreview, StoreError> {
        self.preview_record_trash_cleanup_at(now_ms())
    }

    fn preview_record_trash_cleanup_at(
        &self,
        current_time_ms: i64,
    ) -> Result<RecordTrashCleanupPreview, StoreError> {
        let settings = self.product_settings()?;
        let cutoff_at_ms = settings
            .record_trash_retention_days
            .map(|days| {
                i64::from(days)
                    .checked_mul(24 * 60 * 60 * 1000)
                    .and_then(|duration| current_time_ms.checked_sub(duration))
                    .ok_or_else(|| {
                        StoreError::Conflict("record trash retention overflowed".to_owned())
                    })
            })
            .transpose()?;
        let Some(cutoff_at_ms) = cutoff_at_ms else {
            return Ok(RecordTrashCleanupPreview {
                retention_days: None,
                cutoff_at_ms: None,
                eligible: Vec::new(),
                blocked_count: 0,
                preview_token: cleanup_preview_token(None, &[], 0),
            });
        };
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT record.id, record.workspace_id, record.notebook_id,
                    record.trashed_at_ms, COUNT(attachment.id)
             FROM records record
             LEFT JOIN attachments attachment ON attachment.record_id = record.id
             WHERE record.state = 'trashed'
               AND record.trashed_at_ms IS NOT NULL
               AND record.trashed_at_ms <= ?1
               AND record.sync_state IN ('local', 'synced')
               AND record.applied_revision = record.revision
               AND NOT EXISTS (
                    SELECT 1 FROM recovery_operations operation
                    WHERE operation.record_id = record.id
                      AND operation.phase NOT IN ('completed', 'superseded')
               )
               AND NOT EXISTS (
                    SELECT 1
                    FROM attachment_file_operations operation
                    JOIN attachments current_attachment
                      ON current_attachment.id = operation.attachment_id
                    WHERE current_attachment.record_id = record.id
                      AND (
                        operation.phase IN ('queued', 'prepared', 'needs_recovery')
                        OR operation.backup_app_data_relative_path IS NOT NULL
                      )
               )
               AND NOT EXISTS (
                    SELECT 1 FROM attachments current_attachment
                    WHERE current_attachment.record_id = record.id
                      AND (
                        current_attachment.membership_state <> 'active'
                        OR current_attachment.relocation_state <> 'ready'
                      )
               )
               AND NOT EXISTS (
                    SELECT 1 FROM notebooks notebook
                    WHERE notebook.id = record.notebook_id
                      AND (
                        notebook.numbering_sync_pending = 1
                        OR notebook.attachment_directory_sync_pending = 1
                      )
               )
               AND NOT EXISTS (
                    SELECT 1 FROM notebook_conflicts conflict
                    WHERE conflict.notebook_id = record.notebook_id
               )
               AND NOT EXISTS (
                    SELECT 1 FROM composer_receipts receipt
                    WHERE receipt.record_id = record.id AND receipt.state = 'uncertain'
                      AND NOT EXISTS (
                        SELECT 1 FROM composer_receipts newer
                        WHERE newer.record_id = receipt.record_id
                          AND newer.host_kind = receipt.host_kind
                          AND (
                            newer.created_at_ms > receipt.created_at_ms
                            OR (
                              newer.created_at_ms = receipt.created_at_ms
                              AND newer.id > receipt.id
                            )
                          )
                      )
               )
             GROUP BY record.id, record.workspace_id, record.notebook_id,
                      record.trashed_at_ms
             ORDER BY record.trashed_at_ms, record.id",
        )?;
        let rows = statement.query_map([cutoff_at_ms], |row| {
            let attachment_count: i64 = row.get(4)?;
            Ok(RecordTrashCleanupCandidate {
                record_id: row.get(0)?,
                workspace_id: row.get(1)?,
                notebook_id: row.get(2)?,
                trashed_at_ms: row.get(3)?,
                attachment_count: u64::try_from(attachment_count).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(4, Type::Integer, Box::new(error))
                })?,
            })
        })?;
        let eligible = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let total_expired: i64 = connection.query_row(
            "SELECT COUNT(*) FROM records
             WHERE state = 'trashed' AND trashed_at_ms IS NOT NULL AND trashed_at_ms <= ?1",
            [cutoff_at_ms],
            |row| row.get(0),
        )?;
        let eligible_count = i64::try_from(eligible.len()).map_err(|_| {
            StoreError::Conflict("record trash cleanup inventory is too large".to_owned())
        })?;
        let blocked_count =
            u64::try_from(total_expired.saturating_sub(eligible_count)).map_err(|_| {
                StoreError::Conflict("record trash cleanup inventory is invalid".to_owned())
            })?;
        let preview_token = cleanup_preview_token(
            settings.record_trash_retention_days,
            &eligible,
            blocked_count,
        );
        Ok(RecordTrashCleanupPreview {
            retention_days: settings.record_trash_retention_days,
            cutoff_at_ms: Some(cutoff_at_ms),
            eligible,
            blocked_count,
            preview_token,
        })
    }

    pub(crate) fn purge_record_trash(
        &self,
        expected_preview_token: &str,
    ) -> Result<RecordTrashCleanupResult, StoreError> {
        if expected_preview_token.len() != 64
            || !expected_preview_token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(StoreError::Conflict(
                "record trash cleanup preview token is invalid".to_owned(),
            ));
        }
        let preview = self.preview_record_trash_cleanup_at(now_ms())?;
        if preview.preview_token != expected_preview_token {
            return Err(StoreError::Conflict(
                "record trash cleanup preview is stale".to_owned(),
            ));
        }
        if preview.eligible.is_empty() {
            return Ok(RecordTrashCleanupResult {
                deleted_record_count: 0,
                deleted_attachment_count: 0,
            });
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current_retention_days: Option<i64> = transaction.query_row(
            "SELECT record_trash_retention_days
             FROM product_settings WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )?;
        let current_retention_days = current_retention_days
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    StoreError::Conflict(
                        "product settings contain an invalid trash retention".to_owned(),
                    )
                })
            })
            .transpose()?;
        if validate_record_trash_retention_days(current_retention_days)? != preview.retention_days {
            return Err(StoreError::Conflict(
                "record trash cleanup preview is stale".to_owned(),
            ));
        }
        let mut deleted_records = 0_u64;
        let mut deleted_attachments = 0_u64;
        for candidate in &preview.eligible {
            let current = transaction
                .query_row(
                    "SELECT state, notebook_id, trashed_at_ms, revision,
                            applied_revision, sync_state
                     FROM records WHERE id = ?1 AND workspace_id = ?2",
                    params![candidate.record_id, candidate.workspace_id],
                    |row| {
                        Ok(RecordTrashCleanupCurrentState {
                            state: row.get(0)?,
                            notebook_id: row.get(1)?,
                            trashed_at_ms: row.get(2)?,
                            revision: row.get(3)?,
                            applied_revision: row.get(4)?,
                            sync_state: row.get(5)?,
                        })
                    },
                )
                .optional()?;
            let Some(current) = current else {
                return Err(StoreError::Conflict(
                    "record trash cleanup preview is stale".to_owned(),
                ));
            };
            if current.state != "trashed"
                || current.notebook_id != candidate.notebook_id
                || current.trashed_at_ms != Some(candidate.trashed_at_ms)
                || current.revision != current.applied_revision
                || !matches!(current.sync_state.as_str(), "local" | "synced")
                || !record_can_be_permanently_deleted(&transaction, &candidate.record_id)?
            {
                return Err(StoreError::Conflict(
                    "record trash cleanup preview is stale".to_owned(),
                ));
            }
            let attachment_count: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM attachments WHERE record_id = ?1",
                [&candidate.record_id],
                |row| row.get(0),
            )?;
            let attachment_count = u64::try_from(attachment_count).map_err(|_| {
                StoreError::Conflict("record trash cleanup inventory is invalid".to_owned())
            })?;
            if attachment_count != candidate.attachment_count {
                return Err(StoreError::Conflict(
                    "record trash cleanup preview is stale".to_owned(),
                ));
            }
            let changed = transaction.execute(
                "DELETE FROM records
                 WHERE id = ?1 AND workspace_id = ?2 AND state = 'trashed'
                   AND trashed_at_ms = ?3 AND revision = applied_revision
                   AND sync_state IN ('local', 'synced')",
                params![
                    candidate.record_id,
                    candidate.workspace_id,
                    candidate.trashed_at_ms
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "record trash cleanup preview is stale".to_owned(),
                ));
            }
            deleted_records = deleted_records.saturating_add(1);
            deleted_attachments = deleted_attachments.saturating_add(attachment_count);
        }
        transaction.commit()?;
        Ok(RecordTrashCleanupResult {
            deleted_record_count: deleted_records,
            deleted_attachment_count: deleted_attachments,
        })
    }

    pub fn record_update_check(
        &self,
        last_error_code: Option<&str>,
    ) -> Result<UpdateSettings, StoreError> {
        if let Some(code) = last_error_code {
            validate_update_error_code(code)?;
        }
        let now = now_ms();
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE update_settings
             SET last_checked_at_ms = ?1, last_error_code = ?2, updated_at_ms = ?1
             WHERE singleton_id = 1",
            params![now, last_error_code],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("update settings"));
        }
        drop(connection);
        self.update_settings()
    }

    pub fn default_identity_slot(&self) -> Result<Option<DefaultIdentitySlot>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT alias, locked, created_at_ms, updated_at_ms
                 FROM default_identity_slot WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(DefaultIdentitySlot {
                        alias: row.get(0)?,
                        locked: row.get(1)?,
                        created_at_ms: row.get(2)?,
                        updated_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn configure_default_identity_slot(
        &self,
        alias: &str,
        locked: bool,
    ) -> Result<DefaultIdentitySlot, StoreError> {
        let alias = validate_display_name(alias, "default identity alias")?;
        let now = now_ms();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO default_identity_slot (
                singleton_id, alias, locked, created_at_ms, updated_at_ms
             ) VALUES (1, ?1, ?2, ?3, ?3)
             ON CONFLICT(singleton_id) DO UPDATE SET
                alias = excluded.alias,
                locked = excluded.locked,
                updated_at_ms = excluded.updated_at_ms",
            params![alias, locked, now],
        )?;
        drop(connection);
        self.default_identity_slot()?
            .ok_or(StoreError::NotFound("default identity slot"))
    }

    pub fn unbind_default_identity_slot(&self) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM default_identity_slot WHERE singleton_id = 1",
                [],
            )
            .map(|changed| changed == 1)
            .map_err(StoreError::from)
    }

    pub fn set_active_selection(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
    ) -> Result<UiPreferences, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        if let Some(notebook_id) = notebook_id {
            validate_id(notebook_id, "notebook id")?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_workspace_exists(&transaction, workspace_id)?;
        if let Some(notebook_id) = notebook_id {
            ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
            transaction.execute(
                "INSERT INTO notebook_tab_state (notebook_id, is_pinned, updated_at_ms)
                 VALUES (?1, 1, ?2)
                 ON CONFLICT(notebook_id) DO UPDATE SET
                    is_pinned = 1,
                    updated_at_ms = excluded.updated_at_ms",
                params![notebook_id, now_ms()],
            )?;
        }
        let now = now_ms();
        transaction.execute(
            "INSERT INTO workspace_ui_state (workspace_id, selected_notebook_id, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_id) DO UPDATE SET
                selected_notebook_id = excluded.selected_notebook_id,
                updated_at_ms = excluded.updated_at_ms",
            params![workspace_id, notebook_id, now],
        )?;
        transaction.execute(
            "UPDATE app_ui_state
             SET active_workspace_id = ?1, updated_at_ms = ?2 WHERE singleton_id = 1",
            params![workspace_id, now],
        )?;
        let (theme, submit_shortcut, markdown_layout): (String, String, String) = transaction
            .query_row(
                "SELECT theme, submit_shortcut, markdown_layout
             FROM app_ui_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        transaction.commit()?;
        Ok(UiPreferences {
            active_workspace_id: Some(workspace_id.to_owned()),
            selected_notebook_id: notebook_id.map(str::to_owned),
            theme: ThemePreference::from_db_value(&theme)?,
            submit_shortcut: SubmitShortcut::from_db_value(&submit_shortcut)?,
            markdown_layout: MarkdownLayout::from_db_value(&markdown_layout)?,
        })
    }

    pub fn activate_workspace(&self, workspace_id: &str) -> Result<UiPreferences, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        ensure_workspace_exists(&connection, workspace_id)?;
        let selected_notebook_id = workspace_open_notebook_id(&connection, workspace_id)?;
        let ((theme, submit_shortcut, markdown_layout), now) = (
            connection.query_row(
                "SELECT theme, submit_shortcut, markdown_layout
                 FROM app_ui_state WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?,
            now_ms(),
        );
        connection.execute(
            "UPDATE app_ui_state
             SET active_workspace_id = ?1, updated_at_ms = ?2 WHERE singleton_id = 1",
            params![workspace_id, now],
        )?;
        Ok(UiPreferences {
            active_workspace_id: Some(workspace_id.to_owned()),
            selected_notebook_id,
            theme: ThemePreference::from_db_value(&theme)?,
            submit_shortcut: SubmitShortcut::from_db_value(&submit_shortcut)?,
            markdown_layout: MarkdownLayout::from_db_value(&markdown_layout)?,
        })
    }

    pub fn set_theme_preference(
        &self,
        theme: ThemePreference,
    ) -> Result<UiPreferences, StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE app_ui_state SET theme = ?1, updated_at_ms = ?2 WHERE singleton_id = 1",
            params![theme.as_db_value(), now_ms()],
        )?;
        drop(connection);
        self.ui_preferences()
    }

    pub fn set_submit_shortcut(
        &self,
        submit_shortcut: SubmitShortcut,
    ) -> Result<UiPreferences, StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE app_ui_state
             SET submit_shortcut = ?1, updated_at_ms = ?2 WHERE singleton_id = 1",
            params![submit_shortcut.as_db_value(), now_ms()],
        )?;
        drop(connection);
        self.ui_preferences()
    }

    pub fn set_markdown_layout(
        &self,
        markdown_layout: MarkdownLayout,
    ) -> Result<UiPreferences, StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE app_ui_state
             SET markdown_layout = ?1, updated_at_ms = ?2 WHERE singleton_id = 1",
            params![markdown_layout.as_db_value(), now_ms()],
        )?;
        drop(connection);
        self.ui_preferences()
    }

    #[cfg(test)]
    pub fn create_notebook(
        &self,
        workspace_id: &str,
        display_name: &str,
        relative_path: &str,
        numbering_style: NumberingStyle,
    ) -> Result<Notebook, StoreError> {
        self.create_notebook_with_numbering_start(
            workspace_id,
            display_name,
            relative_path,
            numbering_style,
            crate::domain::DEFAULT_NUMBERING_START,
        )
    }

    #[cfg(test)]
    pub fn create_notebook_with_numbering_start(
        &self,
        workspace_id: &str,
        display_name: &str,
        relative_path: &str,
        numbering_style: NumberingStyle,
        numbering_start: u32,
    ) -> Result<Notebook, StoreError> {
        self.create_notebook_with_configuration(
            workspace_id,
            display_name,
            relative_path,
            numbering_style,
            numbering_start,
            None,
        )
    }

    pub fn create_notebook_with_configuration(
        &self,
        workspace_id: &str,
        display_name: &str,
        relative_path: &str,
        numbering_style: NumberingStyle,
        numbering_start: u32,
        attachment_directory: Option<&str>,
    ) -> Result<Notebook, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let display_name = validate_display_name(display_name, "notebook name")?;
        let relative_path = validate_notebook_relative_path(relative_path)?;
        let numbering_start = validate_numbering_start(numbering_start)?;
        let attachment_directory = match attachment_directory {
            Some(value) => validate_attachment_directory(value)?,
            None => default_attachment_directory(&relative_path)?,
        };
        let id = new_id();
        let target_id = new_id();
        let now = now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_workspace_exists(&transaction, workspace_id)?;

        let duplicate: Option<String> = transaction
            .query_row(
                "SELECT id FROM notebooks WHERE workspace_id = ?1 AND relative_path = ?2",
                params![workspace_id, relative_path],
                |row| row.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(StoreError::Conflict(
                "that Markdown file is already bound in this workspace".to_owned(),
            ));
        }

        let ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM notebooks WHERE workspace_id = ?1",
            [workspace_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO notebooks (
                id, workspace_id, target_id, display_name, relative_path, ordinal,
                numbering_style, numbering_start, attachment_directory,
                target_state, last_error_code, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?11)",
            params![
                id,
                workspace_id,
                target_id,
                display_name,
                relative_path,
                ordinal,
                numbering_style.as_db_value(),
                i64::from(numbering_start),
                attachment_directory,
                NotebookTargetState::Unverified.as_db_value(),
                now
            ],
        )?;
        transaction.commit()?;

        Ok(Notebook {
            id,
            workspace_id: workspace_id.to_owned(),
            target_id,
            display_name,
            relative_path,
            ordinal,
            is_pinned: true,
            numbering_style,
            numbering_start,
            numbering_sync_pending: false,
            attachment_directory,
            previous_attachment_directory: None,
            attachment_directory_sync_pending: false,
            target_state: NotebookTargetState::Unverified,
            last_error_code: None,
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    pub fn list_notebooks(&self, workspace_id: &str) -> Result<Vec<Notebook>, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        ensure_workspace_exists(&connection, workspace_id)?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE workspace_id = ?1 ORDER BY ordinal, display_name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([workspace_id], notebook_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn rename_notebook(
        &self,
        workspace_id: &str,
        notebook_id: &str,
        display_name: &str,
    ) -> Result<Notebook, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(notebook_id, "notebook id")?;
        let display_name = validate_display_name(display_name, "notebook name")?;
        let connection = self.connection()?;
        ensure_notebook_belongs_to_workspace(&connection, notebook_id, workspace_id)?;
        let duplicate: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM notebooks
                WHERE workspace_id = ?1 AND id <> ?2
                  AND display_name = ?3 COLLATE NOCASE
             )",
            params![workspace_id, notebook_id, display_name],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(StoreError::Conflict(
                "that notebook display name is already used in this workspace".to_owned(),
            ));
        }
        let changed = connection.execute(
            "UPDATE notebooks SET display_name = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND workspace_id = ?4 AND display_name <> ?1",
            params![display_name, now_ms(), notebook_id, workspace_id],
        )?;
        if changed > 1 {
            return Err(StoreError::Conflict(
                "notebook display name update was not unique".to_owned(),
            ));
        }
        drop(connection);
        self.get_notebook(workspace_id, notebook_id)
    }

    pub fn workspace_open_preference(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceOpenPreference, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        ensure_workspace_exists(&connection, workspace_id)?;
        let preference = connection
            .query_row(
                "SELECT workspace_id, default_notebook_id, use_last_selection, updated_at_ms
                 FROM workspace_open_preferences WHERE workspace_id = ?1",
                [workspace_id],
                |row| {
                    Ok(WorkspaceOpenPreference {
                        workspace_id: row.get(0)?,
                        default_notebook_id: row.get(1)?,
                        use_last_selection: row.get(2)?,
                        updated_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_else(|| WorkspaceOpenPreference {
                workspace_id: workspace_id.to_owned(),
                default_notebook_id: None,
                use_last_selection: true,
                updated_at_ms: 0,
            });
        if let Some(notebook_id) = preference.default_notebook_id.as_deref() {
            ensure_notebook_belongs_to_workspace(&connection, notebook_id, workspace_id)?;
        }
        Ok(preference)
    }

    pub fn set_workspace_open_preference(
        &self,
        workspace_id: &str,
        use_last_selection: bool,
        default_notebook_id: Option<&str>,
    ) -> Result<WorkspaceOpenPreference, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        if let Some(notebook_id) = default_notebook_id {
            validate_id(notebook_id, "notebook id")?;
        }
        if use_last_selection && default_notebook_id.is_some() {
            return Err(StoreError::Conflict(
                "last selection cannot also name a default notebook".to_owned(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_workspace_exists(&transaction, workspace_id)?;
        if let Some(notebook_id) = default_notebook_id {
            ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
            transaction.execute(
                "INSERT INTO notebook_tab_state (notebook_id, is_pinned, updated_at_ms)
                 VALUES (?1, 1, ?2)
                 ON CONFLICT(notebook_id) DO UPDATE SET
                    is_pinned = 1,
                    updated_at_ms = excluded.updated_at_ms",
                params![notebook_id, now_ms()],
            )?;
        }
        transaction.execute(
            "INSERT INTO workspace_open_preferences (
                workspace_id, default_notebook_id, use_last_selection, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_id) DO UPDATE SET
                default_notebook_id = excluded.default_notebook_id,
                use_last_selection = excluded.use_last_selection,
                updated_at_ms = excluded.updated_at_ms",
            params![
                workspace_id,
                default_notebook_id,
                i64::from(use_last_selection),
                now_ms()
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        self.workspace_open_preference(workspace_id)
    }

    pub(crate) fn pending_numbering_notebooks(&self) -> Result<Vec<Notebook>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE numbering_sync_pending = 1
             ORDER BY updated_at_ms, id",
        )?;
        let rows = statement.query_map([], notebook_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn pending_attachment_directory_notebooks(
        &self,
    ) -> Result<Vec<Notebook>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE attachment_directory_sync_pending = 1
             ORDER BY updated_at_ms, id",
        )?;
        let rows = statement.query_map([], notebook_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn queue_notebook_attachment_directory(
        &self,
        notebook: &Notebook,
        next_directory: &str,
        expected_receipt_generation: u64,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        let next_directory = validate_attachment_directory(next_directory)?;
        if notebook.attachment_directory == next_directory
            && !notebook.attachment_directory_sync_pending
        {
            return Ok(notebook.clone());
        }
        let expected_generation = i64::try_from(expected_receipt_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let pending_record_operation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM recovery_operations
                WHERE phase NOT IN ('completed', 'superseded')
                  AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        let pending_attachment_operation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM attachment_file_operations operation
                JOIN attachments attachment ON attachment.id = operation.attachment_id
                JOIN records record ON record.id = attachment.record_id
                WHERE record.notebook_id = ?1
                  AND operation.phase IN ('queued', 'prepared', 'needs_recovery')
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        let pending_relocation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM attachments attachment
                JOIN records record ON record.id = attachment.record_id
                WHERE record.notebook_id = ?1
                  AND attachment.membership_state = 'active'
                  AND attachment.relocation_state <> 'ready'
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        if pending_record_operation || pending_attachment_operation || pending_relocation {
            return Err(StoreError::Conflict(
                "notebook has an unfinished record or attachment operation".to_owned(),
            ));
        }
        let receipt_matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM notebook_file_receipts
                WHERE notebook_id = ?1 AND target_id = ?2 AND generation = ?3
             )",
            params![notebook.id, notebook.target_id, expected_generation],
            |row| row.get(0),
        )?;
        if !receipt_matches {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before attachment directory confirmation".to_owned(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE notebooks
             SET attachment_directory = ?1,
                 previous_attachment_directory = ?2,
                 attachment_directory_sync_pending = 1,
                 updated_at_ms = ?3
             WHERE id = ?4 AND workspace_id = ?5 AND target_id = ?6
               AND attachment_directory = ?2
               AND attachment_directory_sync_pending = 0
               AND numbering_sync_pending = 0
               AND target_state = 'ready'",
            params![
                next_directory,
                notebook.attachment_directory,
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook attachment directory changed before confirmation".to_owned(),
            ));
        }

        let attachment_rows = {
            let mut statement = transaction.prepare(
                "SELECT attachment.id, attachment.media_type,
                        attachment.managed_relative_path, attachment.content_sha256,
                        record.state
                 FROM attachments attachment
                 JOIN records record ON record.id = attachment.record_id
                 WHERE record.notebook_id = ?1
                   AND attachment.membership_state = 'active'
                 ORDER BY attachment.created_at_ms, attachment.id",
            )?;
            let rows = statement.query_map([&notebook.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (attachment_id, media_type, current_path, content_sha256, record_state) in
            attachment_rows
        {
            validate_existing_managed_attachment_path(&current_path, &content_sha256, &media_type)?;
            let destination =
                managed_attachment_relative_path(&next_directory, &content_sha256, &media_type)?;
            if destination == current_path {
                continue;
            }
            if record_state == RecordState::Trashed.as_db_value() {
                transaction.execute(
                    "UPDATE attachments
                     SET managed_relative_path = ?2,
                         previous_managed_relative_path = NULL,
                         relocation_state = 'ready', relocation_error_code = NULL
                     WHERE id = ?1 AND managed_relative_path = ?3
                       AND relocation_state = 'ready'",
                    params![attachment_id, destination, current_path],
                )?;
            } else {
                let changed = transaction.execute(
                    "UPDATE attachments
                     SET managed_relative_path = ?2,
                         previous_managed_relative_path = ?3,
                         relocation_state = 'pending', relocation_error_code = NULL
                     WHERE id = ?1 AND managed_relative_path = ?3
                       AND relocation_state = 'ready'",
                    params![attachment_id, destination, current_path],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict(
                        "attachment changed before directory relocation was queued".to_owned(),
                    ));
                }
            }
        }
        let queued = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(queued)
    }

    pub(crate) fn queue_notebook_numbering_configuration(
        &self,
        notebook: &Notebook,
        next_style: NumberingStyle,
        next_start: u32,
        expected_receipt_generation: u64,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        let next_start = validate_numbering_start(next_start)?;
        if notebook.numbering_style == next_style
            && notebook.numbering_start == next_start
            && !notebook.numbering_sync_pending
        {
            return Ok(notebook.clone());
        }
        let expected_generation = i64::try_from(expected_receipt_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let pending_record_operation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM recovery_operations
                WHERE phase NOT IN ('completed', 'superseded')
                  AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        if pending_record_operation {
            return Err(StoreError::Conflict(
                "notebook has an unfinished record operation".to_owned(),
            ));
        }
        let receipt_matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM notebook_file_receipts
                WHERE notebook_id = ?1 AND target_id = ?2 AND generation = ?3
             )",
            params![notebook.id, notebook.target_id, expected_generation],
            |row| row.get(0),
        )?;
        if !receipt_matches {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before numbering preview confirmation".to_owned(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE notebooks
             SET numbering_style = ?1, numbering_start = ?2,
                 numbering_sync_pending = 1, updated_at_ms = ?3
             WHERE id = ?4 AND workspace_id = ?5 AND target_id = ?6
               AND numbering_style = ?7 AND numbering_start = ?8
               AND numbering_sync_pending = 0
               AND target_state = 'ready'",
            params![
                next_style.as_db_value(),
                i64::from(next_start),
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                notebook.numbering_style.as_db_value(),
                i64::from(notebook.numbering_start)
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook numbering configuration changed before confirmation".to_owned(),
            ));
        }
        let queued = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(queued)
    }

    pub(crate) fn cancel_notebook_numbering_configuration(
        &self,
        notebook: &Notebook,
        previous_style: NumberingStyle,
        previous_start: u32,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        let previous_start = validate_numbering_start(previous_start)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE notebooks
             SET numbering_style = ?1, numbering_start = ?2,
                 numbering_sync_pending = 0, updated_at_ms = ?3
             WHERE id = ?4 AND workspace_id = ?5 AND target_id = ?6
               AND numbering_style = ?7 AND numbering_start = ?8
               AND numbering_sync_pending = 1",
            params![
                previous_style.as_db_value(),
                i64::from(previous_start),
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                notebook.numbering_style.as_db_value(),
                i64::from(notebook.numbering_start)
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "pending notebook numbering configuration changed before cancellation".to_owned(),
            ));
        }
        drop(connection);
        self.get_notebook(&notebook.workspace_id, &notebook.id)
    }

    pub fn set_notebook_pinned(
        &self,
        workspace_id: &str,
        notebook_id: &str,
        pinned: bool,
    ) -> Result<Notebook, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(notebook_id, "notebook id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
        transaction.execute(
            "INSERT INTO notebook_tab_state (notebook_id, is_pinned, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(notebook_id) DO UPDATE SET
                is_pinned = excluded.is_pinned,
                updated_at_ms = excluded.updated_at_ms",
            params![notebook_id, i64::from(pinned), now_ms()],
        )?;
        if !pinned {
            transaction.execute(
                "UPDATE workspace_ui_state
                 SET selected_notebook_id = NULL, updated_at_ms = ?3
                 WHERE workspace_id = ?1 AND selected_notebook_id = ?2",
                params![workspace_id, notebook_id, now_ms()],
            )?;
            transaction.execute(
                "UPDATE workspace_open_preferences
                 SET default_notebook_id = NULL, use_last_selection = 0, updated_at_ms = ?3
                 WHERE workspace_id = ?1 AND default_notebook_id = ?2",
                params![workspace_id, notebook_id, now_ms()],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_notebook(workspace_id, notebook_id)
    }

    pub fn unbind_notebook(
        &self,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(notebook_id, "notebook id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
        let (target_state, numbering_sync_pending): (String, bool) = transaction.query_row(
            "SELECT target_state, numbering_sync_pending FROM notebooks WHERE id = ?1",
            [notebook_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if numbering_sync_pending {
            return Err(StoreError::Conflict(
                "notebook numbering rewrite must finish before unbinding".to_owned(),
            ));
        }
        if target_state != "unbound" {
            ensure_notebook_targets_available(&transaction, Some(notebook_id), Some(notebook_id))?;
            transaction.execute(
                "UPDATE notebooks
                 SET target_state = 'unbound', last_error_code = NULL, updated_at_ms = ?3
                 WHERE id = ?1 AND workspace_id = ?2",
                params![notebook_id, workspace_id, now_ms()],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_notebook(workspace_id, notebook_id)
    }

    pub(crate) fn notebook_lifecycle_available(
        &self,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, StoreError> {
        let notebook = self.get_notebook(workspace_id, notebook_id)?;
        if !matches!(
            notebook.target_state,
            NotebookTargetState::Ready | NotebookTargetState::Unbound
        ) {
            return Err(StoreError::Conflict(
                "速记本文件当前不可执行该操作".to_owned(),
            ));
        }
        let connection = self.connection()?;
        if notebook.numbering_sync_pending {
            return Err(StoreError::Conflict(
                "速记本编号重排仍在等待同步".to_owned(),
            ));
        }
        let pending: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM recovery_operations
                WHERE phase NOT IN ('completed', 'superseded')
                  AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)
             )",
            [notebook_id],
            |row| row.get(0),
        )?;
        if pending {
            return Err(StoreError::Conflict(
                "速记本仍有未完成的文件操作".to_owned(),
            ));
        }
        Ok(notebook)
    }

    pub fn reorder_notebooks(
        &self,
        workspace_id: &str,
        ordered_notebook_ids: &[String],
    ) -> Result<Vec<Notebook>, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        if ordered_notebook_ids.len() > 256 {
            return Err(StoreError::Conflict(
                "too many notebook tabs to reorder".to_owned(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for notebook_id in ordered_notebook_ids {
            validate_id(notebook_id, "notebook id")?;
            if !seen.insert(notebook_id.as_str()) {
                return Err(StoreError::Conflict(
                    "notebook tab order contains duplicates".to_owned(),
                ));
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_workspace_exists(&transaction, workspace_id)?;
        let existing = {
            let mut statement = transaction
                .prepare("SELECT id FROM notebooks WHERE workspace_id = ?1 ORDER BY ordinal, id")?;
            let rows = statement.query_map([workspace_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let existing_set = existing
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        if existing_set != seen {
            return Err(StoreError::Conflict(
                "notebook tab order does not match the workspace".to_owned(),
            ));
        }
        let now = now_ms();
        for (index, notebook_id) in ordered_notebook_ids.iter().enumerate() {
            let ordinal = i64::try_from(index + 1)
                .map_err(|_| StoreError::Conflict("notebook tab order is too large".to_owned()))?
                * 1024;
            transaction.execute(
                "UPDATE notebooks SET ordinal = ?1, updated_at_ms = ?2
                 WHERE id = ?3 AND workspace_id = ?4",
                params![ordinal, now, notebook_id, workspace_id],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.list_notebooks(workspace_id)
    }

    pub fn save_draft(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        attachments: &[DraftAttachment],
    ) -> Result<SavedDraft, StoreError> {
        if body_markdown.len() > 1024 * 1024 || body_markdown.contains('\0') {
            return Err(StoreError::Conflict("draft text is invalid".to_owned()));
        }
        validate_draft_attachments(attachments)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let tab_key = draft_tab_key(&transaction, workspace_id, notebook_id)?;
        save_draft_at_tab_key(
            &transaction,
            workspace_id,
            &tab_key,
            body_markdown,
            attachments,
        )?;
        transaction.commit()?;
        Ok(SavedDraft {
            body_markdown: body_markdown.to_owned(),
            attachments: attachments.to_vec(),
        })
    }

    pub fn save_surface_draft(
        &self,
        surface: &str,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        attachments: &[DraftAttachment],
    ) -> Result<SavedDraft, StoreError> {
        validate_draft_surface(surface)?;
        if body_markdown.len() > 1024 * 1024 || body_markdown.contains('\0') {
            return Err(StoreError::Conflict("draft text is invalid".to_owned()));
        }
        validate_draft_attachments(attachments)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let tab_key = surface_draft_tab_key(&transaction, surface, workspace_id, notebook_id)?;
        save_draft_at_tab_key(
            &transaction,
            workspace_id,
            &tab_key,
            body_markdown,
            attachments,
        )?;
        transaction.commit()?;
        Ok(SavedDraft {
            body_markdown: body_markdown.to_owned(),
            attachments: attachments.to_vec(),
        })
    }

    pub fn load_draft(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
    ) -> Result<SavedDraft, StoreError> {
        let connection = self.connection()?;
        let tab_key = draft_tab_key(&connection, workspace_id, notebook_id)?;
        load_draft_at_tab_key(&connection, workspace_id, &tab_key)
    }

    pub fn load_surface_draft(
        &self,
        surface: &str,
        workspace_id: &str,
        notebook_id: Option<&str>,
    ) -> Result<SavedDraft, StoreError> {
        validate_draft_surface(surface)?;
        let connection = self.connection()?;
        let tab_key = surface_draft_tab_key(&connection, surface, workspace_id, notebook_id)?;
        load_draft_at_tab_key(&connection, workspace_id, &tab_key)
    }

    pub(crate) fn unverified_notebooks(&self) -> Result<Vec<Notebook>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE target_state = 'unverified'
             ORDER BY created_at_ms, id",
        )?;
        let rows = statement.query_map([], notebook_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn initialize_notebook_file_receipt(
        &self,
        receipt: &NotebookFileReceiptInput,
    ) -> Result<Notebook, StoreError> {
        validate_id(&receipt.notebook_id, "notebook id")?;
        validate_id(&receipt.target_id, "target id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT target_id, marker_schema, managed_sha256, file_sha256,
                        file_modified_ns, file_size_bytes, generation
                 FROM notebook_file_receipts WHERE notebook_id = ?1",
                [&receipt.notebook_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        if let Some((
            target_id,
            marker_schema,
            managed_sha256,
            file_sha256,
            stored_modified_ns,
            stored_size_bytes,
            generation,
        )) = existing
        {
            if target_id != receipt.target_id
                || marker_schema != i64::from(receipt.marker_schema)
                || managed_sha256 != receipt.managed_sha256
                || file_sha256 != receipt.file_sha256
                || stored_modified_ns != modified_ns
                || stored_size_bytes != size_bytes
                || generation != 1
            {
                return Err(StoreError::Conflict(
                    "notebook initialization receipt changed".to_owned(),
                ));
            }
        } else {
            transaction.execute(
                "INSERT INTO notebook_file_receipts (
                    notebook_id, target_id, marker_schema, managed_sha256, file_sha256,
                    file_modified_ns, file_size_bytes, generation, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
                params![
                    receipt.notebook_id,
                    receipt.target_id,
                    i64::from(receipt.marker_schema),
                    receipt.managed_sha256,
                    receipt.file_sha256,
                    modified_ns,
                    size_bytes,
                    now_ms()
                ],
            )?;
        }
        transaction.execute(
            "UPDATE notebooks
             SET numbering_sync_pending = 0, target_state = 'ready',
                 last_error_code = NULL, updated_at_ms = ?2
             WHERE id = ?1 AND target_id = ?3",
            params![receipt.notebook_id, now_ms(), receipt.target_id],
        )?;
        let notebook = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&receipt.notebook_id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(notebook)
    }

    pub fn get_notebook(
        &self,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Notebook, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(notebook_id, "notebook id")?;
        let connection = self.connection()?;
        ensure_notebook_belongs_to_workspace(&connection, notebook_id, workspace_id)?;
        connection
            .query_row(
                "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                        numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                        created_at_ms, updated_at_ms, attachment_directory,
                        previous_attachment_directory, attachment_directory_sync_pending,
                        COALESCE((SELECT is_pinned FROM notebook_tab_state
                                  WHERE notebook_id = notebooks.id), 1)
                 FROM notebooks WHERE id = ?1",
                [notebook_id],
                notebook_from_row,
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn get_record(
        &self,
        workspace_id: &str,
        record_id: &str,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(record_id, "record id")?;
        let connection = self.connection()?;
        record_for_workspace(&connection, workspace_id, record_id)
    }

    #[cfg(test)]
    pub fn create_record(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
    ) -> Result<Record, StoreError> {
        self.create_record_impl(workspace_id, notebook_id, body_markdown, &[], None)
    }

    #[cfg(test)]
    pub fn create_record_idempotent(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "create",
            workspace_id,
            &[notebook_id, Some(body_markdown)],
        )?;
        self.create_record_impl(
            workspace_id,
            notebook_id,
            body_markdown,
            &[],
            Some(mutation),
        )
    }

    pub fn create_record_with_attachments_idempotent(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        attachments: &[NewAttachment],
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let attachment_fingerprint = create_attachment_fingerprint(attachments);
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "create",
            workspace_id,
            &[
                notebook_id,
                Some(body_markdown),
                Some(&attachment_fingerprint),
            ],
        )?;
        self.create_record_impl(
            workspace_id,
            notebook_id,
            body_markdown,
            attachments,
            Some(mutation),
        )
    }

    pub(crate) fn create_record_from_draft_attempt(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        attachments: &[NewAttachment],
        proposed_mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        self.create_record_from_surface_draft_attempt(
            None,
            DraftRecordCreate {
                workspace_id,
                notebook_id,
                body_markdown,
                attachments,
                proposed_mutation_id,
                mutation_schema_version,
            },
        )
    }

    pub(crate) fn create_record_from_surface_draft_attempt(
        &self,
        surface: Option<&str>,
        input: DraftRecordCreate<'_>,
    ) -> Result<Record, StoreError> {
        let DraftRecordCreate {
            workspace_id,
            notebook_id,
            body_markdown,
            attachments,
            proposed_mutation_id,
            mutation_schema_version,
        } = input;
        if let Some(surface) = surface {
            validate_draft_surface(surface)?;
        }
        validate_new_attachments(attachments)?;
        let normalized_body = validate_record_markdown(body_markdown, attachments.len())?;
        let attachment_fingerprint = create_attachment_fingerprint(attachments);
        let proposed = mutation_spec(
            proposed_mutation_id,
            mutation_schema_version,
            "create",
            workspace_id,
            &[
                notebook_id,
                Some(body_markdown),
                Some(&attachment_fingerprint),
            ],
        )?;
        let draft_fingerprint = draft_create_fingerprint_from_new(&normalized_body, attachments);
        let resolved_mutation_id = {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            let tab_key = match surface {
                Some(surface) => {
                    surface_draft_tab_key(&transaction, surface, workspace_id, notebook_id)?
                }
                None => draft_tab_key(&transaction, workspace_id, notebook_id)?,
            };
            let existing = transaction
                .query_row(
                    "SELECT mutation_id, draft_fingerprint, request_sha256
                     FROM draft_create_attempts
                     WHERE workspace_id = ?1 AND tab_key = ?2",
                    params![workspace_id, tab_key],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let now = now_ms();
            let mutation_id = match existing {
                Some((mutation_id, saved_draft_fingerprint, saved_request_sha256))
                    if saved_draft_fingerprint == draft_fingerprint
                        && saved_request_sha256 == proposed.request_sha256 =>
                {
                    validate_id(&mutation_id, "mutation id")?;
                    transaction.execute(
                        "UPDATE draft_create_attempts SET updated_at_ms = ?3
                         WHERE workspace_id = ?1 AND tab_key = ?2",
                        params![workspace_id, tab_key, now],
                    )?;
                    mutation_id
                }
                _ => {
                    transaction.execute(
                        "INSERT INTO draft_create_attempts (
                            workspace_id, tab_key, mutation_id, draft_fingerprint,
                            request_sha256, created_at_ms, updated_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                         ON CONFLICT(workspace_id, tab_key) DO UPDATE SET
                            mutation_id = excluded.mutation_id,
                            draft_fingerprint = excluded.draft_fingerprint,
                            request_sha256 = excluded.request_sha256,
                            created_at_ms = excluded.created_at_ms,
                            updated_at_ms = excluded.updated_at_ms",
                        params![
                            workspace_id,
                            tab_key,
                            proposed_mutation_id,
                            draft_fingerprint,
                            proposed.request_sha256,
                            now
                        ],
                    )?;
                    proposed_mutation_id.to_owned()
                }
            };
            transaction.commit()?;
            mutation_id
        };
        self.create_record_with_attachments_idempotent(
            workspace_id,
            notebook_id,
            body_markdown,
            attachments,
            &resolved_mutation_id,
            mutation_schema_version,
        )
    }

    fn create_record_impl(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        body_markdown: &str,
        attachment_inputs: &[NewAttachment],
        mutation: Option<MutationSpec<'_>>,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        if let Some(notebook_id) = notebook_id {
            validate_id(notebook_id, "notebook id")?;
        }
        validate_new_attachments(attachment_inputs)?;
        let body_markdown = validate_record_markdown(body_markdown, attachment_inputs.len())?;
        let id = new_id();
        let now = now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_workspace_exists(&transaction, workspace_id)?;
        if let Some(mutation) = &mutation {
            if let Some(record) = replay_mutation(&transaction, mutation, None)? {
                return Ok(record);
            }
        }
        if let Some(notebook_id) = notebook_id {
            ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
            ensure_notebook_targets_available(&transaction, None, Some(notebook_id))?;
        }

        let logical_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(logical_order), 0) + 1024
             FROM records
             WHERE workspace_id = ?1 AND notebook_id IS ?2 AND state = 'active'",
            params![workspace_id, notebook_id],
            |row| row.get(0),
        )?;
        let sync_state = if notebook_id.is_some() {
            SyncState::Queued
        } else {
            SyncState::Local
        };
        let applied_revision = if notebook_id.is_some() { 0 } else { 1 };
        transaction.execute(
            "INSERT INTO records (
                id, workspace_id, notebook_id, body_markdown, created_at_ms, updated_at_ms,
                logical_order, state, sync_state, revision, applied_revision, trashed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, 1, ?9, NULL)",
            params![
                id,
                workspace_id,
                notebook_id,
                body_markdown,
                now,
                logical_order,
                RecordState::Active.as_db_value(),
                sync_state.as_db_value(),
                revision_to_db(applied_revision)?
            ],
        )?;
        let mut attachments = Vec::with_capacity(attachment_inputs.len());
        for (ordinal, attachment) in attachment_inputs.iter().enumerate() {
            let attachment_id = new_id();
            transaction.execute(
                "INSERT INTO attachments (
                    id, record_id, media_type, managed_relative_path, content_sha256,
                    byte_size, created_at_ms, previous_managed_relative_path,
                    relocation_state, relocation_error_code, ordinal, membership_state
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, 'active')",
                params![
                    attachment_id,
                    id,
                    attachment.media_type,
                    attachment.managed_relative_path,
                    attachment.content_sha256,
                    i64::try_from(attachment.byte_size).map_err(|_| StoreError::Conflict(
                        "attachment size exceeds the database limit".to_owned()
                    ))?,
                    now,
                    attachment.previous_managed_relative_path,
                    if attachment.previous_managed_relative_path.is_some() {
                        AttachmentRelocationState::Pending.as_db_value()
                    } else {
                        AttachmentRelocationState::Ready.as_db_value()
                    },
                    i64::try_from(ordinal).map_err(|_| StoreError::Conflict(
                        "attachment ordinal exceeds the database limit".to_owned()
                    ))?
                ],
            )?;
            attachments.push(Attachment {
                id: attachment_id,
                record_id: id.clone(),
                media_type: attachment.media_type.clone(),
                managed_relative_path: attachment.managed_relative_path.clone(),
                content_sha256: attachment.content_sha256.clone(),
                byte_size: attachment.byte_size,
                created_at_ms: now,
                file_state: AttachmentFileState::Ready,
                previous_managed_relative_path: attachment.previous_managed_relative_path.clone(),
                relocation_state: if attachment.previous_managed_relative_path.is_some() {
                    AttachmentRelocationState::Pending
                } else {
                    AttachmentRelocationState::Ready
                },
                relocation_error_code: None,
            });
        }
        if let Some(notebook_id) = notebook_id {
            queue_recovery_operation(
                &transaction,
                RecoveryIntent {
                    workspace_id,
                    record_id: &id,
                    operation_kind: "create",
                    expected_revision: 0,
                    desired_revision: 1,
                    desired_state: RecordState::Active,
                    source_notebook_id: None,
                    destination_notebook_id: Some(notebook_id),
                    source_logical_order: None,
                    destination_logical_order: Some(logical_order),
                },
                now,
            )?;
        }
        let record = Record {
            id,
            workspace_id: workspace_id.to_owned(),
            notebook_id: notebook_id.map(str::to_owned),
            body_markdown,
            created_at_ms: now,
            updated_at_ms: now,
            logical_order,
            state: RecordState::Active,
            sync_state,
            revision: 1,
            applied_revision,
            trashed_at_ms: None,
            is_pinned: false,
            attachments,
        };
        if let Some(mutation) = &mutation {
            record_mutation_receipt(&transaction, mutation, &record, now)?;
        }
        transaction.commit()?;
        Ok(record)
    }

    pub fn list_records(
        &self,
        workspace_id: &str,
        notebook_id: Option<&str>,
        include_trashed: bool,
    ) -> Result<Vec<Record>, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        if let Some(notebook_id) = notebook_id {
            validate_id(notebook_id, "notebook id")?;
        }
        let connection = self.connection()?;
        ensure_workspace_exists(&connection, workspace_id)?;
        if let Some(notebook_id) = notebook_id {
            ensure_notebook_belongs_to_workspace(&connection, notebook_id, workspace_id)?;
        }

        let sql = if include_trashed {
            "SELECT id, workspace_id, notebook_id, body_markdown, created_at_ms, updated_at_ms,
                    logical_order, state, sync_state, revision, applied_revision, trashed_at_ms
             FROM records
             WHERE workspace_id = ?1 AND notebook_id IS ?2
             ORDER BY logical_order, created_at_ms"
        } else {
            "SELECT id, workspace_id, notebook_id, body_markdown, created_at_ms, updated_at_ms,
                    logical_order, state, sync_state, revision, applied_revision, trashed_at_ms
             FROM records
             WHERE workspace_id = ?1 AND notebook_id IS ?2 AND state = 'active'
             ORDER BY logical_order, created_at_ms"
        };
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(params![workspace_id, notebook_id], record_from_row)?;
        let mut records = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for record in &mut records {
            record.attachments = attachments_for_record(&connection, &record.id)?;
            record.is_pinned = record_is_pinned(&connection, &record.id)?;
        }
        Ok(records)
    }

    pub fn set_record_pinned(
        &self,
        workspace_id: &str,
        record_id: &str,
        pinned: bool,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(record_id, "record id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut record = record_for_workspace(&transaction, workspace_id, record_id)?;
        if record.state != RecordState::Active {
            return Err(StoreError::Conflict(
                "only active records can be pinned".to_owned(),
            ));
        }
        if pinned {
            transaction.execute(
                "INSERT INTO record_pins (record_id, pinned_at_ms) VALUES (?1, ?2)
                 ON CONFLICT(record_id) DO UPDATE SET pinned_at_ms = excluded.pinned_at_ms",
                params![record_id, now_ms()],
            )?;
        } else {
            transaction.execute("DELETE FROM record_pins WHERE record_id = ?1", [record_id])?;
        }
        transaction.commit()?;
        record.is_pinned = pinned;
        Ok(record)
    }

    #[cfg(test)]
    pub fn update_record(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        body_markdown: &str,
    ) -> Result<Record, StoreError> {
        self.update_record_impl(
            workspace_id,
            record_id,
            expected_revision,
            body_markdown,
            None,
            None,
        )
    }

    pub fn update_record_idempotent(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        body_markdown: &str,
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let expected_revision_text = expected_revision.to_string();
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "edit",
            workspace_id,
            &[
                Some(record_id),
                Some(&expected_revision_text),
                Some(body_markdown),
            ],
        )?;
        self.update_record_impl(
            workspace_id,
            record_id,
            expected_revision,
            body_markdown,
            None,
            Some(mutation),
        )
    }

    pub fn revise_record_attachment_set_idempotent(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        revision: RecordAttachmentSetRevision<'_>,
    ) -> Result<Record, StoreError> {
        validate_new_attachments(revision.new_attachments)?;
        let expected_revision_text = expected_revision.to_string();
        let retained_fingerprint = revision.retained_attachment_ids.join(":");
        let new_attachment_fingerprint = create_attachment_fingerprint(revision.new_attachments);
        let mutation = mutation_spec(
            revision.mutation_id,
            revision.mutation_schema_version,
            "edit",
            workspace_id,
            &[
                Some(record_id),
                Some(&expected_revision_text),
                Some(revision.body_markdown),
                Some(&retained_fingerprint),
                Some(&new_attachment_fingerprint),
            ],
        )?;
        self.update_record_impl(
            workspace_id,
            record_id,
            expected_revision,
            revision.body_markdown,
            Some((revision.retained_attachment_ids, revision.new_attachments)),
            Some(mutation),
        )
    }

    fn update_record_impl(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        body_markdown: &str,
        attachment_set: Option<(&[String], &[NewAttachment])>,
        mutation: Option<MutationSpec<'_>>,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(record_id, "record id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(mutation) = &mutation {
            if let Some(record) = replay_mutation(&transaction, mutation, Some(record_id))? {
                return Ok(record);
            }
        }
        let record = record_for_workspace(&transaction, workspace_id, record_id)?;
        require_active_revision(&record, expected_revision)?;
        let current_attachment_ids = record
            .attachments
            .iter()
            .map(|attachment| attachment.id.as_str())
            .collect::<Vec<_>>();
        let (final_attachment_count, attachments_changed) = match attachment_set {
            Some((retained_attachment_ids, new_attachments)) => {
                validate_attachment_set_revision(
                    &record.attachments,
                    retained_attachment_ids,
                    new_attachments,
                )?;
                (
                    retained_attachment_ids.len() + new_attachments.len(),
                    current_attachment_ids
                        != retained_attachment_ids
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                        || !new_attachments.is_empty(),
                )
            }
            None => (record.attachments.len(), false),
        };
        let body_markdown = validate_record_markdown(body_markdown, final_attachment_count)?;
        if record.body_markdown == body_markdown && !attachments_changed {
            if let Some(mutation) = &mutation {
                record_mutation_receipt(&transaction, mutation, &record, now_ms())?;
                transaction.commit()?;
            }
            return Ok(record);
        }
        ensure_record_mutation_available(&transaction, record_id)?;
        ensure_notebook_targets_available(
            &transaction,
            record.notebook_id.as_deref(),
            record.notebook_id.as_deref(),
        )?;
        let expected_revision_db = revision_to_db(expected_revision)?;

        let revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let revision_db = i64::try_from(revision)
            .map_err(|_| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let now = now_ms();
        let requires_file_sync = record.notebook_id.is_some();
        let sync_state = if requires_file_sync {
            SyncState::Queued
        } else {
            SyncState::Local
        };
        let applied_revision = if requires_file_sync {
            record.applied_revision
        } else {
            revision
        };
        let changed = transaction.execute(
            "UPDATE records
             SET body_markdown = ?1, updated_at_ms = ?2, sync_state = ?3, revision = ?4,
                 applied_revision = ?5
             WHERE id = ?6 AND workspace_id = ?7 AND revision = ?8 AND state = 'active'",
            params![
                body_markdown,
                now,
                sync_state.as_db_value(),
                revision_db,
                revision_to_db(applied_revision)?,
                record_id,
                workspace_id,
                expected_revision_db
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "record changed before the edit could be saved".to_owned(),
            ));
        }
        if let Some((retained_attachment_ids, new_attachments)) = attachment_set {
            apply_attachment_set_revision(
                &transaction,
                &record,
                retained_attachment_ids,
                new_attachments,
                revision,
                now,
            )?;
        }
        if requires_file_sync {
            queue_recovery_operation(
                &transaction,
                RecoveryIntent {
                    workspace_id,
                    record_id,
                    operation_kind: "edit",
                    expected_revision,
                    desired_revision: revision,
                    desired_state: RecordState::Active,
                    source_notebook_id: record.notebook_id.as_deref(),
                    destination_notebook_id: record.notebook_id.as_deref(),
                    source_logical_order: Some(record.logical_order),
                    destination_logical_order: Some(record.logical_order),
                },
                now,
            )?;
        }
        let updated = record_for_workspace(&transaction, workspace_id, record_id)?;
        if let Some(mutation) = &mutation {
            record_mutation_receipt(&transaction, mutation, &updated, now)?;
        }
        transaction.commit()?;
        Ok(updated)
    }

    #[cfg(test)]
    pub fn trash_record(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
    ) -> Result<Record, StoreError> {
        self.set_record_state(
            workspace_id,
            record_id,
            expected_revision,
            RecordState::Active,
            RecordState::Trashed,
            None,
        )
    }

    pub fn trash_record_idempotent(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let expected_revision_text = expected_revision.to_string();
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "trash",
            workspace_id,
            &[Some(record_id), Some(&expected_revision_text)],
        )?;
        self.set_record_state(
            workspace_id,
            record_id,
            expected_revision,
            RecordState::Active,
            RecordState::Trashed,
            Some(mutation),
        )
    }

    #[cfg(test)]
    pub fn restore_record(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
    ) -> Result<Record, StoreError> {
        self.set_record_state(
            workspace_id,
            record_id,
            expected_revision,
            RecordState::Trashed,
            RecordState::Active,
            None,
        )
    }

    pub fn restore_record_idempotent(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let expected_revision_text = expected_revision.to_string();
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "restore",
            workspace_id,
            &[Some(record_id), Some(&expected_revision_text)],
        )?;
        self.set_record_state(
            workspace_id,
            record_id,
            expected_revision,
            RecordState::Trashed,
            RecordState::Active,
            Some(mutation),
        )
    }

    #[cfg(test)]
    pub fn migrate_record(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        destination_notebook_id: Option<&str>,
    ) -> Result<Record, StoreError> {
        self.migrate_record_impl(
            workspace_id,
            record_id,
            expected_revision,
            destination_notebook_id,
            None,
        )
    }

    pub fn migrate_record_idempotent(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        destination_notebook_id: Option<&str>,
        mutation_id: &str,
        mutation_schema_version: u32,
    ) -> Result<Record, StoreError> {
        let expected_revision_text = expected_revision.to_string();
        let mutation = mutation_spec(
            mutation_id,
            mutation_schema_version,
            "migrate",
            workspace_id,
            &[
                Some(record_id),
                Some(&expected_revision_text),
                destination_notebook_id,
            ],
        )?;
        self.migrate_record_impl(
            workspace_id,
            record_id,
            expected_revision,
            destination_notebook_id,
            Some(mutation),
        )
    }

    fn migrate_record_impl(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        destination_notebook_id: Option<&str>,
        mutation: Option<MutationSpec<'_>>,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(record_id, "record id")?;
        if let Some(notebook_id) = destination_notebook_id {
            validate_id(notebook_id, "destination notebook id")?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(mutation) = &mutation {
            if let Some(record) = replay_mutation(&transaction, mutation, Some(record_id))? {
                return Ok(record);
            }
        }
        let record = record_for_workspace(&transaction, workspace_id, record_id)?;
        require_active_revision(&record, expected_revision)?;
        let expected_revision_db = revision_to_db(expected_revision)?;
        if record.notebook_id.as_deref() == destination_notebook_id {
            if let Some(mutation) = &mutation {
                record_mutation_receipt(&transaction, mutation, &record, now_ms())?;
                transaction.commit()?;
            }
            return Ok(record);
        }
        if let Some(notebook_id) = destination_notebook_id {
            ensure_notebook_belongs_to_workspace(&transaction, notebook_id, workspace_id)?;
        }
        ensure_record_mutation_available(&transaction, record_id)?;
        ensure_notebook_targets_available(
            &transaction,
            record.notebook_id.as_deref(),
            destination_notebook_id,
        )?;

        let logical_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(logical_order), 0) + 1024
             FROM records
             WHERE workspace_id = ?1 AND notebook_id IS ?2 AND state = 'active'",
            params![workspace_id, destination_notebook_id],
            |row| row.get(0),
        )?;
        let revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let revision_db = i64::try_from(revision)
            .map_err(|_| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let now = now_ms();
        let requires_file_sync = record.notebook_id.is_some() || destination_notebook_id.is_some();
        let sync_state = if requires_file_sync {
            SyncState::Queued
        } else {
            SyncState::Local
        };
        let applied_revision = if requires_file_sync {
            record.applied_revision
        } else {
            revision
        };
        let changed = transaction.execute(
            "UPDATE records
             SET notebook_id = ?1, logical_order = ?2, updated_at_ms = ?3,
                 sync_state = ?4, revision = ?5, applied_revision = ?6
             WHERE id = ?7 AND workspace_id = ?8 AND revision = ?9 AND state = 'active'",
            params![
                destination_notebook_id,
                logical_order,
                now,
                sync_state.as_db_value(),
                revision_db,
                revision_to_db(applied_revision)?,
                record_id,
                workspace_id,
                expected_revision_db
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "record changed before the migration could be saved".to_owned(),
            ));
        }
        let destination_attachment_directory = match destination_notebook_id {
            Some(notebook_id) => transaction.query_row(
                "SELECT attachment_directory FROM notebooks WHERE id = ?1",
                [notebook_id],
                |row| row.get::<_, String>(0),
            )?,
            None => INBOX_ATTACHMENT_DIRECTORY.to_owned(),
        };
        queue_record_attachment_relocations(
            &transaction,
            record_id,
            &destination_attachment_directory,
        )?;
        if requires_file_sync {
            queue_recovery_operation(
                &transaction,
                RecoveryIntent {
                    workspace_id,
                    record_id,
                    operation_kind: "migrate",
                    expected_revision,
                    desired_revision: revision,
                    desired_state: RecordState::Active,
                    source_notebook_id: record.notebook_id.as_deref(),
                    destination_notebook_id,
                    source_logical_order: Some(record.logical_order),
                    destination_logical_order: Some(logical_order),
                },
                now,
            )?;
        }
        let migrated = Record {
            notebook_id: destination_notebook_id.map(str::to_owned),
            logical_order,
            updated_at_ms: now,
            sync_state,
            revision,
            applied_revision,
            attachments: attachments_for_record(&transaction, record_id)?,
            ..record
        };
        if let Some(mutation) = &mutation {
            record_mutation_receipt(&transaction, mutation, &migrated, now)?;
        }
        transaction.commit()?;
        Ok(migrated)
    }

    pub(crate) fn recovery_operation_for_record_revision(
        &self,
        record_id: &str,
        desired_revision: u64,
    ) -> Result<Option<RecoveryOperation>, StoreError> {
        validate_id(record_id, "record id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, workspace_id, record_id, operation_kind, phase,
                        expected_revision, desired_revision, desired_record_state,
                        source_notebook_id, destination_notebook_id,
                        source_logical_order, destination_logical_order,
                        attempt_count, last_error_code
                 FROM recovery_operations
                 WHERE record_id = ?1 AND desired_revision = ?2",
                params![record_id, revision_to_db(desired_revision)?],
                recovery_operation_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(crate) fn pending_recovery_operations(&self) -> Result<Vec<RecoveryOperation>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, record_id, operation_kind, phase,
                    expected_revision, desired_revision, desired_record_state,
                    source_notebook_id, destination_notebook_id,
                    source_logical_order, destination_logical_order,
                    attempt_count, last_error_code
             FROM recovery_operations
             WHERE phase NOT IN ('completed', 'superseded')
             ORDER BY created_at_ms, id",
        )?;
        let rows = statement.query_map([], recovery_operation_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn attachment_file_operations_for_record_revision(
        &self,
        record_id: &str,
        record_revision: u64,
    ) -> Result<Vec<AttachmentFileOperation>, StoreError> {
        validate_id(record_id, "record id")?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT operation.attachment_id, attachment.record_id, record.workspace_id,
                    operation.action, operation.phase, operation.record_revision,
                    attachment.managed_relative_path, attachment.media_type,
                    attachment.content_sha256, attachment.byte_size, operation.trash_path,
                    operation.backup_app_data_relative_path, operation.attempt_count,
                    operation.last_error_code
             FROM attachment_file_operations operation
             JOIN attachments attachment ON attachment.id = operation.attachment_id
             JOIN records record ON record.id = attachment.record_id
             WHERE attachment.record_id = ?1 AND operation.record_revision = ?2
             ORDER BY attachment.created_at_ms, attachment.id",
        )?;
        let rows = statement.query_map(
            params![record_id, revision_to_db(record_revision)?],
            attachment_file_operation_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn pending_attachment_file_operations(
        &self,
    ) -> Result<Vec<AttachmentFileOperation>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT operation.attachment_id, attachment.record_id, record.workspace_id,
                    operation.action, operation.phase, operation.record_revision,
                    attachment.managed_relative_path, attachment.media_type,
                    attachment.content_sha256, attachment.byte_size, operation.trash_path,
                    operation.backup_app_data_relative_path, operation.attempt_count,
                    operation.last_error_code
             FROM attachment_file_operations operation
             JOIN attachments attachment ON attachment.id = operation.attachment_id
             JOIN records record ON record.id = attachment.record_id
             WHERE operation.phase IN ('queued', 'prepared', 'needs_recovery')
                OR operation.backup_app_data_relative_path IS NOT NULL
             ORDER BY operation.updated_at_ms, operation.attachment_id",
        )?;
        let rows = statement.query_map([], attachment_file_operation_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn queue_attachment_relocations_for_configured_directories(
        &self,
    ) -> Result<usize, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT attachment.id, attachment.managed_relative_path,
                        attachment.media_type, attachment.content_sha256,
                        record.state, record.notebook_id,
                        notebook.attachment_directory, operation.phase
                 FROM attachments attachment
                 JOIN records record ON record.id = attachment.record_id
                 LEFT JOIN notebooks notebook ON notebook.id = record.notebook_id
                 LEFT JOIN attachment_file_operations operation
                    ON operation.attachment_id = attachment.id
                 WHERE attachment.membership_state = 'active'
                   AND attachment.relocation_state = 'ready'
                 ORDER BY attachment.created_at_ms, attachment.id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut queued = 0_usize;
        let now = now_ms();
        for (
            attachment_id,
            current_path,
            media_type,
            content_sha256,
            record_state,
            notebook_id,
            notebook_directory,
            file_operation_phase,
        ) in candidates
        {
            let directory = match (notebook_id.as_deref(), notebook_directory.as_deref()) {
                (Some(_), Some(directory)) => directory,
                (None, None) => INBOX_ATTACHMENT_DIRECTORY,
                _ => {
                    return Err(StoreError::Conflict(
                        "attachment notebook directory is unavailable".to_owned(),
                    ));
                }
            };
            let desired_path =
                managed_attachment_relative_path(directory, &content_sha256, &media_type)?;
            validate_existing_managed_attachment_path(&current_path, &content_sha256, &media_type)?;
            if current_path == desired_path {
                continue;
            }
            if matches!(
                file_operation_phase.as_deref(),
                Some("queued" | "prepared" | "needs_recovery")
            ) {
                continue;
            }
            if record_state == RecordState::Trashed.as_db_value() {
                transaction.execute(
                    "UPDATE attachments
                     SET managed_relative_path = ?2,
                         previous_managed_relative_path = NULL,
                         relocation_state = 'ready', relocation_error_code = NULL
                     WHERE id = ?1 AND relocation_state = 'ready'
                       AND managed_relative_path = ?3",
                    params![attachment_id, desired_path, current_path],
                )?;
                continue;
            }
            if record_state != RecordState::Active.as_db_value() {
                return Err(StoreError::Conflict(
                    "attachment record state is invalid".to_owned(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE attachments
                 SET managed_relative_path = ?2,
                     previous_managed_relative_path = ?3,
                     relocation_state = 'pending', relocation_error_code = NULL
                 WHERE id = ?1 AND relocation_state = 'ready'
                   AND managed_relative_path = ?3",
                params![attachment_id, desired_path, current_path],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "attachment changed before relocation could be queued".to_owned(),
                ));
            }
            if let Some(notebook_id) = notebook_id {
                transaction.execute(
                    "UPDATE notebooks
                     SET attachment_directory_sync_pending = 1, updated_at_ms = ?2
                     WHERE id = ?1",
                    params![notebook_id, now],
                )?;
            }
            queued += 1;
        }
        transaction.commit()?;
        Ok(queued)
    }

    pub(crate) fn pending_attachment_relocations(
        &self,
    ) -> Result<Vec<AttachmentRelocationOperation>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT attachment.id, attachment.record_id, record.workspace_id,
                    record.notebook_id, record.revision, record.applied_revision,
                    attachment.managed_relative_path,
                    attachment.previous_managed_relative_path,
                    attachment.media_type, attachment.content_sha256,
                    attachment.byte_size, attachment.relocation_state,
                    attachment.relocation_error_code,
                    COALESCE(notebook.attachment_directory_sync_pending, 0)
             FROM attachments attachment
             JOIN records record ON record.id = attachment.record_id
             LEFT JOIN notebooks notebook ON notebook.id = record.notebook_id
             WHERE attachment.membership_state = 'active'
               AND attachment.relocation_state <> 'ready'
             ORDER BY attachment.created_at_ms, attachment.id",
        )?;
        let rows = statement.query_map([], attachment_relocation_operation_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn record_has_unprepared_attachment_relocations(
        &self,
        record_id: &str,
    ) -> Result<bool, StoreError> {
        validate_id(record_id, "record id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM attachments
                    WHERE record_id = ?1
                      AND membership_state = 'active'
                      AND relocation_state IN ('pending', 'conflict')
                 )",
                [record_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn notebook_has_unprepared_attachment_relocations(
        &self,
        notebook_id: &str,
    ) -> Result<bool, StoreError> {
        validate_id(notebook_id, "notebook id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                    FROM attachments attachment
                    JOIN records record ON record.id = attachment.record_id
                    WHERE record.notebook_id = ?1
                      AND record.state = 'active'
                      AND attachment.membership_state = 'active'
                      AND attachment.relocation_state IN ('pending', 'conflict')
                 )",
                [notebook_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn mark_attachment_relocation_state(
        &self,
        operation: &AttachmentRelocationOperation,
        next_state: AttachmentRelocationState,
        error_code: Option<&'static str>,
    ) -> Result<(), StoreError> {
        if next_state == AttachmentRelocationState::Ready {
            return Err(StoreError::Conflict(
                "ready relocation state requires grouped completion".to_owned(),
            ));
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE attachments
             SET relocation_state = ?2, relocation_error_code = ?3
             WHERE id = ?1 AND record_id = ?4
               AND managed_relative_path = ?5
               AND previous_managed_relative_path = ?6
               AND relocation_state = ?7",
            params![
                operation.attachment_id,
                next_state.as_db_value(),
                error_code,
                operation.record_id,
                operation.managed_relative_path,
                operation.previous_managed_relative_path,
                operation.state.as_db_value()
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::Conflict(
                "attachment relocation changed before its receipt was saved".to_owned(),
            ))
        }
    }

    pub(crate) fn attachment_path_live_reference_count(
        &self,
        workspace_id: &str,
        managed_relative_path: &str,
    ) -> Result<u64, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM attachments attachment
             JOIN records record ON record.id = attachment.record_id
             WHERE record.workspace_id = ?1
               AND attachment.membership_state IN ('active', 'detaching')
               AND (
                    attachment.managed_relative_path = ?2
                    OR (
                        attachment.previous_managed_relative_path = ?2
                        AND attachment.relocation_state IN ('pending', 'conflict')
                    )
               )",
            params![workspace_id, managed_relative_path],
            |row| row.get(0),
        )?;
        u64::try_from(count)
            .map_err(|_| StoreError::Conflict("attachment reference count is invalid".to_owned()))
    }

    pub(crate) fn complete_attachment_relocation_group(
        &self,
        operations: &[AttachmentRelocationOperation],
        terminal_code: Option<&'static str>,
    ) -> Result<(), StoreError> {
        if operations.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for operation in operations {
            let changed = transaction.execute(
                "UPDATE attachments
                 SET previous_managed_relative_path = NULL,
                     relocation_state = 'ready', relocation_error_code = ?2
                 WHERE id = ?1 AND record_id = ?3
                   AND managed_relative_path = ?4
                   AND previous_managed_relative_path = ?5
                   AND relocation_state = 'cleanup_pending'",
                params![
                    operation.attachment_id,
                    terminal_code,
                    operation.record_id,
                    operation.managed_relative_path,
                    operation.previous_managed_relative_path
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "attachment relocation group changed before completion".to_owned(),
                ));
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn latest_composer_receipt(
        &self,
        record_id: &str,
        host_kind: &str,
    ) -> Result<Option<ComposerReceipt>, StoreError> {
        validate_id(record_id, "record id")?;
        validate_composer_host_kind(host_kind)?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, state, detail_json, created_at_ms
                 FROM composer_receipts
                 WHERE record_id = ?1 AND host_kind = ?2
                 ORDER BY created_at_ms DESC, id DESC
                 LIMIT 1",
                params![record_id, host_kind],
                |row| {
                    Ok(ComposerReceipt {
                        id: row.get(0)?,
                        state: row.get(1)?,
                        detail_json: row.get(2)?,
                        created_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(crate) fn latest_composer_receipts_for_host(
        &self,
        host_kind: &str,
    ) -> Result<Vec<(String, ComposerReceipt)>, StoreError> {
        validate_composer_host_kind(host_kind)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT current.record_id, current.id, current.state,
                    current.detail_json, current.created_at_ms
             FROM composer_receipts AS current
             WHERE current.host_kind = ?1
               AND NOT EXISTS (
                    SELECT 1
                    FROM composer_receipts AS newer
                    WHERE newer.host_kind = current.host_kind
                      AND newer.record_id = current.record_id
                      AND (
                           newer.created_at_ms > current.created_at_ms
                           OR (
                                newer.created_at_ms = current.created_at_ms
                                AND newer.id > current.id
                           )
                      )
               )
             ORDER BY current.created_at_ms DESC, current.id DESC",
        )?;
        let rows = statement.query_map([host_kind], |row| {
            Ok((
                row.get(0)?,
                ComposerReceipt {
                    id: row.get(1)?,
                    state: row.get(2)?,
                    detail_json: row.get(3)?,
                    created_at_ms: row.get(4)?,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn composer_receipt_details_for_host(
        &self,
        host_kind: &str,
    ) -> Result<Vec<String>, StoreError> {
        validate_composer_host_kind(host_kind)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT detail_json
             FROM composer_receipts
             WHERE host_kind = ?1
             ORDER BY created_at_ms DESC, id DESC
             LIMIT 256",
        )?;
        let rows = statement.query_map([host_kind], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn record_composer_receipt(
        &self,
        record_id: &str,
        host_kind: &str,
        state: &'static str,
        detail_json: &str,
    ) -> Result<(), StoreError> {
        validate_id(record_id, "record id")?;
        validate_composer_host_kind(host_kind)?;
        if !matches!(state, "partial" | "uncertain" | "complete")
            || detail_json.len() > 64 * 1024
            || serde_json::from_str::<serde_json::Value>(detail_json).is_err()
        {
            return Err(StoreError::Conflict(
                "composer receipt is invalid".to_owned(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let record_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE id = ?1)",
            [record_id],
            |row| row.get(0),
        )?;
        if !record_exists {
            return Err(StoreError::NotFound("record"));
        }
        transaction.execute(
            "INSERT INTO composer_receipts (
                id, record_id, host_kind, state, detail_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id(), record_id, host_kind, state, detail_json, now_ms()],
        )?;
        transaction.execute(
            "DELETE FROM composer_receipts
             WHERE record_id = ?1 AND host_kind = ?2 AND id NOT IN (
                SELECT id FROM composer_receipts
                WHERE record_id = ?1 AND host_kind = ?2
                ORDER BY created_at_ms DESC, id DESC LIMIT 16
             )",
            params![record_id, host_kind],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn attachment_path_reference_count(
        &self,
        workspace_id: &str,
        managed_relative_path: &str,
    ) -> Result<u64, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM attachments attachment
             JOIN records record ON record.id = attachment.record_id
             WHERE record.workspace_id = ?1
               AND attachment.membership_state IN ('active', 'detaching')
               AND attachment.managed_relative_path = ?2",
            params![workspace_id, managed_relative_path],
            |row| row.get(0),
        )?;
        u64::try_from(count)
            .map_err(|_| StoreError::Conflict("attachment reference count is invalid".to_owned()))
    }

    pub(crate) fn mark_attachment_file_prepared(
        &self,
        operation: &AttachmentFileOperation,
        backup_app_data_relative_path: &str,
    ) -> Result<(), StoreError> {
        validate_attachment_backup_relative_path(backup_app_data_relative_path)?;
        if !matches!(operation.action.as_str(), "trash" | "detach") {
            return Err(StoreError::Conflict(
                "attachment prepare action is invalid".to_owned(),
            ));
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE attachment_file_operations
             SET phase = 'prepared', backup_app_data_relative_path = ?4,
                 attempt_count = attempt_count + 1, last_error_code = NULL,
                 updated_at_ms = ?5, completed_at_ms = NULL
             WHERE attachment_id = ?1 AND record_revision = ?2 AND action = ?3
               AND phase IN ('queued', 'prepared', 'needs_recovery')",
            params![
                operation.attachment_id,
                revision_to_db(operation.record_revision)?,
                operation.action,
                backup_app_data_relative_path,
                now_ms()
            ],
        )?;
        require_single_attachment_operation_change(changed)
    }

    pub(crate) fn mark_attachment_file_trashed(
        &self,
        operation: &AttachmentFileOperation,
        trash_path: &str,
    ) -> Result<(), StoreError> {
        if !Path::new(trash_path).is_absolute() || trash_path.contains('\0') {
            return Err(StoreError::Conflict(
                "attachment trash receipt path is invalid".to_owned(),
            ));
        }
        if !matches!(operation.action.as_str(), "trash" | "detach") {
            return Err(StoreError::Conflict(
                "attachment trash action is invalid".to_owned(),
            ));
        }
        let now = now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE attachment_file_operations
             SET phase = 'trashed', trash_path = ?4, last_error_code = NULL,
                 updated_at_ms = ?5, completed_at_ms = ?5
             WHERE attachment_id = ?1 AND record_revision = ?2 AND action = ?3
               AND phase IN ('prepared', 'needs_recovery')",
            params![
                operation.attachment_id,
                revision_to_db(operation.record_revision)?,
                operation.action,
                trash_path,
                now
            ],
        )?;
        require_single_attachment_operation_change(changed)?;
        if operation.action == "detach" {
            mark_attachment_membership_detached(&transaction, operation)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_attachment_file_terminal(
        &self,
        operation: &AttachmentFileOperation,
        phase: &'static str,
        last_error_code: Option<&'static str>,
    ) -> Result<(), StoreError> {
        if !matches!(
            phase,
            "ready" | "preserved_shared" | "preserved_changed" | "missing"
        ) {
            return Err(StoreError::Conflict(
                "attachment terminal phase is invalid".to_owned(),
            ));
        }
        let now = now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE attachment_file_operations
             SET phase = ?3, trash_path = NULL, last_error_code = ?4,
                 updated_at_ms = ?5, completed_at_ms = ?5
             WHERE attachment_id = ?1 AND record_revision = ?2
               AND phase IN ('queued', 'prepared', 'needs_recovery')",
            params![
                operation.attachment_id,
                revision_to_db(operation.record_revision)?,
                phase,
                last_error_code,
                now
            ],
        )?;
        require_single_attachment_operation_change(changed)?;
        if operation.action == "detach" {
            mark_attachment_membership_detached(&transaction, operation)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_attachment_file_recovery_needed(
        &self,
        operation: &AttachmentFileOperation,
        error_code: &'static str,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE attachment_file_operations
             SET phase = 'needs_recovery', attempt_count = attempt_count + 1,
                 last_error_code = ?3, updated_at_ms = ?4, completed_at_ms = NULL
             WHERE attachment_id = ?1 AND record_revision = ?2
               AND phase IN ('queued', 'prepared', 'needs_recovery')",
            params![
                operation.attachment_id,
                revision_to_db(operation.record_revision)?,
                error_code,
                now_ms()
            ],
        )?;
        require_single_attachment_operation_change(changed)
    }

    pub(crate) fn clear_attachment_file_backup(
        &self,
        operation: &AttachmentFileOperation,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE attachment_file_operations
             SET backup_app_data_relative_path = NULL, updated_at_ms = ?3
             WHERE attachment_id = ?1 AND record_revision = ?2",
            params![
                operation.attachment_id,
                revision_to_db(operation.record_revision)?,
                now_ms()
            ],
        )?;
        require_single_attachment_operation_change(changed)
    }

    pub(crate) fn recovery_file_steps(
        &self,
        operation_id: &str,
    ) -> Result<Vec<RecoveryFileStep>, StoreError> {
        validate_id(operation_id, "operation id")?;
        let connection = self.connection()?;
        ensure_recovery_operation_exists(&connection, operation_id)?;
        let mut statement = connection.prepare(
            "SELECT operation_id, step_index, notebook_id, role, effect, state,
                    target_workspace_relative_path, staged_sibling_name,
                    backup_app_data_relative_path, expected_target_id,
                    before_file_sha256, after_file_sha256,
                    before_managed_sha256, after_managed_sha256,
                    before_modified_ns, target_existed, applied_at_ms
             FROM recovery_file_steps
             WHERE operation_id = ?1
             ORDER BY step_index",
        )?;
        let rows = statement.query_map([operation_id], recovery_file_step_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn notebook_file_receipt(
        &self,
        notebook_id: &str,
    ) -> Result<Option<NotebookFileReceipt>, StoreError> {
        validate_id(notebook_id, "notebook id")?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT managed_sha256, file_sha256, generation
                 FROM notebook_file_receipts WHERE notebook_id = ?1",
                [notebook_id],
                |row| {
                    let generation: i64 = row.get(2)?;
                    let generation = u64::try_from(generation).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(2, Type::Integer, Box::new(error))
                    })?;
                    Ok(NotebookFileReceipt {
                        managed_sha256: row.get(0)?,
                        file_sha256: row.get(1)?,
                        generation,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(crate) fn notebook_conflict(
        &self,
        workspace_id: &str,
        notebook_id: &str,
    ) -> Result<Option<NotebookConflictEvidence>, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(notebook_id, "notebook id")?;
        let connection = self.connection()?;
        ensure_notebook_belongs_to_workspace(&connection, notebook_id, workspace_id)?;
        connection
            .query_row(
                "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                        expected_managed_sha256, observed_file_sha256,
                        observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
                 FROM notebook_conflicts
                 WHERE workspace_id = ?1 AND notebook_id = ?2",
                params![workspace_id, notebook_id],
                notebook_conflict_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(crate) fn pending_file_version_adoptions(
        &self,
    ) -> Result<Vec<NotebookConflictEvidence>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                    expected_managed_sha256, observed_file_sha256,
                    observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
             FROM notebook_conflicts
             WHERE reason_code = 'file_version_adoption_pending'
             ORDER BY updated_at_ms, notebook_id",
        )?;
        let rows = statement.query_map([], notebook_conflict_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn record_notebook_conflict(
        &self,
        notebook: &Notebook,
        receipt_generation: u64,
        expected_managed_sha256: &str,
        observed_file_sha256: &str,
        observed_managed_sha256: Option<&str>,
        reason_code: &'static str,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        validate_id(&notebook.target_id, "target id")?;
        validate_sha256(expected_managed_sha256, "expected managed digest")?;
        validate_sha256(observed_file_sha256, "observed file digest")?;
        if let Some(digest) = observed_managed_sha256 {
            validate_sha256(digest, "observed managed digest")?;
        }
        if reason_code.is_empty()
            || reason_code.len() > 128
            || !reason_code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(StoreError::Conflict(
                "notebook conflict reason is invalid".to_owned(),
            ));
        }
        let generation = i64::try_from(receipt_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let receipt_matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM notebook_file_receipts
                WHERE notebook_id = ?1 AND target_id = ?2
                  AND generation = ?3 AND managed_sha256 = ?4
             )",
            params![
                notebook.id,
                notebook.target_id,
                generation,
                expected_managed_sha256
            ],
            |row| row.get(0),
        )?;
        if !receipt_matches {
            return Err(StoreError::Conflict(
                "notebook conflict receipt changed before capture".to_owned(),
            ));
        }
        let now = now_ms();
        transaction.execute(
            "INSERT INTO notebook_conflicts (
                notebook_id, workspace_id, target_id, receipt_generation,
                expected_managed_sha256, observed_file_sha256,
                observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(notebook_id) DO UPDATE SET
                workspace_id = excluded.workspace_id,
                target_id = excluded.target_id,
                receipt_generation = excluded.receipt_generation,
                expected_managed_sha256 = excluded.expected_managed_sha256,
                observed_file_sha256 = excluded.observed_file_sha256,
                observed_managed_sha256 = excluded.observed_managed_sha256,
                reason_code = excluded.reason_code,
                updated_at_ms = excluded.updated_at_ms",
            params![
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                generation,
                expected_managed_sha256,
                observed_file_sha256,
                observed_managed_sha256,
                reason_code,
                now
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE notebooks
             SET target_state = 'conflict', last_error_code = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND workspace_id = ?4 AND target_id = ?5
               AND target_state IN ('ready', 'conflict')",
            params![
                reason_code,
                now,
                notebook.id,
                notebook.workspace_id,
                notebook.target_id
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook cannot enter conflict from its current state".to_owned(),
            ));
        }
        let updated = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(updated)
    }

    pub(crate) fn begin_notebook_file_version_adoption(
        &self,
        notebook: &Notebook,
        expected_evidence: &NotebookConflictEvidence,
        updates: &[NotebookConflictRecordUpdate],
    ) -> Result<NotebookConflictEvidence, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        for update in updates {
            validate_id(&update.record_id, "record id")?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let current_evidence = transaction
            .query_row(
                "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                        expected_managed_sha256, observed_file_sha256,
                        observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
                 FROM notebook_conflicts WHERE notebook_id = ?1",
                [&notebook.id],
                notebook_conflict_from_row,
            )
            .optional()?
            .ok_or(StoreError::Conflict(
                "notebook conflict evidence is missing".to_owned(),
            ))?;
        if &current_evidence != expected_evidence
            || current_evidence.reason_code != "managed_region_changed"
            || current_evidence.workspace_id != notebook.workspace_id
            || current_evidence.target_id != notebook.target_id
        {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before file adoption".to_owned(),
            ));
        }
        let state: (String, bool, bool) = transaction.query_row(
            "SELECT target_state, numbering_sync_pending, attachment_directory_sync_pending
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if state != ("conflict".to_owned(), false, false) {
            return Err(StoreError::Conflict(
                "notebook cannot adopt a file version in its current state".to_owned(),
            ));
        }
        ensure_conflict_has_only_unstaged_edits(&transaction, &notebook.id)?;

        let current_records = {
            let mut statement = transaction.prepare(
                "SELECT record.id, record.revision, record.body_markdown,
                        (SELECT COUNT(*) FROM attachments
                         WHERE record_id = record.id AND membership_state = 'active')
                 FROM records record
                 WHERE record.notebook_id = ?1 AND record.state = 'active'
                 ORDER BY record.logical_order, record.created_at_ms, record.id",
            )?;
            let rows = statement.query_map([&notebook.id], |row| {
                let revision: i64 = row.get(1)?;
                let revision = u64::try_from(revision).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(1, Type::Integer, Box::new(error))
                })?;
                let attachment_count: i64 = row.get(3)?;
                let attachment_count = usize::try_from(attachment_count).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(3, Type::Integer, Box::new(error))
                })?;
                Ok((
                    row.get::<_, String>(0)?,
                    revision,
                    row.get::<_, String>(2)?,
                    attachment_count,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if current_records.len() != updates.len() {
            return Err(StoreError::Conflict(
                "record set changed before file adoption".to_owned(),
            ));
        }
        let now = now_ms();
        for ((record_id, revision, current_body, attachment_count), update) in
            current_records.iter().zip(updates)
        {
            if record_id != &update.record_id || *revision != update.expected_revision {
                return Err(StoreError::Conflict(
                    "record identity or revision changed before file adoption".to_owned(),
                ));
            }
            let body = validate_record_markdown(&update.body_markdown, *attachment_count)?;
            if &body == current_body {
                continue;
            }
            let desired_revision = revision
                .checked_add(1)
                .ok_or_else(|| StoreError::Conflict("record revision overflow".to_owned()))?;
            let changed = transaction.execute(
                "UPDATE records
                 SET body_markdown = ?1, revision = ?2, sync_state = 'conflict',
                     updated_at_ms = ?3
                 WHERE id = ?4 AND notebook_id = ?5 AND revision = ?6 AND state = 'active'",
                params![
                    body,
                    revision_to_db(desired_revision)?,
                    now,
                    record_id,
                    notebook.id,
                    revision_to_db(*revision)?
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "record changed while beginning file adoption".to_owned(),
                ));
            }
        }
        transaction.execute(
            "UPDATE recovery_operations
             SET phase = 'superseded', last_error_code = 'superseded_by_file_version_adoption',
                 updated_at_ms = ?2, completed_at_ms = ?2
             WHERE phase NOT IN ('completed', 'superseded')
               AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)",
            params![notebook.id, now],
        )?;
        let intent_at = now.max(current_evidence.updated_at_ms.saturating_add(1));
        let changed = transaction.execute(
            "UPDATE notebook_conflicts
             SET reason_code = 'file_version_adoption_pending', updated_at_ms = ?2
             WHERE notebook_id = ?1 AND updated_at_ms = ?3",
            params![notebook.id, intent_at, current_evidence.updated_at_ms],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before file adoption".to_owned(),
            ));
        }
        let intent = transaction.query_row(
            "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                    expected_managed_sha256, observed_file_sha256,
                    observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
             FROM notebook_conflicts WHERE notebook_id = ?1",
            [&notebook.id],
            notebook_conflict_from_row,
        )?;
        transaction.commit()?;
        Ok(intent)
    }

    pub(crate) fn complete_notebook_conflict_resolution(
        &self,
        notebook: &Notebook,
        expected_evidence: &NotebookConflictEvidence,
        receipt: &NotebookFileReceiptInput,
        mark_records_synced: bool,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        if receipt.notebook_id != notebook.id || receipt.target_id != notebook.target_id {
            return Err(StoreError::Conflict(
                "notebook conflict receipt identity changed".to_owned(),
            ));
        }
        validate_sha256(&receipt.managed_sha256, "resolved managed digest")?;
        validate_sha256(&receipt.file_sha256, "resolved file digest")?;
        let generation = i64::try_from(expected_evidence.receipt_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let current_evidence = transaction
            .query_row(
                "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                        expected_managed_sha256, observed_file_sha256,
                        observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
                 FROM notebook_conflicts WHERE notebook_id = ?1",
                [&notebook.id],
                notebook_conflict_from_row,
            )
            .optional()?
            .ok_or(StoreError::Conflict(
                "notebook conflict evidence is missing".to_owned(),
            ))?;
        if &current_evidence != expected_evidence
            || current_evidence.workspace_id != notebook.workspace_id
            || current_evidence.target_id != notebook.target_id
        {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before completion".to_owned(),
            ));
        }
        if mark_records_synced && current_evidence.reason_code != "file_version_adoption_pending" {
            return Err(StoreError::Conflict(
                "file version adoption intent is missing".to_owned(),
            ));
        }
        if !mark_records_synced && current_evidence.reason_code != "managed_region_changed" {
            return Err(StoreError::Conflict(
                "WakeGPT version adoption intent is stale".to_owned(),
            ));
        }
        let receipt_changed = transaction.execute(
            "UPDATE notebook_file_receipts
             SET marker_schema = ?1, managed_sha256 = ?2, file_sha256 = ?3,
                 file_modified_ns = ?4, file_size_bytes = ?5,
                 generation = generation + 1, updated_at_ms = ?6
             WHERE notebook_id = ?7 AND target_id = ?8 AND generation = ?9
               AND managed_sha256 = ?10",
            params![
                i64::from(receipt.marker_schema),
                receipt.managed_sha256,
                receipt.file_sha256,
                modified_ns,
                size_bytes,
                now_ms(),
                notebook.id,
                notebook.target_id,
                generation,
                expected_evidence.expected_managed_sha256
            ],
        )?;
        if receipt_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook receipt changed before conflict completion".to_owned(),
            ));
        }
        let now = now_ms();
        if mark_records_synced {
            transaction.execute(
                "UPDATE records
                 SET applied_revision = revision, sync_state = 'synced', updated_at_ms = ?2
                 WHERE notebook_id = ?1 AND state = 'active'",
                params![notebook.id, now],
            )?;
        }
        let notebook_changed = transaction.execute(
            "UPDATE notebooks
             SET target_state = 'ready', last_error_code = NULL, updated_at_ms = ?3
             WHERE id = ?1 AND workspace_id = ?2 AND target_state = 'conflict'",
            params![notebook.id, notebook.workspace_id, now],
        )?;
        if notebook_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook conflict state changed before completion".to_owned(),
            ));
        }
        let evidence_deleted = transaction.execute(
            "DELETE FROM notebook_conflicts WHERE notebook_id = ?1 AND updated_at_ms = ?2",
            params![notebook.id, current_evidence.updated_at_ms],
        )?;
        if evidence_deleted != 1 {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before completion".to_owned(),
            ));
        }
        let completed = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    pub(crate) fn unbind_notebook_conflict(
        &self,
        notebook: &Notebook,
        expected_evidence: &NotebookConflictEvidence,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.workspace_id, "workspace id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let current_evidence = transaction
            .query_row(
                "SELECT notebook_id, workspace_id, target_id, receipt_generation,
                        expected_managed_sha256, observed_file_sha256,
                        observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
                 FROM notebook_conflicts WHERE notebook_id = ?1",
                [&notebook.id],
                notebook_conflict_from_row,
            )
            .optional()?
            .ok_or(StoreError::Conflict(
                "notebook conflict evidence is missing".to_owned(),
            ))?;
        if &current_evidence != expected_evidence
            || current_evidence.workspace_id != notebook.workspace_id
            || current_evidence.target_id != notebook.target_id
        {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before unbinding".to_owned(),
            ));
        }
        ensure_conflict_has_only_unstaged_edits(&transaction, &notebook.id)?;
        let state: (String, bool, bool) = transaction.query_row(
            "SELECT target_state, numbering_sync_pending, attachment_directory_sync_pending
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if state != ("conflict".to_owned(), false, false) {
            return Err(StoreError::Conflict(
                "notebook cannot be unbound in its current state".to_owned(),
            ));
        }
        let now = now_ms();
        transaction.execute(
            "UPDATE recovery_operations
             SET phase = 'superseded', last_error_code = 'superseded_by_conflict_unbind',
                 updated_at_ms = ?2, completed_at_ms = ?2
             WHERE phase NOT IN ('completed', 'superseded')
               AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)",
            params![notebook.id, now],
        )?;
        transaction.execute(
            "UPDATE records
             SET applied_revision = revision, sync_state = 'local', updated_at_ms = ?2
             WHERE notebook_id = ?1 AND state = 'active'",
            params![notebook.id, now],
        )?;
        let changed = transaction.execute(
            "UPDATE notebooks
             SET target_state = 'unbound', last_error_code = NULL, updated_at_ms = ?3
             WHERE id = ?1 AND workspace_id = ?2 AND target_state = 'conflict'",
            params![notebook.id, notebook.workspace_id, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook conflict state changed before unbinding".to_owned(),
            ));
        }
        let deleted = transaction.execute(
            "DELETE FROM notebook_conflicts WHERE notebook_id = ?1 AND updated_at_ms = ?2",
            params![notebook.id, current_evidence.updated_at_ms],
        )?;
        if deleted != 1 {
            return Err(StoreError::Conflict(
                "notebook conflict evidence changed before unbinding".to_owned(),
            ));
        }
        let unbound = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(unbound)
    }

    pub(crate) fn update_notebook_document_receipt(
        &self,
        notebook: &Notebook,
        expected_generation: u64,
        expected_managed_sha256: &str,
        receipt: &NotebookFileReceiptInput,
    ) -> Result<(), StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.target_id, "target id")?;
        if receipt.notebook_id != notebook.id || receipt.target_id != notebook.target_id {
            return Err(StoreError::Conflict(
                "notebook receipt identity changed".to_owned(),
            ));
        }
        let generation = i64::try_from(expected_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE notebook_file_receipts
             SET marker_schema = ?1, managed_sha256 = ?2, file_sha256 = ?3,
                 file_modified_ns = ?4, file_size_bytes = ?5,
                 generation = generation + 1, updated_at_ms = ?6
             WHERE notebook_id = ?7 AND target_id = ?8 AND generation = ?9
               AND managed_sha256 = ?10",
            params![
                i64::from(receipt.marker_schema),
                receipt.managed_sha256,
                receipt.file_sha256,
                modified_ns,
                size_bytes,
                now_ms(),
                notebook.id,
                notebook.target_id,
                generation,
                expected_managed_sha256
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before the document save".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE notebooks
             SET target_state = 'ready', last_error_code = NULL, updated_at_ms = ?2
             WHERE id = ?1",
            params![notebook.id, now_ms()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn complete_notebook_numbering_sync(
        &self,
        notebook: &Notebook,
        expected_generation: u64,
        expected_managed_sha256: &str,
        receipt: &NotebookFileReceiptInput,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.target_id, "target id")?;
        if receipt.notebook_id != notebook.id || receipt.target_id != notebook.target_id {
            return Err(StoreError::Conflict(
                "notebook numbering receipt identity changed".to_owned(),
            ));
        }
        let generation = i64::try_from(expected_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let receipt_changed = transaction.execute(
            "UPDATE notebook_file_receipts
             SET marker_schema = ?1, managed_sha256 = ?2, file_sha256 = ?3,
                 file_modified_ns = ?4, file_size_bytes = ?5,
                 generation = generation + 1, updated_at_ms = ?6
             WHERE notebook_id = ?7 AND target_id = ?8 AND generation = ?9
               AND managed_sha256 = ?10",
            params![
                i64::from(receipt.marker_schema),
                receipt.managed_sha256,
                receipt.file_sha256,
                modified_ns,
                size_bytes,
                now_ms(),
                notebook.id,
                notebook.target_id,
                generation,
                expected_managed_sha256
            ],
        )?;
        if receipt_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before numbering completion".to_owned(),
            ));
        }
        let notebook_changed = transaction.execute(
            "UPDATE notebooks
             SET numbering_sync_pending = 0, target_state = 'ready',
                 last_error_code = NULL, updated_at_ms = ?1
             WHERE id = ?2 AND workspace_id = ?3 AND target_id = ?4
               AND numbering_style = ?5 AND numbering_start = ?6
               AND numbering_sync_pending = 1",
            params![
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                notebook.numbering_style.as_db_value(),
                i64::from(notebook.numbering_start)
            ],
        )?;
        if notebook_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook numbering configuration changed before completion".to_owned(),
            ));
        }
        let completed = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    pub(crate) fn complete_notebook_attachment_directory_sync(
        &self,
        notebook: &Notebook,
        expected_generation: u64,
        expected_managed_sha256: &str,
        receipt: &NotebookFileReceiptInput,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.target_id, "target id")?;
        if receipt.notebook_id != notebook.id || receipt.target_id != notebook.target_id {
            return Err(StoreError::Conflict(
                "notebook attachment directory receipt identity changed".to_owned(),
            ));
        }
        let generation = i64::try_from(expected_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let receipt_changed = transaction.execute(
            "UPDATE notebook_file_receipts
             SET marker_schema = ?1, managed_sha256 = ?2, file_sha256 = ?3,
                 file_modified_ns = ?4, file_size_bytes = ?5,
                 generation = generation + 1, updated_at_ms = ?6
             WHERE notebook_id = ?7 AND target_id = ?8 AND generation = ?9
               AND managed_sha256 = ?10",
            params![
                i64::from(receipt.marker_schema),
                receipt.managed_sha256,
                receipt.file_sha256,
                modified_ns,
                size_bytes,
                now_ms(),
                notebook.id,
                notebook.target_id,
                generation,
                expected_managed_sha256
            ],
        )?;
        if receipt_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before attachment directory completion".to_owned(),
            ));
        }
        let notebook_changed = transaction.execute(
            "UPDATE notebooks
             SET previous_attachment_directory = NULL,
                 attachment_directory_sync_pending = 0,
                 target_state = 'ready', last_error_code = NULL,
                 updated_at_ms = ?1
             WHERE id = ?2 AND workspace_id = ?3 AND target_id = ?4
               AND attachment_directory = ?5
               AND attachment_directory_sync_pending = 1",
            params![
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                notebook.attachment_directory
            ],
        )?;
        if notebook_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook attachment directory changed before completion".to_owned(),
            ));
        }
        let completed = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    pub(crate) fn relocate_notebook_target(
        &self,
        notebook: &Notebook,
        new_relative_path: &str,
        expected_generation: u64,
        expected_managed_sha256: &str,
        receipt: &NotebookFileReceiptInput,
    ) -> Result<Notebook, StoreError> {
        validate_id(&notebook.id, "notebook id")?;
        validate_id(&notebook.target_id, "target id")?;
        let new_relative_path = validate_notebook_relative_path(new_relative_path)?;
        if receipt.notebook_id != notebook.id || receipt.target_id != notebook.target_id {
            return Err(StoreError::Conflict(
                "notebook relocation receipt identity changed".to_owned(),
            ));
        }
        let generation = i64::try_from(expected_generation)
            .map_err(|_| StoreError::Conflict("file receipt generation is too large".to_owned()))?;
        let modified_ns = i64::try_from(receipt.file_modified_ns)
            .map_err(|_| StoreError::Conflict("file timestamp is too large".to_owned()))?;
        let size_bytes = i64::try_from(receipt.file_size_bytes)
            .map_err(|_| StoreError::Conflict("file size is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let duplicate: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM notebooks
                WHERE workspace_id = ?1 AND relative_path = ?2 AND id <> ?3
             )",
            params![notebook.workspace_id, new_relative_path, notebook.id],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(StoreError::Conflict(
                "the relocated Markdown file is already bound".to_owned(),
            ));
        }
        let staged_operation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM recovery_file_steps step
                JOIN recovery_operations operation ON operation.id = step.operation_id
                WHERE step.notebook_id = ?1
                  AND operation.phase NOT IN ('completed', 'superseded')
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        if staged_operation {
            return Err(StoreError::Conflict(
                "notebook target cannot move during a staged file operation".to_owned(),
            ));
        }
        let receipt_changed = transaction.execute(
            "UPDATE notebook_file_receipts
             SET marker_schema = ?1, managed_sha256 = ?2, file_sha256 = ?3,
                 file_modified_ns = ?4, file_size_bytes = ?5,
                 generation = generation + 1, updated_at_ms = ?6
             WHERE notebook_id = ?7 AND target_id = ?8 AND generation = ?9
               AND managed_sha256 = ?10",
            params![
                i64::from(receipt.marker_schema),
                receipt.managed_sha256,
                receipt.file_sha256,
                modified_ns,
                size_bytes,
                now_ms(),
                notebook.id,
                notebook.target_id,
                generation,
                expected_managed_sha256
            ],
        )?;
        if receipt_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook file receipt changed before relocation".to_owned(),
            ));
        }
        let notebook_changed = transaction.execute(
            "UPDATE notebooks
             SET relative_path = ?1,
                 target_state = CASE WHEN target_state = 'unbound' THEN 'unbound' ELSE 'ready' END,
                 last_error_code = NULL, updated_at_ms = ?2
             WHERE id = ?3 AND workspace_id = ?4 AND target_id = ?5 AND relative_path = ?6",
            params![
                new_relative_path,
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id,
                notebook.relative_path
            ],
        )?;
        if notebook_changed != 1 {
            return Err(StoreError::Conflict(
                "notebook target changed before relocation".to_owned(),
            ));
        }
        let relocated = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(relocated)
    }

    pub(crate) fn mark_notebook_target_unavailable(
        &self,
        notebook: &Notebook,
        error_code: &'static str,
    ) -> Result<Notebook, StoreError> {
        if !matches!(
            error_code,
            "notebook_converted_to_plain" | "notebook_moved_to_trash"
        ) {
            return Err(StoreError::Conflict(
                "unsupported notebook lifecycle state".to_owned(),
            ));
        }
        validate_id(&notebook.id, "notebook id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_notebook_belongs_to_workspace(&transaction, &notebook.id, &notebook.workspace_id)?;
        let pending_operation: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM recovery_operations
                WHERE phase NOT IN ('completed', 'superseded')
                  AND (source_notebook_id = ?1 OR destination_notebook_id = ?1)
             )",
            [&notebook.id],
            |row| row.get(0),
        )?;
        if pending_operation {
            return Err(StoreError::Conflict(
                "notebook target has an unfinished file operation".to_owned(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE notebooks
             SET target_state = 'unavailable', last_error_code = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND workspace_id = ?4 AND target_id = ?5
               AND target_state IN ('ready', 'unbound')",
            params![
                error_code,
                now_ms(),
                notebook.id,
                notebook.workspace_id,
                notebook.target_id
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "notebook is not available for this file operation".to_owned(),
            ));
        }
        let updated = transaction.query_row(
            "SELECT id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending, target_state, last_error_code,
                    created_at_ms, updated_at_ms, attachment_directory,
                    previous_attachment_directory, attachment_directory_sync_pending,
                    COALESCE((SELECT is_pinned FROM notebook_tab_state
                              WHERE notebook_id = notebooks.id), 1)
             FROM notebooks WHERE id = ?1",
            [&notebook.id],
            notebook_from_row,
        )?;
        transaction.commit()?;
        Ok(updated)
    }

    pub(crate) fn stage_recovery_files(
        &self,
        operation_id: &str,
        steps: &[RecoveryFileStepInput],
    ) -> Result<(), StoreError> {
        validate_id(operation_id, "operation id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let applied: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM recovery_file_steps
             WHERE operation_id = ?1 AND state <> 'staged'",
            [operation_id],
            |row| row.get(0),
        )?;
        if applied != 0 {
            return Err(StoreError::Conflict(
                "recovery files were already applied".to_owned(),
            ));
        }
        transaction.execute(
            "DELETE FROM recovery_file_steps WHERE operation_id = ?1",
            [operation_id],
        )?;
        for step in steps {
            transaction.execute(
                "INSERT INTO recovery_file_steps (
                    operation_id, step_index, notebook_id, role, effect, state,
                    target_workspace_relative_path, staged_sibling_name,
                    backup_app_data_relative_path, expected_target_id,
                    before_file_sha256, after_file_sha256,
                    before_managed_sha256, after_managed_sha256,
                    before_modified_ns, target_existed, applied_at_ms
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9,
                    ?10, ?11, ?12, ?13, ?14, ?15, NULL
                 )",
                params![
                    operation_id,
                    i64::try_from(step.step_index).map_err(|_| {
                        StoreError::Conflict("recovery step index is too large".to_owned())
                    })?,
                    step.notebook_id,
                    step.role,
                    step.effect,
                    step.target_workspace_relative_path,
                    step.staged_sibling_name,
                    step.backup_app_data_relative_path,
                    step.expected_target_id,
                    step.before_file_sha256,
                    step.after_file_sha256,
                    step.before_managed_sha256,
                    step.after_managed_sha256,
                    i64::try_from(step.before_modified_ns).map_err(|_| {
                        StoreError::Conflict("file timestamp is too large".to_owned())
                    })?,
                    i64::from(step.target_existed)
                ],
            )?;
        }
        let changed = transaction.execute(
            "UPDATE recovery_operations
             SET phase = 'staged', updated_at_ms = ?2
             WHERE id = ?1 AND phase IN ('queued', 'needs_recovery')",
            params![operation_id, now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "recovery operation cannot be staged in its current phase".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_recovery_applying(&self, operation_id: &str) -> Result<(), StoreError> {
        validate_id(operation_id, "operation id")?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE recovery_operations
             SET phase = 'applying', attempt_count = attempt_count + 1,
                 last_error_code = NULL, updated_at_ms = ?2
             WHERE id = ?1 AND phase IN ('staged', 'needs_recovery')",
            params![operation_id, now_ms()],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let phase: Option<String> = connection
            .query_row(
                "SELECT phase FROM recovery_operations WHERE id = ?1",
                [operation_id],
                |row| row.get(0),
            )
            .optional()?;
        match phase.as_deref() {
            Some("applying") => Ok(()),
            Some(_) => Err(StoreError::Conflict(
                "recovery operation cannot start applying".to_owned(),
            )),
            None => Err(StoreError::NotFound("recovery operation")),
        }
    }

    pub(crate) fn mark_recovery_step_applied(
        &self,
        operation_id: &str,
        step_index: usize,
    ) -> Result<(), StoreError> {
        validate_id(operation_id, "operation id")?;
        let step_index = i64::try_from(step_index)
            .map_err(|_| StoreError::Conflict("recovery step index is too large".to_owned()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE recovery_file_steps
             SET state = 'applied', applied_at_ms = ?3
             WHERE operation_id = ?1 AND step_index = ?2 AND state = 'staged'
               AND EXISTS (
                    SELECT 1 FROM recovery_operations
                    WHERE id = ?1 AND phase = 'applying'
               )",
            params![operation_id, step_index, now_ms()],
        )?;
        if changed == 1 {
            transaction.commit()?;
            return Ok(());
        }
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM recovery_file_steps
                 WHERE operation_id = ?1 AND step_index = ?2",
                params![operation_id, step_index],
                |row| row.get(0),
            )
            .optional()?;
        match state.as_deref() {
            Some("applied") => {
                transaction.commit()?;
                Ok(())
            }
            Some(_) => Err(StoreError::Conflict(
                "recovery file step cannot be marked applied".to_owned(),
            )),
            None => Err(StoreError::NotFound("recovery file step")),
        }
    }

    pub(crate) fn mark_recovery_files_applied(&self, operation_id: &str) -> Result<(), StoreError> {
        validate_id(operation_id, "operation id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let phase: Option<String> = transaction
            .query_row(
                "SELECT phase FROM recovery_operations WHERE id = ?1",
                [operation_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(phase) = phase else {
            return Err(StoreError::NotFound("recovery operation"));
        };
        if phase != "applying" && phase != "files_applied" {
            return Err(StoreError::Conflict(
                "recovery operation cannot mark files applied".to_owned(),
            ));
        }
        let (total, remaining): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN state <> 'applied' THEN 1 ELSE 0 END), 0)
             FROM recovery_file_steps WHERE operation_id = ?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if total == 0 {
            return Err(StoreError::Conflict(
                "recovery operation has no staged file steps".to_owned(),
            ));
        }
        if remaining != 0 {
            return Err(StoreError::Conflict(
                "not all recovery file steps were applied".to_owned(),
            ));
        }
        if phase == "files_applied" {
            transaction.commit()?;
            return Ok(());
        }
        let changed = transaction.execute(
            "UPDATE recovery_operations
             SET phase = 'files_applied', updated_at_ms = ?2
             WHERE id = ?1 AND phase = 'applying'",
            params![operation_id, now_ms()],
        )?;
        if changed == 1 {
            transaction.commit()?;
            Ok(())
        } else {
            Err(StoreError::Conflict(
                "recovery operation cannot mark files applied".to_owned(),
            ))
        }
    }

    pub(crate) fn mark_recovery_needed(
        &self,
        operation_id: &str,
        error_code: &str,
    ) -> Result<(), StoreError> {
        validate_id(operation_id, "operation id")?;
        let connection = self.connection()?;
        connection.execute(
            "UPDATE recovery_operations
             SET phase = 'needs_recovery', last_error_code = ?2, updated_at_ms = ?3
             WHERE id = ?1 AND phase NOT IN ('completed', 'superseded')",
            params![operation_id, error_code, now_ms()],
        )?;
        Ok(())
    }

    pub(crate) fn complete_recovery_operation(
        &self,
        operation: &RecoveryOperation,
        receipts: &[NotebookFileReceiptInput],
    ) -> Result<Record, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let now = now_ms();
        for receipt in receipts {
            transaction.execute(
                "INSERT INTO notebook_file_receipts (
                    notebook_id, target_id, marker_schema, managed_sha256, file_sha256,
                    file_modified_ns, file_size_bytes, generation, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
                 ON CONFLICT(notebook_id) DO UPDATE SET
                    target_id = excluded.target_id,
                    marker_schema = excluded.marker_schema,
                    managed_sha256 = excluded.managed_sha256,
                    file_sha256 = excluded.file_sha256,
                    file_modified_ns = excluded.file_modified_ns,
                    file_size_bytes = excluded.file_size_bytes,
                    generation = notebook_file_receipts.generation + 1,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    receipt.notebook_id,
                    receipt.target_id,
                    i64::from(receipt.marker_schema),
                    receipt.managed_sha256,
                    receipt.file_sha256,
                    i64::try_from(receipt.file_modified_ns).map_err(|_| {
                        StoreError::Conflict("file timestamp is too large".to_owned())
                    })?,
                    i64::try_from(receipt.file_size_bytes).map_err(|_| {
                        StoreError::Conflict("file size is too large".to_owned())
                    })?,
                    now
                ],
            )?;
            transaction.execute(
                "UPDATE notebooks
                 SET numbering_sync_pending = 0, target_state = 'ready',
                     last_error_code = NULL, updated_at_ms = ?2
                 WHERE id = ?1",
                params![receipt.notebook_id, now],
            )?;
        }

        let current =
            record_for_workspace(&transaction, &operation.workspace_id, &operation.record_id)?;
        if current.revision != operation.desired_revision {
            return Err(StoreError::Conflict(
                "record changed while its file effects were being applied".to_owned(),
            ));
        }
        let final_sync_state = if current.notebook_id.is_some() {
            SyncState::Synced
        } else {
            SyncState::Local
        };
        transaction.execute(
            "UPDATE records
             SET applied_revision = ?1, sync_state = ?2, updated_at_ms = ?3
             WHERE id = ?4 AND revision = ?1",
            params![
                revision_to_db(operation.desired_revision)?,
                final_sync_state.as_db_value(),
                now,
                operation.record_id
            ],
        )?;
        transaction.execute(
            "UPDATE recovery_file_steps
             SET state = 'verified' WHERE operation_id = ?1 AND state = 'applied'",
            [&operation.id],
        )?;
        let changed = transaction.execute(
            "UPDATE recovery_operations
             SET phase = 'completed', last_error_code = NULL,
                 updated_at_ms = ?2, completed_at_ms = ?2
             WHERE id = ?1 AND phase IN ('applying', 'files_applied')",
            params![operation.id, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "recovery operation cannot be completed".to_owned(),
            ));
        }
        let mut completed = current;
        completed.applied_revision = operation.desired_revision;
        completed.sync_state = final_sync_state;
        completed.updated_at_ms = now;
        transaction.commit()?;
        Ok(completed)
    }

    fn set_record_state(
        &self,
        workspace_id: &str,
        record_id: &str,
        expected_revision: u64,
        expected_state: RecordState,
        next_state: RecordState,
        mutation: Option<MutationSpec<'_>>,
    ) -> Result<Record, StoreError> {
        validate_id(workspace_id, "workspace id")?;
        validate_id(record_id, "record id")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(mutation) = &mutation {
            if let Some(record) = replay_mutation(&transaction, mutation, Some(record_id))? {
                return Ok(record);
            }
        }
        let record = record_for_workspace(&transaction, workspace_id, record_id)?;
        if record.state != expected_state {
            return Err(StoreError::Conflict(format!(
                "record is {}, expected {}",
                record.state.as_db_value(),
                expected_state.as_db_value()
            )));
        }
        require_revision(&record, expected_revision)?;
        ensure_record_mutation_available(&transaction, record_id)?;
        ensure_notebook_targets_available(
            &transaction,
            record.notebook_id.as_deref(),
            record.notebook_id.as_deref(),
        )?;
        let expected_revision_db = revision_to_db(expected_revision)?;

        let revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let revision_db = i64::try_from(revision)
            .map_err(|_| StoreError::Conflict("record revision is exhausted".to_owned()))?;
        let now = now_ms();
        let requires_file_sync = record.notebook_id.is_some();
        let sync_state = if requires_file_sync {
            SyncState::Queued
        } else {
            SyncState::Local
        };
        let applied_revision = if requires_file_sync {
            record.applied_revision
        } else {
            revision
        };
        let trashed_at_ms = if next_state == RecordState::Trashed {
            Some(now)
        } else {
            None
        };
        let changed = transaction.execute(
            "UPDATE records
             SET state = ?1, updated_at_ms = ?2, sync_state = ?3, revision = ?4,
                 applied_revision = ?5, trashed_at_ms = ?6
             WHERE id = ?7 AND workspace_id = ?8 AND revision = ?9 AND state = ?10",
            params![
                next_state.as_db_value(),
                now,
                sync_state.as_db_value(),
                revision_db,
                revision_to_db(applied_revision)?,
                trashed_at_ms,
                record_id,
                workspace_id,
                expected_revision_db,
                expected_state.as_db_value()
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "record changed before its state could be saved".to_owned(),
            ));
        }
        if requires_file_sync {
            let operation_kind = if next_state == RecordState::Trashed {
                "trash"
            } else {
                "restore"
            };
            queue_recovery_operation(
                &transaction,
                RecoveryIntent {
                    workspace_id,
                    record_id,
                    operation_kind,
                    expected_revision,
                    desired_revision: revision,
                    desired_state: next_state,
                    source_notebook_id: record.notebook_id.as_deref(),
                    destination_notebook_id: record.notebook_id.as_deref(),
                    source_logical_order: Some(record.logical_order),
                    destination_logical_order: Some(record.logical_order),
                },
                now,
            )?;
        }
        queue_attachment_file_operations(&transaction, record_id, revision, next_state, now)?;
        let updated = Record {
            state: next_state,
            updated_at_ms: now,
            sync_state,
            revision,
            applied_revision,
            trashed_at_ms,
            ..record
        };
        if let Some(mutation) = &mutation {
            record_mutation_receipt(&transaction, mutation, &updated, now)?;
        }
        transaction.commit()?;
        Ok(updated)
    }
}

struct RecoveryIntent<'a> {
    workspace_id: &'a str,
    record_id: &'a str,
    operation_kind: &'static str,
    expected_revision: u64,
    desired_revision: u64,
    desired_state: RecordState,
    source_notebook_id: Option<&'a str>,
    destination_notebook_id: Option<&'a str>,
    source_logical_order: Option<i64>,
    destination_logical_order: Option<i64>,
}

struct MutationSpec<'a> {
    mutation_id: &'a str,
    schema_version: u32,
    operation_kind: &'static str,
    workspace_id: &'a str,
    request_sha256: String,
}

fn create_attachment_fingerprint(attachments: &[NewAttachment]) -> String {
    attachments
        .iter()
        .map(|attachment| {
            format!(
                "{}:{}:{}",
                attachment.content_sha256,
                attachment.managed_relative_path,
                attachment
                    .previous_managed_relative_path
                    .as_deref()
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn draft_create_fingerprint_from_new(
    normalized_body_markdown: &str,
    attachments: &[NewAttachment],
) -> String {
    draft_create_fingerprint(
        normalized_body_markdown,
        attachments.iter().map(|attachment| {
            (
                attachment.media_type.as_str(),
                attachment.byte_size,
                attachment.content_sha256.as_str(),
            )
        }),
    )
}

fn draft_create_fingerprint<'a>(
    normalized_body_markdown: &str,
    attachments: impl Iterator<Item = (&'a str, u64, &'a str)>,
) -> String {
    let mut hasher = Sha256::new();
    hash_mutation_part(&mut hasher, Some("wakegpt-draft-create-v1"));
    hash_mutation_part(&mut hasher, Some(normalized_body_markdown));
    for (media_type, byte_size, content_sha256) in attachments {
        hash_mutation_part(&mut hasher, Some(media_type));
        hash_mutation_part(&mut hasher, Some(&byte_size.to_string()));
        hash_mutation_part(&mut hasher, Some(content_sha256));
    }
    let digest = hasher.finalize();
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(fingerprint, "{byte:02x}");
    }
    fingerprint
}

fn mutation_spec<'a>(
    mutation_id: &'a str,
    schema_version: u32,
    operation_kind: &'static str,
    workspace_id: &'a str,
    parts: &[Option<&str>],
) -> Result<MutationSpec<'a>, StoreError> {
    validate_id(mutation_id, "mutation id")?;
    if schema_version != MUTATION_SCHEMA_VERSION {
        return Err(StoreError::Conflict(format!(
            "unsupported mutation schema version: {schema_version}"
        )));
    }
    let mut hasher = Sha256::new();
    hash_mutation_part(&mut hasher, Some("wakegpt-mutation-v1"));
    hash_mutation_part(&mut hasher, Some(operation_kind));
    hash_mutation_part(&mut hasher, Some(workspace_id));
    for part in parts {
        hash_mutation_part(&mut hasher, *part);
    }
    let digest = hasher.finalize();
    let mut request_sha256 = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(request_sha256, "{byte:02x}");
    }
    Ok(MutationSpec {
        mutation_id,
        schema_version,
        operation_kind,
        workspace_id,
        request_sha256,
    })
}

fn hash_mutation_part(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn replay_mutation(
    connection: &Connection,
    spec: &MutationSpec<'_>,
    expected_record_id: Option<&str>,
) -> Result<Option<Record>, StoreError> {
    let receipt = connection
        .query_row(
            "SELECT schema_version, operation_kind, workspace_id, record_id,
                    request_sha256, result_revision
             FROM mutation_receipts WHERE mutation_id = ?1",
            [spec.mutation_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((schema_version, operation_kind, workspace_id, record_id, request_sha256, revision)) =
        receipt
    else {
        return Ok(None);
    };
    let schema_version = u32::try_from(schema_version)
        .map_err(|_| StoreError::Conflict("mutation receipt schema is invalid".to_owned()))?;
    let result_revision = u64::try_from(revision)
        .map_err(|_| StoreError::Conflict("mutation receipt revision is invalid".to_owned()))?;
    if schema_version != spec.schema_version
        || operation_kind != spec.operation_kind
        || workspace_id != spec.workspace_id
        || request_sha256 != spec.request_sha256
        || expected_record_id.is_some_and(|expected| expected != record_id)
    {
        return Err(StoreError::Conflict(
            "mutation id was already used for a different request".to_owned(),
        ));
    }
    let record = record_for_workspace(connection, spec.workspace_id, &record_id)?;
    if record.revision < result_revision {
        return Err(StoreError::Conflict(
            "mutation receipt is newer than its record".to_owned(),
        ));
    }
    Ok(Some(record))
}

fn record_mutation_receipt(
    connection: &Connection,
    spec: &MutationSpec<'_>,
    record: &Record,
    now: i64,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO mutation_receipts (
            mutation_id, schema_version, operation_kind, workspace_id,
            record_id, request_sha256, result_revision, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            spec.mutation_id,
            i64::from(spec.schema_version),
            spec.operation_kind,
            spec.workspace_id,
            record.id,
            spec.request_sha256,
            revision_to_db(record.revision)?,
            now
        ],
    )?;
    Ok(())
}

fn queue_recovery_operation(
    connection: &Connection,
    intent: RecoveryIntent<'_>,
    now: i64,
) -> Result<String, StoreError> {
    let operation_id = new_id();
    connection.execute(
        "INSERT INTO recovery_operations (
            id, workspace_id, record_id, operation_kind, phase,
            expected_revision, desired_revision, desired_record_state,
            source_notebook_id, destination_notebook_id,
            source_logical_order, destination_logical_order,
            attempt_count, last_error_code, created_at_ms, updated_at_ms, completed_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            0, NULL, ?12, ?12, NULL
         )",
        params![
            operation_id,
            intent.workspace_id,
            intent.record_id,
            intent.operation_kind,
            revision_to_db(intent.expected_revision)?,
            revision_to_db(intent.desired_revision)?,
            intent.desired_state.as_db_value(),
            intent.source_notebook_id,
            intent.destination_notebook_id,
            intent.source_logical_order,
            intent.destination_logical_order,
            now
        ],
    )?;
    Ok(operation_id)
}

fn queue_attachment_file_operations(
    connection: &Connection,
    record_id: &str,
    record_revision: u64,
    desired_state: RecordState,
    now: i64,
) -> Result<(), StoreError> {
    let action = if desired_state == RecordState::Trashed {
        "trash"
    } else {
        "restore"
    };
    let attachment_ids = {
        let mut statement = connection.prepare(
            "SELECT id FROM attachments
             WHERE record_id = ?1 AND membership_state = 'active'
             ORDER BY ordinal, created_at_ms, id",
        )?;
        let rows = statement.query_map([record_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for attachment_id in attachment_ids {
        connection.execute(
            "INSERT INTO attachment_file_operations (
                attachment_id, action, phase, record_revision, trash_path,
                backup_app_data_relative_path, attempt_count, last_error_code,
                created_at_ms, updated_at_ms, completed_at_ms
             ) VALUES (?1, ?2, 'queued', ?3, NULL, NULL, 0, NULL, ?4, ?4, NULL)
             ON CONFLICT(attachment_id) DO UPDATE SET
                action = excluded.action,
                phase = 'queued',
                record_revision = excluded.record_revision,
                trash_path = CASE
                    WHEN excluded.action = 'restore'
                    THEN attachment_file_operations.trash_path
                    ELSE NULL
                END,
                backup_app_data_relative_path = CASE
                    WHEN excluded.action = 'restore'
                    THEN attachment_file_operations.backup_app_data_relative_path
                    ELSE NULL
                END,
                attempt_count = 0,
                last_error_code = NULL,
                created_at_ms = excluded.created_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                completed_at_ms = NULL",
            params![attachment_id, action, revision_to_db(record_revision)?, now],
        )?;
    }
    Ok(())
}

fn queue_attachment_detach_operation(
    connection: &Connection,
    attachment_id: &str,
    record_revision: u64,
    now: i64,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO attachment_file_operations (
            attachment_id, action, phase, record_revision, trash_path,
            backup_app_data_relative_path, attempt_count, last_error_code,
            created_at_ms, updated_at_ms, completed_at_ms
         ) VALUES (?1, 'detach', 'queued', ?2, NULL, NULL, 0, NULL, ?3, ?3, NULL)
         ON CONFLICT(attachment_id) DO UPDATE SET
            action = 'detach', phase = 'queued', record_revision = excluded.record_revision,
            trash_path = NULL, backup_app_data_relative_path = NULL,
            attempt_count = 0, last_error_code = NULL,
            created_at_ms = excluded.created_at_ms,
            updated_at_ms = excluded.updated_at_ms, completed_at_ms = NULL",
        params![attachment_id, revision_to_db(record_revision)?, now],
    )?;
    Ok(())
}

fn apply_attachment_set_revision(
    connection: &Connection,
    record: &Record,
    retained_attachment_ids: &[String],
    new_attachments: &[NewAttachment],
    revision: u64,
    now: i64,
) -> Result<(), StoreError> {
    let retained_ordinals = retained_attachment_ids
        .iter()
        .enumerate()
        .map(|(ordinal, id)| (id.as_str(), ordinal))
        .collect::<std::collections::HashMap<_, _>>();

    for attachment in &record.attachments {
        if let Some(ordinal) = retained_ordinals.get(attachment.id.as_str()) {
            let changed = connection.execute(
                "UPDATE attachments
                 SET ordinal = ?2
                 WHERE id = ?1 AND record_id = ?3
                   AND membership_state = 'active' AND relocation_state = 'ready'",
                params![
                    attachment.id,
                    i64::try_from(*ordinal).map_err(|_| StoreError::Conflict(
                        "attachment ordinal exceeds the database limit".to_owned()
                    ))?,
                    record.id
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "attachment changed before its order could be saved".to_owned(),
                ));
            }
        } else {
            let changed = connection.execute(
                "UPDATE attachments
                 SET membership_state = 'detaching'
                 WHERE id = ?1 AND record_id = ?2
                   AND membership_state = 'active' AND relocation_state = 'ready'",
                params![attachment.id, record.id],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "attachment changed before removal could be queued".to_owned(),
                ));
            }
            queue_attachment_detach_operation(connection, &attachment.id, revision, now)?;
        }
    }

    for (offset, attachment) in new_attachments.iter().enumerate() {
        let ordinal = retained_attachment_ids
            .len()
            .checked_add(offset)
            .ok_or_else(|| StoreError::Conflict("attachment ordinal is invalid".to_owned()))?;
        let prior_id = connection
            .query_row(
                "SELECT id FROM attachments
                 WHERE record_id = ?1 AND content_sha256 = ?2
                   AND membership_state IN ('detaching', 'detached')",
                params![record.id, attachment.content_sha256],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(attachment_id) = prior_id {
            connection.execute(
                "DELETE FROM attachment_file_operations WHERE attachment_id = ?1",
                [&attachment_id],
            )?;
            let changed = connection.execute(
                "UPDATE attachments
                 SET media_type = ?2, managed_relative_path = ?3,
                     content_sha256 = ?4, byte_size = ?5,
                     previous_managed_relative_path = NULL,
                     relocation_state = 'ready', relocation_error_code = NULL,
                     ordinal = ?6, membership_state = 'active'
                 WHERE id = ?1 AND record_id = ?7
                   AND membership_state IN ('detaching', 'detached')",
                params![
                    attachment_id,
                    attachment.media_type,
                    attachment.managed_relative_path,
                    attachment.content_sha256,
                    i64::try_from(attachment.byte_size).map_err(|_| StoreError::Conflict(
                        "attachment size exceeds the database limit".to_owned()
                    ))?,
                    i64::try_from(ordinal).map_err(|_| StoreError::Conflict(
                        "attachment ordinal exceeds the database limit".to_owned()
                    ))?,
                    record.id
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(
                    "detached attachment changed before it could be reused".to_owned(),
                ));
            }
        } else {
            connection.execute(
                "INSERT INTO attachments (
                    id, record_id, media_type, managed_relative_path, content_sha256,
                    byte_size, created_at_ms, previous_managed_relative_path,
                    relocation_state, relocation_error_code, ordinal, membership_state
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 'ready', NULL, ?8, 'active')",
                params![
                    new_id(),
                    record.id,
                    attachment.media_type,
                    attachment.managed_relative_path,
                    attachment.content_sha256,
                    i64::try_from(attachment.byte_size).map_err(|_| StoreError::Conflict(
                        "attachment size exceeds the database limit".to_owned()
                    ))?,
                    now,
                    i64::try_from(ordinal).map_err(|_| StoreError::Conflict(
                        "attachment ordinal exceeds the database limit".to_owned()
                    ))?
                ],
            )?;
        }
    }
    Ok(())
}

fn queue_record_attachment_relocations(
    connection: &Connection,
    record_id: &str,
    destination_directory: &str,
) -> Result<(), StoreError> {
    if destination_directory != INBOX_ATTACHMENT_DIRECTORY {
        validate_attachment_directory(destination_directory)?;
    }
    let attachments = {
        let mut statement = connection.prepare(
            "SELECT id, media_type, managed_relative_path, content_sha256
             FROM attachments
             WHERE record_id = ?1 AND membership_state = 'active'
             ORDER BY ordinal, created_at_ms, id",
        )?;
        let rows = statement.query_map([record_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (attachment_id, media_type, current_path, content_sha256) in attachments {
        validate_existing_managed_attachment_path(&current_path, &content_sha256, &media_type)?;
        let destination =
            managed_attachment_relative_path(destination_directory, &content_sha256, &media_type)?;
        if destination == current_path {
            continue;
        }
        let changed = connection.execute(
            "UPDATE attachments
             SET managed_relative_path = ?2,
                 previous_managed_relative_path = ?3,
                 relocation_state = 'pending', relocation_error_code = NULL
             WHERE id = ?1 AND record_id = ?4
               AND managed_relative_path = ?3
               AND relocation_state = 'ready'",
            params![attachment_id, destination, current_path, record_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "attachment changed before record migration could queue relocation".to_owned(),
            ));
        }
    }
    Ok(())
}

fn require_single_attachment_operation_change(changed: usize) -> Result<(), StoreError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "attachment file operation changed before it could be updated".to_owned(),
        ))
    }
}

fn validate_attachment_backup_relative_path(value: &str) -> Result<(), StoreError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(StoreError::Conflict(
            "attachment backup path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_composer_host_kind(value: &str) -> Result<(), StoreError> {
    if value.len() != 72
        || !value.starts_with("chatgpt:")
        || !value[8..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StoreError::Conflict(
            "composer host identity is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let current: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::Conflict(format!(
            "database schema {current} is newer than supported schema {SCHEMA_VERSION}"
        )));
    }
    if current == 0 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(
            "CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                root_path TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE notebooks (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                target_id TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                numbering_style TEXT NOT NULL CHECK (numbering_style IN (
                    'none', 'numeric', 'bullet', 'task', 'time_prefix', 'date_heading_numeric'
                )),
                numbering_start INTEGER NOT NULL DEFAULT 1
                    CHECK (numbering_start BETWEEN 1 AND 1000000000),
                numbering_sync_pending INTEGER NOT NULL DEFAULT 0
                    CHECK (numbering_sync_pending IN (0, 1)),
                attachment_directory TEXT NOT NULL DEFAULT 'attachments',
                previous_attachment_directory TEXT,
                attachment_directory_sync_pending INTEGER NOT NULL DEFAULT 0
                    CHECK (attachment_directory_sync_pending IN (0, 1)),
                target_state TEXT NOT NULL CHECK (target_state IN (
                    'unverified', 'ready', 'conflict', 'unavailable', 'unbound'
                )),
                last_error_code TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE (workspace_id, relative_path)
             ) STRICT;
             CREATE TABLE records (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                notebook_id TEXT REFERENCES notebooks(id) ON DELETE SET NULL,
                body_markdown TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                logical_order INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('active', 'trashed')),
                sync_state TEXT NOT NULL CHECK (sync_state IN (
                    'local', 'queued', 'synced', 'conflict', 'target_unavailable'
                )),
                revision INTEGER NOT NULL CHECK (
                    revision > 0 AND revision <= 9007199254740991
                ),
                applied_revision INTEGER NOT NULL CHECK (
                    applied_revision >= 0
                    AND applied_revision <= revision
                    AND applied_revision <= 9007199254740991
                ),
                trashed_at_ms INTEGER
             ) STRICT;
             CREATE INDEX records_by_tab_order
                ON records(workspace_id, notebook_id, state, logical_order);
             CREATE TABLE attachments (
                id TEXT PRIMARY KEY,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                media_type TEXT NOT NULL,
                managed_relative_path TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
                created_at_ms INTEGER NOT NULL,
                previous_managed_relative_path TEXT,
                relocation_state TEXT NOT NULL DEFAULT 'ready' CHECK (
                    relocation_state IN ('ready', 'pending', 'cleanup_pending', 'conflict')
                ),
                relocation_error_code TEXT,
                UNIQUE(record_id, content_sha256)
             ) STRICT;
             CREATE TABLE drafts (
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                tab_key TEXT NOT NULL,
                body_markdown TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(workspace_id, tab_key)
             ) STRICT;
             CREATE TABLE notebook_file_receipts (
                notebook_id TEXT PRIMARY KEY REFERENCES notebooks(id) ON DELETE CASCADE,
                target_id TEXT NOT NULL UNIQUE,
                marker_schema INTEGER NOT NULL CHECK (marker_schema > 0),
                managed_sha256 TEXT NOT NULL CHECK (
                    length(managed_sha256) = 64
                    AND managed_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                file_sha256 TEXT NOT NULL CHECK (
                    length(file_sha256) = 64
                    AND file_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                file_modified_ns INTEGER NOT NULL CHECK (file_modified_ns >= 0),
                file_size_bytes INTEGER NOT NULL CHECK (file_size_bytes >= 0),
                generation INTEGER NOT NULL CHECK (generation > 0),
                updated_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE mutation_receipts (
                mutation_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL CHECK (schema_version = 1),
                operation_kind TEXT NOT NULL CHECK (operation_kind IN (
                    'create', 'edit', 'trash', 'restore', 'migrate'
                )),
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                request_sha256 TEXT NOT NULL CHECK (
                    length(request_sha256) = 64
                    AND request_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                result_revision INTEGER NOT NULL CHECK (result_revision > 0),
                created_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE recovery_operations (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                operation_kind TEXT NOT NULL CHECK (operation_kind IN (
                    'create', 'edit', 'trash', 'restore', 'migrate'
                )),
                phase TEXT NOT NULL CHECK (phase IN (
                    'queued', 'staged', 'applying', 'files_applied',
                    'completed', 'needs_recovery', 'superseded'
                )),
                expected_revision INTEGER NOT NULL CHECK (expected_revision >= 0),
                desired_revision INTEGER NOT NULL CHECK (desired_revision > 0),
                desired_record_state TEXT NOT NULL CHECK (
                    desired_record_state IN ('active', 'trashed')
                ),
                source_notebook_id TEXT REFERENCES notebooks(id) ON DELETE RESTRICT,
                destination_notebook_id TEXT REFERENCES notebooks(id) ON DELETE RESTRICT,
                source_logical_order INTEGER,
                destination_logical_order INTEGER,
                attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
                last_error_code TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                CHECK (desired_revision = expected_revision + 1),
                CHECK (
                    operation_kind <> 'migrate'
                    OR source_notebook_id IS NOT destination_notebook_id
                ),
                CHECK (
                    completed_at_ms IS NULL
                    OR phase IN ('completed', 'superseded')
                ),
                UNIQUE (record_id, desired_revision)
             ) STRICT;
             CREATE INDEX pending_recovery_operations
                ON recovery_operations(phase, created_at_ms)
                WHERE phase NOT IN ('completed', 'superseded');
             CREATE TABLE recovery_file_steps (
                operation_id TEXT NOT NULL REFERENCES recovery_operations(id) ON DELETE CASCADE,
                step_index INTEGER NOT NULL CHECK (step_index >= 0),
                notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE RESTRICT,
                role TEXT NOT NULL CHECK (role IN ('source', 'destination', 'single')),
                effect TEXT NOT NULL CHECK (effect IN ('upsert', 'remove')),
                state TEXT NOT NULL CHECK (
                    state IN ('staged', 'applied', 'verified', 'rolled_back')
                ),
                target_workspace_relative_path TEXT NOT NULL,
                staged_sibling_name TEXT NOT NULL,
                backup_app_data_relative_path TEXT NOT NULL,
                expected_target_id TEXT NOT NULL,
                before_file_sha256 TEXT NOT NULL,
                after_file_sha256 TEXT NOT NULL,
                before_managed_sha256 TEXT,
                after_managed_sha256 TEXT NOT NULL,
                before_modified_ns INTEGER NOT NULL CHECK (before_modified_ns >= 0),
                target_existed INTEGER NOT NULL CHECK (target_existed IN (0, 1)),
                applied_at_ms INTEGER,
                PRIMARY KEY (operation_id, step_index),
                UNIQUE (operation_id, notebook_id)
             ) STRICT;",
        )?;
        transaction.execute_batch(CREATE_COMPOSER_RECEIPTS)?;
        transaction.execute_batch(CREATE_COMPOSER_RECEIPT_LATEST_INDEX)?;
        transaction.execute_batch(CREATE_ATTACHMENT_FILE_OPERATIONS)?;
        transaction.execute_batch(CREATE_RECORD_PINS)?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        transaction.execute_batch(CREATE_PRODUCT_SETTINGS_FRESH)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 1 {
        let legacy_receipts: i64 =
            connection.query_row("SELECT COUNT(*) FROM file_receipts", [], |row| row.get(0))?;
        let legacy_recovery: i64 =
            connection.query_row("SELECT COUNT(*) FROM recovery_operations", [], |row| {
                row.get(0)
            })?;
        if legacy_receipts != 0 || legacy_recovery != 0 {
            return Err(StoreError::Conflict(
                "schema 1 contains unknown file recovery state; migration was not started"
                    .to_owned(),
            ));
        }

        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(
            "ALTER TABLE records ADD COLUMN trashed_at_ms INTEGER;
             ALTER TABLE records ADD COLUMN applied_revision INTEGER NOT NULL DEFAULT 0
                CHECK (
                    applied_revision >= 0
                    AND applied_revision <= revision
                    AND applied_revision <= 9007199254740991
                );
             UPDATE records
             SET applied_revision = CASE
                    WHEN sync_state IN ('local', 'synced') THEN revision
                    ELSE 0
                 END,
                 trashed_at_ms = CASE
                    WHEN state = 'trashed' THEN updated_at_ms
                    ELSE NULL
                 END;
             ALTER TABLE notebooks ADD COLUMN target_state TEXT NOT NULL DEFAULT 'unverified'
                CHECK (target_state IN (
                    'unverified', 'ready', 'conflict', 'unavailable', 'unbound'
                ));
             ALTER TABLE notebooks ADD COLUMN last_error_code TEXT;
             DROP TABLE file_receipts;
             DROP TABLE recovery_operations;
             CREATE TABLE notebook_file_receipts (
                notebook_id TEXT PRIMARY KEY REFERENCES notebooks(id) ON DELETE CASCADE,
                target_id TEXT NOT NULL UNIQUE,
                marker_schema INTEGER NOT NULL CHECK (marker_schema > 0),
                managed_sha256 TEXT NOT NULL CHECK (
                    length(managed_sha256) = 64
                    AND managed_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                file_sha256 TEXT NOT NULL CHECK (
                    length(file_sha256) = 64
                    AND file_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                file_modified_ns INTEGER NOT NULL CHECK (file_modified_ns >= 0),
                file_size_bytes INTEGER NOT NULL CHECK (file_size_bytes >= 0),
                generation INTEGER NOT NULL CHECK (generation > 0),
                updated_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE recovery_operations (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                operation_kind TEXT NOT NULL CHECK (operation_kind IN (
                    'create', 'edit', 'trash', 'restore', 'migrate'
                )),
                phase TEXT NOT NULL CHECK (phase IN (
                    'queued', 'staged', 'applying', 'files_applied',
                    'completed', 'needs_recovery', 'superseded'
                )),
                expected_revision INTEGER NOT NULL CHECK (expected_revision >= 0),
                desired_revision INTEGER NOT NULL CHECK (desired_revision > 0),
                desired_record_state TEXT NOT NULL CHECK (
                    desired_record_state IN ('active', 'trashed')
                ),
                source_notebook_id TEXT REFERENCES notebooks(id) ON DELETE RESTRICT,
                destination_notebook_id TEXT REFERENCES notebooks(id) ON DELETE RESTRICT,
                source_logical_order INTEGER,
                destination_logical_order INTEGER,
                attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
                last_error_code TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                CHECK (desired_revision = expected_revision + 1),
                CHECK (
                    operation_kind <> 'migrate'
                    OR source_notebook_id IS NOT destination_notebook_id
                ),
                CHECK (
                    completed_at_ms IS NULL
                    OR phase IN ('completed', 'superseded')
                ),
                UNIQUE (record_id, desired_revision)
             ) STRICT;
             CREATE INDEX pending_recovery_operations
                ON recovery_operations(phase, created_at_ms)
                WHERE phase NOT IN ('completed', 'superseded');
             CREATE TABLE recovery_file_steps (
                operation_id TEXT NOT NULL REFERENCES recovery_operations(id) ON DELETE CASCADE,
                step_index INTEGER NOT NULL CHECK (step_index >= 0),
                notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE RESTRICT,
                role TEXT NOT NULL CHECK (role IN ('source', 'destination', 'single')),
                effect TEXT NOT NULL CHECK (effect IN ('upsert', 'remove')),
                state TEXT NOT NULL CHECK (
                    state IN ('staged', 'applied', 'verified', 'rolled_back')
                ),
                target_workspace_relative_path TEXT NOT NULL,
                staged_sibling_name TEXT NOT NULL,
                backup_app_data_relative_path TEXT NOT NULL,
                expected_target_id TEXT NOT NULL,
                before_file_sha256 TEXT NOT NULL,
                after_file_sha256 TEXT NOT NULL,
                before_managed_sha256 TEXT,
                after_managed_sha256 TEXT NOT NULL,
                before_modified_ns INTEGER NOT NULL CHECK (before_modified_ns >= 0),
                target_existed INTEGER NOT NULL CHECK (target_existed IN (0, 1)),
                applied_at_ms INTEGER,
                PRIMARY KEY (operation_id, step_index),
                UNIQUE (operation_id, notebook_id)
             ) STRICT;
             CREATE TABLE mutation_receipts (
                mutation_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL CHECK (schema_version = 1),
                operation_kind TEXT NOT NULL CHECK (operation_kind IN (
                    'create', 'edit', 'trash', 'restore', 'migrate'
                )),
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                request_sha256 TEXT NOT NULL CHECK (
                    length(request_sha256) = 64
                    AND request_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                result_revision INTEGER NOT NULL CHECK (result_revision > 0),
                created_at_ms INTEGER NOT NULL
             ) STRICT;",
        )?;
        transaction.execute_batch(CREATE_ATTACHMENT_FILE_OPERATIONS)?;
        transaction.execute_batch(CREATE_RECORD_PINS)?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 2 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(
            "CREATE TABLE mutation_receipts (
                mutation_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL CHECK (schema_version = 1),
                operation_kind TEXT NOT NULL CHECK (operation_kind IN (
                    'create', 'edit', 'trash', 'restore', 'migrate'
                )),
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                request_sha256 TEXT NOT NULL CHECK (
                    length(request_sha256) = 64
                    AND request_sha256 NOT GLOB '*[^0-9a-f]*'
                ),
                result_revision INTEGER NOT NULL CHECK (result_revision > 0),
                created_at_ms INTEGER NOT NULL
             ) STRICT;",
        )?;
        transaction.execute_batch(CREATE_ATTACHMENT_FILE_OPERATIONS)?;
        transaction.execute_batch(CREATE_RECORD_PINS)?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 3 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_ATTACHMENT_FILE_OPERATIONS)?;
        transaction.execute_batch(CREATE_RECORD_PINS)?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 4 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_RECORD_PINS)?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 5 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_UI_STATE)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 6 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(ADD_EDITOR_PREFERENCES)?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 7 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_NOTEBOOK_TAB_STATE)?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if current == 8 {
        let transaction = connection.unchecked_transaction()?;
        add_notebook_numbering_sync_pending(&transaction)?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if matches!(current, 9 | 10) {
        let transaction = connection.unchecked_transaction()?;
        add_notebook_numbering_start(&transaction)?;
        transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == LEGACY_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_DEFAULT_IDENTITY_SLOT)?;
        transaction.pragma_update(None, "user_version", DEFAULT_IDENTITY_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == DEFAULT_IDENTITY_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        add_workspace_connection_state(&transaction)?;
        transaction.pragma_update(None, "user_version", WORKSPACE_CONNECTION_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == WORKSPACE_CONNECTION_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_DRAFT_CREATE_ATTEMPTS)?;
        transaction.pragma_update(None, "user_version", DRAFT_CREATE_ATTEMPT_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == DRAFT_CREATE_ATTEMPT_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_NOTEBOOK_CONFLICTS)?;
        transaction.pragma_update(None, "user_version", NOTEBOOK_CONFLICT_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == NOTEBOOK_CONFLICT_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        add_attachment_set_revision_state(&transaction)?;
        transaction.pragma_update(None, "user_version", ATTACHMENT_SET_REVISION_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == ATTACHMENT_SET_REVISION_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_COMPOSER_RECEIPTS)?;
        transaction.execute_batch(CREATE_COMPOSER_RECEIPT_LATEST_INDEX)?;
        transaction.pragma_update(None, "user_version", COMPOSER_RECEIPT_LATEST_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == COMPOSER_RECEIPT_LATEST_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_UPDATE_SETTINGS)?;
        transaction.pragma_update(None, "user_version", UPDATE_SETTINGS_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == UPDATE_SETTINGS_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        add_update_network_controls(&transaction)?;
        transaction.pragma_update(None, "user_version", UPDATE_NETWORK_CONTROLS_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == UPDATE_NETWORK_CONTROLS_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_PRODUCT_SETTINGS_MIGRATION)?;
        transaction.pragma_update(None, "user_version", PRODUCT_SETTINGS_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let migrated: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if migrated == PRODUCT_SETTINGS_SCHEMA_VERSION {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(CREATE_WORKSPACE_OPEN_PREFERENCES)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    Ok(())
}

fn add_notebook_numbering_sync_pending(connection: &Connection) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('notebooks')
            WHERE name = 'numbering_sync_pending'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        connection.execute_batch(ADD_NOTEBOOK_NUMBERING_SYNC_PENDING)?;
    }
    Ok(())
}

fn add_update_network_controls(connection: &Connection) -> Result<(), StoreError> {
    let has_external_network_enabled: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('update_settings')
            WHERE name = 'external_network_enabled'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_external_network_enabled {
        connection.execute_batch(ADD_EXTERNAL_NETWORK_ENABLED)?;
    }

    let has_automatic_downloads_enabled: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('update_settings')
            WHERE name = 'automatic_downloads_enabled'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_automatic_downloads_enabled {
        connection.execute_batch(ADD_AUTOMATIC_DOWNLOADS_ENABLED)?;
    }
    Ok(())
}

fn add_notebook_numbering_start(connection: &Connection) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('notebooks')
            WHERE name = 'numbering_start'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        connection.execute_batch(ADD_NOTEBOOK_NUMBERING_START)?;
    }
    add_attachment_directory_configuration(connection)
}

fn add_workspace_connection_state(connection: &Connection) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('workspaces')
            WHERE name = 'is_connected'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        connection.execute_batch(ADD_WORKSPACE_CONNECTION_STATE)?;
    }
    Ok(())
}

fn add_attachment_set_revision_state(connection: &Connection) -> Result<(), StoreError> {
    let attachment_column_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('attachments')
         WHERE name IN ('ordinal', 'membership_state')",
        [],
        |row| row.get(0),
    )?;
    match attachment_column_count {
        0 => connection.execute_batch(ADD_ATTACHMENT_SET_REVISION_STATE)?,
        2 => {
            let invalid_rows: bool = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM attachments
                    WHERE ordinal < 0
                       OR membership_state NOT IN ('active', 'detaching', 'detached')
                 )",
                [],
                |row| row.get(0),
            )?;
            if invalid_rows {
                return Err(StoreError::Conflict(
                    "attachment membership schema contains invalid data".to_owned(),
                ));
            }
        }
        _ => {
            return Err(StoreError::Conflict(
                "attachment membership schema is incomplete".to_owned(),
            ));
        }
    }

    let operation_sql: String = connection.query_row(
        "SELECT sql FROM sqlite_master
             WHERE type = 'table' AND name = 'attachment_file_operations'",
        [],
        |row| row.get(0),
    )?;
    if !operation_sql.contains("'detach'") {
        connection.execute_batch(REBUILD_ATTACHMENT_FILE_OPERATIONS_FOR_DETACH)?;
    }
    Ok(())
}

fn add_attachment_directory_configuration(connection: &Connection) -> Result<(), StoreError> {
    let notebook_columns = [
        "attachment_directory",
        "previous_attachment_directory",
        "attachment_directory_sync_pending",
    ];
    let notebook_column_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('notebooks')
         WHERE name IN (
            'attachment_directory',
            'previous_attachment_directory',
            'attachment_directory_sync_pending'
         )",
        [],
        |row| row.get(0),
    )?;
    if notebook_column_count == 0 {
        connection.execute_batch(
            "ALTER TABLE notebooks
                ADD COLUMN attachment_directory TEXT NOT NULL DEFAULT 'attachments';
             ALTER TABLE notebooks
                ADD COLUMN previous_attachment_directory TEXT;
             ALTER TABLE notebooks
                ADD COLUMN attachment_directory_sync_pending INTEGER NOT NULL DEFAULT 0
                CHECK (attachment_directory_sync_pending IN (0, 1));",
        )?;
        let notebooks = {
            let mut statement = connection
                .prepare("SELECT id, relative_path FROM notebooks ORDER BY created_at_ms, id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (notebook_id, relative_path) in notebooks {
            let directory = default_attachment_directory(&relative_path)?;
            connection.execute(
                "UPDATE notebooks SET attachment_directory = ?2 WHERE id = ?1",
                params![notebook_id, directory],
            )?;
        }
    } else if notebook_column_count != notebook_columns.len() as i64 {
        return Err(StoreError::Conflict(
            "attachment directory schema is incomplete".to_owned(),
        ));
    }

    let attachments_table_exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name = 'attachments'
         )",
        [],
        |row| row.get(0),
    )?;
    if !attachments_table_exists {
        connection.execute_batch(
            "CREATE TABLE attachments (
                id TEXT PRIMARY KEY,
                record_id TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
                media_type TEXT NOT NULL,
                managed_relative_path TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
                created_at_ms INTEGER NOT NULL,
                previous_managed_relative_path TEXT,
                relocation_state TEXT NOT NULL DEFAULT 'ready' CHECK (
                    relocation_state IN ('ready', 'pending', 'cleanup_pending', 'conflict')
                ),
                relocation_error_code TEXT,
                UNIQUE(record_id, content_sha256)
             ) STRICT;",
        )?;
    }
    let attachment_column_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('attachments')
         WHERE name IN (
            'previous_managed_relative_path',
            'relocation_state',
            'relocation_error_code'
         )",
        [],
        |row| row.get(0),
    )?;
    if attachment_column_count == 0 {
        connection.execute_batch(
            "ALTER TABLE attachments
                ADD COLUMN previous_managed_relative_path TEXT;
             ALTER TABLE attachments
                ADD COLUMN relocation_state TEXT NOT NULL DEFAULT 'ready'
                CHECK (relocation_state IN ('ready', 'pending', 'cleanup_pending', 'conflict'));
             ALTER TABLE attachments
                ADD COLUMN relocation_error_code TEXT;",
        )?;
    } else if attachment_column_count != 3 {
        return Err(StoreError::Conflict(
            "attachment relocation schema is incomplete".to_owned(),
        ));
    }

    let directories = {
        let mut statement = connection.prepare(
            "SELECT id, attachment_directory, previous_attachment_directory
             FROM notebooks ORDER BY created_at_ms, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (_, current, previous) in directories {
        validate_attachment_directory(&current)?;
        if let Some(previous) = previous {
            validate_attachment_directory(&previous)?;
        }
    }
    Ok(())
}

fn workspace_from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        display_name: row.get(1)?,
        root_path: row.get(2)?,
        created_at_ms: row.get(3)?,
        updated_at_ms: row.get(4)?,
    })
}

fn notebook_conflict_from_row(row: &Row<'_>) -> rusqlite::Result<NotebookConflictEvidence> {
    let receipt_generation: i64 = row.get(3)?;
    let receipt_generation = u64::try_from(receipt_generation).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, Type::Integer, Box::new(error))
    })?;
    Ok(NotebookConflictEvidence {
        notebook_id: row.get(0)?,
        workspace_id: row.get(1)?,
        target_id: row.get(2)?,
        receipt_generation,
        expected_managed_sha256: row.get(4)?,
        observed_file_sha256: row.get(5)?,
        observed_managed_sha256: row.get(6)?,
        reason_code: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

fn attachment_file_operation_from_row(row: &Row<'_>) -> rusqlite::Result<AttachmentFileOperation> {
    let record_revision: i64 = row.get(5)?;
    let record_revision = u64::try_from(record_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, Type::Integer, Box::new(error))
    })?;
    let expected_byte_size: i64 = row.get(9)?;
    let expected_byte_size = u64::try_from(expected_byte_size).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, Type::Integer, Box::new(error))
    })?;
    let attempt_count: i64 = row.get(12)?;
    let attempt_count = u64::try_from(attempt_count).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(12, Type::Integer, Box::new(error))
    })?;
    Ok(AttachmentFileOperation {
        attachment_id: row.get(0)?,
        record_id: row.get(1)?,
        workspace_id: row.get(2)?,
        action: row.get(3)?,
        phase: row.get(4)?,
        record_revision,
        managed_relative_path: row.get(6)?,
        media_type: row.get(7)?,
        expected_sha256: row.get(8)?,
        expected_byte_size,
        trash_path: row.get(10)?,
        backup_app_data_relative_path: row.get(11)?,
        attempt_count,
        last_error_code: row.get(13)?,
    })
}

fn attachment_relocation_operation_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<AttachmentRelocationOperation> {
    let record_revision: i64 = row.get(4)?;
    let record_revision = u64::try_from(record_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, Type::Integer, Box::new(error))
    })?;
    let applied_revision: i64 = row.get(5)?;
    let applied_revision = u64::try_from(applied_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, Type::Integer, Box::new(error))
    })?;
    let byte_size: i64 = row.get(10)?;
    let byte_size = u64::try_from(byte_size).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(10, Type::Integer, Box::new(error))
    })?;
    let state_value: String = row.get(11)?;
    let state = AttachmentRelocationState::from_db_value(&state_value)
        .map_err(|error| domain_conversion_error(11, error))?;
    let previous_managed_relative_path = row.get::<_, Option<String>>(7)?.ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            Type::Null,
            Box::new(DomainError::new(
                "unfinished attachment relocation has no previous path",
            )),
        )
    })?;
    Ok(AttachmentRelocationOperation {
        attachment_id: row.get(0)?,
        record_id: row.get(1)?,
        workspace_id: row.get(2)?,
        notebook_id: row.get(3)?,
        record_revision,
        applied_revision,
        managed_relative_path: row.get(6)?,
        previous_managed_relative_path,
        media_type: row.get(8)?,
        content_sha256: row.get(9)?,
        byte_size,
        state,
        error_code: row.get(12)?,
        notebook_directory_sync_pending: row.get::<_, i64>(13)? != 0,
    })
}

fn notebook_from_row(row: &Row<'_>) -> rusqlite::Result<Notebook> {
    let style: String = row.get(6)?;
    let numbering_start: i64 = row.get(7)?;
    let numbering_start = u32::try_from(numbering_start).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, Type::Integer, Box::new(error))
    })?;
    validate_numbering_start(numbering_start).map_err(|error| domain_conversion_error(7, error))?;
    let target_state: String = row.get(9)?;
    let attachment_directory: String = row.get(13)?;
    let attachment_directory = validate_attachment_directory(&attachment_directory)
        .map_err(|error| domain_conversion_error(13, error))?;
    let previous_attachment_directory: Option<String> = row.get(14)?;
    if let Some(previous) = previous_attachment_directory.as_deref() {
        validate_attachment_directory(previous)
            .map_err(|error| domain_conversion_error(14, error))?;
    }
    Ok(Notebook {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        target_id: row.get(2)?,
        display_name: row.get(3)?,
        relative_path: row.get(4)?,
        ordinal: row.get(5)?,
        is_pinned: row.get::<_, i64>(16)? != 0,
        numbering_style: NumberingStyle::from_db_value(&style)
            .map_err(|error| domain_conversion_error(6, error))?,
        numbering_start,
        numbering_sync_pending: row.get::<_, i64>(8)? != 0,
        attachment_directory,
        previous_attachment_directory,
        attachment_directory_sync_pending: row.get::<_, i64>(15)? != 0,
        target_state: NotebookTargetState::from_db_value(&target_state)
            .map_err(|error| domain_conversion_error(9, error))?,
        last_error_code: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<Record> {
    let state: String = row.get(7)?;
    let sync_state: String = row.get(8)?;
    let revision: i64 = row.get(9)?;
    let revision = u64::try_from(revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, Type::Integer, Box::new(error))
    })?;
    let applied_revision: i64 = row.get(10)?;
    let applied_revision = u64::try_from(applied_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(10, Type::Integer, Box::new(error))
    })?;
    Ok(Record {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        notebook_id: row.get(2)?,
        body_markdown: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
        logical_order: row.get(6)?,
        state: RecordState::from_db_value(&state)
            .map_err(|error| domain_conversion_error(7, error))?,
        sync_state: SyncState::from_db_value(&sync_state)
            .map_err(|error| domain_conversion_error(8, error))?,
        revision,
        applied_revision,
        trashed_at_ms: row.get(11)?,
        is_pinned: false,
        attachments: Vec::new(),
    })
}

fn recovery_operation_from_row(row: &Row<'_>) -> rusqlite::Result<RecoveryOperation> {
    let expected_revision: i64 = row.get(5)?;
    let expected_revision = u64::try_from(expected_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, Type::Integer, Box::new(error))
    })?;
    let desired_revision: i64 = row.get(6)?;
    let desired_revision = u64::try_from(desired_revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, Type::Integer, Box::new(error))
    })?;
    let desired_record_state: String = row.get(7)?;
    let attempt_count: i64 = row.get(12)?;
    let attempt_count = u64::try_from(attempt_count).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(12, Type::Integer, Box::new(error))
    })?;
    Ok(RecoveryOperation {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        record_id: row.get(2)?,
        operation_kind: row.get(3)?,
        phase: row.get(4)?,
        expected_revision,
        desired_revision,
        desired_record_state: RecordState::from_db_value(&desired_record_state)
            .map_err(|error| domain_conversion_error(7, error))?,
        source_notebook_id: row.get(8)?,
        destination_notebook_id: row.get(9)?,
        source_logical_order: row.get(10)?,
        destination_logical_order: row.get(11)?,
        attempt_count,
        last_error_code: row.get(13)?,
    })
}

fn recovery_file_step_from_row(row: &Row<'_>) -> rusqlite::Result<RecoveryFileStep> {
    let step_index: i64 = row.get(1)?;
    let step_index = usize::try_from(step_index).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, Type::Integer, Box::new(error))
    })?;
    let before_modified_ns: i64 = row.get(14)?;
    let before_modified_ns = u64::try_from(before_modified_ns).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(14, Type::Integer, Box::new(error))
    })?;
    let target_existed: i64 = row.get(15)?;
    Ok(RecoveryFileStep {
        operation_id: row.get(0)?,
        step_index,
        notebook_id: row.get(2)?,
        role: row.get(3)?,
        effect: row.get(4)?,
        state: row.get(5)?,
        target_workspace_relative_path: row.get(6)?,
        staged_sibling_name: row.get(7)?,
        backup_app_data_relative_path: row.get(8)?,
        expected_target_id: row.get(9)?,
        before_file_sha256: row.get(10)?,
        after_file_sha256: row.get(11)?,
        before_managed_sha256: row.get(12)?,
        after_managed_sha256: row.get(13)?,
        before_modified_ns,
        target_existed: target_existed != 0,
        applied_at_ms: row.get(16)?,
    })
}

fn mark_attachment_membership_detached(
    connection: &Connection,
    operation: &AttachmentFileOperation,
) -> Result<(), StoreError> {
    let changed = connection.execute(
        "UPDATE attachments
         SET membership_state = 'detached'
         WHERE id = ?1 AND record_id = ?2 AND membership_state = 'detaching'",
        params![operation.attachment_id, operation.record_id],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "attachment membership changed before detach completion".to_owned(),
        ))
    }
}

fn record_for_workspace(
    connection: &Connection,
    workspace_id: &str,
    record_id: &str,
) -> Result<Record, StoreError> {
    let mut record = connection
        .query_row(
            "SELECT id, workspace_id, notebook_id, body_markdown, created_at_ms, updated_at_ms,
                    logical_order, state, sync_state, revision, applied_revision, trashed_at_ms
             FROM records WHERE id = ?1 AND workspace_id = ?2",
            params![record_id, workspace_id],
            record_from_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound("record"))?;
    record.attachments = attachments_for_record(connection, record_id)?;
    record.is_pinned = record_is_pinned(connection, record_id)?;
    Ok(record)
}

fn record_is_pinned(connection: &Connection, record_id: &str) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM record_pins WHERE record_id = ?1)",
            [record_id],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn validate_sha256(value: &str, label: &str) -> Result<(), StoreError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    Err(StoreError::Conflict(format!("{label} is invalid")))
}

fn attachments_for_record(
    connection: &Connection,
    record_id: &str,
) -> Result<Vec<crate::domain::Attachment>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT a.id, a.record_id, a.media_type, a.managed_relative_path, a.content_sha256,
                a.byte_size, a.created_at_ms, operation.phase,
                a.previous_managed_relative_path, a.relocation_state, a.relocation_error_code
         FROM attachments a
         LEFT JOIN attachment_file_operations operation ON operation.attachment_id = a.id
         WHERE a.record_id = ?1 AND a.membership_state = 'active'
         ORDER BY a.ordinal, a.created_at_ms, a.id",
    )?;
    let rows = statement.query_map([record_id], |row| {
        let byte_size: i64 = row.get(5)?;
        let byte_size = u64::try_from(byte_size).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(5, Type::Integer, Box::new(error))
        })?;
        let relocation_value: String = row.get(9)?;
        let relocation_state = AttachmentRelocationState::from_db_value(&relocation_value)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
            })?;
        let operation_file_state =
            AttachmentFileState::from_operation_phase(row.get::<_, Option<String>>(7)?.as_deref())
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(error))
                })?;
        let file_state = if matches!(
            relocation_state,
            AttachmentRelocationState::Pending | AttachmentRelocationState::Conflict
        ) {
            AttachmentFileState::RecoveryRequired
        } else {
            operation_file_state
        };
        Ok(crate::domain::Attachment {
            id: row.get(0)?,
            record_id: row.get(1)?,
            media_type: row.get(2)?,
            managed_relative_path: row.get(3)?,
            content_sha256: row.get(4)?,
            byte_size,
            created_at_ms: row.get(6)?,
            file_state,
            previous_managed_relative_path: row.get(8)?,
            relocation_state,
            relocation_error_code: row.get(10)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn validate_new_attachments(attachments: &[NewAttachment]) -> Result<(), StoreError> {
    const MAX_COUNT: usize = 10;
    const MAX_SINGLE_BYTES: u64 = 20 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
    if attachments.len() > MAX_COUNT {
        return Err(StoreError::Conflict("too many attachments".to_owned()));
    }
    let mut total = 0_u64;
    let mut digests = std::collections::HashSet::new();
    for attachment in attachments {
        if attachment.byte_size == 0 || attachment.byte_size > MAX_SINGLE_BYTES {
            return Err(StoreError::Conflict(
                "attachment size is invalid".to_owned(),
            ));
        }
        total = total
            .checked_add(attachment.byte_size)
            .ok_or_else(|| StoreError::Conflict("attachment total is too large".to_owned()))?;
        if total > MAX_TOTAL_BYTES {
            return Err(StoreError::Conflict(
                "attachment total is too large".to_owned(),
            ));
        }
        if attachment.content_sha256.len() != 64
            || !attachment
                .content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || !digests.insert(attachment.content_sha256.as_str())
        {
            return Err(StoreError::Conflict(
                "attachment digest is invalid or duplicated".to_owned(),
            ));
        }
        validate_managed_attachment_relative_path(
            &attachment.managed_relative_path,
            &attachment.content_sha256,
            &attachment.media_type,
        )?;
        if let Some(previous) = attachment.previous_managed_relative_path.as_deref() {
            validate_managed_attachment_relative_path(
                previous,
                &attachment.content_sha256,
                &attachment.media_type,
            )?;
            if previous == attachment.managed_relative_path {
                return Err(StoreError::Conflict(
                    "attachment relocation source and destination are identical".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_attachment_set_revision(
    current: &[Attachment],
    retained_attachment_ids: &[String],
    new_attachments: &[NewAttachment],
) -> Result<(), StoreError> {
    validate_new_attachments(new_attachments)?;
    if retained_attachment_ids.len() + new_attachments.len() > 10 {
        return Err(StoreError::Conflict("too many attachments".to_owned()));
    }

    let current_by_id = current
        .iter()
        .map(|attachment| (attachment.id.as_str(), attachment))
        .collect::<std::collections::HashMap<_, _>>();
    let mut retained_ids = std::collections::HashSet::new();
    let mut desired_digests = std::collections::HashSet::new();
    let mut total = 0_u64;
    for attachment_id in retained_attachment_ids {
        validate_id(attachment_id, "attachment id")?;
        if !retained_ids.insert(attachment_id.as_str()) {
            return Err(StoreError::Conflict(
                "retained attachment id is duplicated".to_owned(),
            ));
        }
        let attachment = current_by_id.get(attachment_id.as_str()).ok_or_else(|| {
            StoreError::Conflict("retained attachment does not belong to the record".to_owned())
        })?;
        if !desired_digests.insert(attachment.content_sha256.as_str()) {
            return Err(StoreError::Conflict(
                "attachment digest is duplicated in the final set".to_owned(),
            ));
        }
        total = total
            .checked_add(attachment.byte_size)
            .ok_or_else(|| StoreError::Conflict("attachment total is too large".to_owned()))?;
    }
    for attachment in new_attachments {
        if !desired_digests.insert(attachment.content_sha256.as_str()) {
            return Err(StoreError::Conflict(
                "attachment digest is duplicated in the final set".to_owned(),
            ));
        }
        total = total
            .checked_add(attachment.byte_size)
            .ok_or_else(|| StoreError::Conflict("attachment total is too large".to_owned()))?;
    }
    if total > 100 * 1024 * 1024 {
        return Err(StoreError::Conflict(
            "attachment total is too large".to_owned(),
        ));
    }
    Ok(())
}

fn validate_existing_managed_attachment_path(
    value: &str,
    content_sha256: &str,
    media_type: &str,
) -> Result<(), StoreError> {
    if validate_managed_attachment_relative_path(value, content_sha256, media_type).is_ok() {
        return Ok(());
    }
    let inbox_path =
        managed_attachment_relative_path(INBOX_ATTACHMENT_DIRECTORY, content_sha256, media_type)?;
    let file_name = Path::new(&inbox_path)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StoreError::Conflict("attachment identity path is invalid".to_owned()))?;
    let legacy = format!(".wakegpt/attachments/{}/{file_name}", &content_sha256[..2]);
    if value == legacy {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "existing attachment path does not match its content identity".to_owned(),
        ))
    }
}

fn require_revision(record: &Record, expected_revision: u64) -> Result<(), StoreError> {
    if record.revision == expected_revision {
        Ok(())
    } else {
        Err(StoreError::Conflict(format!(
            "record revision changed: expected {expected_revision}, found {}",
            record.revision
        )))
    }
}

fn revision_to_db(revision: u64) -> Result<i64, StoreError> {
    i64::try_from(revision)
        .map_err(|_| StoreError::Conflict("record revision exceeds the database limit".to_owned()))
}

fn require_active_revision(record: &Record, expected_revision: u64) -> Result<(), StoreError> {
    if record.state != RecordState::Active {
        return Err(StoreError::Conflict(
            "trashed records must be restored before editing or moving".to_owned(),
        ));
    }
    require_revision(record, expected_revision)
}

fn domain_conversion_error(index: usize, error: DomainError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn ensure_workspace_exists(connection: &Connection, workspace_id: &str) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1)",
        [workspace_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound("workspace"))
    }
}

fn selected_notebook_id(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT selected_notebook_id FROM workspace_ui_state WHERE workspace_id = ?1",
            [workspace_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(StoreError::from)
}

fn workspace_open_notebook_id(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<String>, StoreError> {
    let preference: Option<(Option<String>, bool)> = connection
        .query_row(
            "SELECT default_notebook_id, use_last_selection
             FROM workspace_open_preferences WHERE workspace_id = ?1",
            [workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match preference {
        None | Some((_, true)) => selected_notebook_id(connection, workspace_id),
        Some((Some(default_notebook_id), false)) => {
            ensure_notebook_belongs_to_workspace(connection, &default_notebook_id, workspace_id)?;
            Ok(Some(default_notebook_id))
        }
        Some((None, false)) => Ok(None),
    }
}

fn draft_tab_key(
    connection: &Connection,
    workspace_id: &str,
    notebook_id: Option<&str>,
) -> Result<String, StoreError> {
    validate_id(workspace_id, "workspace id")?;
    ensure_workspace_exists(connection, workspace_id)?;
    match notebook_id {
        Some(notebook_id) => {
            validate_id(notebook_id, "notebook id")?;
            ensure_notebook_belongs_to_workspace(connection, notebook_id, workspace_id)?;
            Ok(notebook_id.to_owned())
        }
        None => Ok("inbox".to_owned()),
    }
}

fn validate_draft_surface(surface: &str) -> Result<(), StoreError> {
    if surface == QUICK_CAPTURE_DRAFT_SURFACE {
        Ok(())
    } else {
        Err(StoreError::Conflict("draft surface is invalid".to_owned()))
    }
}

fn surface_draft_tab_key(
    connection: &Connection,
    surface: &str,
    workspace_id: &str,
    notebook_id: Option<&str>,
) -> Result<String, StoreError> {
    let target = draft_tab_key(connection, workspace_id, notebook_id)?;
    Ok(format!("surface:{surface}:{target}"))
}

fn save_draft_at_tab_key(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    tab_key: &str,
    body_markdown: &str,
    attachments: &[DraftAttachment],
) -> Result<(), StoreError> {
    let next_create_fingerprint = if body_markdown.is_empty() && attachments.is_empty() {
        None
    } else {
        Some(draft_create_fingerprint(
            body_markdown.trim_end(),
            attachments.iter().map(|attachment| {
                (
                    attachment.media_type.as_str(),
                    attachment.byte_size,
                    attachment.content_sha256.as_str(),
                )
            }),
        ))
    };
    match next_create_fingerprint {
        Some(fingerprint) => {
            transaction.execute(
                "DELETE FROM draft_create_attempts
                 WHERE workspace_id = ?1 AND tab_key = ?2 AND draft_fingerprint <> ?3",
                params![workspace_id, tab_key, fingerprint],
            )?;
        }
        None => {
            transaction.execute(
                "DELETE FROM draft_create_attempts
                 WHERE workspace_id = ?1 AND tab_key = ?2",
                params![workspace_id, tab_key],
            )?;
        }
    }
    transaction.execute(
        "DELETE FROM draft_attachments WHERE workspace_id = ?1 AND tab_key = ?2",
        params![workspace_id, tab_key],
    )?;
    if body_markdown.is_empty() && attachments.is_empty() {
        transaction.execute(
            "DELETE FROM drafts WHERE workspace_id = ?1 AND tab_key = ?2",
            params![workspace_id, tab_key],
        )?;
        return Ok(());
    }
    let now = now_ms();
    transaction.execute(
        "INSERT INTO drafts (workspace_id, tab_key, body_markdown, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(workspace_id, tab_key) DO UPDATE SET
            body_markdown = excluded.body_markdown,
            updated_at_ms = excluded.updated_at_ms",
        params![workspace_id, tab_key, body_markdown, now],
    )?;
    for (ordinal, attachment) in attachments.iter().enumerate() {
        transaction.execute(
            "INSERT INTO draft_attachments (
                workspace_id, tab_key, token, media_type, byte_size,
                content_sha256, display_name, ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace_id,
                tab_key,
                attachment.token,
                attachment.media_type,
                i64::try_from(attachment.byte_size).map_err(|_| StoreError::Conflict(
                    "draft attachment size is too large".to_owned()
                ))?,
                attachment.content_sha256,
                attachment.display_name,
                i64::try_from(ordinal).map_err(|_| StoreError::Conflict(
                    "draft attachment order is too large".to_owned()
                ))?
            ],
        )?;
    }
    Ok(())
}

fn load_draft_at_tab_key(
    connection: &Connection,
    workspace_id: &str,
    tab_key: &str,
) -> Result<SavedDraft, StoreError> {
    let body_markdown = connection
        .query_row(
            "SELECT body_markdown FROM drafts WHERE workspace_id = ?1 AND tab_key = ?2",
            params![workspace_id, tab_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default();
    let mut statement = connection.prepare(
        "SELECT token, media_type, byte_size, content_sha256, display_name
         FROM draft_attachments
         WHERE workspace_id = ?1 AND tab_key = ?2
         ORDER BY ordinal, token",
    )?;
    let rows = statement.query_map(params![workspace_id, tab_key], |row| {
        let byte_size: i64 = row.get(2)?;
        let byte_size = u64::try_from(byte_size).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, Type::Integer, Box::new(error))
        })?;
        Ok(DraftAttachment {
            token: row.get(0)?,
            media_type: row.get(1)?,
            byte_size,
            content_sha256: row.get(3)?,
            display_name: row.get(4)?,
        })
    })?;
    let attachments = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(SavedDraft {
        body_markdown,
        attachments,
    })
}

fn validate_draft_attachments(attachments: &[DraftAttachment]) -> Result<(), StoreError> {
    if attachments.len() > 10 {
        return Err(StoreError::Conflict(
            "draft contains too many attachments".to_owned(),
        ));
    }
    let mut total = 0_u64;
    let mut tokens = std::collections::HashSet::new();
    let mut digests = std::collections::HashSet::new();
    for attachment in attachments {
        validate_id(&attachment.token, "draft attachment token")?;
        if !tokens.insert(attachment.token.as_str())
            || !digests.insert(attachment.content_sha256.as_str())
            || attachment.display_name.trim().is_empty()
            || attachment.display_name.len() > 512
            || attachment.display_name.contains('\0')
            || !matches!(
                attachment.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
            || attachment.content_sha256.len() != 64
            || !attachment
                .content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || attachment.byte_size == 0
            || attachment.byte_size > 20 * 1024 * 1024
        {
            return Err(StoreError::Conflict(
                "draft attachment metadata is invalid".to_owned(),
            ));
        }
        total = total
            .checked_add(attachment.byte_size)
            .ok_or_else(|| StoreError::Conflict("draft attachments are too large".to_owned()))?;
        if total > 100 * 1024 * 1024 {
            return Err(StoreError::Conflict(
                "draft attachments are too large".to_owned(),
            ));
        }
    }
    Ok(())
}

fn ensure_recovery_operation_exists(
    connection: &Connection,
    operation_id: &str,
) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_operations WHERE id = ?1)",
        [operation_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound("recovery operation"))
    }
}

fn record_can_be_permanently_deleted(
    connection: &Connection,
    record_id: &str,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT
                NOT EXISTS (
                    SELECT 1 FROM recovery_operations operation
                    WHERE operation.record_id = ?1
                      AND operation.phase NOT IN ('completed', 'superseded')
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM attachment_file_operations operation
                    JOIN attachments attachment ON attachment.id = operation.attachment_id
                    WHERE attachment.record_id = ?1
                      AND (
                        operation.phase IN ('queued', 'prepared', 'needs_recovery')
                        OR operation.backup_app_data_relative_path IS NOT NULL
                      )
                )
                AND NOT EXISTS (
                    SELECT 1 FROM attachments attachment
                    WHERE attachment.record_id = ?1
                      AND (
                        attachment.membership_state <> 'active'
                        OR attachment.relocation_state <> 'ready'
                      )
                )
                AND NOT EXISTS (
                    SELECT 1 FROM notebooks notebook
                    JOIN records record ON record.notebook_id = notebook.id
                    WHERE record.id = ?1
                      AND (
                        notebook.numbering_sync_pending = 1
                        OR notebook.attachment_directory_sync_pending = 1
                      )
                )
                AND NOT EXISTS (
                    SELECT 1 FROM notebook_conflicts conflict
                    JOIN records record ON record.notebook_id = conflict.notebook_id
                    WHERE record.id = ?1
                )
                AND NOT EXISTS (
                    SELECT 1 FROM composer_receipts receipt
                    WHERE receipt.record_id = ?1 AND receipt.state = 'uncertain'
                      AND NOT EXISTS (
                        SELECT 1 FROM composer_receipts newer
                        WHERE newer.record_id = receipt.record_id
                          AND newer.host_kind = receipt.host_kind
                          AND (
                            newer.created_at_ms > receipt.created_at_ms
                            OR (
                              newer.created_at_ms = receipt.created_at_ms
                              AND newer.id > receipt.id
                            )
                          )
                      )
                )",
            [record_id],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn cleanup_preview_token(
    retention_days: Option<u32>,
    candidates: &[RecordTrashCleanupCandidate],
    blocked_count: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"wakegpt-record-trash-cleanup-v2\0");
    match retention_days {
        Some(days) => {
            hasher.update([1]);
            hasher.update(days.to_be_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.update(blocked_count.to_be_bytes());
    for candidate in candidates {
        hasher.update(candidate.record_id.as_bytes());
        hasher.update([0]);
        hasher.update(candidate.workspace_id.as_bytes());
        hasher.update([0]);
        if let Some(notebook_id) = candidate.notebook_id.as_deref() {
            hasher.update(notebook_id.as_bytes());
        }
        hasher.update([0]);
        hasher.update(candidate.trashed_at_ms.to_be_bytes());
        hasher.update(candidate.attachment_count.to_be_bytes());
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn ensure_record_mutation_available(
    connection: &Connection,
    record_id: &str,
) -> Result<(), StoreError> {
    let pending_operation: Option<String> = connection
        .query_row(
            "SELECT id FROM recovery_operations
             WHERE record_id = ?1 AND phase NOT IN ('completed', 'superseded')
             ORDER BY created_at_ms, id
             LIMIT 1",
            [record_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(operation_id) = pending_operation {
        return Err(StoreError::Conflict(format!(
            "record has unfinished file operation {operation_id}"
        )));
    }
    let pending_attachment: Option<String> = connection
        .query_row(
            "SELECT operation.attachment_id
             FROM attachment_file_operations operation
             JOIN attachments attachment ON attachment.id = operation.attachment_id
             WHERE attachment.record_id = ?1
               AND operation.phase IN ('queued', 'prepared', 'needs_recovery')
             ORDER BY operation.updated_at_ms, operation.attachment_id
             LIMIT 1",
            [record_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(attachment_id) = pending_attachment {
        return Err(StoreError::Conflict(format!(
            "record has unfinished attachment operation {attachment_id}"
        )));
    }
    let pending_relocation: Option<String> = connection
        .query_row(
            "SELECT id FROM attachments
             WHERE record_id = ?1 AND membership_state = 'active'
               AND relocation_state <> 'ready'
             ORDER BY created_at_ms, id LIMIT 1",
            [record_id],
            |row| row.get(0),
        )
        .optional()?;
    match pending_relocation {
        Some(attachment_id) => Err(StoreError::Conflict(format!(
            "record has unfinished attachment relocation {attachment_id}"
        ))),
        None => Ok(()),
    }
}

fn ensure_notebook_targets_available(
    connection: &Connection,
    source_notebook_id: Option<&str>,
    destination_notebook_id: Option<&str>,
) -> Result<(), StoreError> {
    if source_notebook_id.is_none() && destination_notebook_id.is_none() {
        return Ok(());
    }
    let mut checked = std::collections::HashSet::new();
    for notebook_id in [source_notebook_id, destination_notebook_id]
        .into_iter()
        .flatten()
    {
        if !checked.insert(notebook_id) {
            continue;
        }
        let target_state = connection
            .query_row(
                "SELECT target_state FROM notebooks WHERE id = ?1",
                [notebook_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match target_state.as_deref() {
            Some("unbound") => {
                return Err(StoreError::Conflict(
                    "速记本已解除绑定，请重新绑定后再修改记录".to_owned(),
                ));
            }
            Some("conflict") => {
                return Err(StoreError::Conflict(
                    "速记本存在同步冲突，请先处理后再修改记录".to_owned(),
                ));
            }
            Some("unavailable") => {
                return Err(StoreError::Conflict(
                    "速记本文件当前不可用，请先恢复文件".to_owned(),
                ));
            }
            Some(_) => {}
            None => return Err(StoreError::NotFound("notebook")),
        }
    }
    let pending_operation: Option<String> = connection
        .query_row(
            "SELECT id FROM recovery_operations
             WHERE phase NOT IN ('completed', 'superseded')
               AND (
                    (?1 IS NOT NULL AND (
                        source_notebook_id = ?1 OR destination_notebook_id = ?1
                    ))
                    OR
                    (?2 IS NOT NULL AND (
                        source_notebook_id = ?2 OR destination_notebook_id = ?2
                    ))
               )
             ORDER BY created_at_ms, id
             LIMIT 1",
            params![source_notebook_id, destination_notebook_id],
            |row| row.get(0),
        )
        .optional()?;
    match pending_operation {
        Some(operation_id) => Err(StoreError::Conflict(format!(
            "notebook target has unfinished file operation {operation_id}"
        ))),
        None => Ok(()),
    }
}

fn ensure_conflict_has_only_unstaged_edits(
    connection: &Connection,
    notebook_id: &str,
) -> Result<(), StoreError> {
    let unsafe_operation: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM recovery_operations operation
            WHERE operation.phase NOT IN ('completed', 'superseded')
              AND (operation.source_notebook_id = ?1 OR operation.destination_notebook_id = ?1)
              AND (
                    operation.operation_kind <> 'edit'
                    OR operation.source_notebook_id IS NOT ?1
                    OR operation.destination_notebook_id IS NOT ?1
                    OR EXISTS(
                        SELECT 1 FROM recovery_file_steps step
                        WHERE step.operation_id = operation.id
                    )
              )
         )",
        [notebook_id],
        |row| row.get(0),
    )?;
    if unsafe_operation {
        return Err(StoreError::Conflict(
            "this conflict has a staged or cross-notebook operation; choose the WakeGPT version"
                .to_owned(),
        ));
    }
    let pending_attachment: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM records record
            JOIN attachments attachment ON attachment.record_id = record.id
            LEFT JOIN attachment_file_operations operation
                   ON operation.attachment_id = attachment.id
            WHERE record.notebook_id = ?1
              AND attachment.membership_state <> 'detached'
              AND (
                    attachment.relocation_state <> 'ready'
                    OR operation.phase IN ('queued', 'prepared', 'needs_recovery')
              )
         )",
        [notebook_id],
        |row| row.get(0),
    )?;
    if pending_attachment {
        return Err(StoreError::Conflict(
            "attachment recovery must finish before resolving this conflict".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_notebook_belongs_to_workspace(
    connection: &Connection,
    notebook_id: &str,
    workspace_id: &str,
) -> Result<(), StoreError> {
    let owner: Option<String> = connection
        .query_row(
            "SELECT workspace_id FROM notebooks WHERE id = ?1",
            [notebook_id],
            |row| row.get(0),
        )
        .optional()?;
    match owner {
        Some(owner) if owner == workspace_id => Ok(()),
        Some(_) => Err(StoreError::Conflict(
            "notebook does not belong to the selected workspace".to_owned(),
        )),
        None => Err(StoreError::NotFound("notebook")),
    }
}

fn validate_update_interval(value: u32) -> Result<(), StoreError> {
    if matches!(value, 6 | 12 | 24 | 48) {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "update check interval is unsupported".to_owned(),
        ))
    }
}

fn validate_update_version(value: &str) -> Result<(), StoreError> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(StoreError::Conflict(
            "skipped update version is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_update_error_code(value: &str) -> Result<(), StoreError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(StoreError::Conflict(
            "update error code is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES, DEFAULT_RECORD_TRASH_RETENTION_DAYS,
    };
    use std::sync::{mpsc, Arc};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn update_freeze_drains_and_blocks_database_operations() {
        let store = Arc::new(store());
        let guard = store.freeze_for_update().unwrap();
        let worker_store = Arc::clone(&store);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender.send(worker_store.schema_version().unwrap()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(guard);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            SCHEMA_VERSION,
        );
        worker.join().unwrap();
    }

    #[test]
    fn update_backup_is_private_verified_and_rejects_pending_recovery() {
        let store = store();
        let directory = std::env::temp_dir().join(format!("wakegpt-update-{}", new_id()));
        fs::create_dir(&directory).unwrap();
        let destination = directory.join("wakegpt.sqlite3");
        let mut guard = store.freeze_for_update().unwrap();
        let backup = store
            .backup_database_for_update(&mut guard, &destination)
            .unwrap();
        assert!(backup.byte_size > 0);
        assert_eq!(backup.sha256.len(), 64);
        #[cfg(unix)]
        assert_eq!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
        drop(guard);
        assert_eq!(
            Connection::open_with_flags(&destination, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            SCHEMA_VERSION,
        );

        let workspace = store
            .create_workspace("Update pending", "/tmp/wakegpt-update-pending")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Pending",
                "Pending.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        store
            .create_record(&workspace.id, Some(&notebook.id), "Pending record")
            .unwrap();
        let second_destination = directory.join("blocked.sqlite3");
        let mut guard = store.freeze_for_update().unwrap();
        assert!(matches!(
            store.backup_database_for_update(&mut guard, &second_destination),
            Err(StoreError::Conflict(message)) if message == "update recovery operations are pending"
        ));
        assert!(!second_destination.exists());
        drop(guard);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quick_capture_drafts_are_isolated_from_main_window_drafts() {
        let store = store();
        let workspace = store
            .create_workspace("Quick capture", "/tmp/wakegpt-quick-capture")
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Main window", &[])
            .unwrap();
        store
            .save_surface_draft(
                QUICK_CAPTURE_DRAFT_SURFACE,
                &workspace.id,
                None,
                "Menu bar",
                &[],
            )
            .unwrap();

        assert_eq!(
            store.load_draft(&workspace.id, None).unwrap().body_markdown,
            "Main window"
        );
        assert_eq!(
            store
                .load_surface_draft(QUICK_CAPTURE_DRAFT_SURFACE, &workspace.id, None)
                .unwrap()
                .body_markdown,
            "Menu bar"
        );
        assert!(store
            .load_surface_draft("unknown", &workspace.id, None)
            .unwrap_err()
            .to_string()
            .contains("draft surface is invalid"));
    }

    #[test]
    fn quick_capture_create_attempts_do_not_collide_with_main_window_attempts() {
        let store = store();
        let workspace = store
            .create_workspace("Quick create", "/tmp/wakegpt-quick-create")
            .unwrap();
        store
            .save_surface_draft(
                QUICK_CAPTURE_DRAFT_SURFACE,
                &workspace.id,
                None,
                "Same text",
                &[],
            )
            .unwrap();
        let quick = store
            .create_record_from_surface_draft_attempt(
                Some(QUICK_CAPTURE_DRAFT_SURFACE),
                DraftRecordCreate {
                    workspace_id: &workspace.id,
                    notebook_id: None,
                    body_markdown: "Same text",
                    attachments: &[],
                    proposed_mutation_id: &new_id(),
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        let quick_retry = store
            .create_record_from_surface_draft_attempt(
                Some(QUICK_CAPTURE_DRAFT_SURFACE),
                DraftRecordCreate {
                    workspace_id: &workspace.id,
                    notebook_id: None,
                    body_markdown: "Same text",
                    attachments: &[],
                    proposed_mutation_id: &new_id(),
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Same text", &[])
            .unwrap();
        let main = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Same text",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        assert_eq!(quick_retry.id, quick.id);
        assert_ne!(main.id, quick.id);
        assert_eq!(
            store
                .list_records(&workspace.id, None, false)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn quick_capture_requires_a_connected_workspace() {
        let store = store();
        let workspace = store
            .create_workspace("Disconnected capture", "/tmp/wakegpt-disconnected-capture")
            .unwrap();
        assert_eq!(
            store.get_connected_workspace(&workspace.id).unwrap().id,
            workspace.id
        );
        store.disconnect_workspace(&workspace.id).unwrap();
        assert!(matches!(
            store.get_connected_workspace(&workspace.id),
            Err(StoreError::NotFound("connected workspace"))
        ));
    }

    fn table_rows(connection: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let column_count = statement.column_count();
        statement
            .query_map([], |row| {
                (0..column_count)
                    .map(|column| row.get(column))
                    .collect::<Result<Vec<rusqlite::types::Value>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn composer_receipt_latest_index_columns(connection: &Connection) -> Vec<(String, bool)> {
        let mut statement = connection
            .prepare(
                "SELECT name, desc
                 FROM pragma_index_xinfo('composer_receipts_by_host_record_latest')
                 WHERE key = 1
                 ORDER BY seqno",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn settle_record_file_operation(store: &Store, record: &Record) {
        let connection = store.connection().unwrap();
        connection
            .execute(
                "UPDATE recovery_operations
                 SET phase = 'completed', completed_at_ms = ?2, updated_at_ms = ?2
                 WHERE record_id = ?1 AND phase NOT IN ('completed', 'superseded')",
                params![record.id, now_ms()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE records
                 SET sync_state = 'synced', applied_revision = revision
                 WHERE id = ?1",
                [&record.id],
            )
            .unwrap();
    }

    fn recovery_step(
        step_index: usize,
        notebook: &Notebook,
        role: &'static str,
    ) -> RecoveryFileStepInput {
        RecoveryFileStepInput {
            step_index,
            notebook_id: notebook.id.clone(),
            role,
            effect: "upsert",
            target_workspace_relative_path: notebook.relative_path.clone(),
            staged_sibling_name: format!(".{step_index}.stage"),
            backup_app_data_relative_path: format!("operation/{step_index}.before"),
            expected_target_id: notebook.target_id.clone(),
            before_file_sha256: "a".repeat(64),
            after_file_sha256: "b".repeat(64),
            before_managed_sha256: Some("c".repeat(64)),
            after_managed_sha256: "d".repeat(64),
            before_modified_ns: 42,
            target_existed: true,
        }
    }

    #[test]
    fn migration_installs_expected_schema() {
        let store = store();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let connection = store.connection().unwrap();
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                WHERE type = 'table' AND name IN (
                    'notebook_file_receipts', 'recovery_operations', 'recovery_file_steps',
                    'mutation_receipts', 'attachment_file_operations', 'record_pins',
                    'app_ui_state', 'workspace_ui_state', 'draft_attachments',
                    'notebook_tab_state', 'default_identity_slot', 'draft_create_attempts',
                    'notebook_conflicts', 'product_settings', 'workspace_open_preferences'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 15);
        let connection_state_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('workspaces')
                 WHERE name = 'is_connected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(connection_state_columns, 1);
        assert_eq!(
            composer_receipt_latest_index_columns(&connection),
            vec![
                ("host_kind".to_owned(), false),
                ("record_id".to_owned(), false),
                ("created_at_ms".to_owned(), true),
                ("id".to_owned(), true),
            ]
        );
    }

    #[test]
    fn local_data_reset_inventory_counts_primary_user_state() {
        let store = store();
        let workspace = store.create_workspace("Reset inventory", "/tmp").unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Product notes",
                "Product.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        store
            .create_record(&workspace.id, Some(&notebook.id), "Synthetic record")
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Synthetic draft", &[])
            .unwrap();

        let inventory = store.local_data_reset_inventory().unwrap();
        assert_eq!(inventory.workspace_count, 1);
        assert_eq!(inventory.notebook_count, 1);
        assert_eq!(inventory.record_count, 1);
        assert_eq!(inventory.attachment_count, 0);
        assert_eq!(inventory.draft_count, 1);
        assert_eq!(inventory.draft_attachment_count, 0);
        assert_eq!(inventory.composer_receipt_count, 0);
    }

    #[test]
    fn default_identity_slot_can_be_configured_updated_and_unbound() {
        let store = store();
        assert_eq!(store.default_identity_slot().unwrap(), None);

        let configured = store
            .configure_default_identity_slot("  官方工作身份  ", true)
            .unwrap();
        assert_eq!(configured.alias, "官方工作身份");
        assert!(configured.locked);

        let updated = store
            .configure_default_identity_slot("官方工作身份", false)
            .unwrap();
        assert!(!updated.locked);
        assert_eq!(updated.created_at_ms, configured.created_at_ms);
        assert!(updated.updated_at_ms >= configured.updated_at_ms);

        assert!(store.unbind_default_identity_slot().unwrap());
        assert_eq!(store.default_identity_slot().unwrap(), None);
        assert!(!store.unbind_default_identity_slot().unwrap());
    }

    #[test]
    fn schema_eleven_adds_default_identity_slot_without_rewriting_existing_data() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f1212-1212-7121-8121-121212121212',
                    'V11', '/tmp/wakegpt-v11', 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute_batch("DROP TABLE default_identity_slot; PRAGMA user_version = 11;")
            .unwrap();

        let store = Store::from_connection(connection).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.list_workspaces().unwrap().len(), 1);
        assert_eq!(store.default_identity_slot().unwrap(), None);
    }

    #[test]
    fn schema_twelve_adds_workspace_connection_state_without_rewriting_data() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f1313-1313-7131-8131-131313131313',
                    'V12', '/tmp/wakegpt-v12', 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO default_identity_slot (
                    singleton_id, alias, locked, created_at_ms, updated_at_ms
                 ) VALUES (1, 'V12 identity', 1, 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE workspaces DROP COLUMN is_connected; PRAGMA user_version = 12;",
            )
            .unwrap();

        let store = Store::from_connection(connection).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.list_workspaces().unwrap().len(), 1);
        assert_eq!(
            store.default_identity_slot().unwrap().unwrap().alias,
            "V12 identity"
        );
    }

    #[test]
    fn schema_thirteen_adds_durable_draft_attempts_without_rewriting_data() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f1414-1414-7141-8141-141414141414',
                    'V13', '/tmp/wakegpt-v13', 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO drafts (workspace_id, tab_key, body_markdown, updated_at_ms)
                 VALUES (
                    '018f1414-1414-7141-8141-141414141414',
                    'inbox', 'Retained V13 draft', 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute_batch("DROP TABLE draft_create_attempts; PRAGMA user_version = 13;")
            .unwrap();

        let store = Store::from_connection(connection).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store
                .load_draft("018f1414-1414-7141-8141-141414141414", None)
                .unwrap()
                .body_markdown,
            "Retained V13 draft"
        );
        let first = store
            .create_record_from_draft_attempt(
                "018f1414-1414-7141-8141-141414141414",
                None,
                "Retained V13 draft",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let retry = store
            .create_record_from_draft_attempt(
                "018f1414-1414-7141-8141-141414141414",
                None,
                "Retained V13 draft",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_eq!(retry.id, first.id);
    }

    #[test]
    fn schema_fourteen_adds_notebook_conflicts_without_rewriting_existing_data() {
        let store = store();
        let workspace = store
            .create_workspace("V14 workspace", "/tmp/wakegpt-v14")
            .unwrap();
        store
            .create_notebook(
                &workspace.id,
                "V14 notebook",
                "retained-v14.md",
                NumberingStyle::Task,
            )
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Retained V14 draft", &[])
            .unwrap();
        store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Retained V14 draft",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        let preserved_tables = [
            "workspaces",
            "notebooks",
            "records",
            "drafts",
            "draft_create_attempts",
        ];
        let connection = store.connection().unwrap();
        let before = preserved_tables
            .iter()
            .map(|table| (*table, table_rows(&connection, table)))
            .collect::<Vec<_>>();
        connection
            .execute_batch("DROP TABLE notebook_conflicts; PRAGMA user_version = 14;")
            .unwrap();

        migrate(&connection).unwrap();

        let schema_version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(schema_version, SCHEMA_VERSION);
        let conflict_table_exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'notebook_conflicts'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(conflict_table_exists);
        let conflicts: i64 = connection
            .query_row("SELECT COUNT(*) FROM notebook_conflicts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(conflicts, 0);

        for (table, rows) in before {
            assert_eq!(
                table_rows(&connection, table),
                rows,
                "schema 14→15 migration rewrote {table}"
            );
        }
    }

    #[test]
    fn schema_fifteen_adds_ordered_attachment_membership_and_preserves_operations() {
        let store = store();
        let workspace = store
            .create_workspace("V15 workspace", "/tmp/wakegpt-v15")
            .unwrap();
        let attachment = |character: char| {
            let digest = character.to_string().repeat(64);
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: digest,
                byte_size: 8,
            }
        };
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "V15 attachments",
                &[attachment('a'), attachment('b')],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &record.id,
                record.revision,
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        let connection = store.connection().unwrap();
        connection
            .execute_batch(
                "DROP INDEX composer_receipts_by_host_record_latest;
                 DROP INDEX pending_attachment_file_operations;
                 ALTER TABLE attachment_file_operations RENAME TO attachment_file_operations_v16;
                 CREATE TABLE attachment_file_operations (
                    attachment_id TEXT PRIMARY KEY REFERENCES attachments(id) ON DELETE CASCADE,
                    action TEXT NOT NULL CHECK (action IN ('trash', 'restore')),
                    phase TEXT NOT NULL CHECK (phase IN (
                        'queued', 'prepared', 'trashed', 'ready', 'preserved_shared',
                        'preserved_changed', 'missing', 'needs_recovery'
                    )),
                    record_revision INTEGER NOT NULL CHECK (
                        record_revision > 0 AND record_revision <= 9007199254740991
                    ),
                    trash_path TEXT,
                    backup_app_data_relative_path TEXT,
                    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
                    last_error_code TEXT,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    completed_at_ms INTEGER,
                    CHECK (
                        completed_at_ms IS NULL OR phase IN (
                            'trashed', 'ready', 'preserved_shared', 'preserved_changed', 'missing'
                        )
                    )
                 ) STRICT;
                 INSERT INTO attachment_file_operations
                 SELECT * FROM attachment_file_operations_v16;
                 DROP TABLE attachment_file_operations_v16;
                 CREATE INDEX pending_attachment_file_operations
                    ON attachment_file_operations(phase, updated_at_ms)
                    WHERE phase IN ('queued', 'prepared', 'needs_recovery');
                 ALTER TABLE attachments DROP COLUMN ordinal;
                 ALTER TABLE attachments DROP COLUMN membership_state;
                 PRAGMA user_version = 15;",
            )
            .unwrap();

        migrate(&connection).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let attachments = connection
            .prepare(
                "SELECT content_sha256, ordinal, membership_state
                 FROM attachments WHERE record_id = ?1 ORDER BY ordinal",
            )
            .unwrap()
            .query_map([&record.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            attachments,
            vec![
                ("a".repeat(64), 0, "active".to_owned()),
                ("b".repeat(64), 1, "active".to_owned()),
            ]
        );
        let operation_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM attachment_file_operations
                 WHERE action = 'trash' AND record_revision = ?1",
                [revision_to_db(trashed.revision).unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(operation_count, 2);
        connection
            .execute(
                "UPDATE attachment_file_operations SET action = 'detach'
                 WHERE attachment_id = (
                    SELECT attachment_id FROM attachment_file_operations LIMIT 1
                 )",
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_seventeen_adds_composer_latest_index_without_rewriting_data() {
        let store = store();
        let workspace = store
            .create_workspace("V16 workspace", "/tmp/wakegpt-v16")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Retained V16 record")
            .unwrap();
        let host_kind = format!("chatgpt:{}", "a".repeat(64));
        store
            .record_composer_receipt(
                &record.id,
                &host_kind,
                "partial",
                r#"{"recordRevision":1,"textInserted":true}"#,
            )
            .unwrap();
        store
            .record_composer_receipt(
                &record.id,
                &host_kind,
                "uncertain",
                r#"{"recordRevision":1,"textUncertain":true}"#,
            )
            .unwrap();

        let connection = store.connection().unwrap();
        connection
            .execute_batch("DROP INDEX composer_receipts_by_host_record_latest;")
            .unwrap();
        connection
            .pragma_update(None, "user_version", ATTACHMENT_SET_REVISION_SCHEMA_VERSION)
            .unwrap();
        let table_names = connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let before = table_names
            .iter()
            .map(|table| (table.clone(), table_rows(&connection, table)))
            .collect::<Vec<_>>();
        let expected_latest: (String, String, String, i64) = connection
            .query_row(
                "SELECT id, state, detail_json, created_at_ms
                 FROM composer_receipts
                 WHERE record_id = ?1 AND host_kind = ?2
                 ORDER BY created_at_ms DESC, id DESC
                 LIMIT 1",
                params![record.id, host_kind],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(
            composer_receipt_latest_index_columns(&connection),
            vec![
                ("host_kind".to_owned(), false),
                ("record_id".to_owned(), false),
                ("created_at_ms".to_owned(), true),
                ("id".to_owned(), true),
            ]
        );
        for (table, rows) in before {
            assert_eq!(
                table_rows(&connection, &table),
                rows,
                "schema 16→17 migration rewrote {table}"
            );
        }
        drop(connection);

        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                receipt.id,
                receipt.state,
                receipt.detail_json,
                receipt.created_at_ms
            ),
            expected_latest
        );
    }

    #[test]
    fn schema_eighteen_adds_bounded_update_settings_on_the_path_to_latest() {
        let store = store();
        let workspace = store
            .create_workspace("V17 workspace", "/tmp/wakegpt-v17")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Retained V17 record")
            .unwrap();
        let connection = store.connection().unwrap();
        let workspace_rows = table_rows(&connection, "workspaces");
        let record_rows = table_rows(&connection, "records");
        connection
            .execute_batch("DROP TABLE update_settings; PRAGMA user_version = 17;")
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(table_rows(&connection, "workspaces"), workspace_rows);
        assert_eq!(table_rows(&connection, "records"), record_rows);
        drop(connection);

        assert_eq!(store.get_workspace(&workspace.id).unwrap().id, workspace.id);
        assert_eq!(
            store.get_record(&workspace.id, &record.id).unwrap().id,
            record.id
        );

        assert_eq!(
            store.update_settings().unwrap(),
            UpdateSettings {
                external_network_enabled: true,
                automatic_checks_enabled: true,
                automatic_downloads_enabled: false,
                check_interval_hours: 24,
                skipped_version: None,
                last_checked_at_ms: None,
                last_error_code: None,
                updated_at_ms: 0,
            }
        );
    }

    #[test]
    fn schema_nineteen_adds_network_controls_without_rewriting_schema_eighteen_data() {
        let store = store();
        let workspace = store
            .create_workspace("V18 workspace", "/tmp/wakegpt-v18")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Retained V18 record")
            .unwrap();
        let connection = store.connection().unwrap();
        let workspace_rows = table_rows(&connection, "workspaces");
        let record_rows = table_rows(&connection, "records");
        connection
            .execute_batch(
                "DROP TABLE update_settings;
                 CREATE TABLE update_settings (
                    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
                    automatic_checks_enabled INTEGER NOT NULL DEFAULT 1
                        CHECK (automatic_checks_enabled IN (0, 1)),
                    check_interval_hours INTEGER NOT NULL DEFAULT 24
                        CHECK (check_interval_hours IN (6, 12, 24, 48)),
                    skipped_version TEXT CHECK (
                        skipped_version IS NULL
                        OR (length(skipped_version) BETWEEN 1 AND 64)
                    ),
                    last_checked_at_ms INTEGER CHECK (
                        last_checked_at_ms IS NULL OR last_checked_at_ms >= 0
                    ),
                    last_error_code TEXT CHECK (
                        last_error_code IS NULL
                        OR (length(last_error_code) BETWEEN 1 AND 64)
                    ),
                    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
                 ) STRICT;
                 INSERT INTO update_settings VALUES (
                    1, 0, 48, '1.2.3', 123456789, 'retained_error', 987654321
                 );
                 PRAGMA user_version = 18;",
            )
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(table_rows(&connection, "workspaces"), workspace_rows);
        assert_eq!(table_rows(&connection, "records"), record_rows);
        drop(connection);

        assert_eq!(
            store.update_settings().unwrap(),
            UpdateSettings {
                external_network_enabled: true,
                automatic_checks_enabled: false,
                automatic_downloads_enabled: false,
                check_interval_hours: 48,
                skipped_version: Some("1.2.3".to_owned()),
                last_checked_at_ms: Some(123456789),
                last_error_code: Some("retained_error".to_owned()),
                updated_at_ms: 987654321,
            }
        );
        let changed = store.set_update_settings(false, false, true, 48).unwrap();
        assert!(!changed.external_network_enabled);
        assert!(!changed.automatic_checks_enabled);
        assert!(changed.automatic_downloads_enabled);
        assert_eq!(changed.check_interval_hours, 48);
        let skipped = store
            .set_skipped_update_version(Some("1.2.3-beta.1"))
            .unwrap();
        assert_eq!(skipped.skipped_version.as_deref(), Some("1.2.3-beta.1"));
        let checked = store
            .record_update_check(Some("update_network_failed"))
            .unwrap();
        assert!(checked.last_checked_at_ms.is_some());
        assert_eq!(
            checked.last_error_code.as_deref(),
            Some("update_network_failed")
        );
        assert!(store.set_update_settings(true, true, false, 1).is_err());
        assert!(store.set_skipped_update_version(Some("../1.2.3")).is_err());
        assert!(store.record_update_check(Some("Invalid-Code")).is_err());
        assert_eq!(store.get_workspace(&workspace.id).unwrap().id, workspace.id);
        assert_eq!(
            store.get_record(&workspace.id, &record.id).unwrap().id,
            record.id
        );
    }

    #[test]
    fn schema_twenty_preserves_existing_data_and_defaults_migrations_to_permanent_retention() {
        let store = store();
        let workspace = store
            .create_workspace("V19 workspace", "/tmp/wakegpt-v19")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Retained V19 record")
            .unwrap();
        let connection = store.connection().unwrap();
        let workspace_rows = table_rows(&connection, "workspaces");
        let record_rows = table_rows(&connection, "records");
        connection
            .execute_batch("DROP TABLE product_settings; PRAGMA user_version = 19;")
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(table_rows(&connection, "workspaces"), workspace_rows);
        assert_eq!(table_rows(&connection, "records"), record_rows);
        drop(connection);

        assert_eq!(
            store.product_settings().unwrap(),
            ProductSettings {
                record_trash_retention_days: None,
                notebook_scan_ignore_directories: DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                protected_notebook_scan_ignore_directories:
                    DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                codex_integration_paused: false,
                updated_at_ms: 0,
            }
        );
        assert_eq!(store.get_workspace(&workspace.id).unwrap().id, workspace.id);
        assert_eq!(
            store.get_record(&workspace.id, &record.id).unwrap().id,
            record.id
        );
    }

    #[test]
    fn fresh_schema_twenty_uses_thirty_days_and_persists_product_settings() {
        let store = store();
        let initial = store.product_settings().unwrap();
        assert_eq!(
            initial.record_trash_retention_days,
            Some(DEFAULT_RECORD_TRASH_RETENTION_DAYS)
        );
        assert_eq!(
            initial.notebook_scan_ignore_directories,
            DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
        );
        assert!(!initial.codex_integration_paused);

        let mut configured = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        configured.extend(["generated/cache".to_owned(), "vendor".to_owned()]);
        let changed = store.set_product_settings(Some(90), &configured).unwrap();
        assert_eq!(changed.record_trash_retention_days, Some(90));
        assert_eq!(changed.notebook_scan_ignore_directories, configured);
        assert_eq!(
            changed.protected_notebook_scan_ignore_directories,
            DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
        );
        assert!(
            store
                .set_codex_integration_paused(true)
                .unwrap()
                .codex_integration_paused
        );
        assert!(store.set_product_settings(Some(0), &configured).is_err());
        assert!(store
            .set_product_settings(Some(30), &["../private".to_owned()])
            .is_err());
        assert!(store
            .set_product_settings(Some(30), &["vendor".to_owned()])
            .is_err());
    }

    #[test]
    fn schema_twenty_one_adds_workspace_open_preferences_without_changing_selection() {
        let store = store();
        let workspace = store
            .create_workspace("V20 workspace", "/tmp/wakegpt-v20")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Retained V20 notebook",
                "retained-v20.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        store
            .set_active_selection(&workspace.id, Some(&notebook.id))
            .unwrap();
        let connection = store.connection().unwrap();
        let workspace_rows = table_rows(&connection, "workspaces");
        let notebook_rows = table_rows(&connection, "notebooks");
        let ui_rows = table_rows(&connection, "workspace_ui_state");
        connection
            .execute_batch("DROP TABLE workspace_open_preferences; PRAGMA user_version = 20;")
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(table_rows(&connection, "workspaces"), workspace_rows);
        assert_eq!(table_rows(&connection, "notebooks"), notebook_rows);
        assert_eq!(table_rows(&connection, "workspace_ui_state"), ui_rows);
        drop(connection);
        assert_eq!(
            store.workspace_open_preference(&workspace.id).unwrap(),
            WorkspaceOpenPreference {
                workspace_id: workspace.id.clone(),
                default_notebook_id: None,
                use_last_selection: true,
                updated_at_ms: 0,
            }
        );
        assert_eq!(
            store
                .activate_workspace(&workspace.id)
                .unwrap()
                .selected_notebook_id,
            Some(notebook.id)
        );
    }

    #[test]
    fn notebook_display_name_and_workspace_open_preference_are_independent() {
        let store = store();
        let workspace = store
            .create_workspace("Notebook settings", "/tmp/wakegpt-notebook-settings")
            .unwrap();
        let other_workspace = store
            .create_workspace("Other settings", "/tmp/wakegpt-other-settings")
            .unwrap();
        let first = store
            .create_notebook(&workspace.id, "First", "first.md", NumberingStyle::Numeric)
            .unwrap();
        let second = store
            .create_notebook(
                &workspace.id,
                "Second",
                "second.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let foreign = store
            .create_notebook(
                &other_workspace.id,
                "Foreign",
                "foreign.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        store
            .set_active_selection(&workspace.id, Some(&first.id))
            .unwrap();

        let renamed = store
            .rename_notebook(&workspace.id, &second.id, "  产品记录  ")
            .unwrap();
        assert_eq!(renamed.display_name, "产品记录");
        assert_eq!(renamed.relative_path, "second.md");
        assert_eq!(renamed.target_id, second.target_id);
        assert!(store
            .rename_notebook(&workspace.id, &second.id, "First")
            .is_err());
        assert!(store
            .rename_notebook(&workspace.id, &foreign.id, "Wrong")
            .is_err());

        let default = store
            .set_workspace_open_preference(&workspace.id, false, Some(&second.id))
            .unwrap();
        assert_eq!(
            default.default_notebook_id.as_deref(),
            Some(second.id.as_str())
        );
        assert!(!default.use_last_selection);
        assert_eq!(
            store
                .activate_workspace(&workspace.id)
                .unwrap()
                .selected_notebook_id,
            Some(second.id.clone())
        );
        assert!(store
            .set_workspace_open_preference(&workspace.id, false, Some(&foreign.id))
            .is_err());
        assert!(store
            .set_workspace_open_preference(&workspace.id, true, Some(&first.id))
            .is_err());

        let inbox = store
            .set_workspace_open_preference(&workspace.id, false, None)
            .unwrap();
        assert_eq!(inbox.default_notebook_id, None);
        assert!(!inbox.use_last_selection);
        assert_eq!(
            store
                .activate_workspace(&workspace.id)
                .unwrap()
                .selected_notebook_id,
            None
        );

        store
            .set_workspace_open_preference(&workspace.id, false, Some(&second.id))
            .unwrap();
        store
            .set_notebook_pinned(&workspace.id, &second.id, false)
            .unwrap();
        assert_eq!(
            store.workspace_open_preference(&workspace.id).unwrap(),
            WorkspaceOpenPreference {
                workspace_id: workspace.id.clone(),
                default_notebook_id: None,
                use_last_selection: false,
                updated_at_ms: store
                    .workspace_open_preference(&workspace.id)
                    .unwrap()
                    .updated_at_ms,
            }
        );

        store
            .set_active_selection(&workspace.id, Some(&first.id))
            .unwrap();
        store
            .set_workspace_open_preference(&workspace.id, true, None)
            .unwrap();
        assert_eq!(
            store
                .activate_workspace(&workspace.id)
                .unwrap()
                .selected_notebook_id,
            Some(first.id)
        );
    }

    #[test]
    fn trash_cleanup_requires_a_fresh_preview_and_excludes_unsafe_records() {
        let store = store();
        let workspace = store
            .create_workspace("Cleanup", "/tmp/wakegpt-cleanup")
            .unwrap();
        let configured = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        store.set_product_settings(Some(30), &configured).unwrap();
        let safe = store
            .create_record(&workspace.id, None, "Expired and safe")
            .unwrap();
        let safe = store
            .trash_record(&workspace.id, &safe.id, safe.revision)
            .unwrap();
        let blocked = store
            .create_record(&workspace.id, None, "Expired but uncertain")
            .unwrap();
        let blocked = store
            .trash_record(&workspace.id, &blocked.id, blocked.revision)
            .unwrap();
        store
            .record_composer_receipt(
                &blocked.id,
                &format!("chatgpt:{}", "a".repeat(64)),
                "uncertain",
                "{}",
            )
            .unwrap();
        let old = 1_000_000_i64;
        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "UPDATE records SET trashed_at_ms = ?1 WHERE id IN (?2, ?3)",
                    params![old, safe.id, blocked.id],
                )
                .unwrap();
        }

        let preview = store
            .preview_record_trash_cleanup_at(old + 31 * 86_400_000)
            .unwrap();
        assert_eq!(preview.eligible.len(), 1);
        assert_eq!(preview.eligible[0].record_id, safe.id);
        assert_eq!(preview.blocked_count, 1);
        assert!(store.purge_record_trash(&"0".repeat(64)).is_err());

        let current_preview = store.preview_record_trash_cleanup().unwrap();
        let result = store
            .purge_record_trash(&current_preview.preview_token)
            .unwrap();
        assert_eq!(result.deleted_record_count, 1);
        assert_eq!(result.deleted_attachment_count, 0);
        assert!(matches!(
            store.get_record(&workspace.id, &safe.id),
            Err(StoreError::NotFound("record"))
        ));
        assert_eq!(
            store.get_record(&workspace.id, &blocked.id).unwrap().state,
            RecordState::Trashed
        );
        assert!(store
            .purge_record_trash(&current_preview.preview_token)
            .is_err());
    }

    #[test]
    fn trash_cleanup_excludes_notebook_conflicts_and_pending_file_work() {
        let store = store();
        let workspace = store
            .create_workspace(
                "Cleanup notebook blockers",
                "/tmp/wakegpt-cleanup-notebooks",
            )
            .unwrap();
        let numbering_notebook = store
            .create_notebook(
                &workspace.id,
                "Numbering pending",
                "numbering.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let attachment_notebook = store
            .create_notebook(
                &workspace.id,
                "Attachment pending",
                "attachments.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let conflict_notebook = store
            .create_notebook(
                &workspace.id,
                "Conflict pending",
                "conflict.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let mut records = Vec::new();
        for body in ["Safe", "Numbering", "Attachment", "Conflict"] {
            let record = store.create_record(&workspace.id, None, body).unwrap();
            records.push(
                store
                    .trash_record(&workspace.id, &record.id, record.revision)
                    .unwrap(),
            );
        }
        let old = 1_000_000_i64;
        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "UPDATE records SET trashed_at_ms = ?1 WHERE workspace_id = ?2",
                    params![old, workspace.id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE records SET notebook_id = ?1 WHERE id = ?2",
                    params![numbering_notebook.id, records[1].id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE records SET notebook_id = ?1 WHERE id = ?2",
                    params![attachment_notebook.id, records[2].id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE records SET notebook_id = ?1 WHERE id = ?2",
                    params![conflict_notebook.id, records[3].id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE notebooks SET numbering_sync_pending = 1 WHERE id = ?1",
                    [&numbering_notebook.id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE notebooks SET attachment_directory_sync_pending = 1 WHERE id = ?1",
                    [&attachment_notebook.id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO notebook_conflicts (
                        notebook_id, workspace_id, target_id, receipt_generation,
                        expected_managed_sha256, observed_file_sha256,
                        observed_managed_sha256, reason_code, created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, 1, ?4, ?5, NULL, 'external_change', ?6, ?6)",
                    params![
                        conflict_notebook.id,
                        workspace.id,
                        conflict_notebook.target_id,
                        "a".repeat(64),
                        "b".repeat(64),
                        old,
                    ],
                )
                .unwrap();
        }

        let preview = store
            .preview_record_trash_cleanup_at(old + 31 * 86_400_000)
            .unwrap();
        assert_eq!(preview.eligible.len(), 1);
        assert_eq!(preview.eligible[0].record_id, records[0].id);
        assert_eq!(preview.blocked_count, 3);

        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "UPDATE notebooks
                     SET numbering_sync_pending = 0, attachment_directory_sync_pending = 0",
                    [],
                )
                .unwrap();
            connection
                .execute("DELETE FROM notebook_conflicts", [])
                .unwrap();
        }
        let unblocked = store
            .preview_record_trash_cleanup_at(old + 31 * 86_400_000)
            .unwrap();
        assert_eq!(unblocked.eligible.len(), 4);
        assert_eq!(unblocked.blocked_count, 0);
    }

    #[test]
    fn trash_cleanup_preview_token_binds_the_retention_policy() {
        let store = store();
        let workspace = store
            .create_workspace("Cleanup policy", "/tmp/wakegpt-cleanup-policy")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Old under both policies")
            .unwrap();
        let record = store
            .trash_record(&workspace.id, &record.id, record.revision)
            .unwrap();
        let old = now_ms().saturating_sub(120 * 86_400_000);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE records SET trashed_at_ms = ?1 WHERE id = ?2",
                params![old, record.id],
            )
            .unwrap();

        let preview = store.preview_record_trash_cleanup().unwrap();
        assert_eq!(preview.retention_days, Some(30));
        assert_eq!(preview.eligible.len(), 1);
        let configured = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        store.set_product_settings(Some(90), &configured).unwrap();

        assert!(store.purge_record_trash(&preview.preview_token).is_err());
        assert_eq!(
            store.get_record(&workspace.id, &record.id).unwrap().state,
            RecordState::Trashed
        );
        let current = store.preview_record_trash_cleanup().unwrap();
        assert_eq!(current.retention_days, Some(90));
        assert_eq!(current.eligible.len(), 1);
        assert_eq!(
            store.purge_record_trash(&current.preview_token).unwrap(),
            RecordTrashCleanupResult {
                deleted_record_count: 1,
                deleted_attachment_count: 0,
            }
        );
    }

    #[test]
    fn workspace_disconnection_is_reversible_and_preserves_local_state() {
        let store = store();
        let workspace = store
            .create_workspace("Disconnect", "/tmp/wakegpt-disconnect")
            .unwrap();
        let fallback = store
            .create_workspace("Fallback", "/tmp/wakegpt-fallback")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Retained notebook",
                "retained.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Retained inbox record")
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Retained draft", &[])
            .unwrap();
        store.set_active_selection(&workspace.id, None).unwrap();

        let preferences = store.disconnect_workspace(&workspace.id).unwrap();
        assert_eq!(
            preferences.active_workspace_id.as_deref(),
            Some(fallback.id.as_str())
        );
        assert_eq!(store.list_workspaces().unwrap(), vec![fallback.clone()]);
        assert_eq!(store.get_workspace(&workspace.id).unwrap().id, workspace.id);
        assert_eq!(
            store
                .get_record(&workspace.id, &record.id)
                .unwrap()
                .body_markdown,
            "Retained inbox record"
        );
        assert_eq!(
            store.load_draft(&workspace.id, None).unwrap().body_markdown,
            "Retained draft"
        );
        assert_eq!(
            store.get_notebook(&workspace.id, &notebook.id).unwrap().id,
            notebook.id
        );

        let reconnected = store
            .create_workspace("Reconnected", "/tmp/wakegpt-disconnect")
            .unwrap();
        assert_eq!(reconnected.id, workspace.id);
        assert_eq!(reconnected.created_at_ms, workspace.created_at_ms);
        assert_eq!(reconnected.display_name, "Reconnected");
        assert_eq!(store.list_workspaces().unwrap().len(), 2);
        assert!(store
            .create_workspace("Duplicate", "/tmp/wakegpt-disconnect")
            .is_err());
    }

    #[test]
    fn create_mutation_replay_does_not_duplicate_a_record() {
        let store = store();
        let workspace = store
            .create_workspace("Mutation create", "/tmp/wakegpt-mutation-create")
            .unwrap();
        let mutation_id = new_id();
        let first = store
            .create_record_idempotent(
                &workspace.id,
                None,
                "Only once",
                &mutation_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let replay = store
            .create_record_idempotent(
                &workspace.id,
                None,
                "Only once",
                &mutation_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        assert_eq!(replay.id, first.id);
        assert_eq!(
            store
                .list_records(&workspace.id, None, false)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .create_record_idempotent(
                &workspace.id,
                None,
                "Different request",
                &mutation_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap_err()
            .to_string()
            .contains("different request"));
    }

    #[test]
    fn lost_create_response_reuses_the_draft_attempt_across_new_request_ids() {
        let store = store();
        let workspace = store
            .create_workspace("Lost response", "/tmp/wakegpt-lost-create-response")
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Create once", &[])
            .unwrap();

        let first = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Create once",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        store
            .save_draft(&workspace.id, None, "Create once", &[])
            .unwrap();
        let retry_after_lost_response = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Create once",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        assert_eq!(retry_after_lost_response.id, first.id);
        assert_eq!(
            store
                .list_records(&workspace.id, None, false)
                .unwrap()
                .len(),
            1
        );

        store
            .save_draft(&workspace.id, None, "Changed draft", &[])
            .unwrap();
        let changed = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Changed draft",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_ne!(changed.id, first.id);

        store.save_draft(&workspace.id, None, "", &[]).unwrap();
        store
            .save_draft(&workspace.id, None, "Create once", &[])
            .unwrap();
        let intentional_repeat = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Create once",
                &[],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_ne!(intentional_repeat.id, first.id);
        assert_eq!(
            store
                .list_records(&workspace.id, None, false)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn draft_attachment_identity_preserves_or_rotates_the_create_attempt() {
        let store = store();
        let workspace = store
            .create_workspace("Attachment attempt", "/tmp/wakegpt-attachment-attempt")
            .unwrap();
        let first_digest = "a".repeat(64);
        let first_draft = DraftAttachment {
            token: new_id(),
            media_type: "image/png".to_owned(),
            byte_size: 8,
            content_sha256: first_digest.clone(),
            display_name: "first.png".to_owned(),
        };
        let first_attachment = NewAttachment {
            media_type: "image/png".to_owned(),
            managed_relative_path: managed_attachment_relative_path(
                INBOX_ATTACHMENT_DIRECTORY,
                &first_digest,
                "image/png",
            )
            .unwrap(),
            previous_managed_relative_path: None,
            content_sha256: first_digest.clone(),
            byte_size: 8,
        };
        store
            .save_draft(
                &workspace.id,
                None,
                "Image",
                std::slice::from_ref(&first_draft),
            )
            .unwrap();
        let first = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Image",
                std::slice::from_ref(&first_attachment),
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();

        let same_content_new_token = DraftAttachment {
            token: new_id(),
            display_name: "renamed.png".to_owned(),
            ..first_draft
        };
        store
            .save_draft(&workspace.id, None, "Image", &[same_content_new_token])
            .unwrap();
        let retry = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Image",
                std::slice::from_ref(&first_attachment),
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_eq!(retry.id, first.id);

        let second_digest = "b".repeat(64);
        let second_draft = DraftAttachment {
            token: new_id(),
            media_type: "image/png".to_owned(),
            byte_size: 8,
            content_sha256: second_digest.clone(),
            display_name: "second.png".to_owned(),
        };
        let second_attachment = NewAttachment {
            media_type: "image/png".to_owned(),
            managed_relative_path: managed_attachment_relative_path(
                INBOX_ATTACHMENT_DIRECTORY,
                &second_digest,
                "image/png",
            )
            .unwrap(),
            previous_managed_relative_path: None,
            content_sha256: second_digest,
            byte_size: 8,
        };
        store
            .save_draft(&workspace.id, None, "Image", &[second_draft])
            .unwrap();
        let changed = store
            .create_record_from_draft_attempt(
                &workspace.id,
                None,
                "Image",
                &[second_attachment],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_ne!(changed.id, first.id);
    }

    #[test]
    fn edit_and_state_mutation_replays_do_not_increment_revision_twice() {
        let store = store();
        let workspace = store
            .create_workspace("Mutation edit", "/tmp/wakegpt-mutation-edit")
            .unwrap();
        let original = store
            .create_record(&workspace.id, None, "Original")
            .unwrap();
        let edit_id = new_id();
        let edited = store
            .update_record_idempotent(
                &workspace.id,
                &original.id,
                original.revision,
                "Edited",
                &edit_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let edit_replay = store
            .update_record_idempotent(
                &workspace.id,
                &original.id,
                original.revision,
                "Edited",
                &edit_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_eq!(edit_replay.revision, edited.revision);

        let trash_id = new_id();
        let trashed = store
            .trash_record_idempotent(
                &workspace.id,
                &edited.id,
                edited.revision,
                &trash_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let trash_replay = store
            .trash_record_idempotent(
                &workspace.id,
                &edited.id,
                edited.revision,
                &trash_id,
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_eq!(trash_replay.revision, trashed.revision);
        assert_eq!(trash_replay.state, RecordState::Trashed);
    }

    #[test]
    fn attachment_set_revision_updates_body_order_and_membership_once() {
        let store = store();
        let workspace = store
            .create_workspace("Attachment revision", "/tmp/wakegpt-attachment-revision")
            .unwrap();
        let attachment = |digest_character: char| {
            let digest = digest_character.to_string().repeat(64);
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: digest,
                byte_size: 8,
            }
        };
        let original = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Original",
                &[attachment('a'), attachment('b')],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let retained_id = original.attachments[1].id.clone();
        let removed_id = original.attachments[0].id.clone();
        let mutation_id = new_id();
        let added = attachment('c');
        let revised = store
            .revise_record_attachment_set_idempotent(
                &workspace.id,
                &original.id,
                original.revision,
                RecordAttachmentSetRevision {
                    body_markdown: "Revised",
                    retained_attachment_ids: std::slice::from_ref(&retained_id),
                    new_attachments: std::slice::from_ref(&added),
                    mutation_id: &mutation_id,
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        assert_eq!(revised.revision, 2);
        assert_eq!(revised.applied_revision, 2);
        assert_eq!(revised.body_markdown, "Revised");
        assert_eq!(revised.attachments.len(), 2);
        assert_eq!(revised.attachments[0].id, retained_id);
        assert_eq!(revised.attachments[1].content_sha256, "c".repeat(64));

        let operation = store
            .attachment_file_operations_for_record_revision(&original.id, revised.revision)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(operation.attachment_id, removed_id);
        assert_eq!(operation.action, "detach");
        assert_eq!(operation.phase, "queued");

        let replay = store
            .revise_record_attachment_set_idempotent(
                &workspace.id,
                &original.id,
                original.revision,
                RecordAttachmentSetRevision {
                    body_markdown: "Revised",
                    retained_attachment_ids: std::slice::from_ref(&retained_id),
                    new_attachments: std::slice::from_ref(&added),
                    mutation_id: &mutation_id,
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .unwrap();
        assert_eq!(replay.revision, revised.revision);
        assert_eq!(replay.attachments, revised.attachments);

        let connection = store.connection().unwrap();
        let states = connection
            .prepare(
                "SELECT content_sha256, ordinal, membership_state
                 FROM attachments WHERE record_id = ?1 ORDER BY content_sha256",
            )
            .unwrap()
            .query_map([&original.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            states,
            vec![
                ("a".repeat(64), 0, "detaching".to_owned()),
                ("b".repeat(64), 0, "active".to_owned()),
                ("c".repeat(64), 1, "active".to_owned()),
            ]
        );
    }

    #[test]
    fn attachment_set_revision_rejects_an_empty_final_record() {
        let store = store();
        let workspace = store
            .create_workspace("Attachment empty", "/tmp/wakegpt-attachment-empty")
            .unwrap();
        let digest = "d".repeat(64);
        let original = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "",
                &[NewAttachment {
                    media_type: "image/png".to_owned(),
                    managed_relative_path: managed_attachment_relative_path(
                        INBOX_ATTACHMENT_DIRECTORY,
                        &digest,
                        "image/png",
                    )
                    .unwrap(),
                    previous_managed_relative_path: None,
                    content_sha256: digest,
                    byte_size: 8,
                }],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        assert!(store
            .revise_record_attachment_set_idempotent(
                &workspace.id,
                &original.id,
                original.revision,
                RecordAttachmentSetRevision {
                    body_markdown: "",
                    retained_attachment_ids: &[],
                    new_attachments: &[],
                    mutation_id: &new_id(),
                    mutation_schema_version: MUTATION_SCHEMA_VERSION,
                },
            )
            .is_err());
        assert_eq!(
            store.get_record(&workspace.id, &original.id).unwrap(),
            original
        );
    }

    #[test]
    fn mutation_schema_and_ids_are_validated() {
        let store = store();
        let workspace = store
            .create_workspace("Mutation validation", "/tmp/wakegpt-mutation-validation")
            .unwrap();
        assert!(store
            .create_record_idempotent(
                &workspace.id,
                None,
                "Invalid schema",
                &new_id(),
                MUTATION_SCHEMA_VERSION + 1,
            )
            .unwrap_err()
            .to_string()
            .contains("unsupported mutation schema"));
        assert!(store
            .create_record_idempotent(
                &workspace.id,
                None,
                "Invalid id",
                "not-a-uuid",
                MUTATION_SCHEMA_VERSION,
            )
            .is_err());
    }

    #[test]
    fn schema_two_adds_mutation_receipts_without_rewriting_existing_data() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute_batch("DROP TABLE mutation_receipts; PRAGMA user_version = 2;")
            .unwrap();

        let store = Store::from_connection(connection).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let connection = store.connection().unwrap();
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'mutation_receipts'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn schema_four_adds_record_pins_without_rewriting_records() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection.execute("DROP TABLE record_pins", []).unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('workspace-v4', 'V4', '/tmp/wakegpt-v4', 1, 1)",
                [],
            )
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let workspace_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
            .unwrap();
        let pins_table: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'record_pins'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(workspace_count, 1);
        assert!(pins_table);
    }

    #[test]
    fn schema_five_adds_ui_state_without_rewriting_workspaces() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute_batch(
                "DROP TABLE draft_attachments;
                 DROP TABLE workspace_ui_state;
                 DROP TABLE app_ui_state;
                 PRAGMA user_version = 5;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('workspace-v5', 'V5', '/tmp/wakegpt-v5', 1, 1)",
                [],
            )
            .unwrap();

        migrate(&connection).unwrap();

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let workspace_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
            .unwrap();
        let ui_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN (
                    'app_ui_state', 'workspace_ui_state', 'draft_attachments'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(workspace_count, 1);
        assert_eq!(ui_tables, 3);
    }

    #[test]
    fn schema_six_adds_editor_preferences_with_safe_defaults() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('workspace-v6', 'V6', '/tmp/wakegpt-v6', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "DROP TABLE app_ui_state;
                 CREATE TABLE app_ui_state (
                    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
                    active_workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
                    theme TEXT NOT NULL CHECK (theme IN ('system', 'light', 'dark')),
                    updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 INSERT INTO app_ui_state VALUES (1, 'workspace-v6', 'dark', 1);
                 PRAGMA user_version = 6;",
            )
            .unwrap();

        migrate(&connection).unwrap();

        let preferences = Store::from_connection(connection)
            .unwrap()
            .ui_preferences()
            .unwrap();
        assert_eq!(
            preferences.active_workspace_id.as_deref(),
            Some("workspace-v6")
        );
        assert_eq!(preferences.theme, ThemePreference::Dark);
        assert_eq!(preferences.submit_shortcut, SubmitShortcut::Enter);
        assert_eq!(preferences.markdown_layout, MarkdownLayout::Split);
    }

    #[test]
    fn schema_seven_adds_pinned_tab_state_without_rewriting_notebooks() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('018faaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa', 'V7', '/tmp/wakegpt-v7', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notebooks (
                    id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, target_state, last_error_code, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018fbbbb-bbbb-7bbb-8bbb-bbbbbbbbbbbb',
                    '018faaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa',
                    '018fcccc-cccc-7ccc-8ccc-cccccccccccc',
                    'V7 notebook', 'v7.md', 1,
                    'numeric', 'ready', NULL, 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute_batch("DROP TABLE notebook_tab_state; PRAGMA user_version = 7;")
            .unwrap();

        let store = Store::from_connection(connection).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let notebooks = store
            .list_notebooks("018faaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa")
            .unwrap();
        assert_eq!(notebooks.len(), 1);
        assert!(notebooks[0].is_pinned);
        assert_eq!(notebooks[0].display_name, "V7 notebook");
    }

    #[test]
    fn schema_eight_adds_recoverable_numbering_state_without_rewriting_notebooks() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('018fdddd-dddd-7ddd-8ddd-dddddddddddd', 'V8', '/tmp/wakegpt-v8', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notebooks (
                    id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, target_state, last_error_code, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018feeee-eeee-7eee-8eee-eeeeeeeeeeee',
                    '018fdddd-dddd-7ddd-8ddd-dddddddddddd',
                    '018fffff-ffff-7fff-8fff-ffffffffffff',
                    'V8 notebook', 'v8.md', 1,
                    'task', 'ready', NULL, 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE notebooks DROP COLUMN numbering_sync_pending;
                 PRAGMA user_version = 8;",
            )
            .unwrap();

        let store = Store::from_connection(connection).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let notebooks = store
            .list_notebooks("018fdddd-dddd-7ddd-8ddd-dddddddddddd")
            .unwrap();
        assert_eq!(notebooks.len(), 1);
        assert_eq!(notebooks[0].numbering_style, NumberingStyle::Task);
        assert!(!notebooks[0].numbering_sync_pending);
    }

    #[test]
    fn schema_nine_adds_default_numbering_start_without_rewriting_notebooks() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES ('018f1010-1010-7010-8010-101010101010', 'V9', '/tmp/wakegpt-v9', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notebooks (
                    id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, target_state, last_error_code, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f2020-2020-7020-8020-202020202020',
                    '018f1010-1010-7010-8010-101010101010',
                    '018f3030-3030-7030-8030-303030303030',
                    'V9 notebook', 'v9.md', 1,
                    'date_heading_numeric', 'ready', NULL, 1, 1
                 )",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE notebooks DROP COLUMN numbering_start;
                 PRAGMA user_version = 9;",
            )
            .unwrap();

        let store = Store::from_connection(connection).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let notebooks = store
            .list_notebooks("018f1010-1010-7010-8010-101010101010")
            .unwrap();
        assert_eq!(notebooks.len(), 1);
        assert_eq!(
            notebooks[0].numbering_style,
            NumberingStyle::DateHeadingNumeric
        );
        assert_eq!(notebooks[0].numbering_start, 1);
        assert!(!notebooks[0].numbering_sync_pending);
    }

    #[test]
    fn schema_ten_adds_notebook_attachment_homes_and_queues_legacy_blob_relocation() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let digest = "a".repeat(64);
        connection
            .execute_batch(&format!(
                "INSERT INTO workspaces (
                    id, display_name, root_path, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f1111-1010-7010-8010-101010101010',
                    'V10', '/tmp/wakegpt-v10', 1, 1
                 );
                 INSERT INTO notebooks (
                    id, workspace_id, target_id, display_name, relative_path, ordinal,
                    numbering_style, numbering_start, numbering_sync_pending,
                    attachment_directory, previous_attachment_directory,
                    attachment_directory_sync_pending,
                    target_state, last_error_code, created_at_ms, updated_at_ms
                 ) VALUES (
                    '018f2222-2020-7020-8020-202020202020',
                    '018f1111-1010-7010-8010-101010101010',
                    '018f3333-3030-7030-8030-303030303030',
                    'V10 notebook', 'notes/v10.md', 1,
                    'numeric', 1, 0, 'notes/attachments/v10', NULL, 0,
                    'ready', NULL, 1, 1
                 );
                 INSERT INTO records (
                    id, workspace_id, notebook_id, body_markdown,
                    created_at_ms, updated_at_ms, logical_order,
                    state, sync_state, revision, applied_revision, trashed_at_ms
                 ) VALUES (
                    '018f4444-4040-7040-8040-404040404040',
                    '018f1111-1010-7010-8010-101010101010',
                    '018f2222-2020-7020-8020-202020202020',
                    'Legacy image', 1, 1, 1024, 'active', 'synced', 1, 1, NULL
                 );
                 INSERT INTO attachments (
                    id, record_id, media_type, managed_relative_path,
                    content_sha256, byte_size, created_at_ms,
                    previous_managed_relative_path, relocation_state,
                    relocation_error_code
                 ) VALUES (
                    '018f5555-5050-7050-8050-505050505050',
                    '018f4444-4040-7040-8040-404040404040',
                    'image/png', '.wakegpt/attachments/aa/{digest}.png',
                    '{digest}', 8, 1, NULL, 'ready', NULL
                 );
                 ALTER TABLE notebooks DROP COLUMN attachment_directory_sync_pending;
                 ALTER TABLE notebooks DROP COLUMN previous_attachment_directory;
                 ALTER TABLE notebooks DROP COLUMN attachment_directory;
                 ALTER TABLE attachments DROP COLUMN relocation_error_code;
                 ALTER TABLE attachments DROP COLUMN relocation_state;
                 ALTER TABLE attachments DROP COLUMN previous_managed_relative_path;
                 PRAGMA user_version = 10;"
            ))
            .unwrap();

        let store = Store::from_connection(connection).unwrap();
        let notebook = store
            .get_notebook(
                "018f1111-1010-7010-8010-101010101010",
                "018f2222-2020-7020-8020-202020202020",
            )
            .unwrap();
        assert_eq!(notebook.attachment_directory, "notes/attachments/v10");
        assert!(!notebook.attachment_directory_sync_pending);

        assert_eq!(
            store
                .queue_attachment_relocations_for_configured_directories()
                .unwrap(),
            1
        );
        let notebook = store
            .get_notebook(&notebook.workspace_id, &notebook.id)
            .unwrap();
        assert!(notebook.attachment_directory_sync_pending);
        let record = store
            .get_record(
                &notebook.workspace_id,
                "018f4444-4040-7040-8040-404040404040",
            )
            .unwrap();
        let attachment = &record.attachments[0];
        assert_eq!(
            attachment.managed_relative_path,
            format!("notes/attachments/v10/aa/{digest}.png")
        );
        assert_eq!(
            attachment.previous_managed_relative_path.as_deref(),
            Some(format!(".wakegpt/attachments/aa/{digest}.png").as_str())
        );
        assert_eq!(
            attachment.relocation_state,
            AttachmentRelocationState::Pending
        );
    }

    #[test]
    fn creates_and_lists_workspace_notebook_and_record() {
        let store = store();
        let workspace = store
            .create_workspace("Product", "/tmp/wakegpt-product")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Product notes",
                "notes/product.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "First note")
            .unwrap();

        assert_eq!(record.sync_state, SyncState::Queued);
        assert_eq!(store.list_workspaces().unwrap(), vec![workspace]);
        assert_eq!(
            store.list_notebooks(&record.workspace_id).unwrap(),
            vec![notebook]
        );
        assert_eq!(
            store
                .list_records(&record.workspace_id, record.notebook_id.as_deref(), false)
                .unwrap(),
            vec![record]
        );
    }

    #[test]
    fn rejects_cross_workspace_notebook_use() {
        let store = store();
        let first = store
            .create_workspace("First", "/tmp/wakegpt-first")
            .unwrap();
        let second = store
            .create_workspace("Second", "/tmp/wakegpt-second")
            .unwrap();
        let notebook = store
            .create_notebook(&first.id, "First notes", "notes.md", NumberingStyle::None)
            .unwrap();

        let error = store
            .create_record(&second.id, Some(&notebook.id), "Should fail")
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not belong to the selected workspace"));
    }

    #[test]
    fn inbox_record_stays_local() {
        let store = store();
        let workspace = store
            .create_workspace("Inbox", "/tmp/wakegpt-inbox")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Local thought")
            .unwrap();
        assert_eq!(record.sync_state, SyncState::Local);
        assert!(record.notebook_id.is_none());
    }

    #[test]
    fn editing_increments_revision_and_rejects_stale_writes() {
        let store = store();
        let workspace = store.create_workspace("Edit", "/tmp/wakegpt-edit").unwrap();
        let original = store
            .create_record(&workspace.id, None, "Original")
            .unwrap();
        let updated = store
            .update_record(&workspace.id, &original.id, original.revision, "Updated")
            .unwrap();

        assert_eq!(updated.body_markdown, "Updated");
        assert_eq!(updated.revision, original.revision + 1);
        let error = store
            .update_record(&workspace.id, &original.id, original.revision, "Stale")
            .unwrap_err();
        assert!(error.to_string().contains("revision changed"));
    }

    #[test]
    fn trash_and_restore_preserve_identity_and_order() {
        let store = store();
        let workspace = store
            .create_workspace("Trash", "/tmp/wakegpt-trash")
            .unwrap();
        let original = store
            .create_record(&workspace.id, None, "Recover me")
            .unwrap();
        let trashed = store
            .trash_record(&workspace.id, &original.id, original.revision)
            .unwrap();

        assert_eq!(trashed.state, RecordState::Trashed);
        assert!(store
            .list_records(&workspace.id, None, false)
            .unwrap()
            .is_empty());
        let restored = store
            .restore_record(&workspace.id, &trashed.id, trashed.revision)
            .unwrap();
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.logical_order, original.logical_order);
        assert_eq!(restored.state, RecordState::Active);
    }

    #[test]
    fn migration_preserves_record_identity_and_appends_to_destination() {
        let store = store();
        let workspace = store.create_workspace("Move", "/tmp/wakegpt-move").unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Destination",
                "destination.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let existing = store
            .create_record(&workspace.id, Some(&notebook.id), "Existing")
            .unwrap();
        settle_record_file_operation(&store, &existing);
        let inbox = store.create_record(&workspace.id, None, "Move me").unwrap();
        let moved = store
            .migrate_record(&workspace.id, &inbox.id, inbox.revision, Some(&notebook.id))
            .unwrap();

        assert_eq!(moved.id, inbox.id);
        assert_eq!(moved.notebook_id.as_deref(), Some(notebook.id.as_str()));
        assert!(moved.logical_order > existing.logical_order);
        assert_eq!(moved.sync_state, SyncState::Queued);
    }

    #[test]
    fn migration_rejects_a_destination_from_another_workspace() {
        let store = store();
        let first = store
            .create_workspace("First move", "/tmp/wakegpt-move-first")
            .unwrap();
        let second = store
            .create_workspace("Second move", "/tmp/wakegpt-move-second")
            .unwrap();
        let foreign = store
            .create_notebook(&second.id, "Foreign", "foreign.md", NumberingStyle::None)
            .unwrap();
        let record = store.create_record(&first.id, None, "Move me").unwrap();

        let error = store
            .migrate_record(&first.id, &record.id, record.revision, Some(&foreign.id))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not belong to the selected workspace"));
    }

    #[test]
    fn notebook_to_inbox_migration_keeps_source_intent_queued() {
        let store = store();
        let workspace = store
            .create_workspace("Source", "/tmp/wakegpt-source-intent")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Source notes",
                "source.md",
                NumberingStyle::None,
            )
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Move to inbox")
            .unwrap();
        settle_record_file_operation(&store, &record);
        let settled = store.get_record(&workspace.id, &record.id).unwrap();
        let migrated = store
            .migrate_record(&workspace.id, &settled.id, settled.revision, None)
            .unwrap();

        assert_eq!(migrated.sync_state, SyncState::Queued);
        assert_eq!(migrated.applied_revision, settled.applied_revision);
        let connection = store.connection().unwrap();
        let source: Option<String> = connection
            .query_row(
                "SELECT source_notebook_id FROM recovery_operations
                 WHERE record_id = ?1 AND desired_revision = ?2",
                params![record.id, revision_to_db(migrated.revision).unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source.as_deref(), Some(notebook.id.as_str()));
    }

    #[test]
    fn record_reads_keep_attachments_and_allow_attachment_only_edits() {
        let store = store();
        let workspace = store
            .create_workspace("Attachment", "/tmp/wakegpt-attachment")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Temporary body")
            .unwrap();
        let attachment_id = new_id();
        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO attachments (
                        id, record_id, media_type, managed_relative_path,
                        content_sha256, byte_size, created_at_ms
                     ) VALUES (?1, ?2, 'image/png', 'assets/image.png', ?3, 3, ?4)",
                    params![attachment_id, record.id, "a".repeat(64), now_ms()],
                )
                .unwrap();
        }

        let listed = store.list_records(&workspace.id, None, false).unwrap();
        assert_eq!(listed[0].attachments.len(), 1);
        let updated = store
            .update_record(&workspace.id, &record.id, record.revision, "")
            .unwrap();
        assert!(updated.body_markdown.is_empty());
        assert_eq!(updated.attachments.len(), 1);
    }

    #[test]
    fn no_op_edits_and_migrations_do_not_increment_revision() {
        let store = store();
        let workspace = store
            .create_workspace("No op", "/tmp/wakegpt-no-op")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Unchanged")
            .unwrap();

        let unchanged = store
            .update_record(&workspace.id, &record.id, record.revision, "Unchanged")
            .unwrap();
        assert_eq!(unchanged.revision, record.revision);
        let unmoved = store
            .migrate_record(&workspace.id, &record.id, record.revision, None)
            .unwrap();
        assert_eq!(unmoved.revision, record.revision);
    }

    #[test]
    fn pinning_is_persistent_ui_state_and_never_changes_file_order_or_revision() {
        let store = store();
        let workspace = store.create_workspace("Pin", "/tmp/wakegpt-pin").unwrap();
        let record = store.create_record(&workspace.id, None, "Pinned").unwrap();

        let pinned = store
            .set_record_pinned(&workspace.id, &record.id, true)
            .unwrap();
        assert!(pinned.is_pinned);
        assert_eq!(pinned.revision, record.revision);
        assert_eq!(pinned.logical_order, record.logical_order);
        assert!(
            store
                .get_record(&workspace.id, &record.id)
                .unwrap()
                .is_pinned
        );

        let unpinned = store
            .set_record_pinned(&workspace.id, &record.id, false)
            .unwrap();
        assert!(!unpinned.is_pinned);
        assert_eq!(unpinned.revision, record.revision);
    }

    #[test]
    fn composer_receipts_are_bounded_and_scoped_to_a_hashed_host_identity() {
        let store = store();
        let workspace = store
            .create_workspace("Composer", "/tmp/wakegpt-composer")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Insert once")
            .unwrap();
        let host_kind = format!("chatgpt:{}", "a".repeat(64));
        store
            .record_composer_receipt(
                &record.id,
                &host_kind,
                "partial",
                r#"{"textInserted":true,"insertedAttachmentIds":[]}"#,
            )
            .unwrap();
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        validate_id(&receipt.id, "composer receipt id").unwrap();
        assert_eq!(receipt.state, "partial");
        assert!(receipt.detail_json.contains("textInserted"));
        assert!(receipt.created_at_ms > 0);
        store
            .record_composer_receipt(
                &record.id,
                &host_kind,
                "uncertain",
                r#"{"textUncertain":true}"#,
            )
            .unwrap();
        assert_eq!(
            store
                .latest_composer_receipt(&record.id, &host_kind)
                .unwrap()
                .unwrap()
                .state,
            "uncertain"
        );
        assert!(store
            .record_composer_receipt(&record.id, "chatgpt:not-a-digest", "complete", "{}")
            .is_err());
        assert!(store
            .record_composer_receipt(&record.id, &host_kind, "retryable", "{}")
            .is_err());
    }

    #[test]
    fn latest_composer_receipts_for_host_returns_every_records_proven_latest_receipt() {
        let store = store();
        let workspace = store
            .create_workspace("Composer latest", "/tmp/wakegpt-composer-latest")
            .unwrap();
        let records = (0..10)
            .map(|index| {
                store
                    .create_record(&workspace.id, None, &format!("Record {index}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let host_kind = format!("chatgpt:{}", "b".repeat(64));
        let other_host_kind = format!("chatgpt:{}", "c".repeat(64));
        let receipt_id = |value: usize| format!("018f1717-1717-7171-8171-{value:012x}");
        let connection = store.connection().unwrap();
        for (index, record) in records.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO composer_receipts (
                        id, record_id, host_kind, state, detail_json, created_at_ms
                     ) VALUES (?1, ?2, ?3, 'complete', ?4, ?5)",
                    params![
                        receipt_id(index * 10),
                        record.id,
                        host_kind,
                        format!(r#"{{"sequence":{index}}}"#),
                        100_i64 + i64::try_from(index).unwrap(),
                    ],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO composer_receipts (
                    id, record_id, host_kind, state, detail_json, created_at_ms
                 ) VALUES (?1, ?2, ?3, 'uncertain', ?4, 1000)",
                params![
                    receipt_id(1),
                    records[0].id,
                    host_kind,
                    r#"{"latest":"newer-time"}"#,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO composer_receipts (
                    id, record_id, host_kind, state, detail_json, created_at_ms
                 ) VALUES (?1, ?2, ?3, 'partial', ?4, 101)",
                params![
                    receipt_id(11),
                    records[1].id,
                    host_kind,
                    r#"{"latest":"id-tiebreak"}"#,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO composer_receipts (
                    id, record_id, host_kind, state, detail_json, created_at_ms
                 ) VALUES (?1, ?2, ?3, 'uncertain', '{}', 2000)",
                params![receipt_id(999), records[9].id, other_host_kind],
            )
            .unwrap();
        drop(connection);

        let receipts = store.latest_composer_receipts_for_host(&host_kind).unwrap();
        assert_eq!(receipts.len(), 10);
        let expected_record_order = std::iter::once(records[0].id.clone())
            .chain((1..10).rev().map(|index| records[index].id.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            receipts
                .iter()
                .map(|(record_id, _)| record_id.clone())
                .collect::<Vec<_>>(),
            expected_record_order
        );
        assert_eq!(receipts[0].1.id, receipt_id(1));
        assert_eq!(receipts[0].1.state, "uncertain");
        assert_eq!(receipts[0].1.created_at_ms, 1000);
        let tied = receipts
            .iter()
            .find(|(record_id, _)| record_id == &records[1].id)
            .unwrap();
        assert_eq!(tied.1.id, receipt_id(11));
        assert_eq!(tied.1.state, "partial");
        assert!(receipts
            .iter()
            .any(|(record_id, _)| record_id == &records[2].id));
        assert_eq!(
            store
                .latest_composer_receipt(&records[1].id, &host_kind)
                .unwrap()
                .unwrap(),
            tied.1
        );
        assert!(store
            .latest_composer_receipts_for_host("chatgpt:not-a-digest")
            .is_err());
    }

    #[test]
    fn workspace_selection_theme_and_drafts_persist_per_tab() {
        let store = store();
        let workspace = store
            .create_workspace("UI state", "/tmp/wakegpt-ui-state")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Notebook",
                "notebook.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let attachment = DraftAttachment {
            token: new_id(),
            media_type: "image/png".to_owned(),
            byte_size: 32,
            content_sha256: "a".repeat(64),
            display_name: "draft.png".to_owned(),
        };

        store
            .save_draft(&workspace.id, None, "Inbox draft", &[])
            .unwrap();
        store
            .save_draft(
                &workspace.id,
                Some(&notebook.id),
                "Notebook draft",
                std::slice::from_ref(&attachment),
            )
            .unwrap();
        store
            .set_active_selection(&workspace.id, Some(&notebook.id))
            .unwrap();
        store.set_theme_preference(ThemePreference::Dark).unwrap();
        store
            .set_submit_shortcut(SubmitShortcut::CommandEnter)
            .unwrap();
        store.set_markdown_layout(MarkdownLayout::Preview).unwrap();

        assert_eq!(
            store.load_draft(&workspace.id, None).unwrap().body_markdown,
            "Inbox draft"
        );
        let notebook_draft = store.load_draft(&workspace.id, Some(&notebook.id)).unwrap();
        assert_eq!(notebook_draft.body_markdown, "Notebook draft");
        assert_eq!(notebook_draft.attachments, vec![attachment]);
        assert_eq!(
            store.ui_preferences().unwrap(),
            UiPreferences {
                active_workspace_id: Some(workspace.id),
                selected_notebook_id: Some(notebook.id),
                theme: ThemePreference::Dark,
                submit_shortcut: SubmitShortcut::CommandEnter,
                markdown_layout: MarkdownLayout::Preview,
            }
        );
    }

    #[test]
    fn tab_reordering_changes_only_notebook_ordinals() {
        let store = store();
        let workspace = store.create_workspace("Tabs", "/tmp/wakegpt-tabs").unwrap();
        let first = store
            .create_notebook(&workspace.id, "First", "first.md", NumberingStyle::None)
            .unwrap();
        let second = store
            .create_notebook(&workspace.id, "Second", "second.md", NumberingStyle::None)
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&first.id), "Order stays")
            .unwrap();
        let before_order = record.logical_order;

        let reordered = store
            .reorder_notebooks(&workspace.id, &[second.id.clone(), first.id.clone()])
            .unwrap();

        assert_eq!(
            reordered
                .iter()
                .map(|notebook| &notebook.id)
                .collect::<Vec<_>>(),
            vec![&second.id, &first.id]
        );
        assert_eq!(
            store
                .get_record(&workspace.id, &record.id)
                .unwrap()
                .logical_order,
            before_order
        );
    }

    #[test]
    fn tab_pinning_and_notebook_unbinding_are_orthogonal() {
        let store = store();
        let workspace = store
            .create_workspace("Notebook lifecycle", "/tmp/wakegpt-notebook-lifecycle")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Lifecycle",
                "lifecycle.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Retained")
            .unwrap();
        settle_record_file_operation(&store, &record);
        store
            .set_active_selection(&workspace.id, Some(&notebook.id))
            .unwrap();

        let unpinned = store
            .set_notebook_pinned(&workspace.id, &notebook.id, false)
            .unwrap();
        assert!(!unpinned.is_pinned);
        assert_eq!(unpinned.target_state, NotebookTargetState::Unverified);
        assert_eq!(unpinned.relative_path, "lifecycle.md");
        assert_eq!(store.ui_preferences().unwrap().selected_notebook_id, None);

        let unbound = store.unbind_notebook(&workspace.id, &notebook.id).unwrap();
        assert!(!unbound.is_pinned);
        assert_eq!(unbound.target_state, NotebookTargetState::Unbound);
        assert_eq!(unbound.target_id, notebook.target_id);
        assert_eq!(
            store
                .list_records(&workspace.id, Some(&notebook.id), false)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .update_record(&workspace.id, &record.id, record.revision, "Blocked")
            .unwrap_err()
            .to_string()
            .contains("已解除绑定"));
        assert!(store
            .create_record(&workspace.id, Some(&notebook.id), "Blocked create")
            .unwrap_err()
            .to_string()
            .contains("已解除绑定"));

        let repinned = store
            .set_active_selection(&workspace.id, Some(&notebook.id))
            .unwrap();
        assert_eq!(repinned.selected_notebook_id, Some(notebook.id.clone()));
        let repinned_notebook = store.get_notebook(&workspace.id, &notebook.id).unwrap();
        assert!(repinned_notebook.is_pinned);
        assert_eq!(repinned_notebook.target_state, NotebookTargetState::Unbound);
        assert_eq!(
            store
                .unbind_notebook(&workspace.id, &notebook.id)
                .unwrap()
                .target_state,
            NotebookTargetState::Unbound
        );
    }

    #[test]
    fn notebook_relocation_preserves_identity_and_lifecycle_reason() {
        let store = store();
        let workspace = store
            .create_workspace("Notebook relocation", "/tmp/wakegpt-notebook-relocation")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Relocation",
                "notes/original.md",
                NumberingStyle::Numeric,
            )
            .unwrap();
        let initial_receipt = NotebookFileReceiptInput {
            notebook_id: notebook.id.clone(),
            target_id: notebook.target_id.clone(),
            marker_schema: 2,
            managed_sha256: "a".repeat(64),
            file_sha256: "b".repeat(64),
            file_modified_ns: 1,
            file_size_bytes: 10,
        };
        let ready = store
            .initialize_notebook_file_receipt(&initial_receipt)
            .unwrap();
        let relocated_receipt = NotebookFileReceiptInput {
            file_sha256: "c".repeat(64),
            file_modified_ns: 2,
            file_size_bytes: 11,
            ..initial_receipt.clone()
        };

        let relocated = store
            .relocate_notebook_target(
                &ready,
                "archive/renamed.md",
                1,
                &initial_receipt.managed_sha256,
                &relocated_receipt,
            )
            .unwrap();

        assert_eq!(relocated.id, notebook.id);
        assert_eq!(relocated.target_id, notebook.target_id);
        assert_eq!(relocated.relative_path, "archive/renamed.md");
        assert_eq!(relocated.target_state, NotebookTargetState::Ready);
        let receipt = store.notebook_file_receipt(&notebook.id).unwrap().unwrap();
        assert_eq!(receipt.generation, 2);
        assert_eq!(receipt.file_sha256, "c".repeat(64));

        let unavailable = store
            .mark_notebook_target_unavailable(&relocated, "notebook_moved_to_trash")
            .unwrap();
        assert_eq!(unavailable.target_state, NotebookTargetState::Unavailable);
        assert_eq!(
            unavailable.last_error_code.as_deref(),
            Some("notebook_moved_to_trash")
        );
    }

    #[test]
    fn recovery_reads_include_intent_and_ordered_full_file_steps() {
        let store = store();
        let workspace = store
            .create_workspace("Recovery read", "/tmp/wakegpt-recovery-read")
            .unwrap();
        let first = store
            .create_notebook(&workspace.id, "First", "first.md", NumberingStyle::None)
            .unwrap();
        let second = store
            .create_notebook(&workspace.id, "Second", "second.md", NumberingStyle::None)
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&first.id), "Recoverable")
            .unwrap();
        let operation = store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .unwrap()
            .unwrap();

        assert_eq!(operation.desired_record_state, RecordState::Active);
        assert_eq!(operation.source_notebook_id, None);
        assert_eq!(
            operation.destination_notebook_id.as_deref(),
            Some(first.id.as_str())
        );
        assert_eq!(operation.source_logical_order, None);
        assert_eq!(
            operation.destination_logical_order,
            Some(record.logical_order)
        );
        assert_eq!(operation.attempt_count, 0);
        assert_eq!(operation.last_error_code, None);

        store
            .stage_recovery_files(
                &operation.id,
                &[
                    recovery_step(1, &second, "source"),
                    recovery_step(0, &first, "destination"),
                ],
            )
            .unwrap();
        let steps = store.recovery_file_steps(&operation.id).unwrap();
        assert_eq!(
            steps.iter().map(|step| step.step_index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        let first_step = &steps[0];
        assert_eq!(first_step.operation_id, operation.id);
        assert_eq!(first_step.notebook_id, first.id);
        assert_eq!(first_step.role, "destination");
        assert_eq!(first_step.effect, "upsert");
        assert_eq!(first_step.state, "staged");
        assert_eq!(first_step.target_workspace_relative_path, "first.md");
        assert_eq!(first_step.staged_sibling_name, ".0.stage");
        assert_eq!(
            first_step.backup_app_data_relative_path,
            "operation/0.before"
        );
        assert_eq!(first_step.expected_target_id, first.target_id);
        assert_eq!(first_step.before_file_sha256, "a".repeat(64));
        assert_eq!(first_step.after_file_sha256, "b".repeat(64));
        assert_eq!(first_step.before_managed_sha256, Some("c".repeat(64)));
        assert_eq!(first_step.after_managed_sha256, "d".repeat(64));
        assert_eq!(first_step.before_modified_ns, 42);
        assert!(first_step.target_existed);
        assert_eq!(first_step.applied_at_ms, None);
    }

    #[test]
    fn recovery_state_markers_are_idempotent_and_reject_illegal_phases() {
        let store = store();
        let workspace = store
            .create_workspace("Recovery state", "/tmp/wakegpt-recovery-state")
            .unwrap();
        let notebook = store
            .create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::None)
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Stateful")
            .unwrap();
        let operation = store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .unwrap()
            .unwrap();

        assert!(store.mark_recovery_applying(&operation.id).is_err());
        store
            .stage_recovery_files(&operation.id, &[recovery_step(0, &notebook, "single")])
            .unwrap();
        assert!(store.mark_recovery_step_applied(&operation.id, 0).is_err());
        assert!(store.mark_recovery_files_applied(&operation.id).is_err());

        store
            .mark_recovery_needed(&operation.id, "retry-test")
            .unwrap();
        store.mark_recovery_applying(&operation.id).unwrap();
        store.mark_recovery_applying(&operation.id).unwrap();
        let applying = store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .unwrap()
            .unwrap();
        assert_eq!(applying.phase, "applying");
        assert_eq!(applying.attempt_count, 1);

        store.mark_recovery_step_applied(&operation.id, 0).unwrap();
        store.mark_recovery_step_applied(&operation.id, 0).unwrap();
        store.mark_recovery_files_applied(&operation.id).unwrap();
        store.mark_recovery_files_applied(&operation.id).unwrap();
        let files_applied = store
            .recovery_operation_for_record_revision(&record.id, record.revision)
            .unwrap()
            .unwrap();
        assert_eq!(files_applied.phase, "files_applied");
        assert_eq!(files_applied.attempt_count, 1);
        assert!(store.mark_recovery_applying(&operation.id).is_err());
    }

    #[test]
    fn unfinished_record_operation_blocks_another_mutation() {
        let store = store();
        let workspace = store
            .create_workspace("Record gate", "/tmp/wakegpt-record-gate")
            .unwrap();
        let notebook = store
            .create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::None)
            .unwrap();
        let record = store
            .create_record(&workspace.id, Some(&notebook.id), "Original")
            .unwrap();

        let error = store
            .update_record(&workspace.id, &record.id, record.revision, "Blocked")
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("record has unfinished file operation"));
        let unchanged = store.get_record(&workspace.id, &record.id).unwrap();
        assert_eq!(unchanged.body_markdown, "Original");
        assert_eq!(unchanged.revision, record.revision);
    }

    #[test]
    fn unfinished_notebook_operation_blocks_source_and_destination_mutations() {
        let store = store();
        let workspace = store
            .create_workspace("Target gate", "/tmp/wakegpt-target-gate")
            .unwrap();
        let notebook = store
            .create_notebook(&workspace.id, "Notes", "notes.md", NumberingStyle::None)
            .unwrap();
        let settled = store
            .create_record(&workspace.id, Some(&notebook.id), "Settled")
            .unwrap();
        settle_record_file_operation(&store, &settled);
        let pending = store
            .create_record(&workspace.id, Some(&notebook.id), "Pending")
            .unwrap();
        let inbox = store.create_record(&workspace.id, None, "Inbox").unwrap();

        let create_error = store
            .create_record(&workspace.id, Some(&notebook.id), "Create blocked")
            .unwrap_err();
        assert!(create_error
            .to_string()
            .contains("notebook target has unfinished file operation"));
        let source_error = store
            .update_record(
                &workspace.id,
                &settled.id,
                settled.revision,
                "Source blocked",
            )
            .unwrap_err();
        assert!(source_error
            .to_string()
            .contains("notebook target has unfinished file operation"));
        let destination_error = store
            .migrate_record(&workspace.id, &inbox.id, inbox.revision, Some(&notebook.id))
            .unwrap_err();
        assert!(destination_error
            .to_string()
            .contains("notebook target has unfinished file operation"));
        assert_eq!(
            store.get_record(&workspace.id, &pending.id).unwrap(),
            pending
        );
    }

    fn legacy_v1_connection(with_recovery: bool) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE workspaces (
                    id TEXT PRIMARY KEY,
                    display_name TEXT NOT NULL,
                    root_path TEXT NOT NULL UNIQUE,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE notebooks (
                    id TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                    target_id TEXT NOT NULL UNIQUE,
                    display_name TEXT NOT NULL,
                    relative_path TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    numbering_style TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE records (
                    id TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                    notebook_id TEXT REFERENCES notebooks(id),
                    body_markdown TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    logical_order INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    sync_state TEXT NOT NULL,
                    revision INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE file_receipts (
                    record_id TEXT PRIMARY KEY,
                    notebook_id TEXT NOT NULL,
                    record_revision INTEGER NOT NULL,
                    managed_digest TEXT NOT NULL,
                    file_modified_ns INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE recovery_operations (
                    id TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL,
                    operation_kind TEXT NOT NULL,
                    state TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        if with_recovery {
            connection
                .execute(
                    "INSERT INTO recovery_operations
                     VALUES ('op', 'workspace', 'edit', 'queued', '{}', 1, 1)",
                    [],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn schema_one_migrates_only_when_legacy_recovery_tables_are_empty() {
        let connection = legacy_v1_connection(false);
        migrate(&connection).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let blocked = legacy_v1_connection(true);
        let error = migrate(&blocked).unwrap_err();
        assert!(error.to_string().contains("migration was not started"));
        let version: u32 = blocked
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}
