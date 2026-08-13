mod anchored_fs;
mod api;
mod attachments;
mod codex_adapter;
mod commands;
mod data_export;
mod diagnostics;
mod domain;
mod file_sync;
mod lifecycle;
mod local_data_reset;
mod managed_markdown;
mod platform_trash;
mod storage;
mod update_install;
mod updates;

use storage::Store;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let context = tauri::generate_context!();
    let identifier = context.config().identifier.clone();
    if update_install::recover_interrupted_before_start(&identifier, env!("CARGO_PKG_VERSION"))
        .expect("WakeGPT could not safely recover the interrupted application update")
    {
        return;
    }
    if let Some(app_data_dir) = local_data_reset::collecting_app_data_dir(&identifier)
        .expect("WakeGPT could not verify the pending local data reset")
    {
        let profile = codex_adapter::DefaultIdentityProfile::new(app_data_dir)
            .expect("WakeGPT could not verify the protected ChatGPT profile before reset");
        let probe = codex_adapter::default_identity_runtime_probe(&profile);
        let reset_blocker = match probe.state {
            codex_adapter::DefaultIdentityRuntimeState::Stopped => None,
            codex_adapter::DefaultIdentityRuntimeState::Running
            | codex_adapter::DefaultIdentityRuntimeState::Occupied => {
                Some("local_data_reset_default_identity_running")
            }
            codex_adapter::DefaultIdentityRuntimeState::Unavailable => {
                Some("local_data_reset_default_identity_unavailable")
            }
        };
        if let Some(error_code) = reset_blocker {
            local_data_reset::cancel_collecting(&identifier, error_code)
                .expect("WakeGPT could not cancel an unsafe local data reset");
        }
    }
    local_data_reset::apply_pending(&identifier)
        .expect("WakeGPT could not safely finish the pending local data reset");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![lifecycle::AUTOSTART_ARGUMENT]),
        ))
        .plugin(
            tauri_plugin_updater::Builder::new()
                .target(updates::UPDATER_TARGET)
                .build(),
        )
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let local_data_reset_notice_pending = local_data_reset::read_notice(&app_data_dir)
                .map_err(std::io::Error::other)?
                .is_some();
            let diagnostics = diagnostics::LocalDiagnostics::new(&app_data_dir);
            let update_runtime =
                updates::UpdateRuntime::load(&app_data_dir).map_err(std::io::Error::other)?;
            let database_path = app_data_dir.join("wakegpt.sqlite3");
            let store = Store::open(&database_path)?;
            let product_settings = store.product_settings().map_err(std::io::Error::other)?;
            let default_identity_profile =
                codex_adapter::DefaultIdentityProfile::new(app_data_dir.clone())
                    .map_err(std::io::Error::other)?;
            let sync =
                file_sync::SyncCoordinator::new(app_data_dir.join("recovery").join("markdown-v1"))
                    .map_err(std::io::Error::other)?;
            let attachments = attachments::AttachmentCoordinator::new(
                app_data_dir.join("pending-attachments-v1"),
                app_data_dir.join("recovery").join("attachments-v1"),
            )
            .map_err(std::io::Error::other)?;
            store
                .queue_attachment_relocations_for_configured_directories()
                .map_err(std::io::Error::other)?;
            let attachment_prepare_recovery = attachments
                .prepare_pending_relocations(&store)
                .map_err(std::io::Error::other)?;
            let mut recovery_report = sync.replay_pending(&store).map_err(std::io::Error::other)?;
            let attachment_lifecycle_recovery = attachments
                .replay_pending(&store)
                .map_err(std::io::Error::other)?;
            let attachment_finish_recovery = attachments
                .finish_pending_relocations(&store)
                .map_err(std::io::Error::other)?;
            recovery_report.recovered_attachment_operations = attachment_lifecycle_recovery
                .recovered_operations
                + attachment_prepare_recovery.recovered_operations
                + attachment_finish_recovery.recovered_operations;
            recovery_report.pending_attachment_operations =
                attachment_lifecycle_recovery.pending.len()
                    + attachment_prepare_recovery.pending.len()
                    + attachment_finish_recovery.pending.len();
            diagnostics.record_startup(
                storage::SCHEMA_VERSION,
                recovery_report.recovered_operations
                    + recovery_report.recovered_attachment_operations,
                recovery_report.pending.len()
                    + recovery_report.pending_notebooks.len()
                    + recovery_report.pending_attachment_operations,
            );
            let integration_control = lifecycle::IntegrationControl::default();
            integration_control
                .set_paused(product_settings.codex_integration_paused)
                .map_err(std::io::Error::other)?;
            app.manage(store);
            app.manage(sync);
            app.manage(attachments);
            app.manage(file_sync::RecoveryState::new(recovery_report));
            app.manage(integration_control);
            app.manage(lifecycle::QuitControl::default());
            app.manage(codex_adapter::CodexIntegrationRuntime::default());
            app.manage(codex_adapter::CodexCardRefreshSignal::default());
            app.manage(codex_adapter::ComposerOperationGate::default());
            app.manage(default_identity_profile);
            app.manage(diagnostics);
            app.manage(update_runtime);
            lifecycle::install(app)?;
            lifecycle::apply_startup_visibility(app, local_data_reset_notice_pending)?;
            codex_adapter::CodexIntegrationRuntime::start(app.handle().clone())
                .map_err(std::io::Error::other)?;
            Ok(())
        })
        .on_window_event(lifecycle::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::app_status,
            commands::product_settings,
            commands::set_product_settings,
            commands::preview_record_trash_cleanup,
            commands::purge_record_trash,
            commands::local_diagnostics_status,
            commands::list_local_diagnostics,
            commands::update_local_diagnostics_settings,
            commands::export_local_diagnostics,
            commands::clear_local_diagnostics,
            commands::local_data_reset_preview,
            commands::local_data_reset_notice,
            commands::acknowledge_local_data_reset_notice,
            commands::reset_local_data,
            commands::login_item_status,
            commands::set_login_item_enabled,
            updates::update_status,
            updates::set_update_settings,
            updates::skip_update_version,
            updates::check_for_updates,
            updates::download_update,
            updates::discard_downloaded_update,
            updates::install_downloaded_update,
            updates::acknowledge_update_result,
            updates::open_update_release,
            commands::codex_integration_status,
            commands::set_codex_integration_paused,
            commands::restart_codex_integration,
            commands::restart_codex_instance_integration,
            commands::default_identity_status,
            commands::configure_default_identity,
            commands::use_default_identity,
            commands::unbind_default_identity,
            commands::authorize_workspace,
            commands::list_workspaces,
            commands::disconnect_workspace,
            commands::reveal_workspace,
            commands::show_main_window,
            commands::hide_quick_capture_window,
            commands::update_markdown_editor_quit_state,
            commands::confirm_and_quit,
            commands::acknowledge_quick_capture_quit,
            commands::cancel_quit_request,
            commands::ui_preferences,
            commands::activate_workspace,
            commands::set_active_selection,
            commands::set_theme_preference,
            commands::set_submit_shortcut,
            commands::set_markdown_layout,
            commands::load_draft,
            commands::save_draft,
            commands::load_quick_capture_draft,
            commands::save_quick_capture_draft,
            commands::create_notebook,
            commands::bind_existing_notebook,
            commands::list_notebooks,
            commands::preview_notebook_numbering,
            commands::change_notebook_numbering,
            commands::preview_notebook_attachment_directory,
            commands::change_notebook_attachment_directory,
            commands::reorder_notebooks,
            commands::rename_notebook,
            commands::workspace_open_preference,
            commands::set_workspace_open_preference,
            commands::set_notebook_pinned,
            commands::unbind_notebook,
            commands::rebind_notebook,
            commands::recover_notebook_target,
            commands::inspect_notebook_conflict,
            commands::resolve_notebook_conflict,
            commands::convert_notebook_to_plain,
            commands::trash_notebook_file,
            commands::read_notebook_document,
            commands::save_notebook_document,
            commands::pick_record_images,
            commands::stage_record_image,
            commands::discard_pending_image,
            commands::read_pending_image_preview,
            commands::read_record_image_preview,
            commands::read_markdown_image_preview,
            commands::create_record,
            commands::create_quick_capture_record,
            commands::list_records,
            commands::update_record,
            commands::trash_record,
            commands::restore_record,
            commands::migrate_record,
            commands::set_record_pinned,
            commands::export_local_data,
            commands::retry_pending_recovery,
            commands::preview_managed_markdown
        ])
        .build(context)
        .expect("WakeGPT failed to build");

    app.run(lifecycle::handle_run_event);
}

pub fn run_update_guardian_if_requested() -> Option<i32> {
    update_install::run_guardian_if_requested()
}

pub fn run_update_health_probe_if_requested() -> Option<i32> {
    update_install::run_health_probe_if_requested()
}
