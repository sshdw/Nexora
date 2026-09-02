mod application;
mod commands;
mod domain;
mod infrastructure;

use tauri::Manager;

/// Zero-dependency debug logger (debug builds only): prints
/// `[{level}] {target}: {args}` to stderr so provider/agent failures are
/// diagnosable during development. Records carry level + target module +
/// message only — never a request body, a payload field value, or a header
/// (ARCHITECTURE.md §12). Release builds install nothing here.
#[cfg(debug_assertions)]
fn init_debug_logger() {
    /// Sink mirroring every `log` record to stderr with module targets.
    struct DebugStderrLogger;

    impl log::Log for DebugStderrLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            log::max_level() >= metadata.level()
        }

        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                // Level + target/module + message only; never a request body,
                // a payload field value, or a header.
                eprintln!(
                    "[{}] {}: {}",
                    record.level(),
                    record.target(),
                    record.args()
                );
            }
        }

        fn flush(&self) {}
    }

    static DEBUG_STDERR_LOGGER: DebugStderrLogger = DebugStderrLogger;

    // `set_logger` fails only when a logger is already installed; the first
    // installer wins and that is an acceptable no-op, so the result is
    // ignored deliberately. (`set_boxed_logger` is unused because the crate's
    // unified `log` feature set does not enable `std`; a static logger needs
    // no allocation and no dependency change.)
    let _ = log::set_logger(&DEBUG_STDERR_LOGGER);
    log::set_max_level(log::LevelFilter::Debug);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Run the Tauri desktop application.
///
/// Initializes logging, opens (and migrates) the shared `SQLite` database into
/// managed state, and registers every command handler before entering the
/// Tauri event loop.
///
/// # Panics
///
/// Panics when the Tauri runtime fails to start (e.g. the bundled assets or
/// window context cannot be built); startup failure is unrecoverable.
pub fn run() {
    // Initialize logging first so database and migration events are captured
    // (ARCHITECTURE.md §11). The global `log` sink is once-only: debug builds
    // claim it with the verbose module-target logger BEFORE the minimal
    // `infrastructure::logging` sink attempts to install, then re-apply the
    // verbose max level (the shared `init` re-applies its own Info filter).
    #[cfg(debug_assertions)]
    init_debug_logger();
    infrastructure::logging::init();
    #[cfg(debug_assertions)]
    log::set_max_level(log::LevelFilter::Debug);

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
            commands::agent::start_agent_run,
            commands::agent::cancel_agent_run,
            commands::agent::resolve_agent_approval,
            commands::agent::extend_agent_run,
            commands::agent::agent_set_mode,
            commands::agent::pause_agent_run,
            commands::agent::resume_agent_run,
            commands::agent::list_agent_runs,
            commands::agent::list_agent_steps,
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
            // Hold the active-run registry as managed state (Task 5.1): an
            // `Arc` so the agent IPC commands can clone an owned handle into
            // `spawn_blocking` and the spawned run threads.
            app.manage(std::sync::Arc::new(
                application::agent::service::AgentRunRegistry::default(),
            ));
            // Confirm the shared connection is reachable through managed state
            // and record the applied schema version; startup fails loudly if it
            // is not (DATABASE.md §4–§5).
            let db = app.state::<infrastructure::database::Database>();
            // Sweep orphaned 'running' runs from crashed sessions to 'error'
            // at startup (Task 5.2, DP-8): only status='running' rows are
            // touched; all other statuses and row counts untouched.
            {
                let swept = crate::infrastructure::repository::agent_runs::AgentRunRepository::new(
                    db.inner(),
                )
                .fail_orphaned_running_runs("run interrupted by application shutdown")
                .unwrap_or_else(|err| {
                    log::warn!("orphaned run sweep failed: {err}");
                    0
                });
                if swept > 0 {
                    log::info!("swept {swept} orphaned agent runs to error");
                }
            }
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
