use std::error::Error;

use clap::Parser;
use eframe::{App, CreationContext};
use eframe::{NativeOptions, run_native};
use egui::Vec2;
use egui::ViewportBuilder;
use sharekaro::chrome::{launch_chrome_with_cdp, listen_tabs_ws};
use sharekaro::gui::ChromeTabApp;

/// Your CLI args
#[derive(Parser)]
struct Args {
    /// Use real Chrome user profile instead of temp
    #[arg(long)]
    profile: Option<String>,
}
fn main() -> Result<(), eframe::Error> {
    let args = Args::parse();
    let rt = tokio::runtime::Runtime::new().unwrap();
    // start Chrome + CDP
    let (_child, _temp_profile) = launch_chrome_with_cdp(args.profile);
    // spawn your network server
    let (grant_tx, revoke_tx) = rt.block_on(sharekaro::network::spawn_server(
        "0.0.0.0:9234".parse().unwrap(),
    ));
    let app_factory =
        move |cc: &CreationContext<'_>| -> Result<Box<dyn App>, Box<dyn Error + Send + Sync>> {
            Ok(Box::new(ChromeTabApp::new(
                cc,
                grant_tx.clone(),
                revoke_tx.clone(),
            )))
        };

    // 4) Call run_native and propagate its boxed error
    run_native("ShareKaro", NativeOptions::default(), Box::new(app_factory))?;
    Ok(())
}
