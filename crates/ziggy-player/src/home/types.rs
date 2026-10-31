//! Home Assistant entities as Ziggy shows them, and the compressed states Home Assistant sends.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Light,
    Switch,
    Fan,
    Scene,
    Script,
}

impl Domain {
    pub fn of(entity_id: &str) -> Option<Domain> {
        match entity_id.split_once('.')?.0 {
            "light" => Some(Domain::Light),
            "switch" => Some(Domain::Switch),
            "fan" => Some(Domain::Fan),
            "scene" => Some(Domain::Scene),
            "script" => Some(Domain::Script),
            _ => None,
        }
    }

    /// Lights, switches and fans are tiles; scenes and scripts are buttons along the bottom.
    pub fn is_tile(self) -> bool {
        matches!(self, Domain::Light | Domain::Switch | Domain::Fan)
    }

    pub fn name(self) -> &'static str {
        match self {
            Domain::Light => "light",
            Domain::Switch => "switch",
            Domain::Fan => "fan",
            Domain::Scene => "scene",
            Domain::Script => "script",
        }
    }
}

/// One control on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub entity_id: String,
    pub domain: Domain,
    /// Shown instead of Home Assistant's name.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub name: String,
    pub tiles: Vec<Control>,
    pub scenes: Vec<Control>,
}

/// What Ziggy needs of an entity's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityState {
    /// "on", "off", "unavailable", …
    pub state: String,
    /// Home Assistant's friendly name.
    pub name: Option<String>,
    /// 0-255, while a light is on.
    pub brightness: Option<u8>,
}

impl EntityState {
    pub fn is_on(&self) -> bool {
        self.state == "on"
    }

    pub fn available(&self) -> bool {
        !matches!(self.state.as_str(), "unavailable" | "unknown")
    }

    /// Brightness as 1-100.
    pub fn percent(&self) -> Option<u8> {
        self.brightness
            .map(|b| ((u32::from(b) * 100 + 127) / 255).clamp(1, 100) as u8)
    }
}

/// Home Assistant's 0-255 brightness for a percentage.
pub fn brightness_of(percent: u8) -> u8 {
    ((u32::from(percent.min(100)) * 255 + 50) / 100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_come_from_the_entity_id() {
        assert_eq!(Domain::of("light.desk"), Some(Domain::Light));
        assert_eq!(Domain::of("switch.fairy_lights"), Some(Domain::Switch));
        assert_eq!(Domain::of("fan.micah"), Some(Domain::Fan));
        assert_eq!(Domain::of("scene.bedtime"), Some(Domain::Scene));
        assert_eq!(Domain::of("script.reading"), Some(Domain::Script));
        assert_eq!(Domain::of("climate.hall"), None);
        assert_eq!(Domain::of("light"), None);
        assert!(Domain::Fan.is_tile() && !Domain::Scene.is_tile());
    }

    #[test]
    fn brightness_converts_both_ways() {
        let on = |b| EntityState { state: "on".into(), name: None, brightness: Some(b) };
        assert_eq!(on(255).percent(), Some(100));
        assert_eq!(on(178).percent(), Some(70));
        assert_eq!(on(1).percent(), Some(1), "a lit light never reads 0%");
        assert_eq!(brightness_of(70), 179);
        assert_eq!(brightness_of(100), 255);
    }
}
