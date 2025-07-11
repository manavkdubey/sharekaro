use std::error::Error;

use clap::Parser;
use eframe::{App, CreationContext};
use eframe::{NativeOptions, run_native};
use egui::Vec2;
use egui::ViewportBuilder;
use sharekaro::chrome::{launch_chrome_with_cdp, listen_tabs_ws};
use sharekaro::gui::ChromeTabApp;
use sharekaro::network::spawn_server;
use tokio::runtime::{Handle, Runtime};

/// Your CLI args
#[derive(Parser)]
struct Args {
    /// Use real Chrome user profile instead of temp
    #[arg(long)]
    profile: Option<String>,
}
fn main() -> Result<(), eframe::Error> {
    let args = Args::parse();
    let rt = Runtime::new().expect("Failed to create Tokio runtime");
    let handle: Handle = rt.handle().clone();

    // launch Chrome with CDP
    let (_child, _temp_profile) = launch_chrome_with_cdp(args.profile.clone());

    // start the shared‐server once
    let (grant_tx, revoke_tx) = rt.block_on(spawn_server("0.0.0.0:9234".parse().unwrap()));

    let app_factory =
        move |cc: &CreationContext<'_>| -> Result<Box<dyn App>, Box<dyn Error + Send + Sync>> {
            Ok(Box::new(ChromeTabApp::new(
                cc,
                grant_tx.clone(),
                revoke_tx.clone(),
                handle.clone(),
            )))
        };

    run_native("ShareKaro", NativeOptions::default(), Box::new(app_factory))?;
    Ok(())
}
