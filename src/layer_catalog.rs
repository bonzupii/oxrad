// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! The map layers in display order, with the visibility and opacity the map
//! and the layer list both read.

/// A layer the Map workspace can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerId {
    Radar,
    GlobalComposite,
    Warnings,
}

impl LayerId {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Radar => "Radar sweeps",
            Self::GlobalComposite => "Global radar composite",
            Self::Warnings => "Active warnings",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Radar => "The selected radar and the sweeps around the view centre.",
            Self::GlobalComposite => "RainViewer reflectivity composite with two hours of history.",
            Self::Warnings => {
                "Active National Weather Service warning polygons for the United States \
                 and territories."
            }
        }
    }

    pub const fn group(self) -> LayerGroup {
        match self {
            Self::Radar => LayerGroup::Radar,
            Self::GlobalComposite => LayerGroup::Imagery,
            Self::Warnings => LayerGroup::Alerts,
        }
    }
}

/// The heading a layer sits under in the layer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerGroup {
    Radar,
    Imagery,
    Alerts,
}

impl LayerGroup {
    pub const ALL: [Self; 3] = [Self::Radar, Self::Imagery, Self::Alerts];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Radar => "Radar",
            Self::Imagery => "Imagery",
            Self::Alerts => "Alerts",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    id: LayerId,
    visible: bool,
    opacity: f32,
}

/// Visibility and opacity for each map layer, in the order the map stacks them.
pub struct LayerCatalog {
    slots: Vec<Slot>,
}

impl LayerCatalog {
    pub fn new() -> Self {
        Self {
            slots: vec![
                Slot {
                    id: LayerId::Radar,
                    visible: true,
                    opacity: 1.0,
                },
                Slot {
                    id: LayerId::GlobalComposite,
                    visible: true,
                    opacity: 1.0,
                },
                Slot {
                    id: LayerId::Warnings,
                    visible: true,
                    opacity: 1.0,
                },
            ],
        }
    }

    fn slot(&self, id: LayerId) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id == id)
    }

    fn slot_mut(&mut self, id: LayerId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|slot| slot.id == id)
    }

    pub fn visible(&self, id: LayerId) -> bool {
        self.slot(id).is_some_and(|slot| slot.visible)
    }

    pub fn set_visible(&mut self, id: LayerId, visible: bool) {
        if let Some(slot) = self.slot_mut(id) {
            slot.visible = visible;
        }
    }

    pub fn opacity(&self, id: LayerId) -> f32 {
        self.slot(id).map_or(1.0, |slot| slot.opacity)
    }

    /// Clamps the value into the visible range; a non-finite value is ignored.
    pub fn set_opacity(&mut self, id: LayerId, opacity: f32) {
        if !opacity.is_finite() {
            return;
        }
        if let Some(slot) = self.slot_mut(id) {
            slot.opacity = opacity.clamp(0.0, 1.0);
        }
    }

    /// The alpha a painter uses for this layer.
    pub fn alpha(&self, id: LayerId) -> u8 {
        let scaled = (self.opacity(id) * 255.0).round().clamp(0.0, 255.0);
        scaled as u8
    }

    /// The layers under one group heading, in stacking order.
    pub fn in_group(&self, group: LayerGroup) -> Vec<LayerId> {
        self.slots
            .iter()
            .filter(|slot| slot.id.group() == group)
            .map(|slot| slot.id)
            .collect()
    }

    /// The layers whose label contains the query, in stacking order.
    pub fn search(&self, query: &str) -> Vec<LayerId> {
        let needle = query.trim().to_lowercase();
        self.slots
            .iter()
            .filter(|slot| needle.is_empty() || slot.id.label().to_lowercase().contains(&needle))
            .map(|slot| slot.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn every_layer_starts_shown_at_full_opacity() -> Result<(), TestError> {
        let catalog = LayerCatalog::new();
        for id in [LayerId::Radar, LayerId::GlobalComposite, LayerId::Warnings] {
            assert!(catalog.visible(id), "{} must start shown", id.label());
            assert!((catalog.opacity(id) - 1.0).abs() < f32::EPSILON);
        }
        assert_eq!(catalog.alpha(LayerId::Warnings), 255);
        Ok(())
    }

    #[test]
    fn hiding_a_layer_leaves_its_neighbour_alone() -> Result<(), TestError> {
        let mut catalog = LayerCatalog::new();
        catalog.set_visible(LayerId::Warnings, false);
        assert!(!catalog.visible(LayerId::Warnings));
        assert!(catalog.visible(LayerId::Radar));
        assert!(catalog.visible(LayerId::GlobalComposite));
        catalog.set_visible(LayerId::Warnings, true);
        assert!(catalog.visible(LayerId::Warnings));
        Ok(())
    }

    #[test]
    fn opacity_is_clamped_and_a_non_finite_one_is_ignored() -> Result<(), TestError> {
        let mut catalog = LayerCatalog::new();
        catalog.set_opacity(LayerId::Warnings, 0.4);
        assert!((catalog.opacity(LayerId::Warnings) - 0.4).abs() < 1e-6);
        assert_eq!(catalog.alpha(LayerId::Warnings), 102);
        catalog.set_opacity(LayerId::Warnings, 7.0);
        assert!((catalog.opacity(LayerId::Warnings) - 1.0).abs() < f32::EPSILON);
        catalog.set_opacity(LayerId::Warnings, -2.0);
        assert!(catalog.opacity(LayerId::Warnings).abs() < f32::EPSILON);
        catalog.set_opacity(LayerId::Warnings, f32::NAN);
        assert!(catalog.opacity(LayerId::Warnings).abs() < f32::EPSILON);
        Ok(())
    }
    #[test]
    fn groups_keep_their_stacking_order() -> Result<(), TestError> {
        let catalog = LayerCatalog::new();
        assert_eq!(
            catalog.in_group(LayerGroup::Imagery),
            vec![LayerId::GlobalComposite]
        );
        assert_eq!(
            catalog.in_group(LayerGroup::Alerts),
            vec![LayerId::Warnings]
        );
        assert_eq!(catalog.search("").len(), 3);
        Ok(())
    }

    #[test]
    fn search_matches_a_label_without_regard_to_case() -> Result<(), TestError> {
        let catalog = LayerCatalog::new();
        assert_eq!(catalog.search("WARN").len(), 1);
        assert_eq!(catalog.search("  ").len(), 3);
        assert!(catalog.search("solar wind").is_empty());
        assert_eq!(catalog.search("composite"), vec![LayerId::GlobalComposite]);
        Ok(())
    }
}
