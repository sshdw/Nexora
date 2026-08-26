#![allow(clippy::pedantic)]
#![allow(clippy::doc_markdown)]
mod application;
mod commands;
mod domain;
mod infrastructure;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logging first so database and migration events are captured
    // (ARCHITECTURE.md §11).
    infrastructure::logging::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            commands::conversations::create_conversation,
            commands::conversations::list_conversations,
            commands::conversations::conversation_history,
            commands::conversations::rename_conversation,
            commands::conversations::archive_conversation,
            commands::conversations::restore_conversation,
            commands::conversations::delete_conversation,
            commands::conversations::send_message,
            commands::prompts::create_prompt,
            commands::prompts::list_prompts,
            commands::prompts::update_prompt,
            commands::prompts::delete_prompt,
            commands::prompts::insert_prompt_into_conversation,
            commands::attachments::attach_file,
            commands::attachments::list_attachments,
            commands::attachments::remove_attachment,
            commands::providers::list_providers,
            commands::providers::supported_providers,
            commands::providers::list_available_providers,
            commands::providers::is_provider_available,
            commands::providers::create_provider,
            commands::providers::remove_provider,
            commands::credentials::add_provider_credential,
            commands::credentials::update_provider_credential,
            commands::credentials::remove_provider_credential,
            commands::credentials::has_provider_credential,
            commands::search::search,
            commands::import_export::export_conversation,
            commands::import_export::export_conversation_to_file,
            commands::import_export::import_conversation,
            commands::settings::get_setting,
            commands::settings::set_setting,
            commands::settings::delete_setting,
            commands::settings::list_settings,
            commands::data_management::delete_conversation_permanently,
            commands::data_management::delete_prompt_permanently,
            commands::data_management::clear_application_data,
        ])
        .setup(|app| {
            // Locate the per-user application data directory and ensure it
            // exists before opening the database file.
            let db_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&db_dir)?;
            let db_path = db_dir.join("nexora.db");

            // Open the database, apply pragmas, and run pending migrations
            // (ROADMAP.md Phase 0; DATABASE.md §3–§5).
            let opened = infrastructure::database::open(&db_path).map_err(|err| {
                log::error!("sqlite initialization failed: {err}");
                err
            })?;
            // Store the single connection as shared application state so it
            // outlives setup() and remains available for the application's
            // lifetime (ROADMAP.md Phase 0; Tauri managed state).
            app.manage(infrastructure::database::Database::new(opened));
            // Confirm the shared connection is reachable through managed state
            // and record the applied schema version; startup fails loudly if it
            // is not (DATABASE.md §4–§5).
            let db = app.state::<infrastructure::database::Database>();
            let conn = db.lock()?;
            let version: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            log::info!(
                "sqlite initialized at {} (schema v{})",
                db_path.display(),
                version
            );

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
