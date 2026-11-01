// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
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
//! Radar product and sweep selection for a displayed volume.

use eframe::egui::{self, RichText};

use crate::product::RadarProduct;
use crate::radar::RadarVolume;
use crate::render::Sampling;
use crate::theme;

pub enum Selection {
    Product(RadarProduct),
    Elevation(u8),
    Sampling(Sampling),
}

/// One selectable product, or the reason it cannot be chosen.
///
/// A measured moment is missing from a volume; a computed product is missing
/// when the volume has too few elevations to compute it, which is a different
/// sentence for the user.
fn product_button(
    ui: &mut egui::Ui,
    volume: &RadarVolume,
    product: RadarProduct,
    selected: RadarProduct,
    measured: bool,
) -> Option<RadarProduct> {
    let available = volume.supports_product(product);
    let response = ui.add_enabled(
        available,
        egui::Button::selectable(
            product == selected,
            RichText::new(format!("{} · {}", product.label(), product.unit()))
                .size(theme::TYPE_BODY),
        ),
    );
    let response = if available {
        response.on_hover_text(product.description())
    } else if measured {
        response.on_disabled_hover_text("This volume does not contain this product.")
    } else {
        response.on_disabled_hover_text(
            "This volume needs reflectivity on two or more elevations to compute this product.",
        )
    };
    if response.clicked() && product != selected {
        return Some(product);
    }
    None
}

pub fn show(
    ui: &mut egui::Ui,
    capabilities: &[RadarProduct],
    volume: &RadarVolume,
    selected: RadarProduct,
    sampling: Sampling,
    enabled: bool,
) -> Option<Selection> {
    let mut selection = None;
    ui.add_space(theme::SPACE_M);
    ui.label(
        RichText::new("Radar product")
            .size(theme::TYPE_TITLE)
            .strong(),
    );
    ui.add_space(theme::SPACE_XS);
    ui.add_enabled_ui(enabled, |ui| {
        ui.spacing_mut().interact_size.y = 32.0;
        egui::ComboBox::from_id_salt("radar-product")
            .selected_text(
                RichText::new(format!("{} · {}", selected.label(), selected.unit()))
                    .size(theme::TYPE_BODY),
            )
            .width(ui.available_width() - theme::SPACE_L)
            // The suite plus the computed products is taller than the default
            // popup, and a product the user cannot see is a product they do not
            // have.
            .height(460.0)
            .show_ui(ui, |ui| {
                for product in capabilities {
                    if let Some(chosen) = product_button(ui, volume, *product, selected, true) {
                        selection = Some(Selection::Product(chosen));
                    }
                }
                ui.add_space(theme::SPACE_S);
                ui.separator();
                ui.label(
                    RichText::new("Computed from this volume")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
                for product in RadarProduct::DERIVED {
                    if let Some(chosen) = product_button(ui, volume, product, selected, false) {
                        selection = Some(Selection::Product(chosen));
                    }
                }
            });
        let options = volume.tilt_options_for(selected);
        let current = options.iter().find(|(_, _, selected)| *selected);
        if !options.is_empty() {
            ui.add_space(theme::SPACE_S);
            ui.label(RichText::new("Elevation sweep").size(theme::TYPE_BODY));
            let selected_sweep = current.map_or_else(
                || String::from("Select an available sweep"),
                |(number, angle, _)| format!("Sweep {number} · {angle:.2}°"),
            );
            egui::ComboBox::from_id_salt("radar-elevation")
                .selected_text(RichText::new(selected_sweep).size(theme::TYPE_BODY))
                .width(ui.available_width() - theme::SPACE_L)
                .show_ui(ui, |ui| {
                    for (number, angle, active) in &options {
                        if ui
                            .selectable_label(*active, format!("Sweep {number} · {angle:.2}°"))
                            .clicked()
                            && !active
                        {
                            selection = Some(Selection::Elevation(*number));
                        }
                    }
                });
        } else {
            ui.label("This volume does not contain the selected product.");
        }
    });
    ui.add_space(theme::SPACE_S);
    ui.label(RichText::new("Rendering").size(theme::TYPE_BODY));
    for mode in Sampling::ALL {
        if ui
            .selectable_label(sampling == mode, mode.label())
            .on_hover_text(mode.description())
            .clicked()
            && sampling != mode
        {
            selection = Some(Selection::Sampling(mode));
        }
    }
    selection
}
