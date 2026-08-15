use crate::diagnostics::LocalDiagnostics;
use crate::storage::Store;
use serde::Serialize;
use std::{ffi::OsStr, sync::Mutex};

#[cfg(target_os = "macos")]
use tauri::image::Image;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    App, AppHandle, Emitter, Manager, RunEvent, Window, WindowEvent, Wry,
};

const MAIN_WINDOW_LABEL: &str = "main";
const QUICK_CAPTURE_WINDOW_LABEL: &str = "quick-capture";
const TRAY_ID: &str = "wakegpt-menu";
const OPEN_ID: &str = "open-wakegpt";
const QUICK_CAPTURE_ID: &str = "quick-capture";
const TOGGLE_CODEX_ID: &str = "toggle-codex-integration";
const QUIT_ID: &str = "quit-wakegpt";
const QUICK_CAPTURE_READY_EVENT: &str = "wakegpt://quick-capture-ready";
const CODEX_PAUSED_EVENT: &str = "wakegpt://codex-integration-paused";
const QUIT_REQUESTED_EVENT: &str = "wakegpt://quit-requested";
pub const AUTOSTART_ARGUMENT: &str = "--wakegpt-autostart";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayAction {
    Open,
    QuickCapture,
    ToggleCodex,
    Quit,
}

impl TrayAction {
    fn from_id(id: &str) -> Option<Self> {
        match id {
            OPEN_ID => Some(Self::Open),
            QUICK_CAPTURE_ID => Some(Self::QuickCapture),
            TOGGLE_CODEX_ID => Some(Self::ToggleCodex),
            QUIT_ID => Some(Self::Quit),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct IntegrationControl {
    inner: Mutex<IntegrationState>,
}

#[derive(Default)]
pub struct QuitControl {
    inner: Mutex<QuitState>,
}

#[derive(Default)]
struct QuitState {
    editor_revision: u64,
    markdown_editor_open: bool,
    markdown_dirty: bool,
    markdown_saving: bool,
    next_request_id: u64,
    active_request_id: Option<u64>,
    quick_capture_ready_request_id: Option<u64>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitRequest {
    request_id: u64,
}

impl QuitControl {
    pub fn update_markdown_editor(
        &self,
        open: bool,
        dirty: bool,
        saving: bool,
        revision: u64,
    ) -> Result<(), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Quit state is unavailable".to_owned())?;
        if revision < state.editor_revision {
            return Ok(());
        }
        state.editor_revision = revision;
        state.markdown_editor_open = open;
        state.markdown_dirty = open && dirty;
        state.markdown_saving = open && saving;
        Ok(())
    }

    pub fn acknowledge_quick_capture(&self, request_id: u64) -> Result<(), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Quit state is unavailable".to_owned())?;
        if state.active_request_id != Some(request_id) {
            return Err("Quit request is stale".to_owned());
        }
        state.quick_capture_ready_request_id = Some(request_id);
        Ok(())
    }

    pub fn confirm_exit(&self, request_id: u64) -> Result<(), &'static str> {
        let mut state = self.inner.lock().map_err(|_| "quit_state_unavailable")?;
        if state.active_request_id != Some(request_id) {
            return Err("quit_request_stale");
        }
        if state.markdown_saving {
            return Err("markdown_save_in_progress");
        }
        if state.quick_capture_ready_request_id != Some(request_id) {
            return Err("quick_capture_quit_pending");
        }
        state.active_request_id = None;
        state.quick_capture_ready_request_id = None;
        Ok(())
    }

    pub fn cancel_request(&self, request_id: u64) -> Result<(), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Quit state is unavailable".to_owned())?;
        if state.active_request_id == Some(request_id) {
            state.active_request_id = None;
            state.quick_capture_ready_request_id = None;
        }
        Ok(())
    }

    fn begin_request(&self) -> Result<u64, String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Quit state is unavailable".to_owned())?;
        if let Some(request_id) = state.active_request_id {
            return Ok(request_id);
        }
        state.next_request_id = state.next_request_id.wrapping_add(1).max(1);
        let request_id = state.next_request_id;
        state.active_request_id = Some(request_id);
        state.quick_capture_ready_request_id = None;
        Ok(request_id)
    }
}

#[derive(Default)]
struct IntegrationState {
    paused: bool,
    menu_item: Option<MenuItem<Wry>>,
    revision: u64,
}

impl IntegrationControl {
    pub fn is_paused(&self) -> Result<bool, String> {
        self.inner
            .lock()
            .map(|state| state.paused)
            .map_err(|_| "Codex integration state is unavailable".to_owned())
    }

    pub fn set_paused(&self, paused: bool) -> Result<bool, String> {
        loop {
            let (menu_item, revision) = {
                let state = self
                    .inner
                    .lock()
                    .map_err(|_| "Codex integration state is unavailable".to_owned())?;
                (state.menu_item.clone(), state.revision)
            };

            update_integration_menu_label(menu_item.as_ref(), paused)?;

            let mut state = self
                .inner
                .lock()
                .map_err(|_| "Codex integration state is unavailable".to_owned())?;
            if state.revision != revision {
                continue;
            }
            if state.paused != paused {
                state.paused = paused;
                state.revision = state.revision.wrapping_add(1);
            }
            return Ok(paused);
        }
    }

    fn toggle(&self) -> Result<bool, String> {
        loop {
            let (menu_item, paused, revision) = {
                let state = self
                    .inner
                    .lock()
                    .map_err(|_| "Codex integration state is unavailable".to_owned())?;
                (state.menu_item.clone(), !state.paused, state.revision)
            };

            update_integration_menu_label(menu_item.as_ref(), paused)?;

            let mut state = self
                .inner
                .lock()
                .map_err(|_| "Codex integration state is unavailable".to_owned())?;
            if state.revision != revision {
                continue;
            }
            state.paused = paused;
            state.revision = state.revision.wrapping_add(1);
            return Ok(paused);
        }
    }

    fn attach_menu_item(&self, menu_item: MenuItem<Wry>) -> Result<(), String> {
        loop {
            let (paused, revision) = {
                let state = self
                    .inner
                    .lock()
                    .map_err(|_| "Codex integration state is unavailable".to_owned())?;
                (state.paused, state.revision)
            };

            update_integration_menu_label(Some(&menu_item), paused)?;

            let mut state = self
                .inner
                .lock()
                .map_err(|_| "Codex integration state is unavailable".to_owned())?;
            if state.revision != revision {
                continue;
            }
            state.menu_item = Some(menu_item);
            state.revision = state.revision.wrapping_add(1);
            return Ok(());
        }
    }
}

pub fn install(app: &mut App<Wry>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, OPEN_ID, "打开 WakeGPT", true, None::<&str>)?;
    let quick_capture = MenuItem::with_id(
        app,
        QUICK_CAPTURE_ID,
        "快速记录",
        true,
        Some("CmdOrCtrl+Shift+N"),
    )?;
    let toggle_codex = MenuItem::with_id(
        app,
        TOGGLE_CODEX_ID,
        "暂停 ChatGPT 速记卡",
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "退出 WakeGPT", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[&open, &quick_capture, &toggle_codex, &separator, &quit],
    )?;

    app.state::<IntegrationControl>()
        .attach_menu_item(toggle_codex)
        .map_err(std::io::Error::other)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("WakeGPT")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let Some(action) = TrayAction::from_id(event.id.as_ref()) else {
                return;
            };
            handle_tray_action(app, action);
        });

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .icon(menu_bar_template_icon())
            .icon_as_template(true);
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

pub fn is_autostart_launch<I, S>(arguments: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    arguments
        .into_iter()
        .any(|argument| argument.as_ref() == OsStr::new(AUTOSTART_ARGUMENT))
}

pub fn apply_startup_visibility(
    app: &App<Wry>,
    local_data_reset_notice_pending: bool,
) -> tauri::Result<()> {
    if local_data_reset_notice_pending || !is_autostart_launch(std::env::args_os()) {
        return Ok(());
    }
    let window = require_main_window(app.get_webview_window(MAIN_WINDOW_LABEL))?;
    window.hide()
}

pub fn handle_window_event(window: &Window<Wry>, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        match window.label() {
            MAIN_WINDOW_LABEL => {
                api.prevent_close();
                if window.hide().is_err() {
                    diagnose_lifecycle_failure(window.app_handle(), "hide_main_window");
                }
            }
            QUICK_CAPTURE_WINDOW_LABEL => {
                api.prevent_close();
                if window.hide().is_err() {
                    diagnose_lifecycle_failure(window.app_handle(), "hide_quick_capture_window");
                }
            }
            _ => {}
        }
    }
}

pub fn handle_run_event(app: &AppHandle<Wry>, event: RunEvent) {
    if let RunEvent::ExitRequested { code, api, .. } = &event {
        // Programmatic exits are reserved for update/restart flows that already own their
        // confirmation and write freeze. Interactive exits must pass through the renderer guard.
        if code.is_none() {
            api.prevent_exit();
            request_guarded_quit(app);
            return;
        }
    }
    #[cfg(target_os = "macos")]
    if let RunEvent::Reopen {
        has_visible_windows: false,
        ..
    } = event
    {
        if show_main_window(app).is_err() {
            diagnose_lifecycle_failure(app, "reopen_main_window");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}

pub fn show_main_window(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let window = require_main_window(app.get_webview_window(MAIN_WINDOW_LABEL))?;
    window.unminimize()?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

pub fn show_quick_capture_window(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let window = require_main_window(app.get_webview_window(QUICK_CAPTURE_WINDOW_LABEL))?;
    position_quick_capture_window(app, &window)?;
    window.unminimize()?;
    window.show()?;
    window.set_focus()?;
    window.emit(QUICK_CAPTURE_READY_EVENT, ())?;
    Ok(())
}

pub fn hide_quick_capture_window(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let window = require_main_window(app.get_webview_window(QUICK_CAPTURE_WINDOW_LABEL))?;
    window.hide()
}

fn position_quick_capture_window(
    app: &AppHandle<Wry>,
    window: &tauri::WebviewWindow<Wry>,
) -> tauri::Result<()> {
    let cursor = app.cursor_position()?;
    let monitor = app
        .monitor_from_point(cursor.x, cursor.y)?
        .or_else(|| app.available_monitors().ok()?.into_iter().next())
        .ok_or(tauri::Error::WindowNotFound)?;
    let work_area = monitor.work_area();
    let window_size = window.outer_size()?;
    let margin = 8_i32;
    let work_left = work_area.position.x;
    let work_top = work_area.position.y;
    let work_right =
        work_left.saturating_add(i32::try_from(work_area.size.width).unwrap_or(i32::MAX));
    let work_bottom =
        work_top.saturating_add(i32::try_from(work_area.size.height).unwrap_or(i32::MAX));
    let width = i32::try_from(window_size.width).unwrap_or(i32::MAX);
    let height = i32::try_from(window_size.height).unwrap_or(i32::MAX);
    let max_x = work_right.saturating_sub(width).saturating_sub(margin);
    let max_y = work_bottom.saturating_sub(height).saturating_sub(margin);
    let desired_x = (cursor.x.round() as i32).saturating_sub(width / 2);
    let desired_y = work_top.saturating_add(margin);
    let minimum_x = work_left.saturating_add(margin);
    let minimum_y = work_top.saturating_add(margin);
    let x = desired_x.clamp(minimum_x, max_x.max(minimum_x));
    let y = desired_y.clamp(minimum_y, max_y.max(minimum_y));
    window.set_position(tauri::PhysicalPosition::new(x, y))
}

fn handle_tray_action(app: &AppHandle<Wry>, action: TrayAction) {
    match action {
        TrayAction::Open => {
            if show_main_window(app).is_err() {
                diagnose_lifecycle_failure(app, "open_main_window");
            }
        }
        TrayAction::QuickCapture => {
            if show_quick_capture_window(app).is_err() {
                diagnose_lifecycle_failure(app, "open_quick_capture_window");
            }
        }
        TrayAction::ToggleCodex => {
            let control = app.state::<IntegrationControl>();
            let previous = match control.is_paused() {
                Ok(value) => value,
                Err(_) => {
                    diagnose_lifecycle_failure(app, "toggle_codex_integration");
                    return;
                }
            };
            match control.toggle() {
                Ok(paused) => {
                    if app
                        .state::<Store>()
                        .set_codex_integration_paused(paused)
                        .is_err()
                    {
                        let _ = control.set_paused(previous);
                        diagnose_lifecycle_failure(app, "persist_codex_integration_state");
                        return;
                    }
                    if app.emit(CODEX_PAUSED_EVENT, paused).is_err() {
                        let _ = control.set_paused(previous);
                        let _ = app.state::<Store>().set_codex_integration_paused(previous);
                        diagnose_lifecycle_failure(app, "emit_codex_integration_state");
                    }
                }
                Err(_) => diagnose_lifecycle_failure(app, "toggle_codex_integration"),
            }
        }
        TrayAction::Quit => request_guarded_quit(app),
    }
}

fn request_guarded_quit(app: &AppHandle<Wry>) {
    let request = app
        .state::<QuitControl>()
        .begin_request()
        .map(|request_id| QuitRequest { request_id });
    if show_main_window(app).is_err()
        || request
            .and_then(|request| {
                app.emit(QUIT_REQUESTED_EVENT, request)
                    .map_err(|e| e.to_string())
            })
            .is_err()
    {
        diagnose_lifecycle_failure(app, "request_guarded_quit");
    }
}

fn require_main_window<T>(window: Option<T>) -> tauri::Result<T> {
    window.ok_or(tauri::Error::WindowNotFound)
}

fn diagnose_lifecycle_failure(app: &AppHandle<Wry>, operation: &'static str) {
    app.state::<LocalDiagnostics>()
        .record_lifecycle_failure(operation, "lifecycle_operation_failed");
}

fn update_integration_menu_label(
    menu_item: Option<&MenuItem<Wry>>,
    paused: bool,
) -> Result<(), String> {
    let label = if paused {
        "恢复 ChatGPT 速记卡"
    } else {
        "暂停 ChatGPT 速记卡"
    };
    if let Some(menu_item) = menu_item {
        menu_item
            .set_text(label)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn menu_bar_template_icon() -> Image<'static> {
    const WIDTH: usize = 22;
    const HEIGHT: usize = 18;
    const SAMPLES_PER_AXIS: usize = 8;
    const STROKE_RADIUS: f64 = 0.72;

    // Mirror the app's stacked-note mark with only the strokes that survive at
    // menu-bar size: the rear sheet, the folded front sheet, and two text lines.
    let segments = [
        ((8.0, 2.75), (18.0, 2.75)),
        ((18.0, 2.75), (18.0, 13.0)),
        ((4.0, 5.0), (12.5, 5.0)),
        ((12.5, 5.0), (15.5, 8.0)),
        ((15.5, 8.0), (15.5, 15.0)),
        ((15.5, 15.0), (4.0, 15.0)),
        ((4.0, 15.0), (4.0, 5.0)),
        ((12.5, 5.0), (12.5, 8.0)),
        ((12.5, 8.0), (15.5, 8.0)),
        ((7.0, 10.25), (12.5, 10.25)),
        ((7.0, 12.5), (10.75, 12.5)),
    ];

    let distance_squared = |point: (f64, f64), segment: &((f64, f64), (f64, f64))| {
        let delta = (segment.1 .0 - segment.0 .0, segment.1 .1 - segment.0 .1);
        let length_squared = delta.0 * delta.0 + delta.1 * delta.1;
        let projection = (((point.0 - segment.0 .0) * delta.0
            + (point.1 - segment.0 .1) * delta.1)
            / length_squared)
            .clamp(0.0, 1.0);
        let closest = (
            segment.0 .0 + projection * delta.0,
            segment.0 .1 + projection * delta.1,
        );
        (point.0 - closest.0).powi(2) + (point.1 - closest.1).powi(2)
    };

    let mut rgba = vec![0_u8; WIDTH * HEIGHT * 4];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut covered = 0_usize;
            for sample_y in 0..SAMPLES_PER_AXIS {
                for sample_x in 0..SAMPLES_PER_AXIS {
                    let point = (
                        x as f64 + (sample_x as f64 + 0.5) / SAMPLES_PER_AXIS as f64,
                        y as f64 + (sample_y as f64 + 0.5) / SAMPLES_PER_AXIS as f64,
                    );
                    if segments.iter().any(|segment| {
                        distance_squared(point, segment) <= STROKE_RADIUS * STROKE_RADIUS
                    }) {
                        covered += 1;
                    }
                }
            }
            rgba[(y * WIDTH + x) * 4 + 3] =
                ((covered * usize::from(u8::MAX)) / SAMPLES_PER_AXIS.pow(2)) as u8;
        }
    }

    Image::new_owned(rgba, WIDTH as u32, HEIGHT as u32)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    #[test]
    fn maps_only_declared_tray_actions() {
        assert_eq!(TrayAction::from_id(OPEN_ID), Some(TrayAction::Open));
        assert_eq!(
            TrayAction::from_id(QUICK_CAPTURE_ID),
            Some(TrayAction::QuickCapture)
        );
        assert_eq!(
            TrayAction::from_id(TOGGLE_CODEX_ID),
            Some(TrayAction::ToggleCodex)
        );
        assert_eq!(TrayAction::from_id(QUIT_ID), Some(TrayAction::Quit));
        assert_eq!(TrayAction::from_id("unexpected"), None);
    }

    #[test]
    fn integration_control_toggles_deterministically() {
        let control = IntegrationControl::default();
        assert!(!control.is_paused().unwrap());
        assert!(control.toggle().unwrap());
        assert!(control.is_paused().unwrap());
        control.set_paused(false).unwrap();
        assert!(!control.is_paused().unwrap());
    }

    #[test]
    fn integration_control_keeps_concurrent_toggles_linearizable() {
        const THREADS: usize = 8;
        const TOGGLES_PER_THREAD: usize = 101;

        let control = Arc::new(IntegrationControl::default());
        let workers = (0..THREADS)
            .map(|_| {
                let control = Arc::clone(&control);
                thread::spawn(move || {
                    for _ in 0..TOGGLES_PER_THREAD {
                        control.toggle().unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(
            control.is_paused().unwrap(),
            (THREADS * TOGGLES_PER_THREAD) % 2 == 1
        );
    }

    #[test]
    fn missing_main_window_is_an_explicit_tauri_error() {
        let error = require_main_window::<()>(None).unwrap_err();
        assert!(matches!(error, tauri::Error::WindowNotFound));
        assert_eq!(require_main_window(Some(7)).unwrap(), 7);
    }

    #[test]
    fn only_the_exact_autostart_argument_requests_a_quiet_launch() {
        assert!(is_autostart_launch(["wakegpt", AUTOSTART_ARGUMENT]));
        assert!(!is_autostart_launch(["wakegpt"]));
        assert!(!is_autostart_launch([
            "wakegpt",
            "--wakegpt-autostart=true"
        ]));
        assert!(!is_autostart_launch(["wakegpt", "wakegpt-autostart"]));
    }

    #[test]
    fn quit_control_refuses_confirmation_while_markdown_is_saving() {
        let control = QuitControl::default();
        control.update_markdown_editor(true, true, true, 1).unwrap();
        let request_id = control.begin_request().unwrap();
        control.acknowledge_quick_capture(request_id).unwrap();
        assert_eq!(
            control.confirm_exit(request_id),
            Err("markdown_save_in_progress")
        );
        control
            .update_markdown_editor(true, true, false, 2)
            .unwrap();
        control.confirm_exit(request_id).unwrap();
    }

    #[test]
    fn quit_control_requires_the_current_quick_capture_draft() {
        let control = QuitControl::default();
        let request_id = control.begin_request().unwrap();
        assert_eq!(
            control.confirm_exit(request_id),
            Err("quick_capture_quit_pending")
        );
        control.acknowledge_quick_capture(request_id).unwrap();
        control.confirm_exit(request_id).unwrap();
        assert_eq!(control.confirm_exit(request_id), Err("quit_request_stale"));
    }

    #[test]
    fn stale_editor_state_cannot_unlock_a_newer_save() {
        let control = QuitControl::default();
        control.update_markdown_editor(true, true, true, 8).unwrap();
        control
            .update_markdown_editor(true, true, false, 7)
            .unwrap();
        let request_id = control.begin_request().unwrap();
        control.acknowledge_quick_capture(request_id).unwrap();
        assert_eq!(
            control.confirm_exit(request_id),
            Err("markdown_save_in_progress")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn template_icon_is_an_antialiased_stacked_note_mark() {
        let icon = menu_bar_template_icon();
        assert_eq!(icon.width(), 22);
        assert_eq!(icon.height(), 18);
        let alpha = icon
            .rgba()
            .chunks_exact(4)
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>();
        assert!(alpha.contains(&u8::MAX));
        assert!(alpha.iter().any(|value| *value > 0 && *value < u8::MAX));
        let alpha_at = |x: usize, y: usize| alpha[y * 22 + x];
        for x in 0..22 {
            assert_eq!(alpha_at(x, 0), 0);
            assert_eq!(alpha_at(x, 17), 0);
        }
        for y in 0..18 {
            assert_eq!(alpha_at(0, y), 0);
            assert_eq!(alpha_at(21, y), 0);
        }
        assert!(alpha_at(10, 2) > 0);
        assert!(alpha_at(18, 7) > 0);
        assert!(alpha_at(6, 5) > 0);
        assert!(alpha_at(14, 6) > 0);
        assert!(alpha_at(4, 9) > 0);
        assert!(alpha_at(9, 10) > 0);
        assert!(alpha_at(8, 12) > 0);
        assert!(alpha_at(10, 15) > 0);
        assert_eq!(alpha_at(9, 7), 0);
        assert_eq!(alpha_at(2, 14), 0);
        assert_eq!(alpha_at(17, 14), 0);
    }
}
