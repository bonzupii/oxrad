// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
mod app;
mod colormap;
mod error;
mod projection;
mod radar;

use eframe::egui;

use app::OxradApp;
use radar::start_fetch_thread;

const DEFAULT_SITE: &str = "KPAH";

fn main() -> eframe::Result<()> {
    let site = match std::env::args().nth(1) {
        Some(arg) => arg,
        None => String::from(DEFAULT_SITE),
    };
    let (rx, fetch_handle) = start_fetch_thread(site);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "oxrad",
        options,
        Box::new(|_cc| Ok(Box::new(OxradApp::new(rx, fetch_handle)))),
    )
}
