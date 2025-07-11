use crate::chrome::{
    ChromeTab, export_cookies_for_tab, fetch_tabs, get_cookies_for_tab,
    import_and_open_with_cookies,
};
use crate::network::{GrantMessage, RevokeCookie, RevokeMessage};
use eframe::{App, CreationContext};
use egui::{
    Align, CentralPanel, Color32, Direction, FontId, Frame, Label, Layout, Margin, RichText,
    Rounding, ScrollArea, Sense, TopBottomPanel, Vec2,
};
use rfd::FileDialog;
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio::runtime::Handle;
use tokio::sync::broadcast::Sender as BroadcastSender;

pub struct ChromeTabApp {
    tabs: Arc<Mutex<Vec<ChromeTab>>>,
    cookie_import: CookieImportState,
    grant_tx: BroadcastSender<GrantMessage>,
    revoke_tx: BroadcastSender<RevokeMessage>,
    listen_addr: String,
    listening: bool,
    rt_handle: Handle,
    remote_to_local: Arc<Mutex<HashMap<String, String>>>,
}

impl ChromeTabApp {
    pub fn new(
        cc: &CreationContext<'_>,
        grant_tx: BroadcastSender<GrantMessage>,
        revoke_tx: BroadcastSender<RevokeMessage>,
        rt_handle: Handle,
    ) -> Self {
        let tabs = Arc::new(Mutex::new(Vec::new()));
        let tabs_clone = Arc::clone(&tabs);
        thread::spawn(move || {
            loop {
                if let Ok(new_tabs) = fetch_tabs() {
                    *tabs_clone.lock().unwrap() = new_tabs;
                }
                thread::sleep(Duration::from_secs(1));
            }
        });

        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.dark_mode = true;
        cc.egui_ctx.set_style(style);

        Self {
            tabs,
            cookie_import: CookieImportState::default(),
            grant_tx,
            revoke_tx,
            listen_addr: "0.0.0.0:9234".into(),
            listening: false,
            rt_handle,
            remote_to_local: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl App for ChromeTabApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        TopBottomPanel::top("titlebar")
            .exact_height(32.0)
            .frame(
                Frame::none()
                    .fill(Color32::from_gray(20))
                    .inner_margin(Margin::same(4))
                    .outer_margin(Margin {
                        left: 0,
                        right: 0,
                        top: 0,
                        bottom: 2,
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(8.0);
                    ui.heading(RichText::new("ShareKaro").size(16.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("✖").clicked() {
                            std::process::exit(0);
                        }
                        if ui.small_button("⟳").clicked() {
                            if let Ok(new_tabs) = fetch_tabs() {
                                *self.tabs.lock().unwrap() = new_tabs;
                            }
                        }
                    });
                });
            });

        CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Peer to listen on:");
                ui.text_edit_singleline(&mut self.listen_addr);
                let button_label = if self.listening { "Listening…" } else { "Listen" };
                if ui.add_enabled(!self.listening, egui::Button::new(button_label)).clicked() {
                    if let Ok(addr) = self.listen_addr.parse::<SocketAddr>() {
                        let remote_map = Arc::clone(&self.remote_to_local);
                        thread::spawn(move || {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .expect("runtime creation failed");
                            rt.block_on(async move {
                                crate::network::connect_client(addr, remote_map).await;
                            });
                        });
                        self.listening = true;
                    }
                }
            });

            ui.separator();
            let tabs = self.tabs.lock().unwrap();
            if tabs.is_empty() {
                ui.add_space(40.0);
                ui.add(
                    Label::new(
                        RichText::new(
                            "No tabs found.\nEnsure Chrome is running with --remote-debugging-port=9222."
                        )
                        .italics()
                        .color(Color32::from_rgb(200, 100, 100)),
                    )
                    .wrap(),
                );
            } else {
                let card_width = 260.0;
                let cols = (ui.available_width() / (card_width + 16.0)).floor().max(1.0) as usize;
                ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(16.0);
                    ui.columns(cols, |columns| {
                        for (i, tab) in tabs.iter().enumerate() {
                            let col_ui = &mut columns[i % cols];
                            let (rect, resp) = col_ui.allocate_exact_size(
                                Vec2::new(card_width, 80.0),
                                Sense::click(),
                            );
                            let bg = if resp.hovered() { Color32::from_gray(50) } else { Color32::from_gray(40) };
                            col_ui.painter().rect_filled(rect, Rounding::same(8), bg);
                            col_ui.allocate_ui_at_rect(rect.shrink(8.0), |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{}.", i + 1)).strong());
                                    ui.label(
                                        RichText::new(&tab.title)
                                            .font(FontId::proportional(16.0))
                                            .strong(),
                                    );
                                    if ui.small_button("Share").clicked() {
                                        let cookies = get_cookies_for_tab(tab).unwrap_or_default();
                                        let grant = GrantMessage {
                                            tab_id: tab.id.clone(),
                                            url: tab.url.clone(),
                                            cookies,
                                        };
                                        let _ = self.grant_tx.send(grant);
                                    }
                                    if ui.small_button("Revoke").clicked() {
                                        let cookies: Vec<RevokeCookie> =
                                            get_cookies_for_tab(tab)
                                                .unwrap_or_default()
                                                .into_iter()
                                                .map(|c| RevokeCookie { name: c.name, domain: c.domain, path: c.path })
                                                .collect();
                                        let revoke = RevokeMessage { tab_id: tab.id.clone(), cookies };
                                        let _ = self.revoke_tx.send(revoke);
                                    }
                                });
                                ui.add_space(2.0);
                                ui.label(RichText::new(clip(&tab.url, 45)).monospace());
                            });
                            if resp.clicked() {
                                match export_cookies_for_tab(tab) {
                                    Ok(path) => self.cookie_import.last_status = Some(format!("Cookies exported to {}", path)),
                                    Err(e) => self.cookie_import.last_status = Some(format!("Failed to export cookies: {}", e)),
                                }
                            }
                        }
                    });
                });
            }

            ui.add_space(18.0);
            ui.separator();
            ui.heading("Import Cookies and Open Tab");

            let import = &mut self.cookie_import;
            ui.horizontal(|ui| {
                if ui.button("Choose JSON File").clicked() {
                    import.show_dialog = true;
                }
                if let Some(path) = &import.last_path {
                    ui.label(path.display().to_string());
                }
            });

            if import.show_dialog {
                if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    import.last_path = Some(path.clone());
                    import.last_status = Some(format!("Loaded {}", path.display()));
                }
                import.show_dialog = false;
            }

            ui.horizontal(|ui| {
                ui.label("URL to open:");
                ui.text_edit_singleline(&mut import.url_to_open);
                if ui.button("Open").clicked() {
                    if let (Some(path), true) = (&import.last_path, !import.url_to_open.trim().is_empty()) {
                        match import_and_open_with_cookies(path, &import.url_to_open) {
                            Ok(_) => import.last_status = Some("Tab opened successfully".to_string()),
                            Err(e) => import.last_status = Some(format!("Error: {}", e)),
                        }
                    } else {
                        import.last_status = Some("Select a file and enter a URL to proceed".to_string());
                    }
                }
            });

            if let Some(msg) = &import.last_status {
                ui.label(msg);
            }
        });

        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

pub struct CookieImportState {
    pub url_to_open: String,
    pub last_status: Option<String>,
    pub last_path: Option<PathBuf>,
    pub show_dialog: bool,
}

impl Default for CookieImportState {
    fn default() -> Self {
        Self {
            url_to_open: String::new(),
            last_status: None,
            last_path: None,
            show_dialog: false,
        }
    }
}
