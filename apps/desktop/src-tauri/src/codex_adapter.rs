use crate::attachments::{
    AttachmentCoordinator, PendingAttachment, MAX_ATTACHMENTS_PER_RECORD, MAX_ATTACHMENT_BYTES,
    MAX_RECORD_ATTACHMENT_BYTES,
};
use crate::diagnostics::{CodexDiagnosticCounts, LocalDiagnostics};
use crate::domain::{
    managed_attachment_relative_path, new_id, validate_id, validate_numbering_start,
    AttachmentFileState, AttachmentRelocationState, Notebook, NumberingStyle, Record, RecordState,
    SubmitShortcut, UiPreferences, Workspace, DEFAULT_NUMBERING_START, INBOX_ATTACHMENT_DIRECTORY,
};
use crate::file_sync::{
    open_workspace_root, sha256_hex as file_sha256_hex, CoordinatedMutationError, SyncCoordinator,
    SyncError,
};
use crate::lifecycle::{self, IntegrationControl};
use crate::storage::{
    ComposerReceipt, DraftAttachment, NewAttachment, RecordAttachmentSetRevision, Store,
    MUTATION_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tungstenite::client::client_with_config;
use tungstenite::protocol::{Message, WebSocket, WebSocketConfig};

#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSBundle, NSString};

pub const ADAPTER_VERSION: &str = "codex-26.803.81509-v41";
pub const SUPPORTED_CODEX_VERSION: &str = "26.803.81509";
pub const SUPPORTED_CODEX_BUILD: &str = "6415";
pub const HOST_CONTRACT_ID: &str = "chatgpt-26.803.81509-6415";
const CODEX_TARGET_URL: &str = "app://-/index.html";
const DEFAULT_IDENTITY_PROFILE_DIRECTORY: &str = "protected-chatgpt-profile-v1";
// space-seed: One stable loopback port is reserved only for WakeGPT's user-confirmed launch of
// the default ChatGPT profile. Independently launched profiles keep their own verified ports.
const CHATGPT_MANAGED_DEBUG_PORT: u16 = 58_119;
const MAX_ACTIVE_PORT_BYTES: u64 = 512;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CDP_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const CARD_IMAGE_CHUNK_BYTES: usize = 512 * 1024;
#[cfg(target_os = "macos")]
const MAX_PROCESS_ARGUMENT_BYTES: usize = 1024 * 1024;
const SUPERVISOR_INTERVAL: Duration = Duration::from_millis(650);
const CDP_IO_TIMEOUT: Duration = Duration::from_millis(250);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const CARD_SCRIPT: &str = include_str!("../assets/codex_card_v1.js");
#[cfg(target_os = "macos")]
const CHATGPT_BUNDLE_ID: &str = "com.openai.codex";
#[cfg(target_os = "macos")]
const CHATGPT_APP_CANDIDATES: [&str; 2] =
    ["/Applications/ChatGPT.app", "/Applications/Chatgpt.app"];
#[cfg(target_os = "macos")]
const CHATGPT_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
#[cfg(target_os = "macos")]
const CHATGPT_GRACEFUL_EXIT_POLLS: usize = 80;
#[cfg(target_os = "macos")]
const CHATGPT_FORCED_EXIT_POLLS: usize = 34;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterProbe {
    pub adapter_version: &'static str,
    pub supported_codex_version: &'static str,
    pub supported_codex_build: &'static str,
    pub endpoint_state: AdapterEndpointState,
    pub last_error_code: Option<&'static str>,
    pub card_script_sha256: String,
    pub detected_instance_count: usize,
    pub connectable_instance_count: usize,
    pub available_target_count: usize,
    pub connected_target_count: usize,
    pub failed_target_count: usize,
    pub instances: Vec<AdapterInstanceProbe>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdapterInstanceProbe {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub can_restart: bool,
    pub detected_codex_version: Option<String>,
    pub detected_codex_build: Option<String>,
    pub state: AdapterInstanceState,
    pub available_target_count: usize,
    pub connected_target_count: usize,
    pub failed_target_count: usize,
    pub last_error_code: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AdapterInstanceState {
    Unavailable,
    Incompatible,
    Available,
    Connecting,
    Connected,
    PartiallyConnected,
    Failed,
    Paused,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AdapterEndpointState {
    NotDetected,
    StaleEndpoint,
    UnmanagedTargetAvailable,
    UnmanagedTargetIncompatible,
    Connecting,
    Connected,
    PartiallyConnected,
    Paused,
    RestartRequired,
    InjectionFailed,
}

impl AdapterEndpointState {
    pub(crate) const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::NotDetected => "not_detected",
            Self::StaleEndpoint => "stale_endpoint",
            Self::UnmanagedTargetAvailable => "unmanaged_target_available",
            Self::UnmanagedTargetIncompatible => "unmanaged_target_incompatible",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::PartiallyConnected => "partially_connected",
            Self::Paused => "paused",
            Self::RestartRequired => "restart_required",
            Self::InjectionFailed => "injection_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSnapshot {
    state: AdapterEndpointState,
    last_error_code: Option<&'static str>,
    detected_instance_count: usize,
    connectable_instance_count: usize,
    available_target_count: usize,
    connected_target_count: usize,
    failed_target_count: usize,
    instances: Vec<AdapterInstanceProbe>,
}

pub struct CodexIntegrationRuntime {
    inner: Mutex<RuntimeSnapshot>,
}

#[derive(Debug, Default)]
pub struct ComposerOperationGate {
    active: Mutex<HashSet<String>>,
}

#[derive(Debug)]
struct ComposerOperationGuard<'a> {
    gate: &'a ComposerOperationGate,
    key: String,
}

#[derive(Default)]
pub struct CodexCardRefreshSignal {
    generation: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct DefaultIdentityProfile {
    app_data_dir: PathBuf,
    profile_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DefaultIdentityRuntimeState {
    Stopped,
    Running,
    Occupied,
    Unavailable,
}

impl DefaultIdentityRuntimeState {
    pub(crate) const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Running => "running",
            Self::Occupied => "occupied",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DefaultIdentityRuntimeProbe {
    pub platform_supported: bool,
    pub profile_present: bool,
    pub state: DefaultIdentityRuntimeState,
    pub last_error_code: Option<&'static str>,
}

impl DefaultIdentityProfile {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, AdapterRuntimeError> {
        if !app_data_dir.is_absolute() {
            return Err(AdapterRuntimeError::Action(
                "default_identity_app_data_invalid",
            ));
        }
        let metadata = fs::symlink_metadata(&app_data_dir)
            .map_err(|_| AdapterRuntimeError::Action("default_identity_app_data_unavailable"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AdapterRuntimeError::Action(
                "default_identity_app_data_invalid",
            ));
        }
        let app_data_dir = fs::canonicalize(app_data_dir)
            .map_err(|_| AdapterRuntimeError::Action("default_identity_app_data_unavailable"))?;
        let profile_dir = app_data_dir.join(DEFAULT_IDENTITY_PROFILE_DIRECTORY);
        Ok(Self {
            app_data_dir,
            profile_dir,
        })
    }

    pub fn ensure_ready(&self) -> Result<(), AdapterRuntimeError> {
        match fs::symlink_metadata(&self.profile_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(AdapterRuntimeError::Action(
                    "default_identity_profile_invalid",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&self.profile_dir).map_err(|_| {
                    AdapterRuntimeError::Action("default_identity_profile_create_failed")
                })?;
            }
            Err(_) => {
                return Err(AdapterRuntimeError::Action(
                    "default_identity_profile_unavailable",
                ));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.profile_dir, fs::Permissions::from_mode(0o700)).map_err(
                |_| AdapterRuntimeError::Action("default_identity_profile_permissions_failed"),
            )?;
        }
        self.secure_profile_exists().and_then(|exists| {
            if exists {
                Ok(())
            } else {
                Err(AdapterRuntimeError::Action(
                    "default_identity_profile_unavailable",
                ))
            }
        })
    }

    fn secure_profile_exists(&self) -> Result<bool, AdapterRuntimeError> {
        let metadata = match fs::symlink_metadata(&self.profile_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => {
                return Err(AdapterRuntimeError::Action(
                    "default_identity_profile_unavailable",
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AdapterRuntimeError::Action(
                "default_identity_profile_invalid",
            ));
        }
        let canonical = fs::canonicalize(&self.profile_dir)
            .map_err(|_| AdapterRuntimeError::Action("default_identity_profile_unavailable"))?;
        if canonical.parent() != Some(self.app_data_dir.as_path()) {
            return Err(AdapterRuntimeError::Action(
                "default_identity_profile_invalid",
            ));
        }
        Ok(true)
    }

    fn path(&self) -> &Path {
        &self.profile_dir
    }
}

impl CodexCardRefreshSignal {
    fn current(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }
}

impl ComposerOperationGate {
    fn try_acquire(
        &self,
        host_kind: &str,
        record_id: &str,
    ) -> Result<ComposerOperationGuard<'_>, AdapterRuntimeError> {
        let key = format!("{host_kind}:{record_id}");
        let mut active = self
            .active
            .lock()
            .map_err(|_| AdapterRuntimeError::Action("card_composer_operation_unavailable"))?;
        if !active.insert(key.clone()) {
            return Err(AdapterRuntimeError::Action("card_composer_operation_busy"));
        }
        Ok(ComposerOperationGuard { gate: self, key })
    }
}

impl Drop for ComposerOperationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.gate.active.lock() {
            active.remove(&self.key);
        }
    }
}

pub fn request_card_refresh(app: &AppHandle) {
    app.state::<CodexCardRefreshSignal>().bump();
}

impl Default for CodexIntegrationRuntime {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RuntimeSnapshot {
                state: AdapterEndpointState::NotDetected,
                last_error_code: None,
                detected_instance_count: 0,
                connectable_instance_count: 0,
                available_target_count: 0,
                connected_target_count: 0,
                failed_target_count: 0,
                instances: Vec::new(),
            }),
        }
    }
}

impl CodexIntegrationRuntime {
    pub fn start(app: AppHandle) -> Result<(), AdapterRuntimeError> {
        thread::Builder::new()
            .name("wakegpt-codex-adapter".to_owned())
            .spawn(move || supervisor_loop(app))
            .map(|_| ())
            .map_err(|_| AdapterRuntimeError::ThreadStart)
    }

    fn snapshot(&self) -> Result<RuntimeSnapshot, AdapterRuntimeError> {
        self.inner
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| AdapterRuntimeError::LockPoisoned)
    }

    fn replace(&self, state: AdapterEndpointState, last_error_code: Option<&'static str>) {
        if let Ok(mut snapshot) = self.inner.lock() {
            snapshot.state = state;
            snapshot.last_error_code = last_error_code;
        }
    }

    fn replace_snapshot(&self, next: RuntimeSnapshot) -> bool {
        if let Ok(mut snapshot) = self.inner.lock() {
            if *snapshot == next {
                return false;
            }
            *snapshot = next;
            return true;
        }
        false
    }

    pub(crate) fn record_restart_failure(&self, code: &'static str) {
        self.replace(AdapterEndpointState::RestartRequired, Some(code));
    }

    pub(crate) fn wait_until_paused(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.snapshot().is_ok_and(|snapshot| {
                snapshot.state == AdapterEndpointState::Paused
                    && snapshot.connected_target_count == 0
            }) {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[cfg(test)]
    fn replace_for_test(&self, next: RuntimeSnapshot) {
        *self.inner.lock().unwrap() = next;
    }
}

#[derive(Debug)]
pub enum AdapterRuntimeError {
    ThreadStart,
    LockPoisoned,
    Transport(&'static str),
    Protocol(&'static str),
    Action(&'static str),
}

impl AdapterRuntimeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ThreadStart => "codex_adapter_thread_start",
            Self::LockPoisoned => "codex_adapter_lock_unavailable",
            Self::Transport(code) | Self::Protocol(code) | Self::Action(code) => code,
        }
    }
}

impl fmt::Display for AdapterRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for AdapterRuntimeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdapterError {
    ActivePortUnavailable,
    ActivePortInvalid,
    EndpointUnavailable,
    HttpResponseInvalid,
    TargetListInvalid,
    TargetMissing,
    TargetAmbiguous,
    TargetUrlInvalid,
    TargetSocketInvalid,
    VersionUnavailable,
    VersionUnsupported,
}

impl AdapterError {
    const fn code(self) -> &'static str {
        match self {
            Self::ActivePortUnavailable => "codex_active_port_unavailable",
            Self::ActivePortInvalid => "codex_active_port_invalid",
            Self::EndpointUnavailable => "codex_debug_endpoint_unavailable",
            Self::HttpResponseInvalid => "codex_debug_http_invalid",
            Self::TargetListInvalid => "codex_target_list_invalid",
            Self::TargetMissing => "codex_app_target_missing",
            Self::TargetAmbiguous => "codex_app_target_ambiguous",
            Self::TargetUrlInvalid => "codex_app_target_url_invalid",
            Self::TargetSocketInvalid => "codex_app_target_socket_invalid",
            Self::VersionUnavailable => "codex_version_unverified",
            Self::VersionUnsupported => "codex_version_unsupported",
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for AdapterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DebugEndpoint {
    port: u16,
    browser_path: String,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexHostIdentity {
    version: String,
    build: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RawPageTarget {
    id: String,
    #[serde(rename = "type")]
    target_type: String,
    url: String,
    web_socket_debugger_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageTarget {
    process_id: u32,
    port: u16,
    id: String,
    web_socket_debugger_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TargetKey {
    process_id: u32,
    port: u16,
    id: String,
}

impl PageTarget {
    fn key(&self) -> TargetKey {
        TargetKey {
            process_id: self.process_id,
            port: self.port,
            id: self.id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DebugCandidate {
    process_id: u32,
    port: u16,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomInstanceRestartObservation {
    Missing,
    Starting { process_id: libc::pid_t },
    Incompatible { process_id: libc::pid_t },
    Compatible { process_id: libc::pid_t },
}

#[derive(Debug)]
struct TargetDiscovery {
    detected_instance_count: usize,
    connectable_instance_count: usize,
    targets: Vec<PageTarget>,
    last_error: Option<AdapterError>,
    instances: Vec<DiscoveredInstance>,
}

#[derive(Debug, Clone)]
struct RunningInstance {
    process_id: u32,
    is_default: bool,
    can_restart: bool,
    codex_version: Option<String>,
    codex_build: Option<String>,
    candidate: Option<DebugCandidate>,
}

#[derive(Debug, Clone)]
struct DiscoveredInstance {
    process_id: u32,
    is_default: bool,
    can_restart: bool,
    codex_version: Option<String>,
    codex_build: Option<String>,
    connectable: bool,
    target_keys: Vec<TargetKey>,
    last_error: Option<AdapterError>,
}

pub fn probe_unmanaged_endpoint() -> AdapterProbe {
    let card_script_sha256 = sha256_hex(CARD_SCRIPT.as_bytes());
    let discovery = discover_page_targets();
    let instances = build_instance_probes(
        &discovery,
        &HashSet::new(),
        &HashSet::new(),
        &HashMap::new(),
        false,
    );
    let (endpoint_state, last_error_code) = if !discovery.targets.is_empty() {
        (AdapterEndpointState::UnmanagedTargetAvailable, None)
    } else if discovery.detected_instance_count == 0 {
        (
            AdapterEndpointState::NotDetected,
            Some(AdapterError::ActivePortUnavailable.code()),
        )
    } else if discovery.connectable_instance_count == 0 {
        match discovery.last_error {
            Some(error @ (AdapterError::VersionUnavailable | AdapterError::VersionUnsupported)) => {
                (
                    AdapterEndpointState::UnmanagedTargetIncompatible,
                    Some(error.code()),
                )
            }
            _ => (
                AdapterEndpointState::RestartRequired,
                Some(AdapterError::ActivePortUnavailable.code()),
            ),
        }
    } else {
        match discovery.last_error.unwrap_or(AdapterError::TargetMissing) {
            AdapterError::EndpointUnavailable => (
                AdapterEndpointState::StaleEndpoint,
                Some(AdapterError::EndpointUnavailable.code()),
            ),
            error => (
                AdapterEndpointState::UnmanagedTargetIncompatible,
                Some(error.code()),
            ),
        }
    };

    AdapterProbe {
        adapter_version: ADAPTER_VERSION,
        supported_codex_version: SUPPORTED_CODEX_VERSION,
        supported_codex_build: SUPPORTED_CODEX_BUILD,
        endpoint_state,
        last_error_code,
        card_script_sha256,
        detected_instance_count: discovery.detected_instance_count,
        connectable_instance_count: discovery.connectable_instance_count,
        available_target_count: discovery.targets.len(),
        connected_target_count: 0,
        failed_target_count: 0,
        instances,
    }
}

pub fn runtime_probe(runtime: &CodexIntegrationRuntime) -> AdapterProbe {
    if let Ok(snapshot) = runtime.snapshot() {
        return AdapterProbe {
            adapter_version: ADAPTER_VERSION,
            supported_codex_version: SUPPORTED_CODEX_VERSION,
            supported_codex_build: SUPPORTED_CODEX_BUILD,
            endpoint_state: snapshot.state,
            last_error_code: snapshot.last_error_code,
            card_script_sha256: sha256_hex(CARD_SCRIPT.as_bytes()),
            detected_instance_count: snapshot.detected_instance_count,
            connectable_instance_count: snapshot.connectable_instance_count,
            available_target_count: snapshot.available_target_count,
            connected_target_count: snapshot.connected_target_count,
            failed_target_count: snapshot.failed_target_count,
            instances: snapshot.instances,
        };
    }
    probe_unmanaged_endpoint()
}

#[cfg(target_os = "macos")]
pub fn default_identity_runtime_probe(
    profile: &DefaultIdentityProfile,
) -> DefaultIdentityRuntimeProbe {
    let profile_present = match profile.secure_profile_exists() {
        Ok(value) => value,
        Err(error) => {
            return DefaultIdentityRuntimeProbe {
                platform_supported: true,
                profile_present: false,
                state: DefaultIdentityRuntimeState::Unavailable,
                last_error_code: Some(error.code()),
            };
        }
    };
    if !profile_present {
        return DefaultIdentityRuntimeProbe {
            platform_supported: true,
            profile_present: false,
            state: DefaultIdentityRuntimeState::Stopped,
            last_error_code: None,
        };
    }
    match default_identity_process_ids(profile.path()) {
        Ok(processes) if processes.is_empty() => DefaultIdentityRuntimeProbe {
            platform_supported: true,
            profile_present: true,
            state: DefaultIdentityRuntimeState::Stopped,
            last_error_code: None,
        },
        Ok(processes) if processes.len() == 1 => DefaultIdentityRuntimeProbe {
            platform_supported: true,
            profile_present: true,
            state: DefaultIdentityRuntimeState::Running,
            last_error_code: None,
        },
        Ok(_) => DefaultIdentityRuntimeProbe {
            platform_supported: true,
            profile_present: true,
            state: DefaultIdentityRuntimeState::Occupied,
            last_error_code: Some("default_identity_profile_occupied"),
        },
        Err(error) => DefaultIdentityRuntimeProbe {
            platform_supported: true,
            profile_present: true,
            state: DefaultIdentityRuntimeState::Unavailable,
            last_error_code: Some(error.code()),
        },
    }
}

#[cfg(not(target_os = "macos"))]
pub fn default_identity_runtime_probe(
    _profile: &DefaultIdentityProfile,
) -> DefaultIdentityRuntimeProbe {
    DefaultIdentityRuntimeProbe {
        platform_supported: false,
        profile_present: false,
        state: DefaultIdentityRuntimeState::Unavailable,
        last_error_code: Some("default_identity_unsupported_platform"),
    }
}

#[cfg(target_os = "macos")]
pub fn open_or_focus_default_identity(
    profile: &DefaultIdentityProfile,
) -> Result<(), AdapterRuntimeError> {
    profile.ensure_ready()?;
    match default_identity_process_ids(profile.path())? {
        processes if processes.len() > 1 => {
            return Err(AdapterRuntimeError::Action(
                "default_identity_profile_occupied",
            ));
        }
        processes if processes.len() == 1 => {
            return focus_chatgpt_process(processes[0]);
        }
        _ => {}
    }

    let active_port_path = trusted_active_port_path(
        &std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(AdapterRuntimeError::Action("chatgpt_home_unavailable"))?,
        profile.path(),
    )
    .ok_or(AdapterRuntimeError::Action(
        "default_identity_profile_invalid",
    ))?;
    remove_stale_active_port(&active_port_path)?;
    let app_bundle = chatgpt_app_bundle()?;
    let status = chatgpt_open_command(app_bundle, 0, Some(profile.path()))
        .status()
        .map_err(|_| AdapterRuntimeError::Action("default_identity_launch_failed"))?;
    if !status.success() {
        return Err(AdapterRuntimeError::Action(
            "default_identity_launch_failed",
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match default_identity_process_ids(profile.path())? {
            processes if processes.len() > 1 => {
                return Err(AdapterRuntimeError::Action(
                    "default_identity_profile_occupied",
                ));
            }
            processes if processes.len() == 1 => {
                return focus_chatgpt_process(processes[0]);
            }
            _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(200)),
            _ => {
                return Err(AdapterRuntimeError::Action(
                    "default_identity_start_timeout",
                ));
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_or_focus_default_identity(
    _profile: &DefaultIdentityProfile,
) -> Result<(), AdapterRuntimeError> {
    Err(AdapterRuntimeError::Action(
        "default_identity_unsupported_platform",
    ))
}

#[cfg(target_os = "macos")]
pub fn restart_codex_with_integration() -> Result<(), AdapterRuntimeError> {
    let app_bundle = chatgpt_app_bundle()?;
    verify_chatgpt_host_contract(app_bundle)?;
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(AdapterRuntimeError::Action("chatgpt_home_unavailable"))?;
    let default_user_data_dir = home
        .join("Library")
        .join("Application Support")
        .join("Codex");
    let mut default_process_ids = Vec::new();
    for index in 0..running.count() {
        let process_id = running.objectAtIndex(index).processIdentifier();
        if process_id > 0
            && macos_process_arguments(process_id).is_some_and(|arguments| {
                is_default_chatgpt_profile(&arguments, &default_user_data_dir)
            })
        {
            default_process_ids.push(process_id);
        }
    }
    if default_process_ids.len() > 1 {
        return Err(AdapterRuntimeError::Action(
            "chatgpt_default_target_ambiguous",
        ));
    }

    if let Some(default_process_id) = default_process_ids.first().copied() {
        for index in 0..running.count() {
            let process = running.objectAtIndex(index);
            if process.processIdentifier() == default_process_id && !process.terminate() {
                return Err(AdapterRuntimeError::Action(
                    "chatgpt_graceful_termination_failed",
                ));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let still_running =
                NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id)
                    .iter()
                    .any(|process| process.processIdentifier() == default_process_id);
            if !still_running {
                break;
            }
            if Instant::now() >= deadline {
                return Err(AdapterRuntimeError::Action("chatgpt_graceful_exit_timeout"));
            }
            thread::sleep(Duration::from_millis(150));
        }
    }

    if let Some(active_port_path) = default_active_port_path() {
        remove_stale_active_port(&active_port_path)?;
    }

    ensure_debug_port_available(CHATGPT_MANAGED_DEBUG_PORT)?;
    let status = chatgpt_open_command(app_bundle, CHATGPT_MANAGED_DEBUG_PORT, None)
        .status()
        .map_err(|_| AdapterRuntimeError::Action("chatgpt_launch_failed"))?;
    if !status.success() {
        return Err(AdapterRuntimeError::Action("chatgpt_launch_failed"));
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match find_page_target_at_port(CHATGPT_MANAGED_DEBUG_PORT) {
            Ok(_) => return Ok(()),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(200)),
            Err(_) => {
                return Err(AdapterRuntimeError::Action(
                    "chatgpt_debug_endpoint_start_timeout",
                ));
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn restart_codex_instance_with_integration(
    instance_id: &str,
) -> Result<(), AdapterRuntimeError> {
    let app_bundle = chatgpt_app_bundle()?;
    verify_chatgpt_host_contract(app_bundle)?;
    let process_id = parse_instance_process_id(instance_id)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(AdapterRuntimeError::Action("chatgpt_home_unavailable"))?;
    let default_user_data_dir = home
        .join("Library")
        .join("Application Support")
        .join("Codex");
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let mut selected_profile = None;
    for index in 0..running.count() {
        let process = running.objectAtIndex(index);
        if process.processIdentifier() != process_id {
            continue;
        }
        let arguments = macos_process_arguments(process_id).ok_or(AdapterRuntimeError::Action(
            "chatgpt_instance_arguments_unavailable",
        ))?;
        let profile = restartable_custom_profile_dir(&arguments, &home, &default_user_data_dir)
            .ok_or(AdapterRuntimeError::Action(
                "chatgpt_instance_restart_unavailable",
            ))?;
        if !process.terminate() {
            return Err(AdapterRuntimeError::Action(
                "chatgpt_graceful_termination_failed",
            ));
        }
        selected_profile = Some(profile);
        break;
    }
    let profile =
        selected_profile.ok_or(AdapterRuntimeError::Action("chatgpt_instance_not_found"))?;

    wait_for_custom_instance_exit(
        process_id,
        CHATGPT_GRACEFUL_EXIT_POLLS,
        CHATGPT_FORCED_EXIT_POLLS,
        |target_process_id| {
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id)
                .iter()
                .any(|process| process.processIdentifier() == target_process_id)
        },
        |target_process_id| {
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id)
                .iter()
                .find(|process| process.processIdentifier() == target_process_id)
                .is_some_and(|process| process.forceTerminate())
        },
        || thread::sleep(CHATGPT_EXIT_POLL_INTERVAL),
    )?;

    let active_port_path = trusted_active_port_path(&home, &profile).ok_or(
        AdapterRuntimeError::Action("chatgpt_instance_restart_unavailable"),
    )?;
    remove_stale_active_port(&active_port_path)?;
    let integrated_launch_succeeded = chatgpt_open_command(app_bundle, 0, Some(&profile))
        .status()
        .is_ok_and(|status| status.success());
    if !integrated_launch_succeeded {
        recover_custom_profile(app_bundle, &profile, &home, &default_user_data_dir)?;
        return Err(AdapterRuntimeError::Action(
            "chatgpt_integration_launch_failed_profile_recovered",
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let observation = observe_custom_instance_restart(&home, &default_user_data_dir, &profile)?;
        if matches!(
            observation,
            CustomInstanceRestartObservation::Compatible { .. }
                | CustomInstanceRestartObservation::Incompatible { .. }
        ) || Instant::now() >= deadline
        {
            return complete_custom_instance_restart(process_id, &[observation], || {
                recover_custom_profile(app_bundle, &profile, &home, &default_user_data_dir)
            })
            .map(|_| ());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
fn chatgpt_app_bundle() -> Result<&'static Path, AdapterRuntimeError> {
    CHATGPT_APP_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.is_dir() && path.join("Contents/MacOS/ChatGPT").is_file())
        .ok_or(AdapterRuntimeError::Action("chatgpt_application_not_found"))
}

#[cfg(target_os = "macos")]
fn default_identity_process_ids(profile: &Path) -> Result<Vec<libc::pid_t>, AdapterRuntimeError> {
    let canonical_profile = fs::canonicalize(profile)
        .map_err(|_| AdapterRuntimeError::Action("default_identity_profile_unavailable"))?;
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let mut processes = Vec::new();
    for index in 0..running.count() {
        let process_id = running.objectAtIndex(index).processIdentifier();
        if process_id <= 0 {
            continue;
        }
        let arguments = macos_process_arguments(process_id).ok_or(AdapterRuntimeError::Action(
            "default_identity_process_arguments_unavailable",
        ))?;
        let user_data_dir =
            single_process_argument(&arguments, "--user-data-dir").map_err(|_| {
                AdapterRuntimeError::Action("default_identity_process_arguments_invalid")
            })?;
        let Some(user_data_dir) = user_data_dir else {
            continue;
        };
        let Ok(candidate) = fs::canonicalize(user_data_dir) else {
            continue;
        };
        if candidate == canonical_profile {
            processes.push(process_id);
        }
    }
    Ok(processes)
}

#[cfg(target_os = "macos")]
fn focus_chatgpt_process(process_id: libc::pid_t) -> Result<(), AdapterRuntimeError> {
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    for index in 0..running.count() {
        let process = running.objectAtIndex(index);
        if process.processIdentifier() == process_id {
            return process
                .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
                .then_some(())
                .ok_or(AdapterRuntimeError::Action("default_identity_focus_failed"));
        }
    }
    Err(AdapterRuntimeError::Action(
        "default_identity_process_not_found",
    ))
}

#[cfg(target_os = "macos")]
fn parse_instance_process_id(instance_id: &str) -> Result<libc::pid_t, AdapterRuntimeError> {
    let value = instance_id
        .strip_prefix("chatgpt-")
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(AdapterRuntimeError::Action("chatgpt_instance_id_invalid"))?;
    let process_id = value
        .parse::<u32>()
        .ok()
        .and_then(|value| libc::pid_t::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(AdapterRuntimeError::Action("chatgpt_instance_id_invalid"))?;
    Ok(process_id)
}

#[cfg(target_os = "macos")]
fn wait_for_custom_instance_exit<IsRunning, ForceTerminate, Pause>(
    process_id: libc::pid_t,
    graceful_poll_limit: usize,
    forced_poll_limit: usize,
    mut is_running: IsRunning,
    mut force_terminate: ForceTerminate,
    mut pause: Pause,
) -> Result<(), AdapterRuntimeError>
where
    IsRunning: FnMut(libc::pid_t) -> bool,
    ForceTerminate: FnMut(libc::pid_t) -> bool,
    Pause: FnMut(),
{
    for poll in 0..=graceful_poll_limit {
        if !is_running(process_id) {
            return Ok(());
        }
        if poll < graceful_poll_limit {
            pause();
        }
    }
    if !force_terminate(process_id) {
        if !is_running(process_id) {
            return Ok(());
        }
        return Err(AdapterRuntimeError::Action(
            "chatgpt_force_termination_failed",
        ));
    }
    for poll in 0..=forced_poll_limit {
        if !is_running(process_id) {
            return Ok(());
        }
        if poll < forced_poll_limit {
            pause();
        }
    }
    Err(AdapterRuntimeError::Action("chatgpt_force_exit_timeout"))
}

#[cfg(target_os = "macos")]
fn matching_custom_profile_processes(
    home: &Path,
    default_user_data_dir: &Path,
    profile: &Path,
) -> Result<Vec<(libc::pid_t, Vec<String>)>, AdapterRuntimeError> {
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let mut matches = Vec::new();
    for index in 0..running.count() {
        let process = running.objectAtIndex(index);
        let process_id = process.processIdentifier();
        if process_id <= 0 {
            continue;
        }
        let arguments = macos_process_arguments(process_id).ok_or(AdapterRuntimeError::Action(
            "chatgpt_instance_arguments_unavailable",
        ))?;
        if restartable_custom_profile_dir(&arguments, home, default_user_data_dir).as_deref()
            != Some(profile)
        {
            continue;
        }
        matches.push((process_id, arguments));
        if matches.len() > 1 {
            return Err(AdapterRuntimeError::Action("chatgpt_profile_ambiguous"));
        }
    }
    Ok(matches)
}

#[cfg(target_os = "macos")]
fn has_loopback_debug_arguments(arguments: &[String]) -> bool {
    if !is_chatgpt_executable(arguments.first().map(String::as_str)) {
        return false;
    }
    let address = single_process_argument(arguments, "--remote-debugging-address");
    let port = single_process_argument(arguments, "--remote-debugging-port");
    matches!(address, Ok(Some("127.0.0.1")))
        && matches!(port, Ok(Some(value)) if value.parse::<u16>().is_ok())
}

#[cfg(target_os = "macos")]
fn observe_custom_instance_restart(
    home: &Path,
    default_user_data_dir: &Path,
    profile: &Path,
) -> Result<CustomInstanceRestartObservation, AdapterRuntimeError> {
    let mut matches = matching_custom_profile_processes(home, default_user_data_dir, profile)?;
    let Some((process_id, arguments)) = matches.pop() else {
        return Ok(CustomInstanceRestartObservation::Missing);
    };
    if !has_loopback_debug_arguments(&arguments) {
        return Ok(CustomInstanceRestartObservation::Incompatible { process_id });
    }
    let Some(port) = chatgpt_debug_port(&arguments, home) else {
        return Ok(CustomInstanceRestartObservation::Starting { process_id });
    };
    if find_page_targets_at_port(DebugCandidate {
        process_id: process_id as u32,
        port,
    })
    .is_ok()
    {
        Ok(CustomInstanceRestartObservation::Compatible { process_id })
    } else {
        Ok(CustomInstanceRestartObservation::Starting { process_id })
    }
}

#[cfg(target_os = "macos")]
fn complete_custom_instance_restart<Fallback>(
    old_process_id: libc::pid_t,
    observations: &[CustomInstanceRestartObservation],
    fallback_launch: Fallback,
) -> Result<libc::pid_t, AdapterRuntimeError>
where
    Fallback: FnOnce() -> Result<(), AdapterRuntimeError>,
{
    let mut saw_starting = false;
    for observation in observations {
        match *observation {
            CustomInstanceRestartObservation::Missing => {}
            CustomInstanceRestartObservation::Starting { process_id } => {
                if process_id == old_process_id {
                    return Err(AdapterRuntimeError::Action(
                        "chatgpt_replacement_pid_invalid",
                    ));
                }
                saw_starting = true;
            }
            CustomInstanceRestartObservation::Incompatible { process_id } => {
                if process_id == old_process_id {
                    return Err(AdapterRuntimeError::Action(
                        "chatgpt_replacement_pid_invalid",
                    ));
                }
                return Err(AdapterRuntimeError::Action(
                    "chatgpt_profile_claimed_without_debugging",
                ));
            }
            CustomInstanceRestartObservation::Compatible { process_id } => {
                if process_id == old_process_id {
                    return Err(AdapterRuntimeError::Action(
                        "chatgpt_replacement_pid_invalid",
                    ));
                }
                return Ok(process_id);
            }
        }
    }
    if saw_starting {
        return Err(AdapterRuntimeError::Action(
            "chatgpt_debug_endpoint_start_timeout",
        ));
    }
    fallback_launch()?;
    Err(AdapterRuntimeError::Action(
        "chatgpt_integration_start_timeout_profile_recovered",
    ))
}

#[cfg(target_os = "macos")]
fn recover_custom_profile(
    app_bundle: &Path,
    profile: &Path,
    home: &Path,
    default_user_data_dir: &Path,
) -> Result<(), AdapterRuntimeError> {
    let status = chatgpt_open_command_without_integration(app_bundle, profile)
        .status()
        .map_err(|_| AdapterRuntimeError::Action("chatgpt_profile_recovery_launch_failed"))?;
    if !status.success() {
        return Err(AdapterRuntimeError::Action(
            "chatgpt_profile_recovery_launch_failed",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if !matching_custom_profile_processes(home, default_user_data_dir, profile)?.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AdapterRuntimeError::Action(
                "chatgpt_profile_recovery_start_timeout",
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
fn remove_stale_active_port(path: &Path) -> Result<(), AdapterRuntimeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= MAX_ACTIVE_PORT_BYTES =>
        {
            fs::remove_file(path).map_err(|_| {
                AdapterRuntimeError::Action("codex_stale_active_port_remove_failed")
            })?;
        }
        Ok(_) => {
            return Err(AdapterRuntimeError::Action(
                "codex_active_port_identity_invalid",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(AdapterRuntimeError::Action(
                "codex_active_port_metadata_failed",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_debug_port_available(port: u16) -> Result<(), AdapterRuntimeError> {
    std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map(|_| ())
        .map_err(|_| AdapterRuntimeError::Action("chatgpt_debug_port_unavailable"))
}

#[cfg(target_os = "macos")]
fn chatgpt_open_command(app_bundle: &Path, port: u16, user_data_dir: Option<&Path>) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command
        .arg("-n")
        .arg("-a")
        .arg(app_bundle)
        .arg("--args")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"));
    if let Some(user_data_dir) = user_data_dir {
        command.arg("--user-data-dir").arg(user_data_dir);
    }
    command
}

#[cfg(target_os = "macos")]
fn chatgpt_open_command_without_integration(app_bundle: &Path, user_data_dir: &Path) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command
        .arg("-n")
        .arg("-a")
        .arg(app_bundle)
        .arg("--args")
        .arg("--user-data-dir")
        .arg(user_data_dir);
    command
}

#[cfg(not(target_os = "macos"))]
pub fn restart_codex_with_integration() -> Result<(), AdapterRuntimeError> {
    Err(AdapterRuntimeError::Action(
        "codex_managed_launch_unsupported_platform",
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn restart_codex_instance_with_integration(
    _instance_id: &str,
) -> Result<(), AdapterRuntimeError> {
    Err(AdapterRuntimeError::Action(
        "codex_managed_launch_unsupported_platform",
    ))
}

fn discover_page_targets() -> TargetDiscovery {
    #[cfg(target_os = "macos")]
    {
        discover_targets_for_instances(&running_chatgpt_instances())
    }
    #[cfg(not(target_os = "macos"))]
    TargetDiscovery {
        detected_instance_count: 0,
        connectable_instance_count: 0,
        targets: Vec::new(),
        last_error: Some(AdapterError::ActivePortUnavailable),
        instances: Vec::new(),
    }
}

fn discover_targets_for_instances(instances: &[RunningInstance]) -> TargetDiscovery {
    let mut targets = Vec::new();
    let mut target_keys = HashSet::new();
    let mut last_error = None;
    let mut discovered_instances = Vec::with_capacity(instances.len());
    for instance in instances {
        let mut instance_target_keys = Vec::new();
        let version_error = codex_host_contract_error(
            instance.codex_version.as_deref(),
            instance.codex_build.as_deref(),
        );
        let instance_error = if let Some(error) = version_error {
            Some(error)
        } else if let Some(candidate) = instance.candidate {
            match find_page_targets_at_port(candidate) {
                Ok(found) => {
                    for target in found {
                        let key = target.key();
                        instance_target_keys.push(key.clone());
                        if target_keys.insert(key) {
                            targets.push(target);
                        }
                    }
                    None
                }
                Err(error) => Some(error),
            }
        } else {
            Some(AdapterError::ActivePortUnavailable)
        };
        if let Some(error) = instance_error {
            if last_error.is_none() || error != AdapterError::EndpointUnavailable {
                last_error = Some(error);
            }
        }
        discovered_instances.push(DiscoveredInstance {
            process_id: instance.process_id,
            is_default: instance.is_default,
            can_restart: instance.can_restart,
            codex_version: instance.codex_version.clone(),
            codex_build: instance.codex_build.clone(),
            connectable: instance.candidate.is_some() && version_error.is_none(),
            target_keys: instance_target_keys,
            last_error: instance_error,
        });
    }
    targets.sort_by(|left, right| {
        (left.process_id, left.port, left.id.as_str()).cmp(&(
            right.process_id,
            right.port,
            right.id.as_str(),
        ))
    });
    TargetDiscovery {
        detected_instance_count: instances.len(),
        connectable_instance_count: instances
            .iter()
            .filter(|instance| {
                instance.candidate.is_some()
                    && instance
                        .codex_version
                        .as_deref()
                        .zip(instance.codex_build.as_deref())
                        .is_some_and(|(version, build)| is_supported_codex_host(version, build))
            })
            .count(),
        targets,
        last_error: last_error.or_else(|| {
            instances
                .iter()
                .all(|instance| instance.candidate.is_none())
                .then_some(AdapterError::ActivePortUnavailable)
        }),
        instances: discovered_instances,
    }
}

#[cfg(target_os = "macos")]
fn running_chatgpt_instances() -> Vec<RunningInstance> {
    let bundle_id = NSString::from_str(CHATGPT_BUNDLE_ID);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let default_user_data_dir = home.as_ref().map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("Codex")
    });
    let mut instances = Vec::new();
    for index in 0..running.count() {
        let process = running.objectAtIndex(index);
        let pid = process.processIdentifier();
        if pid <= 0 {
            continue;
        }
        let arguments = macos_process_arguments(pid);
        let host_identity = arguments
            .as_deref()
            .and_then(|arguments| arguments.first())
            .and_then(|executable| chatgpt_bundle_identity(executable));
        let is_default = arguments
            .as_deref()
            .zip(default_user_data_dir.as_deref())
            .is_some_and(|(arguments, default_dir)| {
                is_default_chatgpt_profile(arguments, default_dir)
            });
        let candidate = arguments
            .as_deref()
            .zip(home.as_deref())
            .and_then(|(arguments, home)| chatgpt_debug_port(arguments, home))
            .map(|port| DebugCandidate {
                process_id: pid as u32,
                port,
            });
        let can_restart = arguments
            .as_deref()
            .zip(home.as_deref())
            .zip(default_user_data_dir.as_deref())
            .is_some_and(|((arguments, home), default_dir)| {
                restartable_custom_profile_dir(arguments, home, default_dir).is_some()
            });
        instances.push(RunningInstance {
            process_id: pid as u32,
            is_default,
            can_restart,
            codex_version: host_identity
                .as_ref()
                .map(|identity| identity.version.clone()),
            codex_build: host_identity.map(|identity| identity.build),
            candidate,
        });
    }
    let mut port_claims = HashMap::<u16, usize>::new();
    for candidate in instances.iter().filter_map(|instance| instance.candidate) {
        *port_claims.entry(candidate.port).or_default() += 1;
    }
    for instance in &mut instances {
        if instance
            .candidate
            .is_some_and(|candidate| port_claims.get(&candidate.port) != Some(&1))
        {
            instance.candidate = None;
        }
    }
    instances.sort_by_key(|instance| (!instance.is_default, instance.process_id));
    instances
}

#[cfg(target_os = "macos")]
fn is_default_chatgpt_profile(arguments: &[String], default_user_data_dir: &Path) -> bool {
    if !is_chatgpt_executable(arguments.first().map(String::as_str)) {
        return false;
    }
    match single_process_argument(arguments, "--user-data-dir") {
        Ok(None) => true,
        Ok(Some(value)) => Path::new(value) == default_user_data_dir,
        Err(()) => false,
    }
}

#[cfg(target_os = "macos")]
fn restartable_custom_profile_dir(
    arguments: &[String],
    home: &Path,
    default_user_data_dir: &Path,
) -> Option<PathBuf> {
    if !is_chatgpt_executable(arguments.first().map(String::as_str)) {
        return None;
    }
    let user_data_dir = Path::new(single_process_argument(arguments, "--user-data-dir").ok()??);
    if user_data_dir == default_user_data_dir {
        return None;
    }
    trusted_profile_dir(home, user_data_dir)
}

#[cfg(target_os = "macos")]
fn is_chatgpt_executable(executable: Option<&str>) -> bool {
    let Some(executable) = executable else {
        return false;
    };
    CHATGPT_APP_CANDIDATES
        .iter()
        .any(|bundle| executable == format!("{bundle}/Contents/MacOS/ChatGPT"))
}

fn is_supported_codex_host(version: &str, build: &str) -> bool {
    version == SUPPORTED_CODEX_VERSION && build == SUPPORTED_CODEX_BUILD
}

fn codex_host_contract_error(version: Option<&str>, build: Option<&str>) -> Option<AdapterError> {
    match (version, build) {
        (Some(version), Some(build)) if is_supported_codex_host(version, build) => None,
        (Some(_), Some(_)) => Some(AdapterError::VersionUnsupported),
        _ => Some(AdapterError::VersionUnavailable),
    }
}

#[cfg(target_os = "macos")]
fn chatgpt_bundle_identity(executable: &str) -> Option<CodexHostIdentity> {
    let bundle_path = CHATGPT_APP_CANDIDATES
        .iter()
        .copied()
        .find(|bundle| executable == format!("{bundle}/Contents/MacOS/ChatGPT"))?;
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(bundle_path))?;
    if bundle.bundleIdentifier()?.to_string() != CHATGPT_BUNDLE_ID {
        return None;
    }
    let string_value = |key: &str| {
        bundle
            .objectForInfoDictionaryKey(&NSString::from_str(key))?
            .downcast::<NSString>()
            .ok()
            .map(|value| value.to_string())
    };
    let version = string_value("CFBundleShortVersionString")?;
    let build = string_value("CFBundleVersion")?;
    let mut components = version.split('.');
    let version_valid = (0..3).all(|_| {
        components.next().is_some_and(|component| {
            !component.is_empty()
                && component.len() <= 8
                && component.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && components.next().is_none();
    let build_valid =
        !build.is_empty() && build.len() <= 12 && build.bytes().all(|byte| byte.is_ascii_digit());
    (version_valid && build_valid).then_some(CodexHostIdentity { version, build })
}

#[cfg(target_os = "macos")]
fn verify_chatgpt_host_contract(app_bundle: &Path) -> Result<(), AdapterRuntimeError> {
    let executable = app_bundle.join("Contents/MacOS/ChatGPT");
    let identity = executable
        .to_str()
        .and_then(chatgpt_bundle_identity)
        .ok_or(AdapterRuntimeError::Action("codex_version_unverified"))?;
    match codex_host_contract_error(Some(&identity.version), Some(&identity.build)) {
        None => Ok(()),
        Some(error) => Err(AdapterRuntimeError::Action(error.code())),
    }
}

#[cfg(target_os = "macos")]
fn verify_running_chatgpt_host_contract(process_id: u32) -> Result<(), AdapterRuntimeError> {
    let process_id = libc::pid_t::try_from(process_id)
        .ok()
        .filter(|process_id| *process_id > 0)
        .ok_or(AdapterRuntimeError::Action("codex_version_unverified"))?;
    let identity = macos_process_arguments(process_id)
        .and_then(|arguments| arguments.into_iter().next())
        .and_then(|executable| chatgpt_bundle_identity(&executable))
        .ok_or(AdapterRuntimeError::Action("codex_version_unverified"))?;
    match codex_host_contract_error(Some(&identity.version), Some(&identity.build)) {
        None => Ok(()),
        Some(error) => Err(AdapterRuntimeError::Action(error.code())),
    }
}

#[cfg(target_os = "macos")]
fn macos_process_arguments(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut query = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut length = 0_usize;
    // SAFETY: sysctl receives a fixed three-element read-only MIB and a valid length pointer.
    if unsafe {
        libc::sysctl(
            query.as_mut_ptr(),
            query.len() as _,
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || !(std::mem::size_of::<libc::c_int>()..=MAX_PROCESS_ARGUMENT_BYTES).contains(&length)
    {
        return None;
    }
    let mut buffer = vec![0_u8; length];
    // SAFETY: buffer owns `length` writable bytes and sysctl may only reduce the returned length.
    if unsafe {
        libc::sysctl(
            query.as_mut_ptr(),
            query.len() as _,
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || length > buffer.len()
    {
        return None;
    }
    buffer.truncate(length);
    parse_macos_process_arguments(&buffer)
}

#[cfg(target_os = "macos")]
fn parse_macos_process_arguments(buffer: &[u8]) -> Option<Vec<String>> {
    let integer_size = std::mem::size_of::<libc::c_int>();
    let argument_count = libc::c_int::from_ne_bytes(buffer.get(..integer_size)?.try_into().ok()?);
    if !(1..=512).contains(&argument_count) {
        return None;
    }
    let mut cursor = integer_size;
    cursor += buffer.get(cursor..)?.iter().position(|byte| *byte == 0)? + 1;
    while buffer.get(cursor) == Some(&0) {
        cursor += 1;
    }
    let mut arguments = Vec::with_capacity(argument_count as usize);
    for _ in 0..argument_count {
        let remaining = buffer.get(cursor..)?;
        let end = remaining.iter().position(|byte| *byte == 0)?;
        let argument = std::str::from_utf8(&remaining[..end]).ok()?.to_owned();
        arguments.push(argument);
        cursor += end + 1;
    }
    Some(arguments)
}

#[cfg(target_os = "macos")]
fn chatgpt_debug_port(arguments: &[String], home: &Path) -> Option<u16> {
    let executable = arguments.first()?.as_str();
    let expected_executable = CHATGPT_APP_CANDIDATES
        .iter()
        .any(|bundle| executable == format!("{bundle}/Contents/MacOS/ChatGPT"));
    if !expected_executable {
        return None;
    }
    let address = single_process_argument(arguments, "--remote-debugging-address").ok()??;
    if address != "127.0.0.1" {
        return None;
    }
    let port = single_process_argument(arguments, "--remote-debugging-port")
        .ok()??
        .parse::<u16>()
        .ok()?;
    if port > 0 {
        return Some(port);
    }
    let user_data_dir = single_process_argument(arguments, "--user-data-dir")
        .ok()?
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home.join("Library")
                .join("Application Support")
                .join("Codex")
        });
    let active_port_path = trusted_active_port_path(home, &user_data_dir)?;
    read_active_port(&active_port_path)
        .ok()
        .map(|endpoint| endpoint.port)
}

#[cfg(target_os = "macos")]
fn trusted_active_port_path(home: &Path, user_data_dir: &Path) -> Option<PathBuf> {
    trusted_profile_dir(home, user_data_dir).map(|path| path.join("DevToolsActivePort"))
}

#[cfg(target_os = "macos")]
fn trusted_profile_dir(home: &Path, user_data_dir: &Path) -> Option<PathBuf> {
    if !user_data_dir.is_absolute()
        || user_data_dir
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let canonical_home = fs::canonicalize(home).ok()?;
    let canonical_data_dir = fs::canonicalize(user_data_dir).ok()?;
    if !canonical_data_dir.starts_with(canonical_home) {
        return None;
    }
    Some(canonical_data_dir)
}

#[cfg(target_os = "macos")]
fn single_process_argument<'a>(arguments: &'a [String], name: &str) -> Result<Option<&'a str>, ()> {
    let prefix = format!("{name}=");
    let mut value = None;
    let mut index = 1;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        let next = if argument == name {
            index += 1;
            arguments.get(index).map(String::as_str).ok_or(())?
        } else if let Some(next) = argument.strip_prefix(&prefix) {
            next
        } else {
            index += 1;
            continue;
        };
        if next.is_empty() || value.replace(next).is_some() {
            return Err(());
        }
        index += 1;
    }
    Ok(value)
}

fn find_page_targets_at_port(candidate: DebugCandidate) -> Result<Vec<PageTarget>, AdapterError> {
    let json = fetch_target_list(candidate.port)?;
    parse_page_targets(&json, candidate)
}

fn find_page_target_at_port(port: u16) -> Result<PageTarget, AdapterError> {
    find_page_targets_at_port(DebugCandidate {
        process_id: 0,
        port,
    })?
    .into_iter()
    .next()
    .ok_or(AdapterError::TargetMissing)
}

fn default_active_port_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return None;
    }
    Some(
        home.join("Library")
            .join("Application Support")
            .join("Codex")
            .join("DevToolsActivePort"),
    )
}

fn read_active_port(path: &Path) -> Result<DebugEndpoint, AdapterError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AdapterError::ActivePortUnavailable
        } else {
            AdapterError::ActivePortInvalid
        }
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_ACTIVE_PORT_BYTES
    {
        return Err(AdapterError::ActivePortInvalid);
    }
    let contents = fs::read_to_string(path).map_err(|_| AdapterError::ActivePortInvalid)?;
    parse_active_port(&contents)
}

fn parse_active_port(contents: &str) -> Result<DebugEndpoint, AdapterError> {
    let mut lines = contents.lines();
    let port_text = lines.next().ok_or(AdapterError::ActivePortInvalid)?;
    let browser_path = lines.next().ok_or(AdapterError::ActivePortInvalid)?;
    if lines.any(|line| !line.is_empty())
        || port_text != port_text.trim()
        || browser_path != browser_path.trim()
    {
        return Err(AdapterError::ActivePortInvalid);
    }
    let port = port_text
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or(AdapterError::ActivePortInvalid)?;
    let token = browser_path
        .strip_prefix("/devtools/browser/")
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or(AdapterError::ActivePortInvalid)?;
    if !token
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AdapterError::ActivePortInvalid);
    }
    Ok(DebugEndpoint {
        port,
        browser_path: browser_path.to_owned(),
    })
}

fn fetch_target_list(port: u16) -> Result<Vec<u8>, AdapterError> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    // Keep the loopback probe bounded while allowing a cold local endpoint to accept.
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address.into(), timeout)
        .map_err(|_| AdapterError::EndpointUnavailable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| AdapterError::EndpointUnavailable)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| AdapterError::EndpointUnavailable)?;
    let request = format!(
        "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| AdapterError::EndpointUnavailable)?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if let Some(expected_len) = declared_http_response_len(&response)? {
            match response.len().cmp(&expected_len) {
                std::cmp::Ordering::Equal => return parse_http_json_response(&response),
                std::cmp::Ordering::Greater => return Err(AdapterError::HttpResponseInvalid),
                std::cmp::Ordering::Less => {}
            }
        }
        if response.len() >= MAX_HTTP_RESPONSE_BYTES {
            return Err(AdapterError::HttpResponseInvalid);
        }
        let remaining = MAX_HTTP_RESPONSE_BYTES + 1 - response.len();
        let read_limit = chunk.len().min(remaining);
        let read = stream
            .read(&mut chunk[..read_limit])
            .map_err(|_| AdapterError::EndpointUnavailable)?;
        if read == 0 {
            return parse_http_json_response(&response);
        }
        response.extend_from_slice(&chunk[..read]);
    }
}

fn http_header_separator(response: &[u8]) -> Option<usize> {
    response.windows(4).position(|window| window == b"\r\n\r\n")
}

fn declared_http_response_len(response: &[u8]) -> Result<Option<usize>, AdapterError> {
    let Some(separator) = http_header_separator(response) else {
        return if response.len() > MAX_HTTP_HEADER_BYTES {
            Err(AdapterError::HttpResponseInvalid)
        } else {
            Ok(None)
        };
    };
    if separator > MAX_HTTP_HEADER_BYTES {
        return Err(AdapterError::HttpResponseInvalid);
    }
    let headers = std::str::from_utf8(&response[..separator])
        .map_err(|_| AdapterError::HttpResponseInvalid)?;
    let mut lines = headers.lines();
    let status = lines.next().ok_or(AdapterError::HttpResponseInvalid)?;
    if !matches!(status, "HTTP/1.1 200 OK" | "HTTP/1.0 200 OK") {
        return Err(AdapterError::HttpResponseInvalid);
    }
    let mut content_length = None;
    for line in lines {
        if line
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
        {
            return Err(AdapterError::HttpResponseInvalid);
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(AdapterError::HttpResponseInvalid)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(AdapterError::HttpResponseInvalid);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(AdapterError::HttpResponseInvalid);
            }
            let value = value.trim();
            content_length = Some(
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|length| *length > 0)
                    .ok_or(AdapterError::HttpResponseInvalid)?,
            );
        }
    }
    let body_length = content_length.ok_or(AdapterError::HttpResponseInvalid)?;
    let total_length = separator
        .checked_add(4)
        .and_then(|length| length.checked_add(body_length))
        .filter(|length| *length <= MAX_HTTP_RESPONSE_BYTES)
        .ok_or(AdapterError::HttpResponseInvalid)?;
    Ok(Some(total_length))
}

fn parse_http_json_response(response: &[u8]) -> Result<Vec<u8>, AdapterError> {
    let expected_length =
        declared_http_response_len(response)?.ok_or(AdapterError::HttpResponseInvalid)?;
    if response.len() != expected_length {
        return Err(AdapterError::HttpResponseInvalid);
    }
    let separator = http_header_separator(response).ok_or(AdapterError::HttpResponseInvalid)?;
    let body = &response[separator + 4..];
    if body.is_empty() {
        return Err(AdapterError::HttpResponseInvalid);
    }
    Ok(body.to_vec())
}

fn parse_page_targets(
    json: &[u8],
    candidate: DebugCandidate,
) -> Result<Vec<PageTarget>, AdapterError> {
    let targets: Vec<RawPageTarget> =
        serde_json::from_slice(json).map_err(|_| AdapterError::TargetListInvalid)?;
    if targets.len() > 64 {
        return Err(AdapterError::TargetListInvalid);
    }
    let mut matches = Vec::new();
    let mut target_ids = HashSet::new();
    for target in targets
        .into_iter()
        .filter(|target| target.target_type == "page" && target.url == CODEX_TARGET_URL)
    {
        validate_target_id(&target.id)?;
        if !target_ids.insert(target.id.clone()) {
            return Err(AdapterError::TargetAmbiguous);
        }
        validate_websocket_url(&target.web_socket_debugger_url, candidate.port, &target.id)?;
        matches.push(PageTarget {
            process_id: candidate.process_id,
            port: candidate.port,
            id: target.id,
            web_socket_debugger_url: target.web_socket_debugger_url,
        });
    }
    if matches.is_empty() {
        return Err(AdapterError::TargetMissing);
    }
    matches.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(matches)
}

fn validate_target_id(value: &str) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AdapterError::TargetUrlInvalid);
    }
    Ok(())
}

fn validate_websocket_url(value: &str, port: u16, target_id: &str) -> Result<(), AdapterError> {
    let expected_path = format!("/devtools/page/{target_id}");
    let loopback = format!("ws://127.0.0.1:{port}{expected_path}");
    let localhost = format!("ws://localhost:{port}{expected_path}");
    if value == loopback || value == localhost {
        Ok(())
    } else {
        Err(AdapterError::TargetSocketInvalid)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

type CdpSocket = WebSocket<TcpStream>;

#[derive(Debug)]
struct CardSession {
    nonce: String,
    binding_name: String,
    workspace_id: Option<String>,
    notebook_id: Option<String>,
    selection_initialized: bool,
    pending_attachments: Vec<crate::attachments::PendingAttachment>,
    composer_target_scope: String,
    composer_host_kind: Option<String>,
    pending_create_ack: Option<(String, Option<String>)>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindingRequest {
    schema_version: u32,
    adapter_version: String,
    session_nonce: String,
    request_id: String,
    action: String,
    #[serde(default)]
    data: Value,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CardImageUpload {
    index: usize,
    byte_size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardWorkspace {
    id: String,
    display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardNotebook {
    id: String,
    display_name: String,
    target_state: String,
    last_error_code: Option<String>,
    numbering_start: u32,
    numbering_sync_pending: bool,
    attachment_directory: String,
    attachment_directory_sync_pending: bool,
    is_pinned: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardRecord {
    id: String,
    body_markdown: String,
    revision: u64,
    created_label: String,
    attachment_count: usize,
    attachments: Vec<CardRecordAttachment>,
    attachments_ready: bool,
    is_pinned: bool,
    insertion_state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardRecordAttachment {
    id: String,
    media_type: String,
    byte_size: u64,
    available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardPendingImage {
    token: String,
    media_type: String,
    byte_size: u64,
    display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardState {
    schema_version: u32,
    workspaces: Vec<CardWorkspace>,
    selected_workspace_id: Option<String>,
    workspace_name: String,
    notebooks: Vec<CardNotebook>,
    selected_notebook_id: Option<String>,
    recent_records: Vec<CardRecord>,
    pending_image_count: usize,
    pending_images: Vec<CardPendingImage>,
    draft_body_markdown: String,
    selection_key: String,
    submit_shortcut: SubmitShortcut,
    paused: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardDataChanged {
    workspace_id: String,
    notebook_id: Option<String>,
    change_kind: &'static str,
    source: &'static str,
}

pub(crate) fn emit_card_data_changed(
    app: &AppHandle,
    workspace_id: &str,
    notebook_id: Option<&str>,
    change_kind: &'static str,
) {
    request_card_refresh(app);
    let _ = app.emit(
        "wakegpt://data-changed",
        CardDataChanged {
            workspace_id: workspace_id.to_owned(),
            notebook_id: notebook_id.map(str::to_owned),
            change_kind,
            source: "backend",
        },
    );
}

fn card_draft_attachment(attachment: &crate::attachments::PendingAttachment) -> DraftAttachment {
    DraftAttachment {
        token: attachment.token.clone(),
        media_type: attachment.media_type.clone(),
        byte_size: attachment.byte_size,
        content_sha256: attachment.content_sha256.clone(),
        display_name: attachment.display_name.clone(),
    }
}

fn save_card_draft(
    app: &AppHandle,
    session: &CardSession,
    body_markdown: &str,
) -> Result<(), AdapterRuntimeError> {
    let Some(workspace_id) = session.workspace_id.as_deref() else {
        return Ok(());
    };
    let attachments = session
        .pending_attachments
        .iter()
        .map(card_draft_attachment)
        .collect::<Vec<_>>();
    app.state::<Store>()
        .save_draft(
            workspace_id,
            session.notebook_id.as_deref(),
            body_markdown,
            &attachments,
        )
        .map_err(|_| AdapterRuntimeError::Action("card_draft_save_failed"))?;
    emit_card_data_changed(app, workspace_id, session.notebook_id.as_deref(), "draft");
    Ok(())
}

struct SessionWorker {
    connected: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    handle: thread::JoinHandle<Result<(), AdapterRuntimeError>>,
}

fn build_instance_probes(
    discovery: &TargetDiscovery,
    active_keys: &HashSet<TargetKey>,
    connected_keys: &HashSet<TargetKey>,
    failed_targets: &HashMap<TargetKey, (&'static str, Instant)>,
    paused: bool,
) -> Vec<AdapterInstanceProbe> {
    let mut custom_index = 0_usize;
    discovery
        .instances
        .iter()
        .map(|instance| {
            let display_name = if instance.is_default {
                "默认 ChatGPT".to_owned()
            } else {
                custom_index += 1;
                format!("ChatGPT 实例 {custom_index}")
            };
            let mut available_keys = instance.target_keys.iter().cloned().collect::<HashSet<_>>();
            available_keys.extend(
                active_keys
                    .iter()
                    .filter(|key| key.process_id == instance.process_id)
                    .cloned(),
            );
            let connected_target_count = available_keys
                .iter()
                .filter(|key| connected_keys.contains(*key))
                .count();
            let failed = available_keys
                .iter()
                .filter_map(|key| failed_targets.get(key).map(|(code, _)| *code))
                .collect::<Vec<_>>();
            let active_count = available_keys
                .iter()
                .filter(|key| active_keys.contains(*key))
                .count();
            let incompatible = matches!(
                instance.last_error,
                Some(AdapterError::VersionUnavailable | AdapterError::VersionUnsupported)
            );
            let state = if paused {
                AdapterInstanceState::Paused
            } else if incompatible {
                AdapterInstanceState::Incompatible
            } else if !instance.connectable {
                AdapterInstanceState::Unavailable
            } else if available_keys.is_empty() {
                AdapterInstanceState::Failed
            } else if connected_target_count == available_keys.len() && failed.is_empty() {
                AdapterInstanceState::Connected
            } else if connected_target_count > 0 {
                AdapterInstanceState::PartiallyConnected
            } else if active_count > 0 {
                AdapterInstanceState::Connecting
            } else if !failed.is_empty() {
                AdapterInstanceState::Failed
            } else {
                AdapterInstanceState::Available
            };
            AdapterInstanceProbe {
                id: format!("chatgpt-{}", instance.process_id),
                display_name,
                is_default: instance.is_default,
                can_restart: instance.can_restart,
                detected_codex_version: instance.codex_version.clone(),
                detected_codex_build: instance.codex_build.clone(),
                state,
                available_target_count: available_keys.len(),
                connected_target_count,
                failed_target_count: failed.len(),
                last_error_code: failed
                    .first()
                    .copied()
                    .or_else(|| instance.last_error.map(AdapterError::code)),
            }
        })
        .collect()
}

fn supervisor_loop(app: AppHandle) {
    let mut workers = HashMap::<TargetKey, SessionWorker>::new();
    let mut failed_targets = HashMap::<TargetKey, (&'static str, Instant)>::new();
    loop {
        let runtime = app.state::<CodexIntegrationRuntime>();
        let finished = workers
            .iter()
            .filter_map(|(key, worker)| worker.handle.is_finished().then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in finished {
            let Some(worker) = workers.remove(&key) else {
                continue;
            };
            match worker.handle.join() {
                Ok(Ok(())) => {
                    failed_targets.remove(&key);
                }
                Ok(Err(error)) => {
                    failed_targets
                        .insert(key, (error.code(), Instant::now() + Duration::from_secs(2)));
                }
                Err(_) => {
                    failed_targets.insert(
                        key,
                        (
                            "codex_adapter_session_panicked",
                            Instant::now() + Duration::from_secs(2),
                        ),
                    );
                }
            }
        }

        let discovery = discover_page_targets();
        let paused = app
            .state::<IntegrationControl>()
            .is_paused()
            .unwrap_or(true);
        let incompatible_process_ids = discovery
            .instances
            .iter()
            .filter(|instance| {
                matches!(
                    instance.last_error,
                    Some(AdapterError::VersionUnavailable | AdapterError::VersionUnsupported)
                )
            })
            .map(|instance| instance.process_id)
            .collect::<HashSet<_>>();
        for (key, worker) in &workers {
            if incompatible_process_ids.contains(&key.process_id) {
                worker.cancelled.store(true, Ordering::Release);
            }
        }
        if !paused {
            for target in &discovery.targets {
                let key = target.key();
                if workers.contains_key(&key)
                    || failed_targets
                        .get(&key)
                        .is_some_and(|(_, retry_at)| *retry_at > Instant::now())
                {
                    continue;
                }
                let connected = Arc::new(AtomicBool::new(false));
                let worker_connected = Arc::clone(&connected);
                let cancelled = Arc::new(AtomicBool::new(false));
                let worker_cancelled = Arc::clone(&cancelled);
                let worker_app = app.clone();
                let worker_target = target.clone();
                let thread_name = format!(
                    "wakegpt-cdp-{}-{}",
                    target.port,
                    target.id.chars().take(12).collect::<String>()
                );
                match thread::Builder::new().name(thread_name).spawn(move || {
                    let result = run_cdp_session(
                        &worker_app,
                        &worker_target,
                        worker_connected.as_ref(),
                        worker_cancelled.as_ref(),
                    );
                    worker_connected.store(false, Ordering::Release);
                    result
                }) {
                    Ok(handle) => {
                        failed_targets.remove(&key);
                        workers.insert(
                            key,
                            SessionWorker {
                                connected,
                                cancelled,
                                handle,
                            },
                        );
                    }
                    Err(_) => {
                        failed_targets.insert(
                            key,
                            (
                                AdapterRuntimeError::ThreadStart.code(),
                                Instant::now() + Duration::from_secs(2),
                            ),
                        );
                    }
                }
            }
        }

        let mut available_keys = discovery
            .targets
            .iter()
            .map(PageTarget::key)
            .collect::<HashSet<_>>();
        available_keys.extend(workers.keys().cloned());
        failed_targets.retain(|key, _| available_keys.contains(key));
        let active_keys = workers.keys().cloned().collect::<HashSet<_>>();
        let connected_keys = workers
            .iter()
            .filter_map(|(key, worker)| {
                worker
                    .connected
                    .load(Ordering::Acquire)
                    .then_some(key.clone())
            })
            .collect::<HashSet<_>>();
        let connected_target_count = connected_keys.len();
        let failed_target_count = failed_targets.len();
        let unconnectable_instance_count = discovery
            .detected_instance_count
            .saturating_sub(discovery.connectable_instance_count);
        let (state, last_error_code) = if paused {
            (AdapterEndpointState::Paused, None)
        } else if connected_target_count > 0 {
            if connected_target_count == available_keys.len()
                && failed_target_count == 0
                && unconnectable_instance_count == 0
            {
                (AdapterEndpointState::Connected, None)
            } else {
                (
                    AdapterEndpointState::PartiallyConnected,
                    failed_targets
                        .values()
                        .next()
                        .map(|(code, _)| *code)
                        .or_else(|| {
                            discovery
                                .last_error
                                .filter(|error| {
                                    matches!(
                                        error,
                                        AdapterError::VersionUnavailable
                                            | AdapterError::VersionUnsupported
                                    )
                                })
                                .map(AdapterError::code)
                                .or_else(|| {
                                    (unconnectable_instance_count > 0)
                                        .then_some("codex_instances_unconnectable")
                                })
                        }),
                )
            }
        } else if !workers.is_empty() {
            (AdapterEndpointState::Connecting, None)
        } else if !available_keys.is_empty() {
            (
                AdapterEndpointState::InjectionFailed,
                failed_targets
                    .values()
                    .next()
                    .map(|(code, _)| *code)
                    .or(Some("codex_all_targets_injection_failed")),
            )
        } else if discovery.detected_instance_count == 0 {
            (
                AdapterEndpointState::NotDetected,
                Some(AdapterError::ActivePortUnavailable.code()),
            )
        } else if discovery.connectable_instance_count == 0 {
            match discovery.last_error {
                Some(
                    error @ (AdapterError::VersionUnavailable | AdapterError::VersionUnsupported),
                ) => (
                    AdapterEndpointState::UnmanagedTargetIncompatible,
                    Some(error.code()),
                ),
                _ => (
                    AdapterEndpointState::RestartRequired,
                    Some("codex_instances_unconnectable"),
                ),
            }
        } else {
            let error = discovery.last_error.unwrap_or(AdapterError::TargetMissing);
            let state = match error {
                AdapterError::EndpointUnavailable => AdapterEndpointState::StaleEndpoint,
                _ => AdapterEndpointState::UnmanagedTargetIncompatible,
            };
            (state, Some(error.code()))
        };
        let instances = build_instance_probes(
            &discovery,
            &active_keys,
            &connected_keys,
            &failed_targets,
            paused,
        );
        let next_snapshot = RuntimeSnapshot {
            state,
            last_error_code,
            detected_instance_count: discovery.detected_instance_count,
            connectable_instance_count: discovery.connectable_instance_count,
            available_target_count: available_keys.len(),
            connected_target_count,
            failed_target_count,
            instances,
        };
        if runtime.replace_snapshot(next_snapshot.clone()) {
            app.state::<LocalDiagnostics>().record_codex_state(
                next_snapshot.state.diagnostic_code(),
                next_snapshot.last_error_code,
                CodexDiagnosticCounts {
                    detected_instances: next_snapshot.detected_instance_count,
                    connectable_instances: next_snapshot.connectable_instance_count,
                    available_targets: next_snapshot.available_target_count,
                    connected_targets: next_snapshot.connected_target_count,
                    failed_targets: next_snapshot.failed_target_count,
                },
            );
        }
        thread::sleep(SUPERVISOR_INTERVAL);
    }
}

fn run_cdp_session(
    app: &AppHandle,
    target: &PageTarget,
    connected: &AtomicBool,
    cancelled: &AtomicBool,
) -> Result<(), AdapterRuntimeError> {
    #[cfg(target_os = "macos")]
    verify_running_chatgpt_host_contract(target.process_id)?;
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, target.port);
    let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(2))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_connect_failed"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_timeout_setup"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_timeout_setup"))?;
    let config = WebSocketConfig::default()
        .read_buffer_size(32 * 1024)
        .write_buffer_size(0)
        .max_message_size(Some(MAX_CDP_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_CDP_MESSAGE_BYTES));
    let (mut socket, response) = client_with_config(
        target.web_socket_debugger_url.as_str(),
        stream,
        Some(config),
    )
    .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_handshake_failed"))?;
    if response.status().as_u16() != 101 {
        return Err(AdapterRuntimeError::Protocol(
            "codex_cdp_handshake_status_invalid",
        ));
    }
    socket
        .get_mut()
        .set_read_timeout(Some(CDP_IO_TIMEOUT))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_timeout_setup"))?;
    socket
        .get_mut()
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_timeout_setup"))?;

    let nonce = new_id().replace('-', "");
    let binding_name = format!("__wakegptBridge_{}", &nonce[..16]);
    let mut session = CardSession {
        nonce,
        binding_name,
        workspace_id: None,
        notebook_id: None,
        selection_initialized: false,
        pending_attachments: Vec::new(),
        composer_target_scope: composer_target_scope(target),
        composer_host_kind: None,
        pending_create_ack: None,
    };
    let mut observed_refresh_generation = app.state::<CodexCardRefreshSignal>().current();
    let initial_state = build_card_state(app, &mut session)?;
    let bootstrap = json!({
        "schemaVersion": 1,
        "sessionNonce": session.nonce,
        "bindingName": session.binding_name,
        "hostContractId": HOST_CONTRACT_ID,
        "initialState": initial_state,
    });
    let bootstrap_json = serde_json::to_string(&bootstrap)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_bootstrap_serialize_failed"))?;
    let injection_source =
        format!("{CARD_SCRIPT}\n;globalThis.__wakegptMountCodexCard({bootstrap_json});");
    if injection_source.len() > MAX_CDP_MESSAGE_BYTES / 2 {
        return Err(AdapterRuntimeError::Protocol(
            "codex_injection_source_too_large",
        ));
    }

    let mut next_id = 1_u64;
    cdp_command_wait(&mut socket, &mut next_id, "Runtime.enable", json!({}))?;
    cdp_command_wait(&mut socket, &mut next_id, "Page.enable", json!({}))?;
    cdp_command_wait(
        &mut socket,
        &mut next_id,
        "Runtime.addBinding",
        json!({ "name": session.binding_name }),
    )?;
    cdp_command_wait(
        &mut socket,
        &mut next_id,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": injection_source }),
    )?;
    let result = cdp_command_wait(
        &mut socket,
        &mut next_id,
        "Runtime.evaluate",
        json!({
            "expression": injection_source,
            "returnByValue": true,
            "awaitPromise": false,
        }),
    )?;
    if result.get("exceptionDetails").is_some() {
        return Err(AdapterRuntimeError::Protocol(
            "codex_card_injection_exception",
        ));
    }
    validate_mount_result(&result, &session)?;
    let nonce_json = serde_json::to_string(&session.nonce)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_bootstrap_serialize_failed"))?;
    let adapter_json = serde_json::to_string(ADAPTER_VERSION)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_bootstrap_serialize_failed"))?;
    let contract_json = serde_json::to_string(HOST_CONTRACT_ID)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_bootstrap_serialize_failed"))?;
    let health_result = cdp_command_wait(
        &mut socket,
        &mut next_id,
        "Runtime.evaluate",
        json!({
            "expression": format!(
                "(()=>{{const host=document.getElementById('wakegpt-codex-card-v1');return Boolean(host&&host.isConnected&&host.shadowRoot&&host.dataset.sessionNonce==={nonce_json}&&host.dataset.adapterVersion==={adapter_json}&&host.dataset.hostContractId==={contract_json})}})()"
            ),
            "returnByValue": true,
            "awaitPromise": false,
        }),
    )?;
    if health_result
        .pointer("/result/value")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(AdapterRuntimeError::Protocol("codex_card_host_missing"));
    }
    connected.store(true, Ordering::Release);

    let mut last_refresh = Instant::now();
    loop {
        if cancelled.load(Ordering::Acquire)
            || app
                .state::<IntegrationControl>()
                .is_paused()
                .unwrap_or(true)
        {
            let stop_expression = format!("globalThis.__wakegptStopCodexCard?.({nonce_json})");
            let _ = cdp_command_wait(
                &mut socket,
                &mut next_id,
                "Runtime.evaluate",
                json!({
                    "expression": stop_expression,
                    "returnByValue": true,
                }),
            );
            let _ = socket.close(None);
            return Ok(());
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_CDP_MESSAGE_BYTES {
                    return Err(AdapterRuntimeError::Protocol("codex_cdp_message_too_large"));
                }
                let message: Value = serde_json::from_str(text.as_str())
                    .map_err(|_| AdapterRuntimeError::Protocol("codex_cdp_json_invalid"))?;
                if message.get("method").and_then(Value::as_str) == Some("Runtime.bindingCalled") {
                    handle_binding_event(app, &mut socket, &mut next_id, &mut session, &message)?;
                }
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Binary(_)) => {
                return Err(AdapterRuntimeError::Protocol("codex_cdp_binary_unexpected"));
            }
            Ok(Message::Close(_)) => return Ok(()),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(())
            }
            Err(_) => {
                return Err(AdapterRuntimeError::Transport(
                    "codex_cdp_connection_failed",
                ))
            }
        }

        let refresh_generation = app.state::<CodexCardRefreshSignal>().current();
        if refresh_generation != observed_refresh_generation
            || last_refresh.elapsed() >= Duration::from_secs(2)
        {
            let state = build_card_state(app, &mut session)?;
            send_card_response(&mut socket, &mut next_id, &session, None, true, "", state)?;
            observed_refresh_generation = refresh_generation;
            last_refresh = Instant::now();
        }
    }
}

fn validate_mount_result(result: &Value, session: &CardSession) -> Result<(), AdapterRuntimeError> {
    let value = result
        .pointer("/result/value")
        .and_then(Value::as_object)
        .ok_or(AdapterRuntimeError::Protocol("codex_card_mount_invalid"))?;
    let expected_receiver_name = format!("__wakegptReceive_{}", session.nonce);
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(AdapterRuntimeError::Protocol(
            match value.get("code").and_then(Value::as_str) {
                Some("context_selector_ambiguous") => "codex_context_selector_ambiguous",
                Some("host_capability_unavailable") => "codex_host_capability_unavailable",
                Some("host_contract_mismatch") => "codex_host_contract_mismatch",
                _ => "codex_card_mount_rejected",
            },
        ));
    }
    if value.get("adapterVersion").and_then(Value::as_str) != Some(ADAPTER_VERSION)
        || value.get("hostContractId").and_then(Value::as_str) != Some(HOST_CONTRACT_ID)
        || value.get("sessionNonce").and_then(Value::as_str) != Some(session.nonce.as_str())
        || value.get("bindingName").and_then(Value::as_str) != Some(session.binding_name.as_str())
        || value.get("receiverName").and_then(Value::as_str)
            != Some(expected_receiver_name.as_str())
    {
        return Err(AdapterRuntimeError::Protocol("codex_card_mount_rejected"));
    }
    Ok(())
}

fn cdp_command_wait(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    method: &'static str,
    params: Value,
) -> Result<Value, AdapterRuntimeError> {
    let id = cdp_send(socket, next_id, method, params)?;
    let deadline = Instant::now() + CDP_COMMAND_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err(AdapterRuntimeError::Transport("codex_cdp_command_timeout"));
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_CDP_MESSAGE_BYTES {
                    return Err(AdapterRuntimeError::Protocol("codex_cdp_message_too_large"));
                }
                let value: Value = serde_json::from_str(text.as_str())
                    .map_err(|_| AdapterRuntimeError::Protocol("codex_cdp_json_invalid"))?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if value.get("error").is_some() {
                        return Err(AdapterRuntimeError::Protocol("codex_cdp_command_rejected"));
                    }
                    return value
                        .get("result")
                        .cloned()
                        .ok_or(AdapterRuntimeError::Protocol("codex_cdp_result_missing"));
                }
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => {
                return Err(AdapterRuntimeError::Transport(
                    "codex_cdp_connection_closed",
                ));
            }
            Ok(Message::Binary(_)) => {
                return Err(AdapterRuntimeError::Protocol("codex_cdp_binary_unexpected"));
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                return Err(AdapterRuntimeError::Transport(
                    "codex_cdp_connection_failed",
                ))
            }
        }
    }
}

fn cdp_send(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    method: &'static str,
    params: Value,
) -> Result<u64, AdapterRuntimeError> {
    let id = *next_id;
    *next_id = next_id
        .checked_add(1)
        .ok_or(AdapterRuntimeError::Protocol("codex_cdp_id_exhausted"))?;
    let body = serde_json::to_string(&json!({
        "id": id,
        "method": method,
        "params": params,
    }))
    .map_err(|_| AdapterRuntimeError::Protocol("codex_cdp_command_serialize_failed"))?;
    if body.len() > MAX_CDP_MESSAGE_BYTES {
        return Err(AdapterRuntimeError::Protocol("codex_cdp_command_too_large"));
    }
    socket
        .send(Message::Text(body.into()))
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_send_failed"))?;
    socket
        .flush()
        .map_err(|_| AdapterRuntimeError::Transport("codex_cdp_flush_failed"))?;
    Ok(id)
}

fn card_image_uploads(data: &Value) -> Result<Vec<CardImageUpload>, AdapterRuntimeError> {
    let values =
        data.get("images")
            .and_then(Value::as_array)
            .ok_or(AdapterRuntimeError::Action(
                "card_attachment_payload_invalid",
            ))?;
    if values.is_empty() || values.len() > MAX_ATTACHMENTS_PER_RECORD {
        return Err(AdapterRuntimeError::Action("card_too_many_attachments"));
    }
    let mut uploads = Vec::with_capacity(values.len());
    let mut total = 0_u64;
    for (expected_index, value) in values.iter().enumerate() {
        let upload: CardImageUpload = serde_json::from_value(value.clone())
            .map_err(|_| AdapterRuntimeError::Action("card_attachment_payload_invalid"))?;
        if upload.index != expected_index
            || upload.byte_size == 0
            || upload.byte_size > MAX_ATTACHMENT_BYTES
        {
            return Err(AdapterRuntimeError::Action("card_attachment_size_invalid"));
        }
        total = total
            .checked_add(upload.byte_size)
            .ok_or(AdapterRuntimeError::Action(
                "card_attachments_total_too_large",
            ))?;
        if total > MAX_RECORD_ATTACHMENT_BYTES {
            return Err(AdapterRuntimeError::Action(
                "card_attachments_total_too_large",
            ));
        }
        uploads.push(upload);
    }
    Ok(uploads)
}

fn decode_card_image_hex(
    encoded: &str,
    expected_bytes: usize,
) -> Result<Vec<u8>, AdapterRuntimeError> {
    if encoded.len() != expected_bytes.saturating_mul(2) || !encoded.is_ascii() {
        return Err(AdapterRuntimeError::Action("card_attachment_chunk_invalid"));
    }
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut decoded = Vec::with_capacity(expected_bytes);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high =
            nibble(pair[0]).ok_or(AdapterRuntimeError::Action("card_attachment_chunk_invalid"))?;
        let low =
            nibble(pair[1]).ok_or(AdapterRuntimeError::Action("card_attachment_chunk_invalid"))?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn read_card_image_bytes(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    upload_id: &str,
    upload: CardImageUpload,
) -> Result<Vec<u8>, AdapterRuntimeError> {
    let expected_size = usize::try_from(upload.byte_size)
        .map_err(|_| AdapterRuntimeError::Action("card_attachment_size_invalid"))?;
    let nonce_json = serde_json::to_string(&session.nonce)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_binding_response_serialize_failed"))?;
    let upload_id_json = serde_json::to_string(upload_id)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_binding_response_serialize_failed"))?;
    let mut bytes = Vec::with_capacity(expected_size);
    while bytes.len() < expected_size {
        let length = (expected_size - bytes.len()).min(CARD_IMAGE_CHUNK_BYTES);
        let expression = format!(
            "globalThis.__wakegptReadPastedImageChunk?.({nonce_json},{upload_id_json},{},{},{length})",
            upload.index,
            bytes.len(),
        );
        let result = cdp_command_wait(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )?;
        let encoded = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .ok_or(AdapterRuntimeError::Action(
                "card_attachment_chunk_unavailable",
            ))?;
        bytes.extend(decode_card_image_hex(encoded, length)?);
    }
    Ok(bytes)
}

fn card_image_hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(ALPHABET[(byte >> 4) as usize]);
        encoded.push(ALPHABET[(byte & 0x0f) as usize]);
    }
    // The alphabet is fixed ASCII, so this conversion cannot fail.
    String::from_utf8(encoded).expect("hex alphabet must be UTF-8")
}

fn send_card_image_preview_event(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    event: Value,
) -> Result<(), AdapterRuntimeError> {
    let receiver_name = format!("__wakegptReceiveImage_{}", session.nonce);
    let receiver_json = serde_json::to_string(&receiver_name)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_preview_serialize_failed"))?;
    let event_json = serde_json::to_string(&event)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_preview_serialize_failed"))?;
    let result = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": format!("globalThis[{receiver_json}]?.({event_json})"),
            "returnByValue": true,
            "awaitPromise": false,
        }),
    )?;
    if result.pointer("/result/value").and_then(Value::as_bool) != Some(true) {
        return Err(AdapterRuntimeError::Action(
            "card_attachment_preview_receiver_unavailable",
        ));
    }
    Ok(())
}

fn send_card_image_preview(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    request_id: &str,
    preview_key: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<(), AdapterRuntimeError> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err(AdapterRuntimeError::Action(
            "card_attachment_preview_unavailable",
        ));
    }
    card_image_kind(media_type)?;
    send_card_image_preview_event(
        socket,
        next_id,
        session,
        json!({
            "schemaVersion": 1,
            "sessionNonce": session.nonce,
            "requestId": request_id,
            "previewKey": preview_key,
            "phase": "start",
            "mediaType": media_type,
            "byteSize": bytes.len(),
        }),
    )?;
    for (index, chunk) in bytes.chunks(CARD_IMAGE_CHUNK_BYTES).enumerate() {
        send_card_image_preview_event(
            socket,
            next_id,
            session,
            json!({
                "schemaVersion": 1,
                "sessionNonce": session.nonce,
                "requestId": request_id,
                "previewKey": preview_key,
                "phase": "chunk",
                "offset": index * CARD_IMAGE_CHUNK_BYTES,
                "hex": card_image_hex(chunk),
            }),
        )?;
    }
    send_card_image_preview_event(
        socket,
        next_id,
        session,
        json!({
            "schemaVersion": 1,
            "sessionNonce": session.nonce,
            "requestId": request_id,
            "previewKey": preview_key,
            "phase": "complete",
        }),
    )
}

fn card_image_kind(media_type: &str) -> Result<(), AdapterRuntimeError> {
    if matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        Ok(())
    } else {
        Err(AdapterRuntimeError::Action(
            "card_attachment_preview_invalid",
        ))
    }
}

fn discard_card_staged_images(
    coordinator: &AttachmentCoordinator,
    attachments: &[PendingAttachment],
) {
    for attachment in attachments {
        let _ = coordinator.discard_pending(&attachment.token);
    }
}

fn stage_card_image_uploads(
    coordinator: &AttachmentCoordinator,
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    upload_id: &str,
    uploads: Vec<CardImageUpload>,
) -> Result<Vec<PendingAttachment>, AdapterRuntimeError> {
    let mut staged = Vec::with_capacity(uploads.len());
    for upload in uploads {
        let bytes = match read_card_image_bytes(socket, next_id, session, upload_id, upload) {
            Ok(bytes) => bytes,
            Err(error) => {
                discard_card_staged_images(coordinator, &staged);
                return Err(error);
            }
        };
        match coordinator.stage_uploaded(&bytes) {
            Ok(attachment) => staged.push(attachment),
            Err(_) => {
                discard_card_staged_images(coordinator, &staged);
                return Err(AdapterRuntimeError::Action("card_attachment_stage_failed"));
            }
        }
    }
    Ok(staged)
}

fn card_retained_attachment_ids(data: &Value) -> Result<Option<Vec<String>>, AdapterRuntimeError> {
    match data.get("retainedAttachmentIds") {
        None => Ok(None),
        Some(Value::Array(values)) if values.len() <= MAX_ATTACHMENTS_PER_RECORD => {
            let mut ids = Vec::with_capacity(values.len());
            let mut unique = HashSet::with_capacity(values.len());
            for value in values {
                let id = value
                    .as_str()
                    .ok_or(AdapterRuntimeError::Action("card_attachment_set_invalid"))?;
                validate_id(id, "attachment id")
                    .map_err(|_| AdapterRuntimeError::Action("card_attachment_set_invalid"))?;
                if !unique.insert(id) {
                    return Err(AdapterRuntimeError::Action("card_attachment_set_invalid"));
                }
                ids.push(id.to_owned());
            }
            Ok(Some(ids))
        }
        Some(_) => Err(AdapterRuntimeError::Action("card_attachment_set_invalid")),
    }
}

fn card_record_edit_upload(
    data: &Value,
) -> Result<Option<(String, Vec<CardImageUpload>)>, AdapterRuntimeError> {
    match (data.get("uploadId"), data.get("images")) {
        (None, None) => Ok(None),
        (Some(Value::String(upload_id)), Some(_)) => {
            validate_id(upload_id, "card record edit upload id")
                .map_err(|_| AdapterRuntimeError::Action("card_attachment_payload_invalid"))?;
            Ok(Some((upload_id.clone(), card_image_uploads(data)?)))
        }
        _ => Err(AdapterRuntimeError::Action(
            "card_attachment_payload_invalid",
        )),
    }
}

#[derive(Debug)]
struct CardRecordEditPlan {
    retained_attachment_ids: Vec<String>,
    pending_attachments: Vec<PendingAttachment>,
}

fn card_record_edit_plan(
    record: &Record,
    requested_retained_ids: Option<&[String]>,
    staged: &[PendingAttachment],
) -> Result<CardRecordEditPlan, AdapterRuntimeError> {
    let current_ids = record
        .attachments
        .iter()
        .map(|attachment| attachment.id.as_str())
        .collect::<HashSet<_>>();
    let mut retained_ids = requested_retained_ids
        .map(|ids| ids.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_else(|| {
            record
                .attachments
                .iter()
                .map(|attachment| attachment.id.clone())
                .collect()
        });
    if retained_ids.len() != requested_retained_ids.map_or(retained_ids.len(), <[String]>::len)
        || retained_ids
            .iter()
            .any(|id| !current_ids.contains(id.as_str()))
    {
        return Err(AdapterRuntimeError::Action("card_attachment_set_invalid"));
    }

    let current_by_digest = record
        .attachments
        .iter()
        .filter(|attachment| retained_ids.contains(&attachment.id))
        .map(|attachment| (attachment.content_sha256.as_str(), attachment.id.as_str()))
        .collect::<HashMap<_, _>>();
    let mut new_digests = HashSet::new();
    let mut pending_attachments = Vec::new();
    for attachment in staged {
        if let Some(existing_id) = current_by_digest.get(attachment.content_sha256.as_str()) {
            retained_ids.insert((*existing_id).to_owned());
        } else if new_digests.insert(attachment.content_sha256.as_str()) {
            pending_attachments.push(attachment.clone());
        }
    }

    let retained_attachment_ids = record
        .attachments
        .iter()
        .filter(|attachment| retained_ids.contains(&attachment.id))
        .map(|attachment| attachment.id.clone())
        .collect::<Vec<_>>();
    if retained_attachment_ids.len() + pending_attachments.len() > MAX_ATTACHMENTS_PER_RECORD {
        return Err(AdapterRuntimeError::Action("card_too_many_attachments"));
    }
    let retained_bytes = record
        .attachments
        .iter()
        .filter(|attachment| retained_ids.contains(&attachment.id))
        .try_fold(0_u64, |total, attachment| {
            total.checked_add(attachment.byte_size)
        })
        .ok_or(AdapterRuntimeError::Action(
            "card_attachments_total_too_large",
        ))?;
    let total_bytes = pending_attachments
        .iter()
        .try_fold(retained_bytes, |total, attachment| {
            total.checked_add(attachment.byte_size)
        })
        .ok_or(AdapterRuntimeError::Action(
            "card_attachments_total_too_large",
        ))?;
    if total_bytes > MAX_RECORD_ATTACHMENT_BYTES {
        return Err(AdapterRuntimeError::Action(
            "card_attachments_total_too_large",
        ));
    }
    Ok(CardRecordEditPlan {
        retained_attachment_ids,
        pending_attachments,
    })
}

fn handle_binding_event(
    app: &AppHandle,
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &mut CardSession,
    event: &Value,
) -> Result<(), AdapterRuntimeError> {
    let params =
        event
            .get("params")
            .and_then(Value::as_object)
            .ok_or(AdapterRuntimeError::Protocol(
                "codex_binding_params_invalid",
            ))?;
    if params.get("name").and_then(Value::as_str) != Some(session.binding_name.as_str()) {
        return Ok(());
    }
    let payload =
        params
            .get("payload")
            .and_then(Value::as_str)
            .ok_or(AdapterRuntimeError::Protocol(
                "codex_binding_payload_invalid",
            ))?;
    if payload.len() > 256 * 1024 {
        return Err(AdapterRuntimeError::Protocol(
            "codex_binding_payload_too_large",
        ));
    }
    let request: BindingRequest = serde_json::from_str(payload)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_binding_payload_invalid"))?;
    if request.schema_version != 1
        || request.adapter_version != ADAPTER_VERSION
        || request.session_nonce != session.nonce
        || validate_id(&request.request_id, "request id").is_err()
    {
        return Err(AdapterRuntimeError::Protocol(
            "codex_binding_identity_invalid",
        ));
    }
    if request.action == "cardVisibilityChanged" {
        let (visibility, layout) = card_visibility_diagnostic(&request.data)?;
        app.state::<LocalDiagnostics>()
            .record_card_visibility(visibility, layout);
        return Ok(());
    }
    if request.action == "adapterIncompatible" {
        return match bounded_data_string(&request.data, "code", 64)?.as_str() {
            "contextSelectorAmbiguous" => Err(AdapterRuntimeError::Protocol(
                "codex_context_selector_ambiguous",
            )),
            _ => Err(AdapterRuntimeError::Protocol(
                "codex_binding_identity_invalid",
            )),
        };
    }

    let outcome = handle_card_action(app, socket, next_id, session, &request);
    let (ok, message) = match outcome {
        Ok(message) => (true, message),
        Err(error) => (false, card_action_message(error.code())),
    };
    let state = build_card_state(app, session)?;
    send_card_response(
        socket,
        next_id,
        session,
        Some(request.request_id),
        ok,
        message,
        state,
    )
}

fn card_visibility_diagnostic(
    data: &Value,
) -> Result<(&'static str, Option<&'static str>), AdapterRuntimeError> {
    let invalid = || AdapterRuntimeError::Protocol("codex_card_visibility_invalid");
    let visibility = match bounded_data_string(data, "visibility", 32)
        .map_err(|_| invalid())?
        .as_str()
    {
        "visible" => "visible",
        "contextMissing" => "context_missing",
        "fullscreen" => "fullscreen",
        "modal" => "modal",
        "viewportOverlay" => "viewport_overlay",
        "mediaLightbox" => "media_lightbox",
        "contextAmbiguous" => "context_ambiguous",
        _ => {
            return Err(invalid());
        }
    };
    let layout = optional_data_string(data, "layout", 16).map_err(|_| invalid())?;
    let layout = match layout.as_deref() {
        None => None,
        Some("full") => Some("full"),
        Some("compact") => Some("compact"),
        Some("collapsed") => Some("collapsed"),
        Some("drawer") => Some("drawer"),
        _ => {
            return Err(invalid());
        }
    };
    if (visibility == "visible") != layout.is_some() {
        return Err(invalid());
    }
    Ok((visibility, layout))
}

fn initialize_card_selection(
    session: &mut CardSession,
    preferences: &UiPreferences,
    workspaces: &[Workspace],
) {
    if session.selection_initialized {
        return;
    }
    session.selection_initialized = true;
    let Some(workspace_id) = preferences
        .active_workspace_id
        .as_ref()
        .filter(|id| workspaces.iter().any(|workspace| &workspace.id == *id))
    else {
        return;
    };
    session.workspace_id = Some(workspace_id.clone());
    session.notebook_id = preferences.selected_notebook_id.clone();
}

fn select_card_workspace(
    store: &Store,
    session: &mut CardSession,
    workspace_id: &str,
) -> Result<(), AdapterRuntimeError> {
    let exists = store
        .list_workspaces()
        .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?
        .iter()
        .any(|workspace| workspace.id == workspace_id);
    if !exists {
        return Err(AdapterRuntimeError::Action("card_workspace_not_found"));
    }
    session.workspace_id = Some(workspace_id.to_owned());
    session.notebook_id = None;
    session.selection_initialized = true;
    session.pending_attachments.clear();
    Ok(())
}

fn select_card_notebook(
    store: &Store,
    session: &mut CardSession,
    notebook_id: Option<&str>,
) -> Result<(), AdapterRuntimeError> {
    let workspace_id = require_session_workspace(session)?;
    if let Some(notebook_id) = notebook_id {
        let notebook = store
            .get_notebook(&workspace_id, notebook_id)
            .map_err(|_| AdapterRuntimeError::Action("card_notebook_not_found"))?;
        if !notebook.is_pinned {
            return Err(AdapterRuntimeError::Action("card_notebook_not_found"));
        }
    }
    session.notebook_id = notebook_id.map(str::to_owned);
    session.selection_initialized = true;
    session.pending_attachments.clear();
    Ok(())
}

fn handle_card_record_edit(
    app: &AppHandle,
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    request: &BindingRequest,
) -> Result<String, AdapterRuntimeError> {
    let workspace_id = require_session_workspace(session)?;
    let record_id = bounded_data_string(&request.data, "recordId", 128)?;
    let body_markdown =
        optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?.unwrap_or_default();
    let expected_revision = request
        .data
        .get("expectedRevision")
        .and_then(Value::as_u64)
        .ok_or(AdapterRuntimeError::Action("card_record_revision_invalid"))?;
    let requested_retained_ids = card_retained_attachment_ids(&request.data)?;
    let upload = card_record_edit_upload(&request.data)?;
    let store = app.state::<Store>();
    let current = store
        .get_record(&workspace_id, &record_id)
        .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
    if current.notebook_id != session.notebook_id {
        return Err(AdapterRuntimeError::Action("card_record_not_found"));
    }
    if current.revision != expected_revision || current.state != RecordState::Active {
        return Err(AdapterRuntimeError::Action("card_record_changed"));
    }
    if current.attachments.iter().any(|attachment| {
        attachment.relocation_state != AttachmentRelocationState::Ready
            || !matches!(
                attachment.file_state,
                AttachmentFileState::Ready | AttachmentFileState::PreservedShared
            )
    }) {
        return Err(AdapterRuntimeError::Action(
            "card_record_attachment_unavailable",
        ));
    }

    let coordinator = app.state::<AttachmentCoordinator>();
    let staged = match upload {
        Some((upload_id, uploads)) => {
            stage_card_image_uploads(&coordinator, socket, next_id, session, &upload_id, uploads)?
        }
        None => Vec::new(),
    };
    let outcome = (|| {
        let revises_attachment_set = requested_retained_ids.is_some() || !staged.is_empty();
        let plan = card_record_edit_plan(&current, requested_retained_ids.as_deref(), &staged)?;
        if body_markdown.trim().is_empty()
            && plan.retained_attachment_ids.is_empty()
            && plan.pending_attachments.is_empty()
        {
            return Err(AdapterRuntimeError::Action("card_record_empty"));
        }
        let workspace_and_directory = if plan.pending_attachments.is_empty() {
            None
        } else {
            let workspace = store
                .get_workspace(&workspace_id)
                .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?;
            let attachment_directory = match current.notebook_id.as_deref() {
                Some(notebook_id) => {
                    store
                        .get_notebook(&workspace_id, notebook_id)
                        .map_err(|_| AdapterRuntimeError::Action("card_notebook_not_found"))?
                        .attachment_directory
                }
                None => INBOX_ATTACHMENT_DIRECTORY.to_owned(),
            };
            Some((workspace, attachment_directory))
        };
        let pending_tokens = plan
            .pending_attachments
            .iter()
            .map(|attachment| attachment.token.clone())
            .collect::<Vec<_>>();
        let mut materialize_failed = false;
        let mutation = app
            .state::<SyncCoordinator>()
            .execute_record_mutation_with_attachments(&store, &coordinator, |store| {
                let locked = store.get_record(&workspace_id, &record_id)?;
                if locked.notebook_id != session.notebook_id
                    || locked.revision != expected_revision
                    || locked.state != RecordState::Active
                {
                    return Err(crate::storage::StoreError::Conflict(
                        "card_record_changed".to_owned(),
                    ));
                }
                let prepared = match &workspace_and_directory {
                    Some((workspace, attachment_directory)) => coordinator
                        .materialize_locked(workspace, attachment_directory, &pending_tokens)
                        .map_err(|_| {
                            materialize_failed = true;
                            crate::storage::StoreError::Conflict(
                                "card_attachment_materialize_failed".to_owned(),
                            )
                        })?,
                    None => Vec::new(),
                };
                if revises_attachment_set {
                    store.revise_record_attachment_set_idempotent(
                        &workspace_id,
                        &record_id,
                        expected_revision,
                        RecordAttachmentSetRevision {
                            body_markdown: body_markdown.trim_end(),
                            retained_attachment_ids: &plan.retained_attachment_ids,
                            new_attachments: &prepared,
                            mutation_id: &request.request_id,
                            mutation_schema_version: MUTATION_SCHEMA_VERSION,
                        },
                    )
                } else {
                    store.update_record_idempotent(
                        &workspace_id,
                        &record_id,
                        expected_revision,
                        body_markdown.trim_end(),
                        &request.request_id,
                        MUTATION_SCHEMA_VERSION,
                    )
                }
            });
        if materialize_failed {
            return Err(AdapterRuntimeError::Action(
                "card_attachment_materialize_failed",
            ));
        }
        let updated = match mutation {
            Ok(updated) => updated,
            Err(CoordinatedMutationError::Saved { .. }) => {
                emit_card_data_changed(
                    app,
                    &workspace_id,
                    current.notebook_id.as_deref(),
                    "records",
                );
                return Err(AdapterRuntimeError::Action(
                    "card_record_edit_saved_pending",
                ));
            }
            Err(_) => {
                return Err(AdapterRuntimeError::Action("card_record_edit_failed"));
            }
        };
        emit_card_data_changed(
            app,
            &workspace_id,
            current.notebook_id.as_deref(),
            "records",
        );
        if revises_attachment_set && updated.revision != expected_revision {
            coordinator
                .apply_record_lifecycle(&store, updated)
                .map_err(|_| AdapterRuntimeError::Action("card_record_edit_attachment_recovery"))?;
        }
        Ok(if current.notebook_id.is_some() {
            "正文与附件已修改并同步到 Markdown"
        } else {
            "收件箱正文与附件已修改（仅本地）"
        }
        .to_owned())
    })();
    discard_card_staged_images(&coordinator, &staged);
    outcome
}

fn handle_card_action(
    app: &AppHandle,
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &mut CardSession,
    request: &BindingRequest,
) -> Result<String, AdapterRuntimeError> {
    match request.action.as_str() {
        "selectWorkspace" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            let workspace_id = bounded_data_string(&request.data, "workspaceId", 128)?;
            select_card_workspace(app.state::<Store>().inner(), session, &workspace_id)?;
            Ok("已切换工作区".to_owned())
        }
        "selectNotebook" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            let notebook_id = optional_data_string(&request.data, "notebookId", 128)?;
            select_card_notebook(
                app.state::<Store>().inner(),
                session,
                notebook_id.as_deref(),
            )?;
            Ok("已切换速记本".to_owned())
        }
        "unpinNotebook" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            let workspace_id = require_session_workspace(session)?;
            let notebook_id = session
                .notebook_id
                .clone()
                .ok_or(AdapterRuntimeError::Action("card_notebook_not_found"))?;
            let store = app.state::<Store>();
            store
                .set_notebook_pinned(&workspace_id, &notebook_id, false)
                .map_err(|_| AdapterRuntimeError::Action("card_notebook_pin_failed"))?;
            session.notebook_id = None;
            session.pending_attachments.clear();
            emit_card_data_changed(app, &workspace_id, Some(&notebook_id), "notebooks");
            Ok("已关闭 Tab；速记本和文件仍保留".to_owned())
        }
        "saveDraft" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            let requested_workspace_id = optional_data_string(&request.data, "workspaceId", 128)?;
            let requested_notebook_id = optional_data_string(&request.data, "notebookId", 128)?;
            if requested_workspace_id.as_deref() == session.workspace_id.as_deref()
                && requested_notebook_id.as_deref() == session.notebook_id.as_deref()
            {
                save_card_draft(app, session, &body_markdown)?;
            } else if let Some(workspace_id) = requested_workspace_id {
                let store = app.state::<Store>();
                let existing = store
                    .load_draft(&workspace_id, requested_notebook_id.as_deref())
                    .map_err(|_| AdapterRuntimeError::Action("card_draft_read_failed"))?;
                store
                    .save_draft(
                        &workspace_id,
                        requested_notebook_id.as_deref(),
                        &body_markdown,
                        &existing.attachments,
                    )
                    .map_err(|_| AdapterRuntimeError::Action("card_draft_save_failed"))?;
                emit_card_data_changed(
                    app,
                    &workspace_id,
                    requested_notebook_id.as_deref(),
                    "draft",
                );
            }
            Ok(String::new())
        }
        "composerContextChanged" => {
            session.composer_host_kind = Some(composer_host_kind(
                socket,
                next_id,
                &session.composer_target_scope,
            )?);
            Ok(String::new())
        }
        "createRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            if body_markdown.trim().is_empty() && session.pending_attachments.is_empty() {
                return Err(AdapterRuntimeError::Action("card_record_empty"));
            }
            let store = app.state::<Store>();
            let workspace = store
                .get_workspace(&workspace_id)
                .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?;
            let attachment_directory = match session.notebook_id.as_deref() {
                Some(notebook_id) => {
                    store
                        .get_notebook(&workspace_id, notebook_id)
                        .map_err(|_| AdapterRuntimeError::Action("card_notebook_not_found"))?
                        .attachment_directory
                }
                None => crate::domain::INBOX_ATTACHMENT_DIRECTORY.to_owned(),
            };
            let coordinator = app.state::<AttachmentCoordinator>();
            let pending_tokens = session
                .pending_attachments
                .iter()
                .map(|attachment| attachment.token.clone())
                .collect::<Vec<_>>();
            let mut materialize_failed = false;
            let notebook_id = session.notebook_id.clone();
            let saved_locally = match app
                .state::<SyncCoordinator>()
                .execute_record_mutation_with_attachments(&store, &coordinator, |store| {
                    let prepared = coordinator
                        .materialize_locked(&workspace, &attachment_directory, &pending_tokens)
                        .map_err(|_| {
                            materialize_failed = true;
                            crate::storage::StoreError::Conflict(
                                "card_attachment_materialize_failed".to_owned(),
                            )
                        })?;
                    store.create_record_from_draft_attempt(
                        &workspace_id,
                        notebook_id.as_deref(),
                        body_markdown.trim_end(),
                        &prepared,
                        &request.request_id,
                        MUTATION_SCHEMA_VERSION,
                    )
                }) {
                Ok(_) => false,
                Err(CoordinatedMutationError::Saved { .. }) => true,
                Err(_) => {
                    if materialize_failed {
                        return Err(AdapterRuntimeError::Action(
                            "card_attachment_materialize_failed",
                        ));
                    }
                    return Err(AdapterRuntimeError::Action("card_record_create_failed"));
                }
            };
            session.pending_create_ack = Some((workspace_id.clone(), notebook_id.clone()));
            emit_card_data_changed(app, &workspace_id, notebook_id.as_deref(), "records");
            Ok(if saved_locally {
                "记录已保存在本地，Markdown 同步等待恢复"
            } else if notebook_id.is_some() {
                "已记录并同步到 Markdown"
            } else {
                "已保存到收件箱（仅本地）"
            }
            .to_owned())
        }
        "ackCreateRecord" => {
            let (workspace_id, notebook_id) = session
                .pending_create_ack
                .clone()
                .ok_or(AdapterRuntimeError::Action("card_create_ack_unavailable"))?;
            app.state::<Store>()
                .save_draft(&workspace_id, notebook_id.as_deref(), "", &[])
                .map_err(|_| AdapterRuntimeError::Action("card_draft_save_failed"))?;
            if session.workspace_id.as_deref() == Some(workspace_id.as_str())
                && session.notebook_id == notebook_id
            {
                session.pending_attachments.clear();
            }
            session.pending_create_ack = None;
            emit_card_data_changed(app, &workspace_id, notebook_id.as_deref(), "draft");
            Ok(String::new())
        }
        "editRecord" => handle_card_record_edit(app, socket, next_id, session, request),
        "trashRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let expected_revision = request
                .data
                .get("expectedRevision")
                .and_then(Value::as_u64)
                .ok_or(AdapterRuntimeError::Action("card_record_revision_invalid"))?;
            let store = app.state::<Store>();
            let current = store
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if current.revision != expected_revision || current.state != RecordState::Active {
                return Err(AdapterRuntimeError::Action("card_record_changed"));
            }
            let trashed = app
                .state::<SyncCoordinator>()
                .execute_record_mutation(&store, |store| {
                    store.trash_record_idempotent(
                        &workspace_id,
                        &record_id,
                        expected_revision,
                        &request.request_id,
                        MUTATION_SCHEMA_VERSION,
                    )
                })
                .map_err(|_| AdapterRuntimeError::Action("card_record_trash_failed"))?;
            let trashed = app
                .state::<AttachmentCoordinator>()
                .apply_record_lifecycle(&store, trashed)
                .map_err(|_| AdapterRuntimeError::Action("card_attachment_trash_failed"))?;
            emit_card_data_changed(
                app,
                &workspace_id,
                current.notebook_id.as_deref(),
                "records",
            );
            Ok(attachment_lifecycle_message(&trashed, true)
                .unwrap_or("已移到 WakeGPT 废纸篓并自动重排")
                .to_owned())
        }
        "copyRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let destination_notebook_id =
                optional_data_string(&request.data, "destinationNotebookId", 128)?;
            let store = app.state::<Store>();
            let destination_attachment_directory =
                if let Some(notebook_id) = &destination_notebook_id {
                    store
                        .get_notebook(&workspace_id, notebook_id)
                        .map_err(|_| AdapterRuntimeError::Action("card_notebook_not_found"))?
                        .attachment_directory
                } else {
                    INBOX_ATTACHMENT_DIRECTORY.to_owned()
                };
            let current = store
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if current.state != RecordState::Active
                || current.attachments.iter().any(|attachment| {
                    attachment.relocation_state != AttachmentRelocationState::Ready
                        || !matches!(
                            attachment.file_state,
                            AttachmentFileState::Ready | AttachmentFileState::PreservedShared
                        )
                })
            {
                return Err(AdapterRuntimeError::Action(
                    "card_record_copy_attachment_unavailable",
                ));
            }
            let copied_attachments = current
                .attachments
                .iter()
                .map(|attachment| {
                    let managed_relative_path = managed_attachment_relative_path(
                        &destination_attachment_directory,
                        &attachment.content_sha256,
                        &attachment.media_type,
                    )
                    .map_err(|_| {
                        AdapterRuntimeError::Action("card_attachment_destination_invalid")
                    })?;
                    Ok(NewAttachment {
                        media_type: attachment.media_type.clone(),
                        previous_managed_relative_path: (managed_relative_path
                            != attachment.managed_relative_path)
                            .then(|| attachment.managed_relative_path.clone()),
                        managed_relative_path,
                        content_sha256: attachment.content_sha256.clone(),
                        byte_size: attachment.byte_size,
                    })
                })
                .collect::<Result<Vec<_>, AdapterRuntimeError>>()?;
            let attachment_coordinator = app.state::<AttachmentCoordinator>();
            let copied = app
                .state::<SyncCoordinator>()
                .execute_record_mutation_with_prepare(
                    &store,
                    |store| {
                        store.create_record_with_attachments_idempotent(
                            &workspace_id,
                            destination_notebook_id.as_deref(),
                            &current.body_markdown,
                            &copied_attachments,
                            &request.request_id,
                            MUTATION_SCHEMA_VERSION,
                        )
                    },
                    |store, record| {
                        attachment_coordinator
                            .prepare_record_relocations(store, &record.id)
                            .map_err(|error| SyncError::RecoveryRequired(error.code()))
                    },
                )
                .map_err(|_| AdapterRuntimeError::Action("card_record_copy_failed"))?;
            attachment_coordinator
                .finish_pending_relocations(&store)
                .map_err(|_| AdapterRuntimeError::Action("card_attachment_cleanup_failed"))?;
            emit_card_data_changed(app, &workspace_id, copied.notebook_id.as_deref(), "records");
            Ok(if copied.notebook_id.is_some() {
                "已复制并同步到目标 Markdown"
            } else {
                "已复制到收件箱（仅本地）"
            }
            .to_owned())
        }
        "migrateRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let expected_revision = request
                .data
                .get("expectedRevision")
                .and_then(Value::as_u64)
                .ok_or(AdapterRuntimeError::Action("card_record_revision_invalid"))?;
            let destination_notebook_id =
                optional_data_string(&request.data, "destinationNotebookId", 128)?;
            let store = app.state::<Store>();
            let current = store
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if current.revision != expected_revision || current.state != RecordState::Active {
                return Err(AdapterRuntimeError::Action("card_record_changed"));
            }
            let migrated = app
                .state::<SyncCoordinator>()
                .execute_record_mutation_with_prepare(
                    &store,
                    |store| {
                        store.migrate_record_idempotent(
                            &workspace_id,
                            &record_id,
                            expected_revision,
                            destination_notebook_id.as_deref(),
                            &request.request_id,
                            MUTATION_SCHEMA_VERSION,
                        )
                    },
                    |store, record| {
                        app.state::<AttachmentCoordinator>()
                            .prepare_record_relocations(store, &record.id)
                            .map_err(|error| SyncError::RecoveryRequired(error.code()))
                    },
                )
                .map_err(|_| AdapterRuntimeError::Action("card_record_migrate_failed"))?;
            app.state::<AttachmentCoordinator>()
                .finish_pending_relocations(&store)
                .map_err(|_| AdapterRuntimeError::Action("card_attachment_cleanup_failed"))?;
            emit_card_data_changed(
                app,
                &workspace_id,
                migrated.notebook_id.as_deref(),
                "records",
            );
            Ok(if migrated.notebook_id.is_some() {
                "已迁移并同步到目标 Markdown"
            } else {
                "已迁移到收件箱（仅本地）"
            }
            .to_owned())
        }
        "pinRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let expected_revision = request
                .data
                .get("expectedRevision")
                .and_then(Value::as_u64)
                .ok_or(AdapterRuntimeError::Action("card_record_revision_invalid"))?;
            let pinned = request
                .data
                .get("pinned")
                .and_then(Value::as_bool)
                .ok_or(AdapterRuntimeError::Action("card_pin_state_invalid"))?;
            let store = app.state::<Store>();
            let current = store
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if current.revision != expected_revision || current.state != RecordState::Active {
                return Err(AdapterRuntimeError::Action("card_record_changed"));
            }
            store
                .set_record_pinned(&workspace_id, &record_id, pinned)
                .map_err(|_| AdapterRuntimeError::Action("card_record_pin_failed"))?;
            emit_card_data_changed(
                app,
                &workspace_id,
                current.notebook_id.as_deref(),
                "records",
            );
            Ok(if pinned {
                "已置顶"
            } else {
                "已取消置顶"
            }
            .to_owned())
        }
        "stagePastedImages" => {
            let upload_id = bounded_data_string(&request.data, "uploadId", 128)?;
            validate_id(&upload_id, "card image upload id")
                .map_err(|_| AdapterRuntimeError::Action("card_attachment_payload_invalid"))?;
            let uploads = card_image_uploads(&request.data)?;
            if session.pending_attachments.len() + uploads.len() > MAX_ATTACHMENTS_PER_RECORD {
                return Err(AdapterRuntimeError::Action("card_too_many_attachments"));
            }
            let existing_bytes = session
                .pending_attachments
                .iter()
                .try_fold(0_u64, |total, attachment| {
                    total.checked_add(attachment.byte_size)
                })
                .ok_or(AdapterRuntimeError::Action(
                    "card_attachments_total_too_large",
                ))?;
            let upload_bytes = uploads
                .iter()
                .try_fold(0_u64, |total, upload| total.checked_add(upload.byte_size))
                .ok_or(AdapterRuntimeError::Action(
                    "card_attachments_total_too_large",
                ))?;
            if existing_bytes
                .checked_add(upload_bytes)
                .is_none_or(|total| total > MAX_RECORD_ATTACHMENT_BYTES)
            {
                return Err(AdapterRuntimeError::Action(
                    "card_attachments_total_too_large",
                ));
            }
            let coordinator = app.state::<AttachmentCoordinator>();
            let staged = stage_card_image_uploads(
                &coordinator,
                socket,
                next_id,
                session,
                &upload_id,
                uploads,
            )?;
            let mut digests = session
                .pending_attachments
                .iter()
                .map(|attachment| attachment.content_sha256.clone())
                .collect::<HashSet<_>>();
            let mut added = 0_usize;
            for attachment in staged {
                if digests.insert(attachment.content_sha256.clone()) {
                    session.pending_attachments.push(attachment);
                    added += 1;
                } else {
                    let _ = coordinator.discard_pending(&attachment.token);
                }
            }
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            Ok(if added == 0 {
                "图片已存在，未重复添加".to_owned()
            } else {
                format!("已添加 {added} 张图片")
            })
        }
        "loadImagePreview" => {
            let scope = bounded_data_string(&request.data, "scope", 16)?;
            let (preview_key, media_type, bytes) = if scope == "pending" {
                let token = bounded_data_string(&request.data, "token", 128)?;
                validate_id(&token, "pending attachment token")
                    .map_err(|_| AdapterRuntimeError::Action("card_attachment_preview_invalid"))?;
                let pending = session
                    .pending_attachments
                    .iter()
                    .find(|attachment| attachment.token == token)
                    .ok_or(AdapterRuntimeError::Action(
                        "card_attachment_preview_unavailable",
                    ))?;
                let (bytes, detected_media_type) = app
                    .state::<AttachmentCoordinator>()
                    .read_pending_preview(&token)
                    .map_err(|_| {
                        AdapterRuntimeError::Action("card_attachment_preview_unavailable")
                    })?;
                if detected_media_type != pending.media_type
                    || bytes.len() as u64 != pending.byte_size
                {
                    return Err(AdapterRuntimeError::Action(
                        "card_attachment_preview_changed",
                    ));
                }
                (
                    format!("pending:{token}"),
                    pending.media_type.clone(),
                    bytes,
                )
            } else if scope == "record" {
                let workspace_id = require_session_workspace(session)?;
                let record_id = bounded_data_string(&request.data, "recordId", 128)?;
                let attachment_id = bounded_data_string(&request.data, "attachmentId", 128)?;
                let store = app.state::<Store>();
                let record = store.get_record(&workspace_id, &record_id).map_err(|_| {
                    AdapterRuntimeError::Action("card_attachment_preview_unavailable")
                })?;
                if record.state != RecordState::Active || record.notebook_id != session.notebook_id
                {
                    return Err(AdapterRuntimeError::Action(
                        "card_attachment_preview_unavailable",
                    ));
                }
                let attachment = record
                    .attachments
                    .iter()
                    .find(|attachment| attachment.id == attachment_id)
                    .ok_or(AdapterRuntimeError::Action(
                        "card_attachment_preview_unavailable",
                    ))?;
                let workspace = store.get_workspace(&workspace_id).map_err(|_| {
                    AdapterRuntimeError::Action("card_attachment_preview_unavailable")
                })?;
                let bytes = app
                    .state::<AttachmentCoordinator>()
                    .read_record_preview(&workspace, attachment)
                    .map_err(|_| {
                        AdapterRuntimeError::Action("card_attachment_preview_unavailable")
                    })?;
                (
                    format!("record:{}", attachment.id),
                    attachment.media_type.clone(),
                    bytes,
                )
            } else {
                return Err(AdapterRuntimeError::Action(
                    "card_attachment_preview_invalid",
                ));
            };
            send_card_image_preview(
                socket,
                next_id,
                session,
                &request.request_id,
                &preview_key,
                &media_type,
                &bytes,
            )?;
            Ok(String::new())
        }
        "newNotebook" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            let workspace_id = require_session_workspace(session)?;
            let display_name = bounded_data_string(&request.data, "displayName", 120)?;
            let numbering_style = data_numbering_style(&request.data)?;
            let numbering_start = data_numbering_start(&request.data)?;
            let attachment_directory =
                optional_data_string(&request.data, "attachmentDirectory", 1024)?;
            let relative_path = notebook_file_name(&display_name)?;
            let notebook = app
                .state::<SyncCoordinator>()
                .execute_notebook_creation(
                    &app.state::<Store>(),
                    &workspace_id,
                    &relative_path,
                    |store| {
                        store.create_notebook_with_configuration(
                            &workspace_id,
                            &display_name,
                            &relative_path,
                            numbering_style,
                            numbering_start,
                            attachment_directory.as_deref(),
                        )
                    },
                )
                .map_err(|_| AdapterRuntimeError::Action("card_notebook_create_failed"))?;
            select_card_notebook(app.state::<Store>().inner(), session, Some(&notebook.id))?;
            emit_card_data_changed(app, &workspace_id, Some(&notebook.id), "notebooks");
            Ok("已新建 Markdown 速记本".to_owned())
        }
        "bindNotebook" => {
            let body_markdown = optional_data_string(&request.data, "bodyMarkdown", 1024 * 1024)?
                .unwrap_or_default();
            save_card_draft(app, session, &body_markdown)?;
            let workspace_id = require_session_workspace(session)?;
            let numbering_style = data_numbering_style(&request.data)?;
            let numbering_start = data_numbering_start(&request.data)?;
            let attachment_directory =
                optional_data_string(&request.data, "attachmentDirectory", 1024)?;
            let store = app.state::<Store>();
            let workspace = store
                .get_workspace(&workspace_id)
                .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?;
            let Some(selected) = app
                .dialog()
                .file()
                .set_title("绑定已有 Markdown 笔记")
                .add_filter("Markdown", &["md"])
                .blocking_pick_file()
            else {
                return Ok(String::new());
            };
            let selected = selected
                .into_path()
                .map_err(|_| AdapterRuntimeError::Action("card_notebook_path_invalid"))?;
            let relative_path = selected_workspace_relative_path(&workspace.root_path, &selected)?;
            let display_name = selected
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .unwrap_or("Markdown 笔记")
                .to_owned();
            let notebook = app
                .state::<SyncCoordinator>()
                .execute_notebook_binding(
                    &store,
                    &workspace_id,
                    &relative_path,
                    numbering_style,
                    numbering_start,
                    |store| {
                        store.create_notebook_with_configuration(
                            &workspace_id,
                            &display_name,
                            &relative_path,
                            numbering_style,
                            numbering_start,
                            attachment_directory.as_deref(),
                        )
                    },
                )
                .map_err(|_| AdapterRuntimeError::Action("card_notebook_bind_failed"))?;
            select_card_notebook(app.state::<Store>().inner(), session, Some(&notebook.id))?;
            emit_card_data_changed(app, &workspace_id, Some(&notebook.id), "notebooks");
            Ok("已绑定 Markdown 速记本".to_owned())
        }
        "insertRecord" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let record = app
                .state::<Store>()
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if record.state != RecordState::Active {
                return Err(AdapterRuntimeError::Action("card_record_not_active"));
            }
            let workspace = app
                .state::<Store>()
                .get_workspace(&workspace_id)
                .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?;
            let host_kind = composer_host_kind(socket, next_id, &session.composer_target_scope)?;
            session.composer_host_kind = Some(host_kind.clone());
            let operation_gate = app.state::<ComposerOperationGate>();
            let _operation = operation_gate.try_acquire(&host_kind, &record.id)?;
            insert_record_into_composer(
                socket,
                next_id,
                &app.state::<Store>(),
                &host_kind,
                &workspace.root_path,
                &record,
            )
        }
        "resolveComposerInsertion" => {
            let workspace_id = require_session_workspace(session)?;
            let record_id = bounded_data_string(&request.data, "recordId", 128)?;
            let resolution = bounded_data_string(&request.data, "resolution", 16)?;
            let record = app
                .state::<Store>()
                .get_record(&workspace_id, &record_id)
                .map_err(|_| AdapterRuntimeError::Action("card_record_not_found"))?;
            if record.state != RecordState::Active {
                return Err(AdapterRuntimeError::Action("card_record_not_active"));
            }
            let workspace = app
                .state::<Store>()
                .get_workspace(&workspace_id)
                .map_err(|_| AdapterRuntimeError::Action("card_workspace_not_found"))?;
            let host_kind = composer_host_kind(socket, next_id, &session.composer_target_scope)?;
            session.composer_host_kind = Some(host_kind.clone());
            let operation_gate = app.state::<ComposerOperationGate>();
            let _operation = operation_gate.try_acquire(&host_kind, &record.id)?;
            let state = resolve_composer_uncertainty(
                app.state::<Store>().inner(),
                &host_kind,
                &record,
                &resolution,
            )?;
            if resolution == "retry" {
                insert_record_into_composer(
                    socket,
                    next_id,
                    app.state::<Store>().inner(),
                    &host_kind,
                    &workspace.root_path,
                    &record,
                )
            } else if state == "complete" {
                Ok("已按你的确认标记为已插入，未自动发送".to_owned())
            } else {
                Ok("已确认上次结果；其余未插入项仍可单独重试".to_owned())
            }
        }
        "openApp" => {
            lifecycle::show_main_window(app)
                .map_err(|_| AdapterRuntimeError::Action("card_open_app_failed"))?;
            Ok("已打开 WakeGPT".to_owned())
        }
        _ => Err(AdapterRuntimeError::Action("card_action_unsupported")),
    }
}

fn reconcile_card_notebook_selection(session: &mut CardSession, notebooks: &[Notebook]) {
    if session.notebook_id.as_ref().is_some_and(|id| {
        !notebooks
            .iter()
            .any(|notebook| &notebook.id == id && notebook.is_pinned)
    }) {
        session.notebook_id = None;
    }
}

fn card_notebooks_for_state(
    notebooks: &[Notebook],
    selected_notebook_id: Option<&str>,
) -> Vec<CardNotebook> {
    let mut visible = notebooks
        .iter()
        .filter(|notebook| notebook.is_pinned)
        .chain(notebooks.iter().filter(|notebook| !notebook.is_pinned))
        .take(48)
        .collect::<Vec<_>>();
    if let Some(selected) = selected_notebook_id.and_then(|selected_id| {
        notebooks
            .iter()
            .find(|notebook| notebook.id == selected_id && notebook.is_pinned)
    }) {
        if !visible.iter().any(|notebook| notebook.id == selected.id) {
            if visible.len() == 48 {
                visible.pop();
            }
            visible.push(selected);
        }
    }
    visible
        .into_iter()
        .map(|notebook| CardNotebook {
            id: notebook.id.clone(),
            display_name: notebook.display_name.clone(),
            target_state: notebook.target_state.as_db_value().to_owned(),
            last_error_code: notebook.last_error_code.clone(),
            numbering_start: notebook.numbering_start,
            numbering_sync_pending: notebook.numbering_sync_pending,
            attachment_directory: notebook.attachment_directory.clone(),
            attachment_directory_sync_pending: notebook.attachment_directory_sync_pending,
            is_pinned: notebook.is_pinned,
        })
        .collect()
}

fn card_workspaces_for_state(
    workspaces: &[Workspace],
    selected_workspace_id: Option<&str>,
) -> Vec<CardWorkspace> {
    let mut visible = workspaces.iter().take(24).collect::<Vec<_>>();
    if let Some(selected) = selected_workspace_id.and_then(|selected_id| {
        workspaces
            .iter()
            .find(|workspace| workspace.id == selected_id)
    }) {
        if !visible.iter().any(|workspace| workspace.id == selected.id) {
            if visible.len() == 24 {
                visible.pop();
            }
            visible.push(selected);
        }
    }
    visible
        .into_iter()
        .map(|workspace| CardWorkspace {
            id: workspace.id.clone(),
            display_name: workspace.display_name.clone(),
        })
        .collect()
}

fn build_card_state(
    app: &AppHandle,
    session: &mut CardSession,
) -> Result<CardState, AdapterRuntimeError> {
    let store = app.state::<Store>();
    let workspaces = store
        .list_workspaces()
        .map_err(|_| AdapterRuntimeError::Action("card_workspaces_read_failed"))?;
    let preferences = store
        .ui_preferences()
        .map_err(|_| AdapterRuntimeError::Action("card_selection_read_failed"))?;
    let submit_shortcut = preferences.submit_shortcut;
    initialize_card_selection(session, &preferences, &workspaces);
    if session
        .workspace_id
        .as_ref()
        .is_none_or(|id| !workspaces.iter().any(|workspace| &workspace.id == id))
    {
        session.workspace_id = workspaces.first().map(|workspace| workspace.id.clone());
        session.notebook_id = None;
    }
    let workspace_name = session
        .workspace_id
        .as_ref()
        .and_then(|id| workspaces.iter().find(|workspace| &workspace.id == id))
        .map(|workspace| workspace.display_name.clone())
        .unwrap_or_else(|| "未连接工作区".to_owned());
    let notebooks = if let Some(workspace_id) = &session.workspace_id {
        app.state::<SyncCoordinator>()
            .check_workspace_conflicts(&store, workspace_id)
            .map_err(|_| AdapterRuntimeError::Action("card_conflict_check_failed"))?;
        store
            .list_notebooks(workspace_id)
            .map_err(|_| AdapterRuntimeError::Action("card_notebooks_read_failed"))?
    } else {
        Vec::new()
    };
    reconcile_card_notebook_selection(session, &notebooks);
    let draft_body_markdown = if let Some(workspace_id) = &session.workspace_id {
        let saved = store
            .load_draft(workspace_id, session.notebook_id.as_deref())
            .map_err(|_| AdapterRuntimeError::Action("card_draft_read_failed"))?;
        let verified = app
            .state::<AttachmentCoordinator>()
            .restore_pending_draft(&saved.attachments);
        if verified.len() != saved.attachments.len() {
            let cleaned = verified
                .iter()
                .map(card_draft_attachment)
                .collect::<Vec<_>>();
            store
                .save_draft(
                    workspace_id,
                    session.notebook_id.as_deref(),
                    &saved.body_markdown,
                    &cleaned,
                )
                .map_err(|_| AdapterRuntimeError::Action("card_draft_save_failed"))?;
        }
        session.pending_attachments = verified;
        saved.body_markdown
    } else {
        session.pending_attachments.clear();
        String::new()
    };
    let recent_records = if let Some(workspace_id) = &session.workspace_id {
        let records = store
            .list_records(workspace_id, session.notebook_id.as_deref(), false)
            .map_err(|_| AdapterRuntimeError::Action("card_records_read_failed"))?;
        let composer_receipts = match session.composer_host_kind.as_deref() {
            Some(host_kind) => store
                .latest_composer_receipts_for_host(host_kind)
                .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_read_failed"))?
                .into_iter()
                .collect::<HashMap<_, _>>(),
            None => HashMap::new(),
        };
        let mut records = records
            .into_iter()
            .map(|record| {
                let insertion_state = composer_receipts
                    .get(&record.id)
                    .map(|receipt| composer_insertion_state_from_receipt(&record, receipt))
                    .transpose()?
                    .unwrap_or_else(|| "none".to_owned());
                Ok((record, insertion_state))
            })
            .collect::<Result<Vec<_>, AdapterRuntimeError>>()?;
        sort_card_records(&mut records);
        records
            .into_iter()
            .take(8)
            .map(|(record, insertion_state)| CardRecord {
                id: record.id,
                body_markdown: record.body_markdown,
                revision: record.revision,
                created_label: format_record_time(record.created_at_ms),
                attachment_count: record.attachments.len(),
                attachments: record
                    .attachments
                    .iter()
                    .map(|attachment| CardRecordAttachment {
                        id: attachment.id.clone(),
                        media_type: attachment.media_type.clone(),
                        byte_size: attachment.byte_size,
                        available: attachment.relocation_state == AttachmentRelocationState::Ready
                            && matches!(
                                attachment.file_state,
                                AttachmentFileState::Ready | AttachmentFileState::PreservedShared
                            ),
                    })
                    .collect(),
                attachments_ready: record.attachments.iter().all(|attachment| {
                    attachment.relocation_state == AttachmentRelocationState::Ready
                        && matches!(
                            attachment.file_state,
                            AttachmentFileState::Ready | AttachmentFileState::PreservedShared
                        )
                }),
                is_pinned: record.is_pinned,
                insertion_state,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(CardState {
        schema_version: 1,
        workspaces: card_workspaces_for_state(&workspaces, session.workspace_id.as_deref()),
        selected_workspace_id: session.workspace_id.clone(),
        workspace_name,
        notebooks: card_notebooks_for_state(&notebooks, session.notebook_id.as_deref()),
        selected_notebook_id: session.notebook_id.clone(),
        recent_records,
        pending_image_count: session.pending_attachments.len(),
        pending_images: session
            .pending_attachments
            .iter()
            .map(|attachment| CardPendingImage {
                token: attachment.token.clone(),
                media_type: attachment.media_type.clone(),
                byte_size: attachment.byte_size,
                display_name: attachment.display_name.clone(),
            })
            .collect(),
        draft_body_markdown,
        selection_key: format!(
            "{}:{}",
            session.workspace_id.as_deref().unwrap_or("none"),
            session.notebook_id.as_deref().unwrap_or("inbox")
        ),
        submit_shortcut,
        paused: false,
    })
}

fn sort_card_records(records: &mut [(Record, String)]) {
    records.sort_by(|(left, left_state), (right, right_state)| {
        let left_needs_review = matches!(left_state.as_str(), "uncertain" | "staleUncertain");
        let right_needs_review = matches!(right_state.as_str(), "uncertain" | "staleUncertain");
        right_needs_review
            .cmp(&left_needs_review)
            .then_with(|| right.is_pinned.cmp(&left.is_pinned))
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn send_card_response(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    session: &CardSession,
    request_id: Option<String>,
    ok: bool,
    message: impl Into<String>,
    state: CardState,
) -> Result<(), AdapterRuntimeError> {
    let receiver_name = format!("__wakegptReceive_{}", session.nonce);
    let payload = json!({
        "schemaVersion": 1,
        "sessionNonce": session.nonce,
        "requestId": request_id,
        "ok": ok,
        "message": message.into(),
        "state": state,
    });
    let receiver_json = serde_json::to_string(&receiver_name)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_response_serialize_failed"))?;
    let payload_json = serde_json::to_string(&payload)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_response_serialize_failed"))?;
    let expression = format!("globalThis[{receiver_json}]?.({payload_json})");
    cdp_send(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({ "expression": expression, "returnByValue": true }),
    )?;
    Ok(())
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComposerInsertionReceiptDetail {
    #[serde(default)]
    record_revision: Option<u64>,
    #[serde(default)]
    text_inserted: bool,
    #[serde(default)]
    text_uncertain: bool,
    #[serde(default)]
    inserted_attachment_ids: Vec<String>,
    #[serde(default)]
    inserted_content_sha256: Vec<String>,
    #[serde(default)]
    uncertain_attachment_ids: Vec<String>,
}

#[cfg(test)]
fn composer_insertion_state(
    store: &Store,
    record: &Record,
    host_kind: &str,
) -> Result<String, AdapterRuntimeError> {
    let Some(receipt) = store
        .latest_composer_receipt(&record.id, host_kind)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_read_failed"))?
    else {
        return Ok("none".to_owned());
    };
    composer_insertion_state_from_receipt(record, &receipt)
}

fn composer_insertion_state_from_receipt(
    record: &Record,
    receipt: &ComposerReceipt,
) -> Result<String, AdapterRuntimeError> {
    let detail = serde_json::from_str::<ComposerInsertionReceiptDetail>(&receipt.detail_json)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_invalid"))?;
    if composer_receipt_needs_current_revision_confirmation(receipt, &detail, record) {
        return Ok("staleUncertain".to_owned());
    }
    if receipt.state != composer_receipt_state(record, &detail) {
        return Err(AdapterRuntimeError::Action("card_composer_receipt_invalid"));
    }
    Ok(receipt.state.clone())
}

fn composer_host_kind(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    target_scope: &str,
) -> Result<String, AdapterRuntimeError> {
    let result = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": "`${String(location.href).slice(0,3072)}\\n${String(globalThis.performance?.timeOrigin ?? 'unavailable').slice(0,64)}`",
            "returnByValue": true,
        }),
    )?;
    let identity = result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(AdapterRuntimeError::Action(
            "card_composer_identity_unavailable",
        ))?;
    Ok(composer_host_kind_from_identity(target_scope, identity))
}

fn composer_target_scope(target: &PageTarget) -> String {
    sha256_hex(format!("{}:{}:{}", target.process_id, target.port, target.id).as_bytes())
}

fn composer_host_kind_from_identity(target_scope: &str, page_identity: &str) -> String {
    format!(
        "chatgpt:{}",
        sha256_hex(format!("{target_scope}\n{page_identity}").as_bytes())
    )
}

fn insert_record_into_composer(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    store: &Store,
    host_kind: &str,
    workspace_root: &str,
    record: &Record,
) -> Result<String, AdapterRuntimeError> {
    let prior_receipt = store
        .latest_composer_receipt(&record.id, host_kind)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_read_failed"))?;
    let prior_state = prior_receipt
        .as_ref()
        .map(|receipt| receipt.state.clone())
        .unwrap_or_else(|| "none".to_owned());
    let mut detail = match prior_receipt.as_ref() {
        Some(receipt) => {
            serde_json::from_str::<ComposerInsertionReceiptDetail>(&receipt.detail_json)
                .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_invalid"))?
        }
        None => ComposerInsertionReceiptDetail::default(),
    };
    if prior_receipt.as_ref().is_some_and(|receipt| {
        composer_receipt_needs_current_revision_confirmation(receipt, &detail, record)
    }) {
        return Ok(
            "这条记录在上次插入后已修改；请先查看 ChatGPT 输入框，再确认当前版本或重试".to_owned(),
        );
    }
    detail.record_revision = Some(record.revision);
    let inserted_ids = detail
        .inserted_attachment_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    update_composer_receipt_detail(record, &inserted_ids, &mut detail);
    if composer_receipt_has_uncertainty(&detail) {
        persist_composer_receipt(store, host_kind, record, "uncertain", &detail)?;
        return Ok(
            "上次插入结果无法确认；请先查看 ChatGPT 输入框，再选择“已看到”或“没有，重试”"
                .to_owned(),
        );
    }
    if prior_state == "uncertain" {
        return Err(AdapterRuntimeError::Action("card_composer_receipt_invalid"));
    }
    if prior_state == "complete" && composer_receipt_state(record, &detail) == "complete" {
        persist_composer_receipt(store, host_kind, record, "complete", &detail)?;
        return Ok("这条记录已插入当前对话框，未重复添加".to_owned());
    }
    verify_composer_capability(socket, next_id, !record.attachments.is_empty())?;
    let mut inserted_digests = store
        .composer_receipt_details_for_host(host_kind)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_read_failed"))?
        .into_iter()
        .filter_map(|detail| serde_json::from_str::<ComposerInsertionReceiptDetail>(&detail).ok())
        .flat_map(|detail| detail.inserted_content_sha256)
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .collect::<HashSet<_>>();

    let inserted_ids = if record.attachments.is_empty() {
        detail
            .inserted_attachment_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>()
    } else {
        let paths = verified_attachment_paths(workspace_root, record)?;
        insert_record_attachments(
            store,
            host_kind,
            record,
            paths,
            &mut inserted_digests,
            &mut detail,
            |path| attach_image_and_observe(socket, next_id, path),
        )?
    };

    insert_record_text(store, host_kind, record, &mut detail, |body| {
        let body_json = serde_json::to_string(body)
            .map_err(|_| AdapterRuntimeError::Protocol("codex_composer_text_serialize_failed"))?;
        let result = cdp_command_wait(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({
                "expression": format!("globalThis.__wakegptInsertIntoComposer?.({body_json})"),
                "returnByValue": true,
            }),
        )?;
        Ok(result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_bool)
            .unwrap_or(false))
    })?;

    update_composer_receipt_detail(record, &inserted_ids, &mut detail);
    let state = composer_receipt_state(record, &detail);
    persist_composer_receipt(store, host_kind, record, state, &detail)?;

    if state == "complete" {
        Ok("已插入到对话框，未自动发送".to_owned())
    } else if state == "uncertain" {
        Ok("插入结果无法确认；请先查看 ChatGPT 输入框，再选择“已看到”或“没有，重试”".to_owned())
    } else {
        let text_status = if detail.text_inserted {
            "文本已插入"
        } else {
            "文本待重试"
        };
        Ok(format!(
            "部分插入：{text_status}，图片 {}/{}；再次点击只重试失败项，未自动发送",
            detail.inserted_attachment_ids.len(),
            record.attachments.len()
        ))
    }
}

fn verify_composer_capability(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    require_image_input: bool,
) -> Result<(), AdapterRuntimeError> {
    let result = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": format!(
                "String(globalThis.__wakegptComposerCapability?.({require_image_input}) ?? 'unavailable')"
            ),
            "returnByValue": true,
        }),
    )?;
    match result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_str)
    {
        Some("ready") => Ok(()),
        Some("composerAmbiguous") => Err(AdapterRuntimeError::Action(
            "codex_composer_selector_ambiguous",
        )),
        Some("imageInputAmbiguous") => Err(AdapterRuntimeError::Action(
            "codex_image_input_selector_ambiguous",
        )),
        Some("composerMissing" | "imageInputMissing" | "unavailable") | None => Err(
            AdapterRuntimeError::Action("codex_composer_capability_unavailable"),
        ),
        Some(_) => Err(AdapterRuntimeError::Protocol(
            "codex_composer_capability_invalid",
        )),
    }
}

fn insert_record_attachments<F>(
    store: &Store,
    host_kind: &str,
    record: &Record,
    paths: Vec<String>,
    inserted_digests: &mut HashSet<String>,
    detail: &mut ComposerInsertionReceiptDetail,
    mut attach_image: F,
) -> Result<HashSet<String>, AdapterRuntimeError>
where
    F: FnMut(&str) -> Result<bool, AdapterRuntimeError>,
{
    let mut inserted_ids = detail
        .inserted_attachment_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    for (attachment, path) in record.attachments.iter().zip(paths) {
        if inserted_ids.contains(&attachment.id) {
            inserted_digests.insert(attachment.content_sha256.clone());
            continue;
        }
        if inserted_digests.contains(&attachment.content_sha256) {
            inserted_ids.insert(attachment.id.clone());
            inserted_digests.insert(attachment.content_sha256.clone());
            update_composer_receipt_detail(record, &inserted_ids, detail);
            persist_composer_receipt(store, host_kind, record, "partial", detail)?;
            continue;
        }
        detail.uncertain_attachment_ids.push(attachment.id.clone());
        update_composer_receipt_detail(record, &inserted_ids, detail);
        persist_composer_receipt(store, host_kind, record, "uncertain", detail)?;
        if !attach_image(&path)? {
            break;
        }
        detail
            .uncertain_attachment_ids
            .retain(|attachment_id| attachment_id != &attachment.id);
        inserted_ids.insert(attachment.id.clone());
        inserted_digests.insert(attachment.content_sha256.clone());
        update_composer_receipt_detail(record, &inserted_ids, detail);
        persist_composer_receipt(store, host_kind, record, "partial", detail)?;
    }
    Ok(inserted_ids)
}

fn insert_record_text<F>(
    store: &Store,
    host_kind: &str,
    record: &Record,
    detail: &mut ComposerInsertionReceiptDetail,
    mut insert_text: F,
) -> Result<(), AdapterRuntimeError>
where
    F: FnMut(&str) -> Result<bool, AdapterRuntimeError>,
{
    if record.body_markdown.trim().is_empty() {
        detail.text_inserted = true;
        detail.text_uncertain = false;
    } else if !detail.text_inserted && !composer_receipt_has_uncertainty(detail) {
        detail.text_uncertain = true;
        persist_composer_receipt(store, host_kind, record, "uncertain", detail)?;
        if insert_text(&record.body_markdown)? {
            detail.text_inserted = true;
            detail.text_uncertain = false;
        }
    }
    Ok(())
}

fn update_composer_receipt_detail(
    record: &Record,
    inserted_ids: &HashSet<String>,
    detail: &mut ComposerInsertionReceiptDetail,
) {
    detail.record_revision = Some(record.revision);
    detail.inserted_attachment_ids = record
        .attachments
        .iter()
        .filter(|attachment| inserted_ids.contains(&attachment.id))
        .map(|attachment| attachment.id.clone())
        .collect();
    detail.inserted_content_sha256 = record
        .attachments
        .iter()
        .filter(|attachment| inserted_ids.contains(&attachment.id))
        .map(|attachment| attachment.content_sha256.clone())
        .collect();
    let uncertain_ids = detail
        .uncertain_attachment_ids
        .iter()
        .filter(|attachment_id| !inserted_ids.contains(*attachment_id))
        .cloned()
        .collect::<HashSet<_>>();
    detail.uncertain_attachment_ids = record
        .attachments
        .iter()
        .filter(|attachment| uncertain_ids.contains(&attachment.id))
        .map(|attachment| attachment.id.clone())
        .collect();
    if detail.text_inserted {
        detail.text_uncertain = false;
    }
}

fn composer_receipt_has_uncertainty(detail: &ComposerInsertionReceiptDetail) -> bool {
    detail.text_uncertain || !detail.uncertain_attachment_ids.is_empty()
}

fn composer_receipt_needs_current_revision_confirmation(
    receipt: &ComposerReceipt,
    detail: &ComposerInsertionReceiptDetail,
    record: &Record,
) -> bool {
    match detail.record_revision {
        Some(revision) => revision != record.revision,
        None => receipt.created_at_ms <= record.updated_at_ms,
    }
}

fn composer_receipt_state(
    record: &Record,
    detail: &ComposerInsertionReceiptDetail,
) -> &'static str {
    if composer_receipt_has_uncertainty(detail) {
        "uncertain"
    } else if detail.text_inserted
        && detail.inserted_attachment_ids.len() == record.attachments.len()
    {
        "complete"
    } else {
        "partial"
    }
}

fn resolve_composer_uncertainty(
    store: &Store,
    host_kind: &str,
    record: &Record,
    resolution: &str,
) -> Result<&'static str, AdapterRuntimeError> {
    let receipt = store
        .latest_composer_receipt(&record.id, host_kind)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_read_failed"))?
        .ok_or(AdapterRuntimeError::Action(
            "card_composer_uncertainty_missing",
        ))?;
    let mut detail = serde_json::from_str::<ComposerInsertionReceiptDetail>(&receipt.detail_json)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_invalid"))?;
    if composer_receipt_needs_current_revision_confirmation(&receipt, &detail, record) {
        let mut inserted_ids = HashSet::new();
        detail = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            ..ComposerInsertionReceiptDetail::default()
        };
        match resolution {
            "confirm" => {
                detail.text_inserted = true;
                inserted_ids.extend(
                    record
                        .attachments
                        .iter()
                        .map(|attachment| attachment.id.clone()),
                );
            }
            "retry" => {}
            _ => {
                return Err(AdapterRuntimeError::Action(
                    "card_composer_resolution_invalid",
                ));
            }
        }
        update_composer_receipt_detail(record, &inserted_ids, &mut detail);
        let state = composer_receipt_state(record, &detail);
        persist_composer_receipt(store, host_kind, record, state, &detail)?;
        return Ok(state);
    }
    let mut inserted_ids = detail
        .inserted_attachment_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    update_composer_receipt_detail(record, &inserted_ids, &mut detail);
    if receipt.state != "uncertain" || !composer_receipt_has_uncertainty(&detail) {
        return Err(AdapterRuntimeError::Action(
            "card_composer_uncertainty_missing",
        ));
    }
    match resolution {
        "confirm" => {
            if detail.text_uncertain {
                detail.text_inserted = true;
            }
            inserted_ids.extend(detail.uncertain_attachment_ids.iter().cloned());
        }
        "retry" => {}
        _ => {
            return Err(AdapterRuntimeError::Action(
                "card_composer_resolution_invalid",
            ));
        }
    }
    detail.text_uncertain = false;
    detail.uncertain_attachment_ids.clear();
    update_composer_receipt_detail(record, &inserted_ids, &mut detail);
    let state = composer_receipt_state(record, &detail);
    persist_composer_receipt(store, host_kind, record, state, &detail)?;
    Ok(state)
}

fn persist_composer_receipt(
    store: &Store,
    host_kind: &str,
    record: &Record,
    state: &'static str,
    detail: &ComposerInsertionReceiptDetail,
) -> Result<(), AdapterRuntimeError> {
    let detail_json = serde_json::to_string(detail)
        .map_err(|_| AdapterRuntimeError::Protocol("codex_composer_receipt_serialize_failed"))?;
    store
        .record_composer_receipt(&record.id, host_kind, state, &detail_json)
        .map_err(|_| AdapterRuntimeError::Action("card_composer_receipt_write_failed"))
}

fn attach_image_and_observe(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    path: &str,
) -> Result<bool, AdapterRuntimeError> {
    let baseline_result = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": "Number(globalThis.__wakegptAttachmentSignalCount?.() ?? -1)",
            "returnByValue": true,
        }),
    )?;
    let baseline = baseline_result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or(AdapterRuntimeError::Action(
            "card_attachment_observer_unavailable",
        ))?;
    let mode_result = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": "String(globalThis.__wakegptComposerAttachmentMode?.() ?? 'unavailable')",
            "returnByValue": true,
        }),
    )?;
    let mode = mode_result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_str)
        .ok_or(AdapterRuntimeError::Protocol(
            "codex_composer_capability_invalid",
        ))?;
    if mode == "dragDrop" {
        let point_result = cdp_command_wait(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({
                "expression": "globalThis.__wakegptComposerDropPoint?.() || null",
                "returnByValue": true,
            }),
        )?;
        let point = point_result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_object)
            .ok_or(AdapterRuntimeError::Action(
                "codex_composer_capability_unavailable",
            ))?;
        let coordinate = |name: &str| {
            point
                .get(name)
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && (0.0..=100_000.0).contains(value))
                .ok_or(AdapterRuntimeError::Protocol(
                    "codex_composer_capability_invalid",
                ))
        };
        let x = coordinate("x")?;
        let y = coordinate("y")?;
        let drag_data = json!({
            "items": [],
            "files": [path],
            "dragOperationsMask": 1,
        });
        cdp_command_wait(
            socket,
            next_id,
            "Input.dispatchDragEvent",
            json!({ "type": "dragEnter", "x": x, "y": y, "data": drag_data.clone() }),
        )?;
        cdp_command_wait(
            socket,
            next_id,
            "Input.dispatchDragEvent",
            json!({ "type": "dragOver", "x": x, "y": y, "data": drag_data.clone() }),
        )?;
        if let Err(error) = cdp_command_wait(
            socket,
            next_id,
            "Input.dispatchDragEvent",
            json!({ "type": "drop", "x": x, "y": y, "data": drag_data.clone() }),
        ) {
            let _ = cdp_command_wait(
                socket,
                next_id,
                "Input.dispatchDragEvent",
                json!({ "type": "dragCancel", "x": x, "y": y, "data": drag_data }),
            );
            return Err(error);
        }
    } else if mode == "fileInput" {
        let input_result = cdp_command_wait(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({
                "expression": "globalThis.__wakegptComposerImageInput?.() || null",
                "returnByValue": false,
            }),
        )?;
        let Some(object_id) = input_result
            .get("result")
            .and_then(|result| result.get("objectId"))
            .and_then(Value::as_str)
        else {
            return Err(AdapterRuntimeError::Action(
                "codex_composer_capability_unavailable",
            ));
        };
        cdp_command_wait(
            socket,
            next_id,
            "DOM.setFileInputFiles",
            json!({ "files": [path], "objectId": object_id }),
        )?;
        cdp_command_wait(
            socket,
            next_id,
            "Runtime.callFunctionOn",
            json!({
                "objectId": object_id,
                "functionDeclaration": "function(){ this.dispatchEvent(new Event('change',{bubbles:true})); return this.files ? this.files.length : 0; }",
                "returnByValue": true,
            }),
        )?;
    } else {
        return Err(AdapterRuntimeError::Action(
            "codex_composer_capability_unavailable",
        ));
    }
    let observed = cdp_command_wait(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": format!("globalThis.__wakegptWaitForAttachmentSignal?.({baseline})"),
            "returnByValue": true,
            "awaitPromise": true,
        }),
    )?;
    Ok(observed
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn verified_attachment_paths(
    workspace_root: &str,
    record: &crate::domain::Record,
) -> Result<Vec<String>, AdapterRuntimeError> {
    let workspace = crate::domain::Workspace {
        id: record.workspace_id.clone(),
        display_name: "WakeGPT".to_owned(),
        root_path: workspace_root.to_owned(),
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    let root = open_workspace_root(&workspace)
        .map_err(|_| AdapterRuntimeError::Action("card_attachment_root_invalid"))?;
    let mut paths = Vec::with_capacity(record.attachments.len());
    for attachment in &record.attachments {
        let relative = Path::new(&attachment.managed_relative_path);
        let file = root
            .open_regular_file_read(relative)
            .map_err(|_| AdapterRuntimeError::Action("card_attachment_missing"))?;
        let metadata = file
            .metadata()
            .map_err(|_| AdapterRuntimeError::Action("card_attachment_metadata_failed"))?;
        if metadata.len() != attachment.byte_size || metadata.len() > 20 * 1024 * 1024 {
            return Err(AdapterRuntimeError::Action("card_attachment_size_changed"));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AdapterRuntimeError::Action("card_attachment_read_failed"))?;
        if file_sha256_hex(&bytes) != attachment.content_sha256 {
            return Err(AdapterRuntimeError::Action(
                "card_attachment_digest_changed",
            ));
        }
        let absolute = Path::new(workspace_root).join(relative);
        let absolute = absolute
            .to_str()
            .ok_or(AdapterRuntimeError::Action("card_attachment_path_invalid"))?;
        paths.push(absolute.to_owned());
    }
    Ok(paths)
}

fn attachment_lifecycle_message(record: &Record, trashed: bool) -> Option<&'static str> {
    if record
        .attachments
        .iter()
        .any(|attachment| attachment.file_state == AttachmentFileState::Modified)
    {
        return Some(if trashed {
            "记录已删除；已修改的图片为安全起见保留在工作区"
        } else {
            "记录已恢复；同路径图片未被覆盖"
        });
    }
    if record
        .attachments
        .iter()
        .any(|attachment| attachment.file_state == AttachmentFileState::Missing)
    {
        return Some(if trashed {
            "记录已删除；部分图片文件此前已不存在"
        } else {
            "记录已恢复，但部分图片已不在系统废纸篓"
        });
    }
    if record
        .attachments
        .iter()
        .any(|attachment| attachment.file_state == AttachmentFileState::PreservedShared)
    {
        return Some("记录已删除；共享图片仍安全保留");
    }
    None
}

fn bounded_data_string(
    data: &Value,
    key: &'static str,
    max_bytes: usize,
) -> Result<String, AdapterRuntimeError> {
    optional_data_string(data, key, max_bytes)?
        .filter(|value| !value.trim().is_empty())
        .ok_or(AdapterRuntimeError::Action("card_action_data_invalid"))
}

fn optional_data_string(
    data: &Value,
    key: &'static str,
    max_bytes: usize,
) -> Result<Option<String>, AdapterRuntimeError> {
    let Some(value) = data.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or(AdapterRuntimeError::Action("card_action_data_invalid"))?;
    if value.len() > max_bytes || value.bytes().any(|byte| byte == 0) {
        return Err(AdapterRuntimeError::Action("card_action_data_invalid"));
    }
    Ok(Some(value.to_owned()))
}

fn data_numbering_style(data: &Value) -> Result<NumberingStyle, AdapterRuntimeError> {
    serde_json::from_value(
        data.get("numberingStyle")
            .cloned()
            .unwrap_or_else(|| Value::String("numeric".to_owned())),
    )
    .map_err(|_| AdapterRuntimeError::Action("card_numbering_style_invalid"))
}

fn data_numbering_start(data: &Value) -> Result<u32, AdapterRuntimeError> {
    let value = match data.get("numberingStart") {
        Some(value) => value
            .as_u64()
            .ok_or(AdapterRuntimeError::Action("card_numbering_start_invalid"))?,
        None => u64::from(DEFAULT_NUMBERING_START),
    };
    let value = u32::try_from(value)
        .map_err(|_| AdapterRuntimeError::Action("card_numbering_start_invalid"))?;
    validate_numbering_start(value)
        .map_err(|_| AdapterRuntimeError::Action("card_numbering_start_invalid"))
}

fn require_session_workspace(session: &CardSession) -> Result<String, AdapterRuntimeError> {
    session
        .workspace_id
        .clone()
        .ok_or(AdapterRuntimeError::Action("card_workspace_required"))
}

fn notebook_file_name(display_name: &str) -> Result<String, AdapterRuntimeError> {
    let mut output = String::new();
    for character in display_name.trim().chars() {
        if character.is_control() || matches!(character, '/' | '\\' | ':') {
            output.push('_');
        } else {
            output.push(character);
        }
    }
    let output = output.trim_matches([' ', '.']);
    if output.is_empty() || !output.chars().any(char::is_alphanumeric) {
        return Err(AdapterRuntimeError::Action("card_notebook_name_invalid"));
    }
    Ok(format!("{output}.md"))
}

fn selected_workspace_relative_path(
    workspace_root: &str,
    selected: &Path,
) -> Result<String, AdapterRuntimeError> {
    let relative = selected
        .strip_prefix(Path::new(workspace_root))
        .map_err(|_| AdapterRuntimeError::Action("card_notebook_outside_workspace"))?;
    let relative = relative
        .to_str()
        .ok_or(AdapterRuntimeError::Action("card_notebook_path_invalid"))?;
    crate::domain::validate_notebook_relative_path(relative)
        .map_err(|_| AdapterRuntimeError::Action("card_notebook_path_invalid"))
}

fn format_record_time(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000);
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|timestamp| {
            timestamp
                .to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC))
                .format(time::macros::format_description!(
                    "[month]-[day] [hour]:[minute]"
                ))
                .ok()
        })
        .unwrap_or_else(|| "刚刚".to_owned())
}

fn card_action_message(code: &'static str) -> String {
    match code {
        "card_workspace_required" => "请先在 WakeGPT 连接工作区",
        "card_record_empty" => "请输入文本或添加图片",
        "card_too_many_attachments" => "每条记录最多 10 张图片",
        "card_attachment_size_invalid" => "单张图片不能超过 20 MiB",
        "card_attachments_total_too_large" => "单条记录的图片合计不能超过 100 MiB",
        "card_attachment_payload_invalid" | "card_attachment_chunk_invalid" => {
            "剪贴板图片数据无效，请重新粘贴"
        }
        "card_attachment_chunk_unavailable" => "无法读取剪贴板图片，请重新粘贴",
        "card_attachment_stage_failed" => "图片暂存失败，请在 WakeGPT 中检查状态",
        "card_attachment_materialize_failed" => "图片未保存，编辑内容已保留，请重试",
        "card_record_attachment_unavailable" => "记录图片仍在恢复中，暂时不能修改",
        "card_record_edit_saved_pending" => "修改已保存在本地，Markdown 同步等待恢复",
        "card_record_edit_attachment_recovery" => "修改已保存，图片清理等待 WakeGPT 恢复",
        "card_attachment_preview_unavailable" | "card_attachment_preview_receiver_unavailable" => {
            "图片当前不可预览"
        }
        "card_attachment_preview_changed" => "图片内容已变化，为安全起见未加载",
        "card_notebook_outside_workspace" => "只能绑定当前工作区内的 Markdown",
        "card_composer_insert_failed" => "未找到可用的 Codex 输入框",
        "codex_composer_selector_ambiguous" => "检测到多个 Codex 输入框，未写入任何内容",
        "codex_composer_capability_unavailable" => "Codex 输入能力尚未就绪，未写入任何内容",
        "codex_image_input_selector_ambiguous" => "检测到多个 Codex 图片入口，未附加图片",
        "card_attachment_observer_unavailable" => "无法确认图片是否已附加，请在对话框中检查",
        "card_record_copy_attachment_unavailable" => "记录图片不可用，未创建不完整副本",
        "card_record_copy_failed" => "复制未完成，请检查目标速记本",
        "card_record_migrate_failed" => "迁移未完成，原记录仍安全保留",
        "card_record_pin_failed" => "置顶状态未保存",
        "card_notebook_pin_failed" => "Tab 固定状态未保存",
        "card_composer_receipt_invalid" => "插入回执异常，请在 WakeGPT 中恢复",
        "card_composer_operation_busy" => "该记录的另一个插入操作仍在进行，未重复执行",
        "card_composer_operation_unavailable" => "无法锁定当前插入操作，未写入对话框",
        "card_composer_receipt_read_failed" => "无法读取插入回执，未重试插入",
        "card_composer_receipt_write_failed" => "无法保存插入回执，请检查当前输入框",
        "card_composer_identity_unavailable" => "无法确认当前 ChatGPT 对话，未更改插入状态",
        "card_composer_uncertainty_missing" => "没有待核对的插入结果，请刷新后重试",
        "card_composer_resolution_invalid" => "插入核对选项无效，请重试",
        "card_create_ack_unavailable" => "记录确认状态已变化，请刷新后重试",
        "card_record_changed" => "记录已在其他位置变更，请刷新后重试",
        _ => "操作未完成，请在 WakeGPT 中检查状态",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread;

    const MAX_TEST_HTTP_REQUEST_BYTES: usize = 4096;
    const TEST_JSON_SERVER_DEADLINE: Duration = Duration::from_secs(5);

    fn runtime_snapshot(
        state: AdapterEndpointState,
        connected_target_count: usize,
    ) -> RuntimeSnapshot {
        RuntimeSnapshot {
            state,
            last_error_code: None,
            detected_instance_count: connected_target_count,
            connectable_instance_count: connected_target_count,
            available_target_count: connected_target_count,
            connected_target_count,
            failed_target_count: 0,
            instances: Vec::new(),
        }
    }

    #[test]
    fn update_pause_wait_requires_a_drained_codex_runtime() {
        let runtime = CodexIntegrationRuntime::default();
        runtime.replace_for_test(runtime_snapshot(AdapterEndpointState::Paused, 1));
        assert!(!runtime.wait_until_paused(Duration::from_millis(10)));
        runtime.replace_for_test(runtime_snapshot(AdapterEndpointState::Paused, 0));
        assert!(runtime.wait_until_paused(Duration::from_millis(10)));
    }

    fn read_test_http_request(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            if request.len() >= MAX_TEST_HTTP_REQUEST_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "test HTTP request header is too large",
                ));
            }
            let remaining = MAX_TEST_HTTP_REQUEST_BYTES - request.len();
            let read_limit = chunk.len().min(remaining);
            let read = reader.read(&mut chunk[..read_limit])?;
            if read == 0 {
                return Err(std::io::Error::new(
                    if request.is_empty() {
                        std::io::ErrorKind::ConnectionAborted
                    } else {
                        std::io::ErrorKind::UnexpectedEof
                    },
                    if request.is_empty() {
                        "test HTTP probe disconnected before sending bytes"
                    } else {
                        "test HTTP request ended before its headers"
                    },
                ));
            }
            request.extend_from_slice(&chunk[..read]);
        }
        Ok(request)
    }

    fn spawn_test_json_server(
        listener: TcpListener,
        body: String,
        release_receiver: Option<Receiver<()>>,
    ) -> (
        thread::JoinHandle<()>,
        Receiver<()>,
        Receiver<Result<(), String>>,
    ) {
        spawn_test_json_server_with_deadline(
            listener,
            body,
            release_receiver,
            TEST_JSON_SERVER_DEADLINE,
        )
    }

    fn spawn_test_json_server_with_deadline(
        listener: TcpListener,
        body: String,
        release_receiver: Option<Receiver<()>>,
        deadline_duration: Duration,
    ) -> (
        thread::JoinHandle<()>,
        Receiver<()>,
        Receiver<Result<(), String>>,
    ) {
        let port = listener.local_addr().unwrap().port();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            ready_sender.send(()).unwrap();
            let deadline = Instant::now() + deadline_duration;
            let result = loop {
                if Instant::now() >= deadline {
                    break Err(format!(
                        "test HTTP server on port {port} timed out waiting for a valid request"
                    ));
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => break Err(format!("test HTTP accept failed: {error}")),
                };
                if let Err(error) = stream.set_nonblocking(false) {
                    break Err(format!("test HTTP stream setup failed: {error}"));
                }
                let request = match read_test_http_request(&mut stream) {
                    Ok(request) => request,
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionAborted => {
                        continue;
                    }
                    Err(error) => break Err(format!("invalid test HTTP request: {error}")),
                };
                let Ok(request) = std::str::from_utf8(&request) else {
                    break Err("test HTTP request is not UTF-8".to_owned());
                };
                if !request.starts_with("GET /json/list HTTP/1.1\r\n")
                    || !request.contains(&format!("Host: 127.0.0.1:{port}\r\n"))
                {
                    break Err("test HTTP request identity mismatch".to_owned());
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length:{}\r\n\r\n{body}",
                    body.len()
                );
                if let Err(error) = stream.write_all(response.as_bytes()) {
                    break Err(format!("test HTTP response failed: {error}"));
                }
                if let Some(receiver) = release_receiver.as_ref() {
                    let _ = receiver.recv_timeout(Duration::from_secs(2));
                }
                break Ok(());
            };
            let _ = completion_sender.send(result);
        });
        (server, ready_receiver, completion_receiver)
    }

    fn join_test_json_server(
        server: thread::JoinHandle<()>,
        completion: Receiver<Result<(), String>>,
    ) {
        match completion.recv_timeout(TEST_JSON_SERVER_DEADLINE + Duration::from_secs(1)) {
            Ok(result) => result.unwrap(),
            Err(RecvTimeoutError::Timeout) => panic!("test HTTP server exceeded its deadline"),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("test HTTP server stopped without a completion result")
            }
        }
        server.join().unwrap();
    }

    #[test]
    fn test_http_request_reader_accepts_fragmented_headers() {
        let first = b"GET /json/list HTTP/1.1\r\n";
        let second = b"Host: 127.0.0.1:43119\r\nConnection: close\r\n\r\n";
        let mut fragmented = Cursor::new(first).chain(Cursor::new(second));
        let request = read_test_http_request(&mut fragmented).unwrap();
        assert_eq!(request, [first.as_slice(), second.as_slice()].concat());
    }

    #[test]
    fn test_json_server_ignores_an_empty_probe_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = format!(
            r#"[{{"id":"PAGE","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/PAGE"}}]"#
        );
        let expected_body = body.clone().into_bytes();
        let (server, ready, completion) = spawn_test_json_server(listener, body, None);
        ready.recv().unwrap();
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap());
        assert_eq!(fetch_target_list(port).unwrap(), expected_body);
        join_test_json_server(server, completion);
    }

    #[test]
    fn test_json_server_reports_a_bounded_missing_probe() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (server, ready, completion) = spawn_test_json_server_with_deadline(
            listener,
            "[]".to_owned(),
            None,
            Duration::from_millis(50),
        );
        ready.recv().unwrap();
        let result = completion.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(result
            .unwrap_err()
            .contains("timed out waiting for a valid request"));
        server.join().unwrap();
    }

    #[test]
    fn card_visibility_diagnostics_accept_only_bounded_enums() {
        for layout in ["full", "compact", "collapsed", "drawer"] {
            assert_eq!(
                card_visibility_diagnostic(&json!({
                    "visibility": "visible",
                    "layout": layout,
                }))
                .unwrap(),
                ("visible", Some(layout))
            );
        }
        for (wire_value, diagnostic_value) in [
            ("contextMissing", "context_missing"),
            ("fullscreen", "fullscreen"),
            ("modal", "modal"),
            ("viewportOverlay", "viewport_overlay"),
            ("mediaLightbox", "media_lightbox"),
            ("contextAmbiguous", "context_ambiguous"),
        ] {
            assert_eq!(
                card_visibility_diagnostic(&json!({ "visibility": wire_value })).unwrap(),
                (diagnostic_value, None)
            );
        }

        for invalid in [
            json!({}),
            json!({ "visibility": "visible" }),
            json!({ "visibility": "fullscreen", "layout": "full" }),
            json!({ "visibility": "visible", "layout": "unknown" }),
            json!({ "visibility": "../../private", "layout": null }),
            json!({ "visibility": 1, "layout": null }),
        ] {
            assert_eq!(
                card_visibility_diagnostic(&invalid).unwrap_err().code(),
                "codex_card_visibility_invalid"
            );
        }
    }

    #[test]
    fn active_port_parser_is_strict() {
        assert_eq!(
            parse_active_port("58971\n/devtools/browser/2faf2f95-d708-4a96-ae53-c8151c76fd2f\n")
                .unwrap(),
            DebugEndpoint {
                port: 58971,
                browser_path: "/devtools/browser/2faf2f95-d708-4a96-ae53-c8151c76fd2f".to_owned(),
            }
        );
        assert!(parse_active_port("0\n/devtools/browser/value\n").is_err());
        assert!(parse_active_port("58971 \n/devtools/browser/value\n").is_err());
        assert!(parse_active_port("58971\nhttp://remote.invalid\n").is_err());
        assert!(parse_active_port("58971\n/devtools/browser/../../escape\n").is_err());
    }

    #[test]
    fn card_image_upload_contract_rejects_invalid_size_order_and_hex() {
        let uploads = card_image_uploads(&json!({
            "images": [
                { "index": 0, "byteSize": 68 },
                { "index": 1, "byteSize": 80 }
            ]
        }))
        .expect("valid card image metadata");
        assert_eq!(
            uploads,
            vec![
                CardImageUpload {
                    index: 0,
                    byte_size: 68,
                },
                CardImageUpload {
                    index: 1,
                    byte_size: 80,
                },
            ]
        );
        let invalid_order = card_image_uploads(&json!({
            "images": [{ "index": 1, "byteSize": 68 }]
        }))
        .expect_err("non-contiguous indexes must fail closed");
        assert_eq!(invalid_order.code(), "card_attachment_size_invalid");
        let invalid_size = card_image_uploads(&json!({
            "images": [{ "index": 0, "byteSize": MAX_ATTACHMENT_BYTES + 1 }]
        }))
        .expect_err("oversized images must fail closed");
        assert_eq!(invalid_size.code(), "card_attachment_size_invalid");

        assert_eq!(
            decode_card_image_hex("0001feff", 4).expect("decode valid hex"),
            vec![0, 1, 254, 255]
        );
        assert_eq!(card_image_hex(&[0, 1, 254, 255]), "0001feff");
        for invalid in ["0", "zz", "0001"] {
            assert!(decode_card_image_hex(invalid, 1).is_err());
        }
    }

    #[test]
    fn card_record_edit_plan_isolates_new_images_and_rebuilds_the_final_set() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Card edit plan", "/tmp/wakegpt-card-edit-plan")
            .unwrap();
        let first_digest = "a".repeat(64);
        let removed_digest = "b".repeat(64);
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Body",
                &[
                    NewAttachment {
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
                    },
                    NewAttachment {
                        media_type: "image/png".to_owned(),
                        managed_relative_path: managed_attachment_relative_path(
                            INBOX_ATTACHMENT_DIRECTORY,
                            &removed_digest,
                            "image/png",
                        )
                        .unwrap(),
                        previous_managed_relative_path: None,
                        content_sha256: removed_digest.clone(),
                        byte_size: 9,
                    },
                ],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let retained = vec![record.attachments[0].id.clone()];
        let new_digest = "c".repeat(64);
        let pending = |token: &str, digest: &str, size: u64| PendingAttachment {
            token: token.to_owned(),
            media_type: "image/png".to_owned(),
            byte_size: size,
            content_sha256: digest.to_owned(),
            display_name: "image.png".to_owned(),
        };
        let plan = card_record_edit_plan(
            &record,
            Some(&retained),
            &[
                pending(&new_id(), &first_digest, 8),
                pending(&new_id(), &removed_digest, 9),
                pending(&new_id(), &new_digest, 10),
                pending(&new_id(), &new_digest, 10),
            ],
        )
        .unwrap();
        assert_eq!(plan.retained_attachment_ids, retained);
        assert_eq!(
            plan.pending_attachments
                .iter()
                .map(|attachment| attachment.content_sha256.as_str())
                .collect::<Vec<_>>(),
            vec![removed_digest.as_str(), new_digest.as_str()]
        );

        let invalid = vec![new_id()];
        assert_eq!(
            card_record_edit_plan(&record, Some(&invalid), &[])
                .unwrap_err()
                .code(),
            "card_attachment_set_invalid"
        );
    }

    #[test]
    fn target_parser_accepts_all_exact_loopback_app_pages() {
        let candidate = DebugCandidate {
            process_id: 42,
            port: 43119,
        };
        let valid = br#"[
          {
            "id":"PAGE_1",
            "type":"page",
            "url":"app://-/index.html",
            "webSocketDebuggerUrl":"ws://127.0.0.1:43119/devtools/page/PAGE_1"
          },
          {
            "id":"WORKER_1",
            "type":"worker",
            "url":"app://-/worker.js",
            "webSocketDebuggerUrl":"ws://127.0.0.1:43119/devtools/page/WORKER_1"
          }
        ]"#;
        assert_eq!(
            parse_page_targets(valid, candidate).unwrap(),
            vec![PageTarget {
                process_id: 42,
                port: 43119,
                id: "PAGE_1".to_owned(),
                web_socket_debugger_url: "ws://127.0.0.1:43119/devtools/page/PAGE_1".to_owned(),
            }]
        );

        let remote = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("127.0.0.1", "192.0.2.1");
        assert_eq!(
            parse_page_targets(remote.as_bytes(), candidate).unwrap_err(),
            AdapterError::TargetSocketInvalid
        );
        let multiple = br#"[
          {
            "id":"PAGE_1",
            "type":"page",
            "url":"app://-/index.html",
            "webSocketDebuggerUrl":"ws://127.0.0.1:43119/devtools/page/PAGE_1"
          },
          {
            "id":"PAGE_2",
            "type":"page",
            "url":"app://-/index.html",
            "webSocketDebuggerUrl":"ws://127.0.0.1:43119/devtools/page/PAGE_2"
          }
        ]"#;
        assert_eq!(parse_page_targets(multiple, candidate).unwrap().len(), 2);
        let duplicate = String::from_utf8(multiple.to_vec())
            .unwrap()
            .replace("PAGE_2", "PAGE_1");
        assert_eq!(
            parse_page_targets(duplicate.as_bytes(), candidate).unwrap_err(),
            AdapterError::TargetAmbiguous,
        );
    }

    #[test]
    fn target_list_http_probe_is_loopback_and_bounded() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = format!(
            r#"[{{"id":"PAGE","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/PAGE"}}]"#
        );
        let expected_body = body.clone().into_bytes();
        let (server, ready, completion) = spawn_test_json_server(listener, body, None);
        ready.recv().unwrap();
        assert_eq!(fetch_target_list(port).unwrap(), expected_body);
        join_test_json_server(server, completion);
    }

    #[test]
    fn target_list_http_probe_stops_at_content_length_without_waiting_for_eof() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = format!(
            r#"[{{"id":"PAGE","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/PAGE"}}]"#
        );
        let expected_body = body.clone().into_bytes();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (server, ready, completion) =
            spawn_test_json_server(listener, body, Some(release_receiver));
        ready.recv().unwrap();
        assert_eq!(fetch_target_list(port).unwrap(), expected_body);
        release_sender.send(()).unwrap();
        join_test_json_server(server, completion);
    }

    #[test]
    fn discovery_keeps_multiple_processes_and_windows_independent() {
        let first_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let first_port = first_listener.local_addr().unwrap().port();
        let second_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let second_port = second_listener.local_addr().unwrap().port();
        let first_body = format!(
            r#"[
              {{"id":"WINDOW_A","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{first_port}/devtools/page/WINDOW_A"}},
              {{"id":"WINDOW_B","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{first_port}/devtools/page/WINDOW_B"}}
            ]"#
        );
        let second_body = format!(
            r#"[{{"id":"WINDOW_C","type":"page","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{second_port}/devtools/page/WINDOW_C"}}]"#
        );
        let (first_server, first_ready, first_completion) =
            spawn_test_json_server(first_listener, first_body, None);
        let (second_server, second_ready, second_completion) =
            spawn_test_json_server(second_listener, second_body, None);
        first_ready.recv().unwrap();
        second_ready.recv().unwrap();
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, first_port)).unwrap());
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, second_port)).unwrap());
        let discovery = discover_targets_for_instances(&[
            RunningInstance {
                process_id: 101,
                is_default: true,
                can_restart: false,
                codex_version: Some(SUPPORTED_CODEX_VERSION.to_owned()),
                codex_build: Some(SUPPORTED_CODEX_BUILD.to_owned()),
                candidate: Some(DebugCandidate {
                    process_id: 101,
                    port: first_port,
                }),
            },
            RunningInstance {
                process_id: 202,
                is_default: false,
                can_restart: true,
                codex_version: Some(SUPPORTED_CODEX_VERSION.to_owned()),
                codex_build: Some(SUPPORTED_CODEX_BUILD.to_owned()),
                candidate: Some(DebugCandidate {
                    process_id: 202,
                    port: second_port,
                }),
            },
            RunningInstance {
                process_id: 303,
                is_default: false,
                can_restart: true,
                codex_version: Some(SUPPORTED_CODEX_VERSION.to_owned()),
                codex_build: Some(SUPPORTED_CODEX_BUILD.to_owned()),
                candidate: None,
            },
        ]);
        let discovered_process_ids = discovery
            .targets
            .iter()
            .map(|target| target.process_id)
            .collect::<Vec<_>>();
        assert_eq!(
            discovered_process_ids,
            vec![101, 101, 202],
            "both explicit loopback candidates must be probed before server cleanup: {:?}",
            discovery.instances
        );
        join_test_json_server(first_server, first_completion);
        join_test_json_server(second_server, second_completion);
        assert_eq!(discovery.detected_instance_count, 3);
        assert_eq!(discovery.connectable_instance_count, 2);
        assert_eq!(discovery.targets.len(), 3);
        assert_eq!(discovery.instances.len(), 3);
        assert_eq!(
            discovery.last_error,
            Some(AdapterError::ActivePortUnavailable)
        );

        let first_key = discovery.targets[0].key();
        let second_key = discovery.targets[1].key();
        let third_key = discovery.targets[2].key();
        let probes = build_instance_probes(
            &discovery,
            &HashSet::from([first_key.clone(), second_key.clone()]),
            &HashSet::from([first_key, second_key]),
            &HashMap::from([(third_key, ("codex_card_mount_rejected", Instant::now()))]),
            false,
        );
        assert_eq!(probes.len(), 3);
        assert_eq!(probes[0].state, AdapterInstanceState::Connected);
        assert_eq!(probes[0].connected_target_count, 2);
        assert_eq!(probes[1].state, AdapterInstanceState::Failed);
        assert_eq!(probes[2].state, AdapterInstanceState::Unavailable);
        assert!(probes[2].can_restart);
    }

    #[test]
    fn discovery_never_probes_an_anonymous_fixed_port() {
        let discovery = discover_targets_for_instances(&[RunningInstance {
            process_id: 404,
            is_default: true,
            can_restart: false,
            codex_version: Some(SUPPORTED_CODEX_VERSION.to_owned()),
            codex_build: Some(SUPPORTED_CODEX_BUILD.to_owned()),
            candidate: None,
        }]);
        assert!(discovery.targets.is_empty());
        assert_eq!(discovery.connectable_instance_count, 0);
        assert_eq!(discovery.instances.len(), 1);
        assert_eq!(
            discovery.last_error,
            Some(AdapterError::ActivePortUnavailable)
        );
    }

    #[test]
    fn unknown_or_unverified_host_contract_never_creates_an_injection_target() {
        assert_eq!(
            codex_host_contract_error(Some(SUPPORTED_CODEX_VERSION), Some(SUPPORTED_CODEX_BUILD),),
            None
        );
        assert_eq!(
            codex_host_contract_error(Some(SUPPORTED_CODEX_VERSION), Some("9999")),
            Some(AdapterError::VersionUnsupported)
        );
        assert_eq!(
            codex_host_contract_error(None, None),
            Some(AdapterError::VersionUnavailable)
        );

        let unsupported = discover_targets_for_instances(&[RunningInstance {
            process_id: 505,
            is_default: true,
            can_restart: false,
            codex_version: Some(SUPPORTED_CODEX_VERSION.to_owned()),
            codex_build: Some("9999".to_owned()),
            candidate: Some(DebugCandidate {
                process_id: 505,
                port: 9,
            }),
        }]);
        assert!(unsupported.targets.is_empty());
        assert_eq!(unsupported.connectable_instance_count, 0);
        assert_eq!(
            unsupported.last_error,
            Some(AdapterError::VersionUnsupported)
        );
        let probes = build_instance_probes(
            &unsupported,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            false,
        );
        assert_eq!(probes[0].state, AdapterInstanceState::Incompatible);
        assert_eq!(probes[0].detected_codex_build.as_deref(), Some("9999"));

        let unverified = discover_targets_for_instances(&[RunningInstance {
            process_id: 606,
            is_default: false,
            can_restart: true,
            codex_version: None,
            codex_build: None,
            candidate: Some(DebugCandidate {
                process_id: 606,
                port: 9,
            }),
        }]);
        assert!(unverified.targets.is_empty());
        assert_eq!(
            unverified.last_error,
            Some(AdapterError::VersionUnavailable)
        );
    }

    #[test]
    fn card_script_is_shadow_isolated_and_never_sends_the_host_composer() {
        assert!(CARD_SCRIPT.contains(ADAPTER_VERSION));
        assert!(CARD_SCRIPT.contains("attachShadow"));
        assert!(CARD_SCRIPT.contains("MutationObserver"));
        assert!(CARD_SCRIPT.contains("ResizeObserver"));
        assert!(CARD_SCRIPT.contains(":host { all: initial; display:block"));
        assert!(CARD_SCRIPT.contains(":host([hidden]) { display:none !important; }"));
        assert!(CARD_SCRIPT.contains("rect.left >= window.innerWidth * 0.62"));
        assert!(CARD_SCRIPT.contains("parseFloat(style.borderTopLeftRadius) >= 16"));
        assert!(CARD_SCRIPT.contains("style.boxShadow !== \"none\""));
        assert!(CARD_SCRIPT.contains("const resolveContextCandidates = (candidates) =>"));
        assert!(CARD_SCRIPT.contains("distinct.length !== 1"));
        assert!(CARD_SCRIPT.contains("contextResolution.state === \"ambiguous\""));
        assert!(CARD_SCRIPT
            .contains("const pageBlocksCard = (visibleElements = visiblePageElements()) =>"));
        assert!(CARD_SCRIPT.contains("const visiblePageElements = () =>"));
        assert!(CARD_SCRIPT.contains("const belongsToFixedViewportLayer = (element) =>"));
        assert!(CARD_SCRIPT.contains("style.boxShadow !== \"none\""));
        assert!(CARD_SCRIPT.contains("document.querySelector(\":modal\")"));
        assert!(CARD_SCRIPT.contains("document.querySelectorAll(\"img,video,canvas\")"));
        assert!(CARD_SCRIPT.contains("const blockReason = pageBlockReason(visibleElements)"));
        assert!(CARD_SCRIPT.contains("if (blockReason)"));
        assert!(CARD_SCRIPT.contains("reportCardVisibility(blockReason)"));
        assert!(CARD_SCRIPT.contains("notify(\"cardVisibilityChanged\""));
        assert!(CARD_SCRIPT.contains("attributeFilter: [\"aria-modal\", \"class\", \"data-state\""));
        assert!(!CARD_SCRIPT.contains("return candidate === label"));
        assert!(CARD_SCRIPT.contains("\"来源\", \"Source\", \"Sources\""));
        assert!(CARD_SCRIPT.contains("host.dataset.layout = \"compact\""));
        assert!(CARD_SCRIPT.contains("host.dataset.layout = \"collapsed\""));
        assert!(CARD_SCRIPT.contains("host.dataset.layout = \"drawer\""));
        assert!(CARD_SCRIPT.contains("const heartbeatAge = now - lastHeartbeatAt;"));
        assert!(CARD_SCRIPT.contains("&& heartbeatAge > 7500"));
        assert!(CARD_SCRIPT.contains("pendingActions.size === 0 || heartbeatAge > 15000"));
        assert!(CARD_SCRIPT
            .contains("if (!host.isConnected && document.body) document.body.append(host)"));
        assert!(CARD_SCRIPT.contains("observer.observe(document.documentElement"));
        assert!(CARD_SCRIPT.contains("__wakegptActiveSessionNonce"));
        assert!(CARD_SCRIPT.contains("__wakegptStopCodexCard"));
        assert!(CARD_SCRIPT.contains(
            "hostContractId,\n      sessionNonce: bootstrap.sessionNonce,\n      bindingName: bootstrap.bindingName,\n      receiverName,"
        ));
        assert!(CARD_SCRIPT.contains("const preserveTransientUi = !message.requestId"));
        assert!(CARD_SCRIPT.contains("插入到对话框"));
        assert!(CARD_SCRIPT.contains("__wakegptInsertIntoComposer"));
        assert!(CARD_SCRIPT.contains("__wakegptWaitForAttachmentSignal"));
        assert!(CARD_SCRIPT.contains("copyRecord"));
        assert!(CARD_SCRIPT.contains("migrateRecord"));
        assert!(CARD_SCRIPT.contains("pinRecord"));
        assert!(CARD_SCRIPT.contains("重试失败项"));
        assert!(CARD_SCRIPT.contains("event.isComposing"));
        assert!(CARD_SCRIPT.contains("input.addEventListener(\"paste\""));
        assert!(CARD_SCRIPT.contains("imageInput.addEventListener(\"change\""));
        assert!(CARD_SCRIPT.contains("__wakegptReadPastedImageChunk"));
        assert!(CARD_SCRIPT.contains("stagePastedImages"));
        assert!(!CARD_SCRIPT.contains("send(\"pickImages\""));
        assert!(CARD_SCRIPT.contains("state.submitShortcut === \"commandEnter\""));
        assert!(CARD_SCRIPT.contains("selectWorkspace"));
        assert!(CARD_SCRIPT.contains("newNotebook"));
        assert!(CARD_SCRIPT.contains("bindNotebook"));
        assert!(
            CARD_SCRIPT.contains("numberingStartInput.setAttribute(\"aria-label\", \"起始序号\")")
        );
        assert!(CARD_SCRIPT.contains("numberingStart,"));
        assert!(CARD_SCRIPT.contains("收件箱（仅本地）"));
        assert!(CARD_SCRIPT.contains("提交后立即同步到"));
        assert!(CARD_SCRIPT.contains("const notebookBlocksChanges = (notebook)"));
        assert!(CARD_SCRIPT.contains("canRestoreInlineEdit(inlineEditSnapshot, state)"));
        assert!(CARD_SCRIPT.contains("&& !notebookBlocksChanges("));
        assert!(CARD_SCRIPT.contains("notebook.numberingSyncPending"));
        assert!(CARD_SCRIPT.contains("编号重排恢复完成后可继续记录"));
        assert!(CARD_SCRIPT.contains("Lucide 1.26.0 icon nodes"));
        assert!(CARD_SCRIPT
            .contains("shadow.append(style, card, itemMenu, selectPopover, imageLightbox)"));
        assert!(CARD_SCRIPT.contains("__wakegptReceiveImage_"));
        assert!(CARD_SCRIPT.contains(".item-menu { position:fixed"));
        assert!(CARD_SCRIPT.contains("const positionItemMenu = (trigger) =>"));
        assert!(CARD_SCRIPT.contains("below + menuRect.height <= bottomBoundary"));
        assert!(CARD_SCRIPT.contains("trigger.setAttribute(\"aria-expanded\", \"true\")"));
        assert!(CARD_SCRIPT.contains("closeItemMenu(true)"));
        assert!(CARD_SCRIPT.contains("[\"ArrowDown\", \"ArrowUp\", \"Home\", \"End\"]"));
        assert!(CARD_SCRIPT.contains("enabledItems[nextIndex].focus"));
        assert!(CARD_SCRIPT.contains("createWakeSelect"));
        assert!(CARD_SCRIPT.contains("aria-activedescendant"));
        assert!(CARD_SCRIPT.contains("findWakeSelectTypeahead"));
        assert!(CARD_SCRIPT.contains("index >= 0 && index < control.options.length"));
        assert!(CARD_SCRIPT.contains("!control.options.some((option) => !option.disabled)"));
        assert!(CARD_SCRIPT.contains("forced-colors:active"));
        assert!(CARD_SCRIPT.contains("selectPopover.hidden = false"));
        assert!(!CARD_SCRIPT.contains("element(\"select\")"));
        assert!(!CARD_SCRIPT.contains("HTMLSelectElement"));
        assert!(!CARD_SCRIPT.contains("element(\"div\", \"item-menu\", record.id)"));
        assert!(!CARD_SCRIPT.contains("button(\"icon\", \"⋯\""));
        assert!(!CARD_SCRIPT.contains("innerHTML"));
        assert!(!CARD_SCRIPT.contains("KeyboardEvent"));
        assert!(!CARD_SCRIPT.contains("dispatchEvent(new Event(\"submit\""));
        assert!(!CARD_SCRIPT.contains("form.submit"));
        assert!(!CARD_SCRIPT.contains("requestSubmit"));
    }

    #[test]
    fn card_numbering_start_defaults_and_rejects_out_of_range_values() {
        assert_eq!(data_numbering_start(&json!({})).unwrap(), 1);
        assert_eq!(
            data_numbering_start(&json!({ "numberingStart": 42 })).unwrap(),
            42
        );
        assert!(data_numbering_start(&json!({ "numberingStart": 0 })).is_err());
        assert!(data_numbering_start(&json!({ "numberingStart": 1_000_000_001_u64 })).is_err());
        assert!(data_numbering_start(&json!({ "numberingStart": "42" })).is_err());
    }

    #[test]
    fn card_refresh_signal_and_event_payload_are_stable() {
        let signal = CodexCardRefreshSignal::default();
        assert_eq!(signal.current(), 0);
        signal.bump();
        signal.bump();
        assert_eq!(signal.current(), 2);

        let payload = serde_json::to_value(CardDataChanged {
            workspace_id: "WORKSPACE".to_owned(),
            notebook_id: Some("NOTEBOOK".to_owned()),
            change_kind: "records",
            source: "backend",
        })
        .unwrap();
        assert_eq!(payload["workspaceId"], "WORKSPACE");
        assert_eq!(payload["notebookId"], "NOTEBOOK");
        assert_eq!(payload["changeKind"], "records");
        assert_eq!(payload["source"], "backend");
    }

    #[test]
    fn composer_operation_gate_serializes_one_target_record_without_blocking_others() {
        let gate = ComposerOperationGate::default();
        let host = format!("chatgpt:{}", "a".repeat(64));
        let other_host = format!("chatgpt:{}", "b".repeat(64));
        let first = gate.try_acquire(&host, "record-a").unwrap();
        assert_eq!(
            gate.try_acquire(&host, "record-a").unwrap_err().code(),
            "card_composer_operation_busy"
        );
        let other_record = gate.try_acquire(&host, "record-b").unwrap();
        let other_target = gate.try_acquire(&other_host, "record-a").unwrap();
        drop((first, other_record, other_target));
        assert!(gate.try_acquire(&host, "record-a").is_ok());
    }

    #[test]
    fn composer_identity_is_document_stable_and_target_scoped() {
        let first = PageTarget {
            process_id: 41,
            port: 58_119,
            id: "PAGE_A".to_owned(),
            web_socket_debugger_url: "ws://127.0.0.1:58119/devtools/page/PAGE_A".to_owned(),
        };
        let second = PageTarget {
            process_id: 42,
            port: 58_120,
            id: "PAGE_B".to_owned(),
            web_socket_debugger_url: "ws://127.0.0.1:58120/devtools/page/PAGE_B".to_owned(),
        };
        let page_identity = "app://-/index.html#/conversation\n1780000000000";
        let first_scope = composer_target_scope(&first);
        let second_scope = composer_target_scope(&second);
        assert_eq!(first_scope.len(), 64);
        assert_ne!(first_scope, second_scope);
        let first_host = composer_host_kind_from_identity(&first_scope, page_identity);
        assert_eq!(first_host.len(), 72);
        assert_eq!(
            first_host,
            composer_host_kind_from_identity(&first_scope, page_identity)
        );
        assert_ne!(
            first_host,
            composer_host_kind_from_identity(&second_scope, page_identity)
        );
    }

    #[test]
    fn composer_attachment_progress_survives_a_later_cdp_error() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer receipt", "/tmp/wakegpt-composer-receipt")
            .unwrap();
        let first_digest = "a".repeat(64);
        let second_digest = "b".repeat(64);
        let attachments = [
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &first_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: first_digest.clone(),
                byte_size: 1,
            },
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &second_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: second_digest,
                byte_size: 1,
            },
        ];
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "",
                &attachments,
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let host_kind = format!("chatgpt:{}", "c".repeat(64));
        let mut inserted_digests = HashSet::new();
        let mut detail = ComposerInsertionReceiptDetail::default();
        let mut attempt = 0;

        let error = insert_record_attachments(
            &store,
            &host_kind,
            &record,
            vec!["first.png".to_owned(), "second.png".to_owned()],
            &mut inserted_digests,
            &mut detail,
            |_| {
                attempt += 1;
                let receipt = store
                    .latest_composer_receipt(&record.id, &host_kind)
                    .unwrap()
                    .expect("uncertainty must be durable before an image side effect");
                assert_eq!(receipt.state, "uncertain");
                let saved: ComposerInsertionReceiptDetail =
                    serde_json::from_str(&receipt.detail_json).unwrap();
                assert_eq!(saved.record_revision, Some(record.revision));
                if attempt == 1 {
                    assert!(saved.inserted_attachment_ids.is_empty());
                    assert_eq!(
                        saved.uncertain_attachment_ids,
                        vec![record.attachments[0].id.clone()]
                    );
                    Ok(true)
                } else {
                    assert_eq!(
                        saved.inserted_attachment_ids,
                        vec![record.attachments[0].id.clone()]
                    );
                    assert_eq!(
                        saved.uncertain_attachment_ids,
                        vec![record.attachments[1].id.clone()]
                    );
                    Err(AdapterRuntimeError::Transport("forced_cdp_failure"))
                }
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "forced_cdp_failure");
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .expect("the failed image must remain uncertain after the CDP error");
        assert_eq!(receipt.state, "uncertain");
        let saved: ComposerInsertionReceiptDetail =
            serde_json::from_str(&receipt.detail_json).unwrap();
        assert_eq!(saved.record_revision, Some(record.revision));
        assert_eq!(
            saved.inserted_attachment_ids,
            vec![record.attachments[0].id.clone()]
        );
        assert_eq!(saved.inserted_content_sha256, vec![first_digest]);
        assert_eq!(
            saved.uncertain_attachment_ids,
            vec![record.attachments[1].id.clone()]
        );
        assert!(!saved.text_inserted);
    }

    #[test]
    fn composer_attachment_false_result_stays_uncertain_and_stops_the_sequence() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer false", "/tmp/wakegpt-composer-false")
            .unwrap();
        let first_digest = "1".repeat(64);
        let second_digest = "2".repeat(64);
        let attachments = [
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &first_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: first_digest,
                byte_size: 1,
            },
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &second_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: second_digest,
                byte_size: 1,
            },
        ];
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "",
                &attachments,
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let host_kind = format!("chatgpt:{}", "3".repeat(64));
        let mut inserted_digests = HashSet::new();
        let mut detail = ComposerInsertionReceiptDetail::default();
        let mut calls = 0;

        let inserted_ids = insert_record_attachments(
            &store,
            &host_kind,
            &record,
            vec!["first.png".to_owned(), "second.png".to_owned()],
            &mut inserted_digests,
            &mut detail,
            |_| {
                calls += 1;
                let receipt = store
                    .latest_composer_receipt(&record.id, &host_kind)
                    .unwrap()
                    .expect("uncertainty must precede the image side effect");
                assert_eq!(receipt.state, "uncertain");
                Ok(false)
            },
        )
        .unwrap();

        assert_eq!(calls, 1, "a false observation must stop later images");
        assert!(inserted_ids.is_empty());
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state, "uncertain");
        let saved: ComposerInsertionReceiptDetail =
            serde_json::from_str(&receipt.detail_json).unwrap();
        assert_eq!(
            saved.uncertain_attachment_ids,
            vec![record.attachments[0].id.clone()]
        );
    }

    #[test]
    fn composer_confirm_marks_uncertain_work_without_running_another_side_effect() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer confirm", "/tmp/wakegpt-composer-confirm")
            .unwrap();
        let digest = "4".repeat(64);
        let attachment = NewAttachment {
            media_type: "image/png".to_owned(),
            managed_relative_path: managed_attachment_relative_path(
                INBOX_ATTACHMENT_DIRECTORY,
                &digest,
                "image/png",
            )
            .unwrap(),
            previous_managed_relative_path: None,
            content_sha256: digest.clone(),
            byte_size: 1,
        };
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "Body",
                &[attachment],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let host_kind = format!("chatgpt:{}", "5".repeat(64));
        let detail = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            text_uncertain: true,
            uncertain_attachment_ids: vec![record.attachments[0].id.clone()],
            ..ComposerInsertionReceiptDetail::default()
        };
        persist_composer_receipt(&store, &host_kind, &record, "uncertain", &detail).unwrap();

        assert_eq!(
            resolve_composer_uncertainty(&store, &host_kind, &record, "confirm").unwrap(),
            "complete"
        );
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state, "complete");
        let saved: ComposerInsertionReceiptDetail =
            serde_json::from_str(&receipt.detail_json).unwrap();
        assert!(saved.text_inserted);
        assert!(!saved.text_uncertain);
        assert_eq!(
            saved.inserted_attachment_ids,
            vec![record.attachments[0].id.clone()]
        );
        assert!(saved.uncertain_attachment_ids.is_empty());
        assert_eq!(
            composer_insertion_state(&store, &record, &host_kind).unwrap(),
            "complete"
        );
        assert_eq!(
            resolve_composer_uncertainty(&store, &host_kind, &record, "confirm")
                .unwrap_err()
                .code(),
            "card_composer_uncertainty_missing"
        );
    }

    #[test]
    fn composer_retry_clears_uncertainty_before_allowing_another_attempt() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer retry", "/tmp/wakegpt-composer-retry")
            .unwrap();
        let digest = "6".repeat(64);
        let attachment = NewAttachment {
            media_type: "image/png".to_owned(),
            managed_relative_path: managed_attachment_relative_path(
                INBOX_ATTACHMENT_DIRECTORY,
                &digest,
                "image/png",
            )
            .unwrap(),
            previous_managed_relative_path: None,
            content_sha256: digest,
            byte_size: 1,
        };
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "",
                &[attachment],
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let host_kind = format!("chatgpt:{}", "7".repeat(64));
        let detail = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            uncertain_attachment_ids: vec![record.attachments[0].id.clone()],
            ..ComposerInsertionReceiptDetail::default()
        };
        persist_composer_receipt(&store, &host_kind, &record, "uncertain", &detail).unwrap();

        assert_eq!(
            resolve_composer_uncertainty(&store, &host_kind, &record, "retry").unwrap(),
            "partial"
        );
        let cleared = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        let cleared: ComposerInsertionReceiptDetail =
            serde_json::from_str(&cleared.detail_json).unwrap();
        assert!(!composer_receipt_has_uncertainty(&cleared));

        let mut inserted_digests = HashSet::new();
        let mut retried_detail = cleared;
        let mut calls = 0;
        let inserted_ids = insert_record_attachments(
            &store,
            &host_kind,
            &record,
            vec!["retried.png".to_owned()],
            &mut inserted_digests,
            &mut retried_detail,
            |_| {
                calls += 1;
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(
            inserted_ids,
            HashSet::from([record.attachments[0].id.clone()])
        );
    }

    #[test]
    fn composer_receipts_require_explicit_review_after_the_record_changes() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer revision", "/tmp/wakegpt-composer-revision")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Original")
            .unwrap();
        let complete_host = format!("chatgpt:{}", "8".repeat(64));
        let complete = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            text_inserted: true,
            ..ComposerInsertionReceiptDetail::default()
        };
        persist_composer_receipt(&store, &complete_host, &record, "complete", &complete).unwrap();

        let uncertain_host = format!("chatgpt:{}", "9".repeat(64));
        let uncertain = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            text_uncertain: true,
            ..ComposerInsertionReceiptDetail::default()
        };
        persist_composer_receipt(&store, &uncertain_host, &record, "uncertain", &uncertain)
            .unwrap();
        let revised = store
            .update_record(&workspace.id, &record.id, record.revision, "Revised")
            .unwrap();

        assert_eq!(
            composer_insertion_state(&store, &revised, &complete_host).unwrap(),
            "staleUncertain"
        );
        assert_eq!(
            composer_insertion_state(&store, &revised, &uncertain_host).unwrap(),
            "staleUncertain"
        );
        assert_eq!(
            resolve_composer_uncertainty(&store, &uncertain_host, &revised, "confirm").unwrap(),
            "complete"
        );
        let confirmed = store
            .latest_composer_receipt(&record.id, &uncertain_host)
            .unwrap()
            .unwrap();
        let confirmed: ComposerInsertionReceiptDetail =
            serde_json::from_str(&confirmed.detail_json).unwrap();
        assert_eq!(confirmed.record_revision, Some(revised.revision));
        assert!(confirmed.text_inserted);
        assert!(!composer_receipt_has_uncertainty(&confirmed));

        let retry_host = format!("chatgpt:{}", "0".repeat(64));
        persist_composer_receipt(&store, &retry_host, &record, "uncertain", &uncertain).unwrap();
        assert_eq!(
            resolve_composer_uncertainty(&store, &retry_host, &revised, "retry").unwrap(),
            "partial"
        );
        let retry = store
            .latest_composer_receipt(&record.id, &retry_host)
            .unwrap()
            .unwrap();
        let retry: ComposerInsertionReceiptDetail =
            serde_json::from_str(&retry.detail_json).unwrap();
        assert_eq!(retry.record_revision, Some(revised.revision));
        assert!(!retry.text_inserted);
        assert!(!composer_receipt_has_uncertainty(&retry));
    }

    #[test]
    fn legacy_composer_receipt_without_revision_never_claims_a_later_edit() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Legacy composer", "/tmp/wakegpt-legacy-composer")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Original")
            .unwrap();
        let host_kind = format!("chatgpt:{}", "1".repeat(64));
        store
            .record_composer_receipt(
                &record.id,
                &host_kind,
                "complete",
                r#"{"textInserted":true,"insertedAttachmentIds":[]}"#,
            )
            .unwrap();
        let revised = store
            .update_record(
                &workspace.id,
                &record.id,
                record.revision,
                "Current revision",
            )
            .unwrap();

        assert_eq!(
            composer_insertion_state(&store, &revised, &host_kind).unwrap(),
            "staleUncertain"
        );
        assert_eq!(
            resolve_composer_uncertainty(&store, &host_kind, &revised, "retry").unwrap(),
            "partial"
        );
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        let detail: ComposerInsertionReceiptDetail =
            serde_json::from_str(&receipt.detail_json).unwrap();
        assert_eq!(detail.record_revision, Some(revised.revision));
        assert!(!detail.text_inserted);
    }

    #[test]
    fn composer_text_uncertainty_is_durable_before_the_dom_callback() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer text", "/tmp/wakegpt-composer-text")
            .unwrap();
        let record = store
            .create_record(&workspace.id, None, "Text side effect")
            .unwrap();
        let host_kind = format!("chatgpt:{}", "a".repeat(64));
        let mut detail = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            ..ComposerInsertionReceiptDetail::default()
        };
        let mut callback_observed_receipt = false;

        let error = insert_record_text(&store, &host_kind, &record, &mut detail, |body| {
            assert_eq!(body, "Text side effect");
            let receipt = store
                .latest_composer_receipt(&record.id, &host_kind)
                .unwrap()
                .expect("uncertainty must be durable before the text side effect");
            assert_eq!(receipt.state, "uncertain");
            let saved: ComposerInsertionReceiptDetail =
                serde_json::from_str(&receipt.detail_json).unwrap();
            assert_eq!(saved.record_revision, Some(record.revision));
            assert!(saved.text_uncertain);
            assert!(!saved.text_inserted);
            callback_observed_receipt = true;
            Err(AdapterRuntimeError::Transport("forced_text_cdp_failure"))
        })
        .unwrap_err();

        assert!(callback_observed_receipt);
        assert_eq!(error.code(), "forced_text_cdp_failure");
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state, "uncertain");

        let false_host_kind = format!("chatgpt:{}", "b".repeat(64));
        let mut false_detail = ComposerInsertionReceiptDetail {
            record_revision: Some(record.revision),
            ..ComposerInsertionReceiptDetail::default()
        };
        insert_record_text(&store, &false_host_kind, &record, &mut false_detail, |_| {
            Ok(false)
        })
        .unwrap();
        assert!(false_detail.text_uncertain);
        assert_eq!(
            store
                .latest_composer_receipt(&record.id, &false_host_kind)
                .unwrap()
                .unwrap()
                .state,
            "uncertain"
        );
    }

    #[test]
    fn composer_cross_record_digest_skip_is_durable_before_the_next_attempt() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Composer digest", "/tmp/wakegpt-composer-digest")
            .unwrap();
        let first_digest = "d".repeat(64);
        let second_digest = "e".repeat(64);
        let attachments = [
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &first_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: first_digest.clone(),
                byte_size: 1,
            },
            NewAttachment {
                media_type: "image/png".to_owned(),
                managed_relative_path: managed_attachment_relative_path(
                    INBOX_ATTACHMENT_DIRECTORY,
                    &second_digest,
                    "image/png",
                )
                .unwrap(),
                previous_managed_relative_path: None,
                content_sha256: second_digest,
                byte_size: 1,
            },
        ];
        let record = store
            .create_record_with_attachments_idempotent(
                &workspace.id,
                None,
                "",
                &attachments,
                &new_id(),
                MUTATION_SCHEMA_VERSION,
            )
            .unwrap();
        let host_kind = format!("chatgpt:{}", "f".repeat(64));
        let mut inserted_digests = HashSet::from([first_digest.clone()]);
        let mut detail = ComposerInsertionReceiptDetail::default();
        let mut calls = 0;

        let error = insert_record_attachments(
            &store,
            &host_kind,
            &record,
            vec!["first.png".to_owned(), "second.png".to_owned()],
            &mut inserted_digests,
            &mut detail,
            |_| {
                calls += 1;
                Err(AdapterRuntimeError::Transport("forced_cdp_failure"))
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "forced_cdp_failure");
        assert_eq!(calls, 1, "the known digest must not be attached again");
        let receipt = store
            .latest_composer_receipt(&record.id, &host_kind)
            .unwrap()
            .expect("the digest skip must become self-contained for this record");
        let saved: ComposerInsertionReceiptDetail =
            serde_json::from_str(&receipt.detail_json).unwrap();
        assert_eq!(
            saved.inserted_attachment_ids,
            vec![record.attachments[0].id.clone()]
        );
        assert_eq!(saved.inserted_content_sha256, vec![first_digest]);
    }

    fn card_session_for_selection_test() -> CardSession {
        CardSession {
            nonce: "a".repeat(32),
            binding_name: "__wakegptBridge_selection_test".to_owned(),
            workspace_id: None,
            notebook_id: None,
            selection_initialized: false,
            pending_attachments: Vec::new(),
            composer_target_scope: "a".repeat(64),
            composer_host_kind: None,
            pending_create_ack: None,
        }
    }

    #[test]
    fn card_state_prioritizes_pinned_tabs_and_reconciles_closed_selection() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Card tab visibility", "/tmp/wakegpt-card-tab-visibility")
            .unwrap();
        let mut closed_notebook_ids = Vec::new();
        for index in 0..48 {
            let notebook = store
                .create_notebook(
                    &workspace.id,
                    &format!("Closed {index}"),
                    &format!("closed-{index}.md"),
                    NumberingStyle::None,
                )
                .unwrap();
            store
                .set_notebook_pinned(&workspace.id, &notebook.id, false)
                .unwrap();
            closed_notebook_ids.push(notebook.id);
        }
        let pinned = store
            .create_notebook(
                &workspace.id,
                "Pinned after closed tabs",
                "pinned.md",
                NumberingStyle::None,
            )
            .unwrap();
        let notebooks = store.list_notebooks(&workspace.id).unwrap();
        let card_notebooks = card_notebooks_for_state(&notebooks, None);
        assert_eq!(card_notebooks.len(), 48);
        assert_eq!(card_notebooks[0].id, pinned.id);
        assert!(card_notebooks[0].is_pinned);

        let mut session = card_session_for_selection_test();
        session.workspace_id = Some(workspace.id.clone());
        session.notebook_id = Some(pinned.id.clone());
        session.selection_initialized = true;
        reconcile_card_notebook_selection(&mut session, &notebooks);
        assert_eq!(session.notebook_id.as_deref(), Some(pinned.id.as_str()));

        for notebook_id in &closed_notebook_ids {
            store
                .set_notebook_pinned(&workspace.id, notebook_id, true)
                .unwrap();
        }
        let crowded_notebooks = store.list_notebooks(&workspace.id).unwrap();
        assert!(
            crowded_notebooks
                .iter()
                .position(|notebook| notebook.id == pinned.id)
                .is_some_and(|index| index >= 48),
            "the regression fixture must put the selected tab beyond the ordinary card bound"
        );
        let crowded_card_notebooks = card_notebooks_for_state(&crowded_notebooks, Some(&pinned.id));
        assert_eq!(crowded_card_notebooks.len(), 48);
        assert!(
            crowded_card_notebooks
                .iter()
                .any(|notebook| notebook.id == pinned.id),
            "the selected pinned tab must stay in the bounded card state"
        );

        store
            .set_notebook_pinned(&workspace.id, &pinned.id, false)
            .unwrap();
        let notebooks = store.list_notebooks(&workspace.id).unwrap();
        reconcile_card_notebook_selection(&mut session, &notebooks);
        assert_eq!(session.notebook_id, None);
        assert_eq!(
            store.get_notebook(&workspace.id, &pinned.id).unwrap().id,
            pinned.id,
            "closing a tab must retain its notebook"
        );

        session.notebook_id = Some("missing-notebook".to_owned());
        reconcile_card_notebook_selection(&mut session, &notebooks);
        assert_eq!(session.notebook_id, None);
    }

    #[test]
    fn card_record_projection_keeps_unresolved_receipts_ahead_of_recent_items() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Card receipts", "/tmp/wakegpt-card-receipts")
            .unwrap();
        for index in 0..9 {
            store
                .create_record(&workspace.id, None, &format!("Record {index}"))
                .unwrap();
        }
        let records = store.list_records(&workspace.id, None, false).unwrap();
        let unresolved_id = records.last().unwrap().id.clone();
        let mut projected = records
            .into_iter()
            .map(|record| {
                let state = if record.id == unresolved_id {
                    "staleUncertain"
                } else {
                    "none"
                };
                (record, state.to_owned())
            })
            .collect::<Vec<_>>();

        sort_card_records(&mut projected);

        assert_eq!(projected[0].0.id, unresolved_id);
        assert!(projected
            .iter()
            .take(8)
            .any(|(record, _)| record.id == unresolved_id));
    }

    #[test]
    fn card_state_keeps_the_selected_workspace_beyond_the_ordinary_bound() {
        let workspaces = (0..25)
            .map(|index| Workspace {
                id: format!("WORKSPACE-{index:02}"),
                display_name: format!("Workspace {index:02}"),
                root_path: format!("/tmp/wakegpt-card-workspace-{index:02}"),
                created_at_ms: index,
                updated_at_ms: index,
            })
            .collect::<Vec<_>>();
        let selected_workspace_id = workspaces[24].id.clone();

        let card_workspaces = card_workspaces_for_state(&workspaces, Some(&selected_workspace_id));

        assert_eq!(card_workspaces.len(), 24);
        assert_eq!(
            card_workspaces
                .last()
                .map(|workspace| workspace.id.as_str()),
            Some(selected_workspace_id.as_str()),
            "the selected workspace must replace the bounded list's last ordinary item"
        );
    }

    #[test]
    fn main_app_navigation_does_not_replace_initialized_card_selection() {
        let store = Store::open_in_memory().unwrap();
        let workspace = store
            .create_workspace("Selection isolation", "/tmp/wakegpt-card-selection-main")
            .unwrap();
        let notebook = store
            .create_notebook(
                &workspace.id,
                "Card notebook",
                "card.md",
                NumberingStyle::None,
            )
            .unwrap();
        let initial = store
            .set_active_selection(&workspace.id, Some(&notebook.id))
            .unwrap();
        let workspaces = store.list_workspaces().unwrap();
        let mut session = card_session_for_selection_test();

        initialize_card_selection(&mut session, &initial, &workspaces);
        assert_eq!(session.workspace_id.as_deref(), Some(workspace.id.as_str()));
        assert_eq!(session.notebook_id.as_deref(), Some(notebook.id.as_str()));

        let main_app_changed = store.set_active_selection(&workspace.id, None).unwrap();
        initialize_card_selection(&mut session, &main_app_changed, &workspaces);
        assert_eq!(session.workspace_id.as_deref(), Some(workspace.id.as_str()));
        assert_eq!(session.notebook_id.as_deref(), Some(notebook.id.as_str()));
    }

    #[test]
    fn card_navigation_does_not_write_main_app_selection() {
        let store = Store::open_in_memory().unwrap();
        let main_workspace = store
            .create_workspace("Main selection", "/tmp/wakegpt-card-selection-app")
            .unwrap();
        let card_workspace = store
            .create_workspace("Card selection", "/tmp/wakegpt-card-selection-card")
            .unwrap();
        let card_notebook = store
            .create_notebook(
                &card_workspace.id,
                "Independent card notebook",
                "independent.md",
                NumberingStyle::None,
            )
            .unwrap();
        let main_preferences = store
            .set_active_selection(&main_workspace.id, None)
            .unwrap();
        let mut session = card_session_for_selection_test();
        initialize_card_selection(
            &mut session,
            &main_preferences,
            &store.list_workspaces().unwrap(),
        );

        select_card_workspace(&store, &mut session, &card_workspace.id).unwrap();
        select_card_notebook(&store, &mut session, Some(&card_notebook.id)).unwrap();

        assert_eq!(
            session.workspace_id.as_deref(),
            Some(card_workspace.id.as_str())
        );
        assert_eq!(
            session.notebook_id.as_deref(),
            Some(card_notebook.id.as_str())
        );
        let unchanged = store.ui_preferences().unwrap();
        assert_eq!(
            unchanged.active_workspace_id.as_deref(),
            Some(main_workspace.id.as_str())
        );
        assert_eq!(unchanged.selected_notebook_id, None);
    }

    #[test]
    fn mount_result_must_confirm_the_exact_adapter_and_receiver() {
        let session = CardSession {
            nonce: "a".repeat(32),
            binding_name: "__wakegptBridge_test".to_owned(),
            workspace_id: None,
            notebook_id: None,
            selection_initialized: false,
            pending_attachments: Vec::new(),
            composer_target_scope: "a".repeat(64),
            composer_host_kind: None,
            pending_create_ack: None,
        };
        let accepted = json!({
            "result": {
                "value": {
                    "ok": true,
                    "adapterVersion": ADAPTER_VERSION,
                    "hostContractId": HOST_CONTRACT_ID,
                    "sessionNonce": session.nonce,
                    "bindingName": session.binding_name,
                    "receiverName": format!("__wakegptReceive_{}", session.nonce),
                }
            }
        });
        assert!(validate_mount_result(&accepted, &session).is_ok());
        let rejected = json!({
            "result": {
                "value": {
                    "ok": false,
                    "adapterVersion": ADAPTER_VERSION,
                    "hostContractId": HOST_CONTRACT_ID,
                    "sessionNonce": session.nonce,
                    "bindingName": session.binding_name,
                    "receiverName": format!("__wakegptReceive_{}", session.nonce),
                }
            }
        });
        assert_eq!(
            validate_mount_result(&rejected, &session)
                .unwrap_err()
                .code(),
            "codex_card_mount_rejected"
        );
        let wrong_contract = json!({
            "result": {
                "value": {
                    "ok": true,
                    "adapterVersion": ADAPTER_VERSION,
                    "hostContractId": "chatgpt-unknown",
                    "sessionNonce": session.nonce,
                    "bindingName": session.binding_name,
                    "receiverName": format!("__wakegptReceive_{}", session.nonce),
                }
            }
        });
        assert_eq!(
            validate_mount_result(&wrong_contract, &session)
                .unwrap_err()
                .code(),
            "codex_card_mount_rejected"
        );
        let ambiguous = json!({
            "result": { "value": { "ok": false, "code": "context_selector_ambiguous" } }
        });
        assert_eq!(
            validate_mount_result(&ambiguous, &session)
                .unwrap_err()
                .code(),
            "codex_context_selector_ambiguous"
        );
    }

    #[test]
    fn card_notebook_names_cannot_escape_the_workspace() {
        assert_eq!(
            notebook_file_name("Product / notes").unwrap(),
            "Product _ notes.md"
        );
        assert!(notebook_file_name("../").is_err());
        assert!(notebook_file_name(" . ").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn chatgpt_managed_launch_uses_the_app_bundle_and_loopback_debugging() {
        use std::ffi::OsStr;

        let command = chatgpt_open_command(
            Path::new("/Applications/Chatgpt.app"),
            CHATGPT_MANAGED_DEBUG_PORT,
            None,
        );
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/open"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("-n"),
                OsStr::new("-a"),
                OsStr::new("/Applications/Chatgpt.app"),
                OsStr::new("--args"),
                OsStr::new("--remote-debugging-address=127.0.0.1"),
                OsStr::new("--remote-debugging-port=58119"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_launch_uses_port_zero_and_preserves_its_profile() {
        use std::ffi::OsStr;

        let profile = Path::new("/example/Custom Profile");
        let command =
            chatgpt_open_command(Path::new("/Applications/Chatgpt.app"), 0, Some(profile));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("-n"),
                OsStr::new("-a"),
                OsStr::new("/Applications/Chatgpt.app"),
                OsStr::new("--args"),
                OsStr::new("--remote-debugging-address=127.0.0.1"),
                OsStr::new("--remote-debugging-port=0"),
                OsStr::new("--user-data-dir"),
                OsStr::new("/example/Custom Profile"),
            ]
        );
        let fallback = chatgpt_open_command_without_integration(
            Path::new("/Applications/Chatgpt.app"),
            profile,
        );
        assert_eq!(
            fallback.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("-n"),
                OsStr::new("-a"),
                OsStr::new("/Applications/Chatgpt.app"),
                OsStr::new("--args"),
                OsStr::new("--user-data-dir"),
                OsStr::new("/example/Custom Profile"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_missing_replacement_recovers_the_profile_once() {
        let fallback_calls = std::cell::Cell::new(0_u8);
        let error = complete_custom_instance_restart(
            41_001,
            &[
                CustomInstanceRestartObservation::Missing,
                CustomInstanceRestartObservation::Missing,
            ],
            || {
                fallback_calls.set(fallback_calls.get() + 1);
                Ok(())
            },
        )
        .expect_err("an ordinary recovery launch is not an integrated restart success");

        assert_eq!(
            fallback_calls.get(),
            1,
            "recover the same profile exactly once"
        );
        assert_eq!(
            error.code(),
            "chatgpt_integration_start_timeout_profile_recovered"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_graceful_exit_timeout_escalates_only_that_pid() {
        let mut running_observations = [true, true, true, false].into_iter();
        let forced_process = std::cell::Cell::new(None);

        wait_for_custom_instance_exit(
            41_001,
            2,
            2,
            |_| running_observations.next().unwrap_or(false),
            |process_id| {
                forced_process.set(Some(process_id));
                true
            },
            || {},
        )
        .expect("a process that ignores graceful exit must be force-quit before relaunch");

        assert_eq!(forced_process.get(), Some(41_001));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_normal_exit_never_force_quits() {
        let force_calls = std::cell::Cell::new(0_u8);

        wait_for_custom_instance_exit(
            41_001,
            2,
            2,
            |_| false,
            |_| {
                force_calls.set(force_calls.get() + 1);
                true
            },
            || {},
        )
        .expect("an already exited process needs no escalation");

        assert_eq!(force_calls.get(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_force_exit_failure_stops_before_relaunch() {
        let error = wait_for_custom_instance_exit(41_001, 0, 0, |_| true, |_| false, || {})
            .expect_err(
                "a process that cannot be stopped must never be relaunched over its profile",
            );

        assert_eq!(error.code(), "chatgpt_force_termination_failed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_exit_race_does_not_report_force_failure() {
        let mut observations = [true, false].into_iter();

        wait_for_custom_instance_exit(
            41_001,
            0,
            0,
            |_| observations.next().unwrap_or(false),
            |_| false,
            || {},
        )
        .expect("an instance that exits before force-quit must proceed to relaunch");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_incompatible_replacement_is_not_launched_twice() {
        let error = complete_custom_instance_restart(
            41_001,
            &[
                CustomInstanceRestartObservation::Missing,
                CustomInstanceRestartObservation::Incompatible { process_id: 41_002 },
            ],
            || panic!("an existing same-profile process must not be duplicated"),
        )
        .expect_err("a same-profile process without loopback debugging is not compatible");

        assert_eq!(error.code(), "chatgpt_profile_claimed_without_debugging");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_starting_endpoint_times_out_without_duplicate_launch() {
        let error = complete_custom_instance_restart(
            41_001,
            &[CustomInstanceRestartObservation::Starting { process_id: 41_002 }],
            || panic!("a running same-profile process must not be duplicated"),
        )
        .expect_err("a replacement without a ready page target is not connected");

        assert_eq!(error.code(), "chatgpt_debug_endpoint_start_timeout");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_compatible_replacement_requires_a_new_pid() {
        let reused = complete_custom_instance_restart(
            41_001,
            &[CustomInstanceRestartObservation::Compatible { process_id: 41_001 }],
            || panic!("a compatible observation must not use the fallback launch"),
        )
        .expect_err("the terminated PID cannot prove that a replacement launched");
        assert_eq!(reused.code(), "chatgpt_replacement_pid_invalid");

        let replacement = complete_custom_instance_restart(
            41_001,
            &[CustomInstanceRestartObservation::Compatible { process_id: 41_002 }],
            || panic!("a compatible observation must not use the fallback launch"),
        )
        .expect("a compatible same-profile target with a new PID completes the restart");
        assert_eq!(replacement, 41_002);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_identity_profile_is_fixed_private_and_rejects_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = std::env::temp_dir().join(format!("wakegpt-identity-{}", new_id()));
        let app_data = root.join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        let profile = DefaultIdentityProfile::new(app_data.clone()).unwrap();
        assert_eq!(
            profile.path(),
            fs::canonicalize(&app_data)
                .unwrap()
                .join(DEFAULT_IDENTITY_PROFILE_DIRECTORY)
        );

        profile.ensure_ready().unwrap();
        assert_eq!(
            fs::metadata(profile.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::remove_dir(profile.path()).unwrap();
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, profile.path()).unwrap();
        assert_eq!(
            profile.ensure_ready().unwrap_err().code(),
            "default_identity_profile_invalid"
        );
        fs::remove_file(profile.path()).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn custom_instance_restart_identity_is_strict_and_profile_stays_under_home() {
        assert_eq!(parse_instance_process_id("chatgpt-74430").unwrap(), 74430);
        for invalid in ["74430", "chatgpt-", "chatgpt--1", "chatgpt-1x", "chatgpt-0"] {
            assert_eq!(
                parse_instance_process_id(invalid).unwrap_err().code(),
                "chatgpt_instance_id_invalid"
            );
        }

        let root = std::env::temp_dir().join(format!("wakegpt-profile-{}", new_id()));
        let home = root.join("home");
        let custom = home.join("Custom Profile");
        let default = home.join("Library/Application Support/Codex");
        fs::create_dir_all(&custom).unwrap();
        fs::create_dir_all(&default).unwrap();
        let executable = "/Applications/Chatgpt.app/Contents/MacOS/ChatGPT";
        let custom_arguments = vec![
            executable.to_owned(),
            "--user-data-dir".to_owned(),
            custom.to_string_lossy().into_owned(),
        ];
        assert_eq!(
            restartable_custom_profile_dir(&custom_arguments, &home, &default),
            Some(fs::canonicalize(&custom).unwrap())
        );
        let default_arguments = vec![
            executable.to_owned(),
            format!("--user-data-dir={}", default.display()),
        ];
        assert!(restartable_custom_profile_dir(&default_arguments, &home, &default).is_none());
        let duplicate_arguments = vec![
            executable.to_owned(),
            format!("--user-data-dir={}", custom.display()),
            format!("--user-data-dir={}", custom.display()),
        ];
        assert!(restartable_custom_profile_dir(&duplicate_arguments, &home, &default).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_debug_port_preflight_rejects_an_occupied_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = ensure_debug_port_available(port).unwrap_err();
        assert_eq!(error.code(), "chatgpt_debug_port_unavailable");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn chatgpt_debug_port_parser_accepts_custom_profiles_but_rejects_non_loopback() {
        let executable = "/Applications/Chatgpt.app/Contents/MacOS/ChatGPT";
        let home = Path::new("/example");
        assert_eq!(
            chatgpt_debug_port(
                &[
                    executable.to_owned(),
                    "--remote-debugging-address=127.0.0.1".to_owned(),
                    "--remote-debugging-port=64406".to_owned(),
                ],
                home,
            ),
            Some(64406)
        );
        assert_eq!(
            chatgpt_debug_port(
                &[
                    executable.to_owned(),
                    "--remote-debugging-address=127.0.0.1".to_owned(),
                    "--remote-debugging-port".to_owned(),
                    "64406".to_owned(),
                    "--user-data-dir=/example/CustomProfile".to_owned(),
                ],
                home,
            ),
            Some(64406)
        );
        assert_eq!(
            chatgpt_debug_port(
                &[
                    executable.to_owned(),
                    "--remote-debugging-address=127.0.0.1".to_owned(),
                    "--remote-debugging-port=64406".to_owned(),
                    "--user-data-dir=/example/OtherProfile".to_owned(),
                ],
                home,
            ),
            Some(64406)
        );
        assert_eq!(
            chatgpt_debug_port(
                &[
                    executable.to_owned(),
                    "--remote-debugging-address=0.0.0.0".to_owned(),
                    "--remote-debugging-port=64406".to_owned(),
                ],
                home,
            ),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_argument_buffer_parser_is_bounded_and_exact() {
        let arguments = [
            "/Applications/Chatgpt.app/Contents/MacOS/ChatGPT",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=64406",
        ];
        let mut buffer = (arguments.len() as libc::c_int).to_ne_bytes().to_vec();
        buffer.extend_from_slice(arguments[0].as_bytes());
        buffer.extend_from_slice(&[0, 0, 0]);
        for argument in arguments {
            buffer.extend_from_slice(argument.as_bytes());
            buffer.push(0);
        }
        assert_eq!(
            parse_macos_process_arguments(&buffer).unwrap(),
            arguments.map(str::to_owned)
        );
        buffer[0..std::mem::size_of::<libc::c_int>()]
            .copy_from_slice(&(513 as libc::c_int).to_ne_bytes());
        assert!(parse_macos_process_arguments(&buffer).is_none());
    }
}
