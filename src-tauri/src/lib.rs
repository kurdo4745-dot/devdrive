use std::net::TcpStream;
use std::time::{Duration, Instant};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_log::Builder::default().build())
    .setup(|app| {
      // 1. Start DevDrive Axum HTTP UI server on dedicated multi-threaded Tokio runtime
      std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime for Axum UI");
        rt.block_on(async {
          let db_path = devdrive::db::Database::default_path();
          if let Ok(cfg) = devdrive::config::DevDriveConfig::load() {
              let _watcher_handle = devdrive::watcher::start_all_projects_watcher_background(&db_path, &cfg);
          }
          if let Err(e) = devdrive::ui::start_ui_server(4040, false).await {
            eprintln!("DevDrive UI Server exited: {:?}", e);
          }
        });
      });

      // 2. Poll until 127.0.0.1:4040 is accepting connections (up to 3 seconds)
      let start = Instant::now();
      while start.elapsed() < Duration::from_secs(3) {
        if TcpStream::connect("127.0.0.1:4040").is_ok() {
          break;
        }
        std::thread::sleep(Duration::from_millis(25));
      }

      // 3. Navigate the main window directly to http://127.0.0.1:4040
      if let Some(window) = app.get_webview_window("main") {
        if let Ok(url) = "http://127.0.0.1:4040".parse::<tauri::Url>() {
          let _ = window.navigate(url);
        }
      }

      Ok(())
    })
    .on_window_event(|_window, event| {
      if let tauri::WindowEvent::CloseRequested { .. } = event {
        std::process::exit(0);
      }
    })
    .run(tauri::generate_context!())
    .expect("error while building tauri application");
}
