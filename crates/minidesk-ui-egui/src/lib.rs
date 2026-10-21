//! miniDesk UI components and presentation layer using egui.

use egui::{Color32, CornerRadius, RichText, Vec2};
use minidesk_app_core::{CoreEvent, UiCommand};
use minidesk_domain::{
    CapabilitySet, DeviceId, DeviceProfile, NetworkAddress, SessionId, SessionState,
};
use minidesk_storage::SessionHistoryRecord;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationTab {
    Connect,
    SavedDevices,
    History,
    Settings,
    ActiveSession,
}

/// Main state holder for the egui GUI.
pub struct MiniDeskUi {
    // Navigation
    pub active_tab: NavigationTab,

    // Connection Form
    pub remote_address_input: String,
    pub input_error: Option<String>,
    pub connect_capabilities: CapabilitySet,

    // Active session telemetry & state
    pub session_state: SessionState,
    pub status_banner: Option<String>,
    pub error_banner: Option<String>,

    // Incoming session prompt
    pub pending_incoming: Option<IncomingSessionModal>,

    // Cached lists
    pub saved_devices: Vec<DeviceProfile>,
    pub session_history: Vec<SessionHistoryRecord>,

    // Device details
    pub local_device_id: String,
    pub local_device_name: String,
    pub local_listen_port: u16,

    // Remote screen texture and frame
    pub remote_texture: Option<egui::TextureHandle>,
    pub latest_frame: Option<(u32, u32, Vec<u8>)>,

    // Core communication channels
    pub command_sender: mpsc::UnboundedSender<UiCommand>,
    pub event_receiver: mpsc::UnboundedReceiver<CoreEvent>,
}

#[derive(Clone)]
pub struct IncomingSessionModal {
    pub session_id: SessionId,
    pub peer_name: String,
    pub peer_device_id: Option<DeviceId>,
    pub peer_address: NetworkAddress,
    pub requested_capabilities: CapabilitySet,
}

impl MiniDeskUi {
    pub fn new(
        command_sender: mpsc::UnboundedSender<UiCommand>,
        event_receiver: mpsc::UnboundedReceiver<CoreEvent>,
        device_id: String,
        device_name: String,
        listen_port: u16,
    ) -> Self {
        Self {
            active_tab: NavigationTab::Connect,
            remote_address_input: String::new(),
            input_error: None,
            connect_capabilities: CapabilitySet::FULL_CONTROL,
            session_state: SessionState::Idle,
            status_banner: None,
            error_banner: None,
            pending_incoming: None,
            saved_devices: Vec::new(),
            session_history: Vec::new(),
            local_device_id: device_id,
            local_device_name: device_name,
            local_listen_port: listen_port,
            remote_texture: None,
            latest_frame: None,
            command_sender,
            event_receiver,
        }
    }

    /// Process asynchronous messages from AppCore.
    pub fn process_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                CoreEvent::StateChanged(state) => {
                    self.session_state = state;
                    if state == SessionState::Active {
                        self.active_tab = NavigationTab::ActiveSession;
                    } else if state.is_terminal() {
                        self.active_tab = NavigationTab::Connect;
                    }
                }
                CoreEvent::IncomingSessionRequest {
                    session_id,
                    peer_name,
                    peer_device_id,
                    peer_address,
                    requested_capabilities,
                } => {
                    self.pending_incoming = Some(IncomingSessionModal {
                        session_id,
                        peer_name,
                        peer_device_id,
                        peer_address,
                        requested_capabilities,
                    });
                }
                CoreEvent::StatusMessage(msg) => {
                    self.status_banner = Some(msg);
                    self.error_banner = None;
                }
                CoreEvent::ErrorMessage(msg) => {
                    self.error_banner = Some(msg);
                }
                CoreEvent::SavedDevicesUpdated(devices) => {
                    self.saved_devices = devices;
                }
                CoreEvent::FrameReceived { width, height, data } => {
                    self.latest_frame = Some((width, height, data));
                }
                CoreEvent::HistoryUpdated(history) => {
                    self.session_history = history;
                }
            }
        }
    }

    /// Render the entire miniDesk desktop interface.
    pub fn render(&mut self, ctx: &egui::Context) {
        self.process_events();

        if let Some((w, h, data)) = self.latest_frame.take() {
            let color_image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
            self.remote_texture = Some(ctx.load_texture("remote_screen", color_image, egui::TextureOptions::LINEAR));
        }

        self.apply_theme(ctx);

        // Top notification / status banner if present
        if let Some(err) = &self.error_banner {
            egui::TopBottomPanel::top("error_banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚠ Error:").color(Color32::from_rgb(255, 100, 100)).strong());
                    ui.label(RichText::new(err).color(Color32::WHITE));
                });
            });
        }

        // Left Navigation Sidebar
        egui::SidePanel::left("nav_sidebar")
            .resizable(false)
            .exact_width(220.0)
            .show(ctx, |ui| {
                self.render_sidebar(ui);
            });

        // Main Center Panel
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                NavigationTab::Connect => self.render_connect_tab(ui),
                NavigationTab::SavedDevices => self.render_saved_devices_tab(ui),
                NavigationTab::History => self.render_history_tab(ui),
                NavigationTab::Settings => self.render_settings_tab(ui),
                NavigationTab::ActiveSession => self.render_active_session_tab(ui),
            }
        });

        // Incoming Connection Modal if active
        if let Some(incoming) = self.pending_incoming.clone() {
            self.render_incoming_modal(ctx, &incoming);
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(22, 24, 29);
        visuals.window_fill = Color32::from_rgb(30, 32, 38);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(28, 30, 36);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 42, 51);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 58, 70);
        visuals.widgets.active.bg_fill = Color32::from_rgb(37, 99, 235); // Blue accent
        ctx.set_visuals(visuals);
    }

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.heading(RichText::new("⚡ miniDesk").color(Color32::from_rgb(96, 165, 250)).strong());
        ui.label(RichText::new("Direct IP Remote Desktop").size(11.0).color(Color32::GRAY));
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(12.0);

        let nav_item = |ui: &mut egui::Ui, name: &str, tab: NavigationTab, current: NavigationTab| -> bool {
            let selected = tab == current;
            let text = if selected {
                RichText::new(format!("▶ {}", name)).color(Color32::WHITE).strong()
            } else {
                RichText::new(name).color(Color32::LIGHT_GRAY)
            };
            ui.selectable_label(selected, text).clicked()
        };

        if nav_item(ui, "Connect", NavigationTab::Connect, self.active_tab) {
            self.active_tab = NavigationTab::Connect;
        }
        if nav_item(ui, "Saved Devices", NavigationTab::SavedDevices, self.active_tab) {
            self.active_tab = NavigationTab::SavedDevices;
        }
        if nav_item(ui, "Session History", NavigationTab::History, self.active_tab) {
            self.active_tab = NavigationTab::History;
        }
        if nav_item(ui, "Settings", NavigationTab::Settings, self.active_tab) {
            self.active_tab = NavigationTab::Settings;
        }

        if self.session_state.is_active() {
            ui.add_space(8.0);
            if nav_item(ui, "🔴 Active Session", NavigationTab::ActiveSession, self.active_tab) {
                self.active_tab = NavigationTab::ActiveSession;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new(format!("This Device: {}", self.local_device_name)).size(11.0));
            ui.label(RichText::new(format!("Port: {}", self.local_listen_port)).size(11.0).color(Color32::GRAY));
            let short_id = if self.local_device_id.len() > 16 {
                &self.local_device_id[..16]
            } else {
                &self.local_device_id
            };
            ui.label(RichText::new(format!("ID: {}...", short_id)).size(10.0).color(Color32::DARK_GRAY));
        });
    }

    fn render_connect_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Connect to Remote Computer");
        ui.label("Enter destination IPv4, IPv6 address, and optional port (Default 47823):");
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            let text_edit = ui.add(
                egui::TextEdit::singleline(&mut self.remote_address_input)
                    .hint_text("e.g. 192.168.1.50 or 10.0.0.1:47823")
                    .desired_width(320.0),
            );

            let is_connecting = matches!(self.session_state, SessionState::Resolving | SessionState::Authenticating | SessionState::AwaitingApproval | SessionState::Negotiating);

            if is_connecting {
                ui.spinner();
                ui.label(RichText::new(self.session_state.to_string()).color(Color32::KHAKI));
                if ui.button("Cancel").clicked() {
                    let _ = self.command_sender.send(UiCommand::DisconnectSession);
                }
            } else {
                if ui.button(RichText::new("  Connect  ").strong()).clicked() || (text_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    self.try_initiate_connection();
                }
            }
        });

        if let Some(err) = &self.input_error {
            ui.add_space(4.0);
            ui.label(RichText::new(err).color(Color32::from_rgb(255, 100, 100)).size(12.0));
        }

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(10.0);

        ui.heading("Session Permissions to Request");
        ui.horizontal_wrapped(|ui| {
            let mut screen = self.connect_capabilities.contains(CapabilitySet::SCREEN_VIEW);
            if ui.checkbox(&mut screen, "Screen View").changed() {
                if screen { self.connect_capabilities.insert(CapabilitySet::SCREEN_VIEW); }
                else { self.connect_capabilities.remove(CapabilitySet::SCREEN_VIEW); }
            }

            let mut mouse = self.connect_capabilities.contains(CapabilitySet::MOUSE_CONTROL);
            if ui.checkbox(&mut mouse, "Mouse Control").changed() {
                if mouse { self.connect_capabilities.insert(CapabilitySet::MOUSE_CONTROL); }
                else { self.connect_capabilities.remove(CapabilitySet::MOUSE_CONTROL); }
            }

            let mut kb = self.connect_capabilities.contains(CapabilitySet::KEYBOARD_CONTROL);
            if ui.checkbox(&mut kb, "Keyboard Control").changed() {
                if kb { self.connect_capabilities.insert(CapabilitySet::KEYBOARD_CONTROL); }
                else { self.connect_capabilities.remove(CapabilitySet::KEYBOARD_CONTROL); }
            }

            let mut clip = self.connect_capabilities.contains(CapabilitySet::CLIPBOARD_READ);
            if ui.checkbox(&mut clip, "Clipboard Sync").changed() {
                if clip {
                    self.connect_capabilities.insert(CapabilitySet::CLIPBOARD_READ);
                    self.connect_capabilities.insert(CapabilitySet::CLIPBOARD_WRITE);
                } else {
                    self.connect_capabilities.remove(CapabilitySet::CLIPBOARD_READ);
                    self.connect_capabilities.remove(CapabilitySet::CLIPBOARD_WRITE);
                }
            }

            let mut audio = self.connect_capabilities.contains(CapabilitySet::AUDIO_LISTEN);
            if ui.checkbox(&mut audio, "Audio").changed() {
                if audio { self.connect_capabilities.insert(CapabilitySet::AUDIO_LISTEN); }
                else { self.connect_capabilities.remove(CapabilitySet::AUDIO_LISTEN); }
            }

            let mut ft = self.connect_capabilities.contains(CapabilitySet::FILE_UPLOAD);
            if ui.checkbox(&mut ft, "File Transfer").changed() {
                if ft {
                    self.connect_capabilities.insert(CapabilitySet::FILE_UPLOAD);
                    self.connect_capabilities.insert(CapabilitySet::FILE_DOWNLOAD);
                } else {
                    self.connect_capabilities.remove(CapabilitySet::FILE_UPLOAD);
                    self.connect_capabilities.remove(CapabilitySet::FILE_DOWNLOAD);
                }
            }
        });

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(10.0);

        ui.heading("Quick Saved Devices");
        if self.saved_devices.is_empty() {
            ui.label(RichText::new("No saved devices yet. Connect to an IP to save it.").italics().color(Color32::GRAY));
        } else {
            let mut to_connect = None;
            for device in self.saved_devices.iter().take(4) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&device.friendly_name).strong());
                    ui.label(RichText::new(format!("({})", device.last_known_address)).color(Color32::GRAY));
                    if ui.button("Connect").clicked() {
                        to_connect = Some(device.last_known_address.clone());
                    }
                });
            }
            if let Some(addr) = to_connect {
                self.remote_address_input = addr.to_string();
                self.try_initiate_connection();
            }
        }
    }

    fn try_initiate_connection(&mut self) {
        match self.remote_address_input.parse::<NetworkAddress>() {
            Ok(addr) => {
                self.input_error = None;
                let _ = self.command_sender.send(UiCommand::Connect {
                    destination: addr,
                    capabilities: self.connect_capabilities,
                });
            }
            Err(e) => {
                self.input_error = Some(format!("Invalid address format: {}", e));
            }
        }
    }

    fn render_saved_devices_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Saved Computers & Trusted Devices");
        ui.separator();

        if self.saved_devices.is_empty() {
            ui.label("No saved devices found.");
            return;
        }

        let mut to_delete = None;
        let mut to_connect = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for device in &self.saved_devices {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&device.friendly_name).size(16.0).strong());
                        if device.is_trusted {
                            ui.colored_label(Color32::GREEN, "✔ Trusted");
                        }
                    });

                    ui.label(format!("Address: {}", device.last_known_address));
                    ui.label(format!("Device ID: {}", device.device_id));

                    ui.horizontal(|ui| {
                        if ui.button("Connect").clicked() {
                            to_connect = Some(device.last_known_address.clone());
                        }
                        if ui.button("Remove").clicked() {
                            to_delete = Some(device.device_id.clone());
                        }
                    });
                });
                ui.add_space(8.0);
            }
        });

        if let Some(dev_id) = to_delete {
            let _ = self.command_sender.send(UiCommand::DeleteDevice(dev_id));
        }
        if let Some(addr) = to_connect {
            self.remote_address_input = addr.to_string();
            self.try_initiate_connection();
        }
    }

    fn render_history_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Connection History");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear History").clicked() {
                    let _ = self.command_sender.send(UiCommand::ClearHistory);
                }
            });
        });
        ui.separator();

        if self.session_history.is_empty() {
            ui.label("No recent session history.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("history_grid").striped(true).min_col_width(120.0).show(ui, |ui| {
                ui.label(RichText::new("Destination").strong());
                ui.label(RichText::new("Outcome").strong());
                ui.label(RichText::new("Start Time (Unix)").strong());
                ui.end_row();

                for item in &self.session_history {
                    ui.label(&item.remote_name);
                    ui.label(&item.outcome);
                    ui.label(item.start_time_unix.to_string());
                    ui.end_row();
                }
            });
        });
    }

    fn render_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings & Security Policies");
        ui.separator();
        ui.add_space(8.0);

        ui.label(RichText::new("General Security").strong());
        ui.label("• Interactive approval is mandatory by default.");
        ui.label("• Unattended access is disabled.");
        ui.label("• Ephemeral keys are securely zeroized on disconnect.");

        ui.add_space(16.0);
        ui.label(RichText::new("Network Configuration").strong());
        ui.label(format!("• Local Listening Port: {}", self.local_listen_port));
        ui.label("• Relay: Automatic fallback to STUN/TURN when reachable.");
    }

    fn render_active_session_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("🔴 Remote Desktop Viewport");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Disconnect").color(Color32::from_rgb(255, 100, 100))).clicked() {
                    let _ = self.command_sender.send(UiCommand::DisconnectSession);
                }
            });
        });
        ui.separator();

        if let Some(texture) = &self.remote_texture {
            let available_size = ui.available_size();
            let img_size = texture.size_vec2();
            let aspect = img_size.x / img_size.y;
            let target_width = (available_size.y * aspect).min(available_size.x);
            let target_height = target_width / aspect;
            let fit_size = Vec2::new(target_width, target_height);

            let (rect, response) = ui.allocate_exact_size(fit_size, egui::Sense::click_and_drag());
            let mut mesh = egui::Mesh::with_texture(texture.id());
            mesh.add_rect_with_uv(rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            ui.painter().add(mesh);

            // Forward mouse events to remote host
            if let Some(hover_pos) = response.hover_pos() {
                let norm_x = ((hover_pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                let norm_y = ((hover_pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
                let _ = self.command_sender.send(UiCommand::SendInput(minidesk_protocol::InputMessage::MouseMove { x: norm_x, y: norm_y }));

                if response.clicked_by(egui::PointerButton::Primary) {
                    let _ = self.command_sender.send(UiCommand::SendInput(minidesk_protocol::InputMessage::MouseDown { button: minidesk_protocol::MouseButton::Left }));
                    let _ = self.command_sender.send(UiCommand::SendInput(minidesk_protocol::InputMessage::MouseUp { button: minidesk_protocol::MouseButton::Left }));
                } else if response.clicked_by(egui::PointerButton::Secondary) {
                    let _ = self.command_sender.send(UiCommand::SendInput(minidesk_protocol::InputMessage::MouseDown { button: minidesk_protocol::MouseButton::Right }));
                    let _ = self.command_sender.send(UiCommand::SendInput(minidesk_protocol::InputMessage::MouseUp { button: minidesk_protocol::MouseButton::Right }));
                }
            }
        } else {
            let available_size = ui.available_size();
            let (rect, _response) = ui.allocate_exact_size(available_size, egui::Sense::click_and_drag());

            ui.painter().rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(10, 12, 16));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Connecting Video Pipeline...\nWaiting for first frame from host",
                egui::FontId::proportional(16.0),
                Color32::from_rgb(120, 140, 170),
            );
        }
    }

    fn render_incoming_modal(&mut self, ctx: &egui::Context, incoming: &IncomingSessionModal) {
        egui::Window::new("Incoming Connection Request")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.heading("Allow Remote Access?");
                ui.add_space(8.0);
                ui.label(format!("Computer: {}", incoming.peer_name));
                ui.label(format!("Address: {}", incoming.peer_address));
                if let Some(dev_id) = &incoming.peer_device_id {
                    ui.label(format!("Device ID: {}", dev_id));
                }

                ui.add_space(12.0);
                ui.label(RichText::new("Requested Capabilities:").strong());
                ui.label(format!("{:?}", incoming.requested_capabilities));

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Accept").color(Color32::GREEN)).clicked() {
                        let _ = self.command_sender.send(UiCommand::ApproveIncoming {
                            session_id: incoming.session_id,
                            granted_capabilities: incoming.requested_capabilities,
                        });
                        self.pending_incoming = None;
                    }

                    if ui.button(RichText::new("Reject").color(Color32::RED)).clicked() {
                        let _ = self.command_sender.send(UiCommand::RejectIncoming {
                            session_id: incoming.session_id,
                            reason: "Host rejected incoming request".into(),
                        });
                        self.pending_incoming = None;
                    }
                });
            });
    }
}
