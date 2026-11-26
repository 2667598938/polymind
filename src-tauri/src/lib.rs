pub mod commands;
pub mod state;

use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("polymind_core=info".parse().unwrap()))
        .with_target(false)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let home = app.path().app_data_dir()?;
            let state = state::AppState::new(home)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::set_workspace,
            commands::create_session,
            commands::list_sessions,
            commands::delete_session,
            commands::rename_session,
            commands::load_messages,
            commands::send_message,
            commands::cancel_turn,
            commands::resolve_approval,
            commands::upsert_provider,
            commands::delete_provider,
            commands::test_provider,
            commands::list_models,
            commands::save_settings,
            commands::usage_summary,
            commands::list_tools,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::export_session,
        ])
        .run(tauri::generate_context!())
        .expect("PolyMind 启动失败");
}
