use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path};
use uuid::Uuid;

pub const MAX_RECORD_MARKDOWN_BYTES: usize = 1024 * 1024;
pub const DEFAULT_NUMBERING_START: u32 = 1;
pub const MAX_NUMBERING_START: u32 = 1_000_000_000;
pub const INBOX_ATTACHMENT_DIRECTORY: &str = ".wakegpt/attachments/inbox";
const MAX_DISPLAY_NAME_CHARS: usize = 120;
const MAX_ATTACHMENT_DIRECTORY_BYTES: usize = 1024;
#[cfg(test)]
pub const DEFAULT_RECORD_TRASH_RETENTION_DAYS: u32 = 30;
pub const MAX_RECORD_TRASH_RETENTION_DAYS: u32 = 3650;
pub const DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES: [&str; 9] = [
    ".git",
    ".hg",
    ".svn",
    ".wakegpt",
    ".next",
    "node_modules",
    "target",
    "dist",
    "build",
];
const MAX_NOTEBOOK_SCAN_IGNORE_DIRECTORIES: usize = 64;
const MAX_NOTEBOOK_SCAN_IGNORE_DIRECTORY_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainError(String);

impl DomainError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DomainError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub display_name: String,
    pub root_path: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err(DomainError::new(format!(
                "unsupported theme preference: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    pub active_workspace_id: Option<String>,
    pub selected_notebook_id: Option<String>,
    pub theme: ThemePreference,
    pub submit_shortcut: SubmitShortcut,
    pub markdown_layout: MarkdownLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOpenPreference {
    pub workspace_id: String,
    pub default_notebook_id: Option<String>,
    pub use_last_selection: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettings {
    pub external_network_enabled: bool,
    pub automatic_checks_enabled: bool,
    pub automatic_downloads_enabled: bool,
    pub check_interval_hours: u32,
    pub skipped_version: Option<String>,
    pub last_checked_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProductSettings {
    pub record_trash_retention_days: Option<u32>,
    pub notebook_scan_ignore_directories: Vec<String>,
    pub protected_notebook_scan_ignore_directories: Vec<String>,
    pub codex_integration_paused: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DefaultIdentitySlot {
    pub alias: String,
    pub locked: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubmitShortcut {
    Enter,
    CommandEnter,
}

impl SubmitShortcut {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::CommandEnter => "command_enter",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "enter" => Ok(Self::Enter),
            "command_enter" => Ok(Self::CommandEnter),
            _ => Err(DomainError::new(format!(
                "unsupported submit shortcut: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MarkdownLayout {
    Edit,
    Preview,
    Split,
}

impl MarkdownLayout {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Preview => "preview",
            Self::Split => "split",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "edit" => Ok(Self::Edit),
            "preview" => Ok(Self::Preview),
            "split" => Ok(Self::Split),
            _ => Err(DomainError::new(format!(
                "unsupported Markdown layout: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Notebook {
    pub id: String,
    pub workspace_id: String,
    pub target_id: String,
    pub display_name: String,
    pub relative_path: String,
    pub ordinal: i64,
    pub is_pinned: bool,
    pub numbering_style: NumberingStyle,
    pub numbering_start: u32,
    pub numbering_sync_pending: bool,
    pub attachment_directory: String,
    pub previous_attachment_directory: Option<String>,
    pub attachment_directory_sync_pending: bool,
    pub target_state: NotebookTargetState,
    pub last_error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub workspace_id: String,
    pub notebook_id: Option<String>,
    pub body_markdown: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub logical_order: i64,
    pub state: RecordState,
    pub sync_state: SyncState,
    pub revision: u64,
    pub applied_revision: u64,
    pub trashed_at_ms: Option<i64>,
    pub is_pinned: bool,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NotebookTargetState {
    Unverified,
    Ready,
    Conflict,
    Unavailable,
    Unbound,
}

impl NotebookTargetState {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Ready => "ready",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Unbound => "unbound",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "unverified" => Ok(Self::Unverified),
            "ready" => Ok(Self::Ready),
            "conflict" => Ok(Self::Conflict),
            "unavailable" => Ok(Self::Unavailable),
            "unbound" => Ok(Self::Unbound),
            _ => Err(DomainError::new(format!(
                "unsupported notebook target state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub record_id: String,
    pub media_type: String,
    pub managed_relative_path: String,
    pub content_sha256: String,
    pub byte_size: u64,
    pub created_at_ms: i64,
    pub file_state: AttachmentFileState,
    pub previous_managed_relative_path: Option<String>,
    pub relocation_state: AttachmentRelocationState,
    pub relocation_error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentRelocationState {
    Ready,
    Pending,
    CleanupPending,
    Conflict,
}

impl AttachmentRelocationState {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::CleanupPending => "cleanup_pending",
            Self::Conflict => "conflict",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "ready" => Ok(Self::Ready),
            "pending" => Ok(Self::Pending),
            "cleanup_pending" => Ok(Self::CleanupPending),
            "conflict" => Ok(Self::Conflict),
            _ => Err(DomainError::new(format!(
                "unsupported attachment relocation state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentFileState {
    Ready,
    Trashed,
    Missing,
    PreservedShared,
    Modified,
    RecoveryRequired,
}

impl AttachmentFileState {
    pub fn from_operation_phase(phase: Option<&str>) -> Result<Self, DomainError> {
        match phase {
            None | Some("ready") => Ok(Self::Ready),
            Some("trashed") => Ok(Self::Trashed),
            Some("missing") => Ok(Self::Missing),
            Some("preserved_shared") => Ok(Self::PreservedShared),
            Some("preserved_changed") => Ok(Self::Modified),
            Some("queued" | "prepared" | "needs_recovery") => Ok(Self::RecoveryRequired),
            Some(value) => Err(DomainError::new(format!(
                "unsupported attachment file state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecordState {
    Active,
    Trashed,
}

impl RecordState {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Trashed => "trashed",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "active" => Ok(Self::Active),
            "trashed" => Ok(Self::Trashed),
            _ => Err(DomainError::new(format!(
                "unsupported record state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    Local,
    Queued,
    Synced,
    Conflict,
    TargetUnavailable,
}

impl SyncState {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Queued => "queued",
            Self::Synced => "synced",
            Self::Conflict => "conflict",
            Self::TargetUnavailable => "target_unavailable",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "local" => Ok(Self::Local),
            "queued" => Ok(Self::Queued),
            "synced" => Ok(Self::Synced),
            "conflict" => Ok(Self::Conflict),
            "target_unavailable" => Ok(Self::TargetUnavailable),
            _ => Err(DomainError::new(format!("unsupported sync state: {value}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NumberingStyle {
    None,
    Numeric,
    Bullet,
    Task,
    TimePrefix,
    DateHeadingNumeric,
}

impl NumberingStyle {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Numeric => "numeric",
            Self::Bullet => "bullet",
            Self::Task => "task",
            Self::TimePrefix => "time_prefix",
            Self::DateHeadingNumeric => "date_heading_numeric",
        }
    }

    pub fn from_db_value(value: &str) -> Result<Self, DomainError> {
        match value {
            "none" => Ok(Self::None),
            "numeric" => Ok(Self::Numeric),
            "bullet" => Ok(Self::Bullet),
            "task" => Ok(Self::Task),
            "time_prefix" => Ok(Self::TimePrefix),
            "date_heading_numeric" => Ok(Self::DateHeadingNumeric),
            _ => Err(DomainError::new(format!(
                "unsupported numbering style: {value}"
            ))),
        }
    }
}

pub const fn default_numbering_start() -> u32 {
    DEFAULT_NUMBERING_START
}

pub fn validate_numbering_start(value: u32) -> Result<u32, DomainError> {
    if !(1..=MAX_NUMBERING_START).contains(&value) {
        return Err(DomainError::new(format!(
            "numbering start must be between 1 and {MAX_NUMBERING_START}"
        )));
    }
    Ok(value)
}

pub fn new_id() -> String {
    Uuid::now_v7().to_string()
}

pub fn validate_id(value: &str, label: &str) -> Result<(), DomainError> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| DomainError::new(format!("{label} must be a UUID")))
}

pub fn validate_display_name(value: &str, label: &str) -> Result<String, DomainError> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();
    if length == 0 {
        return Err(DomainError::new(format!("{label} cannot be empty")));
    }
    if length > MAX_DISPLAY_NAME_CHARS {
        return Err(DomainError::new(format!(
            "{label} cannot exceed {MAX_DISPLAY_NAME_CHARS} characters"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(DomainError::new(format!(
            "{label} cannot contain control characters"
        )));
    }
    Ok(trimmed.to_owned())
}

pub fn validate_workspace_root(value: &str) -> Result<String, DomainError> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(DomainError::new("workspace root must be an absolute path"));
    }
    if value.contains('\0') {
        return Err(DomainError::new(
            "workspace root contains an invalid character",
        ));
    }
    Ok(path.to_string_lossy().into_owned())
}

pub fn validate_notebook_relative_path(value: &str) -> Result<String, DomainError> {
    if value.is_empty() {
        return Err(DomainError::new("notebook path cannot be empty"));
    }
    if value != value.trim() {
        return Err(DomainError::new(
            "notebook path cannot start or end with whitespace",
        ));
    }
    if value.contains('\\') {
        return Err(DomainError::new("notebook path must use forward slashes"));
    }
    if value.chars().any(char::is_control) {
        return Err(DomainError::new(
            "notebook path cannot contain control characters",
        ));
    }
    if value
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(DomainError::new(
            "notebook path contains an unsafe component",
        ));
    }

    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(DomainError::new(
            "notebook path must stay inside its workspace",
        ));
    }

    let is_markdown = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    if !is_markdown {
        return Err(DomainError::new("notebook path must end in .md"));
    }

    Ok(value.to_owned())
}

pub fn validate_attachment_directory(value: &str) -> Result<String, DomainError> {
    if value.is_empty() {
        return Err(DomainError::new("attachment directory cannot be empty"));
    }
    if value != value.trim() {
        return Err(DomainError::new(
            "attachment directory cannot start or end with whitespace",
        ));
    }
    if value.len() > MAX_ATTACHMENT_DIRECTORY_BYTES {
        return Err(DomainError::new(format!(
            "attachment directory cannot exceed {MAX_ATTACHMENT_DIRECTORY_BYTES} bytes"
        )));
    }
    if value.contains('\\') || value.chars().any(char::is_control) {
        return Err(DomainError::new(
            "attachment directory must use safe forward-slash components",
        ));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
        || value.split('/').any(|component| component.is_empty())
    {
        return Err(DomainError::new(
            "attachment directory must stay inside its workspace",
        ));
    }
    if value.split('/').next() == Some(".wakegpt") {
        return Err(DomainError::new(
            "the .wakegpt directory is reserved for WakeGPT internal data",
        ));
    }
    Ok(value.to_owned())
}

pub fn validate_record_trash_retention_days(
    value: Option<u32>,
) -> Result<Option<u32>, DomainError> {
    if value.is_some_and(|days| days == 0 || days > MAX_RECORD_TRASH_RETENTION_DAYS) {
        return Err(DomainError::new(format!(
            "record trash retention must be between 1 and {MAX_RECORD_TRASH_RETENTION_DAYS} days, or permanent"
        )));
    }
    Ok(value)
}

pub fn validate_notebook_scan_ignore_directories(
    values: &[String],
) -> Result<Vec<String>, DomainError> {
    if values.len() > MAX_NOTEBOOK_SCAN_IGNORE_DIRECTORIES {
        return Err(DomainError::new(format!(
            "notebook scan ignore directories cannot exceed {MAX_NOTEBOOK_SCAN_IGNORE_DIRECTORIES} entries"
        )));
    }
    let estimated_json_bytes = values
        .iter()
        .try_fold(2_usize, |total, value| {
            total.checked_add(value.len().saturating_add(3))
        })
        .ok_or_else(|| DomainError::new("notebook scan ignore directories are too large"))?;
    if estimated_json_bytes > 65_536 {
        return Err(DomainError::new(
            "notebook scan ignore directories are too large",
        ));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        if value.is_empty()
            || value != value.trim()
            || value.len() > MAX_NOTEBOOK_SCAN_IGNORE_DIRECTORY_BYTES
            || value.contains('\\')
            || value.chars().any(char::is_control)
            || value.contains('*')
            || value.contains('?')
            || Path::new(value).is_absolute()
            || value
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
            || Path::new(value)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(DomainError::new(
                "notebook scan ignore directories must be exact workspace-relative directories",
            ));
        }
        if normalized.iter().any(|existing| existing == value) {
            return Err(DomainError::new(
                "notebook scan ignore directories cannot contain duplicates",
            ));
        }
        normalized.push(value.clone());
    }
    if DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
        .iter()
        .any(|required| !normalized.iter().any(|value| value == required))
    {
        return Err(DomainError::new(
            "notebook scan ignore directories must keep the protected defaults",
        ));
    }
    Ok(normalized)
}

pub fn default_attachment_directory(notebook_relative_path: &str) -> Result<String, DomainError> {
    let notebook_relative_path = validate_notebook_relative_path(notebook_relative_path)?;
    let notebook_path = Path::new(&notebook_relative_path);
    let raw_stem = notebook_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("notebook");
    let mut safe_stem = raw_stem
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    while safe_stem.ends_with([' ', '.']) {
        safe_stem.pop();
    }
    if safe_stem.is_empty() {
        safe_stem.push_str("notebook");
    }
    let parent = notebook_path.parent().unwrap_or_else(|| Path::new(""));
    let directory = if parent.as_os_str().is_empty() {
        format!("attachments/{safe_stem}")
    } else {
        format!("{}/attachments/{safe_stem}", parent.to_string_lossy())
    };
    validate_attachment_directory(&directory)
}

pub fn managed_attachment_relative_path(
    attachment_directory: &str,
    content_sha256: &str,
    media_type: &str,
) -> Result<String, DomainError> {
    if attachment_directory != INBOX_ATTACHMENT_DIRECTORY {
        validate_attachment_directory(attachment_directory)?;
    }
    validate_attachment_digest(content_sha256)?;
    let extension = attachment_extension(media_type)?;
    Ok(format!(
        "{attachment_directory}/{}/{}.{}",
        &content_sha256[..2],
        content_sha256,
        extension
    ))
}

pub fn validate_managed_attachment_relative_path(
    value: &str,
    content_sha256: &str,
    media_type: &str,
) -> Result<String, DomainError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DomainError::new(
            "managed attachment path must stay inside its workspace",
        ));
    }
    let parent = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::to_str)
        .ok_or_else(|| DomainError::new("managed attachment path is incomplete"))?;
    let expected = managed_attachment_relative_path(parent, content_sha256, media_type)?;
    if value != expected {
        return Err(DomainError::new(
            "managed attachment path does not match its content identity",
        ));
    }
    Ok(value.to_owned())
}

fn validate_attachment_digest(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(DomainError::new("attachment digest is invalid"));
    }
    Ok(())
}

fn attachment_extension(media_type: &str) -> Result<&'static str, DomainError> {
    match media_type {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        "image/gif" => Ok("gif"),
        "image/webp" => Ok("webp"),
        _ => Err(DomainError::new("attachment media type is unsupported")),
    }
}

pub fn validate_record_markdown(
    value: &str,
    attachment_count: usize,
) -> Result<String, DomainError> {
    if value.len() > MAX_RECORD_MARKDOWN_BYTES {
        return Err(DomainError::new(format!(
            "record Markdown cannot exceed {MAX_RECORD_MARKDOWN_BYTES} bytes"
        )));
    }
    if value.trim().is_empty() && attachment_count == 0 {
        return Err(DomainError::new(
            "a record needs Markdown content or at least one attachment",
        ));
    }
    if value.lines().any(|line| {
        let line = line.trim();
        line.starts_with("<!-- wakegpt:") || line.starts_with("<!-- /wakegpt:")
    }) {
        return Err(DomainError::new(
            "record Markdown cannot contain WakeGPT control marker lines",
        ));
    }
    Ok(value.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notebook_path_rejects_escape_and_non_markdown_files() {
        assert!(validate_notebook_relative_path("../outside.md").is_err());
        assert!(validate_notebook_relative_path("notes/product.txt").is_err());
        assert!(validate_notebook_relative_path("notes\\product.md").is_err());
        assert!(validate_notebook_relative_path("notes//product.md").is_err());
        assert!(validate_notebook_relative_path(" notes/product.md").is_err());
        assert_eq!(
            validate_notebook_relative_path("notes/product.md").unwrap(),
            "notes/product.md"
        );
    }

    #[test]
    fn record_markdown_rejects_reserved_markers() {
        let result = validate_record_markdown("hello\n<!-- /wakegpt:record -->", 0);
        assert!(result.is_err());
    }

    #[test]
    fn attachment_only_record_is_valid() {
        assert_eq!(validate_record_markdown("", 1).unwrap(), "");
    }

    #[test]
    fn product_settings_reject_unsafe_or_ambiguous_values() {
        assert_eq!(validate_record_trash_retention_days(None).unwrap(), None);
        assert_eq!(
            validate_record_trash_retention_days(Some(30)).unwrap(),
            Some(30)
        );
        assert!(validate_record_trash_retention_days(Some(0)).is_err());
        assert!(validate_record_trash_retention_days(Some(3651)).is_err());

        let mut valid = DEFAULT_NOTEBOOK_SCAN_IGNORE_DIRECTORIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        valid.push("generated/cache".to_owned());
        assert_eq!(
            validate_notebook_scan_ignore_directories(&valid).unwrap(),
            valid
        );
        for invalid in [
            vec!["../private".to_owned()],
            vec!["/absolute".to_owned()],
            vec!["generated/**".to_owned()],
            vec!["generated\\cache".to_owned()],
            vec!["same".to_owned(), "same".to_owned()],
            vec!["generated/cache".to_owned()],
        ] {
            assert!(validate_notebook_scan_ignore_directories(&invalid).is_err());
        }
    }

    #[test]
    fn numbering_start_has_a_bounded_positive_range() {
        assert_eq!(validate_numbering_start(1).unwrap(), 1);
        assert_eq!(
            validate_numbering_start(MAX_NUMBERING_START).unwrap(),
            MAX_NUMBERING_START
        );
        assert!(validate_numbering_start(0).is_err());
        assert!(validate_numbering_start(MAX_NUMBERING_START + 1).is_err());
    }

    #[test]
    fn attachment_directory_is_workspace_relative_and_reserves_internal_storage() {
        assert_eq!(
            validate_attachment_directory("notes/attachments/product").unwrap(),
            "notes/attachments/product"
        );
        assert!(validate_attachment_directory("../outside").is_err());
        assert!(validate_attachment_directory("/outside").is_err());
        assert!(validate_attachment_directory("notes\\attachments").is_err());
        assert!(validate_attachment_directory(".wakegpt/attachments/custom").is_err());
    }

    #[test]
    fn default_attachment_directory_is_next_to_the_notebook_and_cross_platform_safe() {
        assert_eq!(
            default_attachment_directory("notes/product.md").unwrap(),
            "notes/attachments/product"
        );
        assert_eq!(
            default_attachment_directory("A:计划.md").unwrap(),
            "attachments/A_计划"
        );
    }

    #[test]
    fn managed_attachment_paths_are_content_addressed_inside_the_selected_home() {
        let digest = "a".repeat(64);
        assert_eq!(
            managed_attachment_relative_path("notes/attachments/product", &digest, "image/png")
                .unwrap(),
            format!("notes/attachments/product/aa/{digest}.png")
        );
        assert_eq!(
            managed_attachment_relative_path(INBOX_ATTACHMENT_DIRECTORY, &digest, "image/jpeg")
                .unwrap(),
            format!("{INBOX_ATTACHMENT_DIRECTORY}/aa/{digest}.jpg")
        );
        assert!(managed_attachment_relative_path("../outside", &digest, "image/png").is_err());
        assert!(validate_managed_attachment_relative_path(
            &format!("notes/attachments/product/aa/{digest}.png"),
            &digest,
            "image/png"
        )
        .is_ok());
        assert!(validate_managed_attachment_relative_path(
            &format!("notes/attachments/other/aa/{digest}.jpg"),
            &digest,
            "image/png"
        )
        .is_err());
    }
}
