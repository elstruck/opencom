// opencom desktop: runs the opencom server in-process and shows its UI in a native window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

struct ServerState(Mutex<Option<opencom::server::Server>>);

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "opencom=info,warn".into()))
        .init();

    // Start the server first. Prefer the standard port so Companion and other
    // machines can find it; fall back to a free port if it is taken.
    let show = opencom::server::default_show_path();
    let server = tauri::async_runtime::block_on(async {
        match opencom::server::start(show.clone(), "127.0.0.1", 7430).await {
            Ok(s) => Ok(s),
            Err(e) => {
                tracing::warn!("port 7430 unavailable ({e:#}); using a free port");
                opencom::server::start(show, "127.0.0.1", 0).await
            }
        }
    });
    let server = match server {
        Ok(s) => s,
        Err(e) => {
            eprintln!("opencom could not start: {e:#}");
            std::process::exit(1);
        }
    };
    let url = server.url();
    println!("opencom running at {url}");

    let app = tauri::Builder::default()
        .manage(ServerState(Mutex::new(Some(server))))
        .setup(move |app| {
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("opencom")
                .inner_size(1280.0, 820.0)
                .min_inner_size(640.0, 400.0)
                .build()?;

            let show_i = MenuItem::with_id(app, "show", "Show opencom", true, None::<&str>)?;
            let top_i = CheckMenuItem::with_id(app, "top", "Always on top", true, false, None::<&str>)?;
            let browser_i = MenuItem::with_id(app, "browser", "Open in browser", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &top_i, &browser_i, &sep, &quit_i])?;
            let url2 = url.clone();
            let top2 = top_i.clone();
            let mut tray = TrayIconBuilder::new().tooltip("opencom").menu(&menu).on_menu_event(move |app, ev| match ev.id.as_ref() {
                "show" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.unminimize();
                        let _ = w.set_focus();
                    }
                }
                "top" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let on = top2.is_checked().unwrap_or(false);
                        let _ = w.set_always_on_top(on);
                    }
                }
                "browser" => open_browser(&url2),
                "quit" => app.exit(0),
                _ => {}
            });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            let _ = win.set_focus();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the opencom window");

    app.run(|handle, ev| {
        if let RunEvent::Exit = ev {
            let srv = handle.state::<ServerState>().0.lock().ok().and_then(|mut g| g.take());
            if let Some(s) = srv {
                if let Err(e) = tauri::async_runtime::block_on(s.stop()) {
                    eprintln!("error while stopping: {e:#}");
                }
            }
        }
    });
}
