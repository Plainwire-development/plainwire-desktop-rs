mod account;
mod api;
mod app;
mod backend;
mod media;
mod model;
mod realtime;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {

    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {

    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let rt = runtime.handle().clone();

    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    let repaint = backend::Repainter::new();
    let worker_repaint = repaint.clone();
    runtime.spawn(backend::run(command_rx, update_tx, worker_repaint));

    let backend = backend::Backend::new(command_tx);
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Plainwire")
            .with_app_id("plainwire-desktop")
            .with_inner_size([1160.0, 760.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    let ui_repaint = repaint.clone();
    eframe::run_native(
        "Plainwire",
        options,
        Box::new(move |cc| {
            let mut app = app::App::new(rt.clone(), backend, update_rx, ui_repaint, &cc.egui_ctx);
            app.init_tray();
            Ok(Box::new(app))
        }),
    )
}
