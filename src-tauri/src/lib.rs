mod application;
mod commands;
mod domain;
mod infrastructure;

use tauri::Manager;

/// Sweep orphaned 'running' agent runs from crashed sessions to 'error' at
/// startup (Task 5.2, DP-8): only `status='running'` rows are touched; all
/// other statuses and row counts untouched.
fn sweep_orphaned_agent_runs(db: &infrastructure::database::Database) {
    let swept = crate::infrastructure::repository::agent_runs::AgentRunRepository::new(db)
        .fail_orphaned_running_runs("run interrupted by application shutdown")
        .unwrap_or_else(|err| {
            log::warn!("orphaned run sweep failed: {err}");
            0
        });
    if swept > 0 {
        log::info!("swept {swept} orphaned agent runs to error");
    }
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
// The handler list grows with every command, so the function body exceeds
// the line-count lint by construction; the body is declarative registration.
#[allow(clippy::too_many_lines)]
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
            commands::context::conversation_context_stats,
            commands::prompts::create_prompt,
            commands::prompts::list_prompts,
            commands::prompts::update_prompt,
            commands::prompts::delete_prompt,
            commands::prompts::insert_prompt_into_conversation,
            commands::repo_audit::repo_audit,
            commands::testgen::testgen_drafts,
            commands::dep_refactor::dep_inventory,
            commands::dep_refactor::refactor_apply,
            commands::attachments::attach_file,
            commands::attachments::list_attachments,
            commands::attachments::remove_attachment,
            commands::providers::list_providers,
            commands::providers::supported_providers,
            commands::providers::list_available_providers,
            commands::providers::is_provider_available,
            commands::providers::provider_health,
            commands::providers::create_provider,
            commands::providers::remove_provider,
            commands::credentials::add_provider_credential,
            commands::credentials::update_provider_credential,
            commands::credentials::remove_provider_credential,
            commands::credentials::has_provider_credential,
            commands::compat::get_compat_config,
            commands::compat::set_compat_config,
            commands::compat::compat_status,
            commands::search::search,
            commands::import_export::export_conversation,
            commands::import_export::export_conversation_to_file,
            commands::import_export::import_conversation,
            commands::import_export::import_vscode_settings,
            commands::import_export::import_mcp_servers,
            commands::import_export::export_setup,
            commands::import_export::export_setup_to_file,
            commands::import_export::import_setup,
            commands::settings::get_setting,
            commands::settings::set_setting,
            commands::settings::delete_setting,
            commands::settings::list_settings,
            commands::workspace::get_workspace_root,
            commands::workspace::set_workspace_root,
            commands::workspace::list_workspace_recent,
            commands::workspace::nexora_init,
            commands::workspace::save_workspace_profile,
            commands::data_management::delete_conversation_permanently,
            commands::data_management::delete_prompt_permanently,
            commands::data_management::clear_application_data,
            commands::flags::flags_status,
            commands::agent::start_agent_run,
            commands::agent::cancel_agent_run,
            commands::agent::resolve_agent_approval,
            commands::agent::extend_agent_run,
            commands::agent::agent_set_mode,
            commands::agent::pause_agent_run,
            commands::agent::resume_agent_run,
            commands::agent::list_agent_runs,
            commands::agent::list_agent_steps,
            commands::agent::inspect_run,
            commands::agent::spend_dashboard,
            commands::agent::activity_feed,
            commands::agent::add_permission_rule,
            commands::agent::remove_permission_rule,
            commands::agent::list_permission_rules,
            commands::tasks::create_task,
            commands::tasks::list_tasks,
            commands::tasks::list_task_steps,
            commands::tasks::update_task,
            commands::tasks::delete_task,
            commands::tasks::start_task_run,
            commands::tasks::stop_task_run,
            commands::terminal::terminal_run,
            commands::terminal::terminal_kill,
            commands::terminal::terminal_explain,
            commands::version_control::git_info,
            commands::version_control::git_file_diff,
            commands::version_control::git_commit_diff,
            commands::version_control::git_stage,
            commands::version_control::git_unstage,
            commands::version_control::git_commit,
            commands::version_control::git_push,
            commands::version_control::git_generate_commit_message,
            commands::version_control::git_explain_commit,
            commands::github::gh_issues,
            commands::github::gh_pulls,
            commands::github::gh_actions,
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
            // Hold the active-task registry as managed state (task manager):
            // an `Arc` so the task IPC commands can clone an owned handle
            // into `spawn_blocking` and the spawned loop threads.
            app.manage(std::sync::Arc::new(
                application::agent::tasks::TaskRegistry::default(),
            ));
            // Hold the single-session terminal registry as managed state
            // (terminal panel): an `Arc` so the terminal IPC commands can
            // claim the session and clone the kill token into
            // `spawn_blocking`.
            app.manage(std::sync::Arc::new(
                application::terminal::TerminalRegistry::new(),
            ));
            // Confirm the shared connection is reachable through managed state
            // and record the applied schema version; startup fails loudly if it
            // is not (DATABASE.md §4–§5).
            let db = app.state::<infrastructure::database::Database>();
            sweep_orphaned_agent_runs(&db);
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
