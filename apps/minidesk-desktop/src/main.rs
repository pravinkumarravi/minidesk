//! miniDesk desktop graphical client entrypoint.

use minidesk_app_core::{AppCore, CoreEvent, UiCommand};
use minidesk_config::AppConfig;
use minidesk_logging::init_logging;
use minidesk_security::DeviceIdentity;
use minidesk_storage::Storage;
use minidesk_ui_egui::MiniDeskUi;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::Level;

struct MiniDeskApp {
    ui: MiniDeskUi,
}

impl eframe::App for MiniDeskApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui.render(ctx);

        if self.ui.session_state.is_active()
            || matches!(
                self.ui.session_state,
                minidesk_domain::SessionState::Resolving
                    | minidesk_domain::SessionState::Authenticating
                    | minidesk_domain::SessionState::Negotiating
            )
        {
            ctx.request_repaint();
        }
    }
}

fn main() -> Result<(), eframe::Error> {
    init_logging(Level::INFO);
    tracing::info!("Starting miniDesk client...");

    let config = AppConfig::default();
    let identity = Arc::new(DeviceIdentity::generate());
    let storage = Storage::open(&config.paths.database_path)
        .or_else(|_| Storage::open_in_memory())
        .expect("Failed to initialize storage");

    let device_id_str = identity.device_id().to_string();
    let device_name = config.device_name.clone();
    let listen_port = config.network.listen_port;

    // Channels for async AppCore <-> UI
    let (ui_cmd_tx, mut ui_cmd_rx) = mpsc::unbounded_channel::<UiCommand>();
    let (core_event_tx, core_event_rx) = mpsc::unbounded_channel::<CoreEvent>();

    // Initial lists
    let initial_devices = storage.get_saved_devices().unwrap_or_default();
    let initial_history = storage.get_session_history(50).unwrap_or_default();
    let _ = core_event_tx.send(CoreEvent::SavedDevicesUpdated(initial_devices));
    let _ = core_event_tx.send(CoreEvent::HistoryUpdated(initial_history));

    let core = Arc::new(AppCore::new(
        config,
        identity,
        storage,
        core_event_tx,
    ));

    // Spawn async background runtime for core commands
    let core_clone = Arc::clone(&core);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Tokio runtime creation failed");

        rt.block_on(async move {
            AppCore::start_host_service(Arc::clone(&core_clone));

            while let Some(cmd) = ui_cmd_rx.recv().await {
                if let Err(e) = core_clone.handle_command(cmd).await {
                    tracing::error!("Error handling UI command: {:?}", e);
                }
            }
        });
    });

    let ui = MiniDeskUi::new(
        ui_cmd_tx,
        core_event_rx,
        device_id_str,
        device_name,
        listen_port,
    );

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([800.0, 520.0])
            .with_title("miniDesk - Cross-Platform Remote Desktop"),
        ..Default::default()
    };

    eframe::run_native(
        "miniDesk",
        native_options,
        Box::new(|_cc| Ok(Box::new(MiniDeskApp { ui }))),
    )
}
