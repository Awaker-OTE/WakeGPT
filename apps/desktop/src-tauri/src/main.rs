// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(exit_code) = wakegpt_desktop_lib::run_update_guardian_if_requested() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = wakegpt_desktop_lib::run_update_health_probe_if_requested() {
        std::process::exit(exit_code);
    }
    wakegpt_desktop_lib::run()
}
