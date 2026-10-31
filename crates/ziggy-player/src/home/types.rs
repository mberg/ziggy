//! Home Assistant entities as Ziggy shows them, and the compressed states Home Assistant sends.

use std::collections::HashMap;

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Light,
    Switch,
    Fan,
    Scene,
    Script,
}

/// True for an ID in Home Assistant's form: lowercase letters, digits and `_`, a dot between
/// the domain and the name, as in `light.desk`.
pub fn is_entity_id(entity_id: &str) -> bool {
    let part = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    entity_id
        .split_once('.')
        .is_some_and(|(domain, object_id)| part(domain) && part(object_id))
}

impl Domain {
    /// The domain of a well-formed entity ID Ziggy can show; None for anything else.
    pub fn of(entity_id: &str) -> Option<Domain> {
        if !is_entity_id(entity_id) {
            return None;
        }
        let (domain, _) = entity_id.split_once('.')?;
        match domain {
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
#[derive(Debug, Clone, PartialEq)]
pub struct EntityState {
    /// "on", "off", "unavailable", …
    pub state: String,
    /// Home Assistant's friendly name.
    pub name: Option<String>,
    /// 0-255, while a light is on.
    pub brightness: Option<u8>,
    /// How many lights a light group has; 0 for a single entity.
    pub members: u16,
}

impl EntityState {
    pub fn is_on(&self) -> bool {
        self.state == "on"
    }

    /// "unknown" still counts: a light reports it briefly after Home Assistant restarts, and
    /// scenes report it until they are run.
    pub fn available(&self) -> bool {
        self.state != "unavailable"
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

/// Subscribed entities in Home Assistant's own form, so its change events can be applied.
#[derive(Debug, Default)]
pub struct States {
    raw: HashMap<String, (String, Map<String, Value>)>,
}

impl States {
    /// Applies one `subscribe_entities` event. Gives back the entities that changed, with their
    /// new states, and the ones removed.
    pub fn apply(&mut self, event: &Value) -> (Vec<(String, EntityState)>, Vec<String>) {
        let mut changed: Vec<String> = Vec::new();
        if let Some(added) = event.get("a").and_then(Value::as_object) {
            for (id, v) in added {
                let state = v.get("s").and_then(Value::as_str).unwrap_or("unknown");
                let attrs = v
                    .get("a")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                self.raw.insert(id.clone(), (state.to_string(), attrs));
                if !changed.contains(id) {
                    changed.push(id.clone());
                }
            }
        }
        if let Some(diffs) = event.get("c").and_then(Value::as_object) {
            for (id, diff) in diffs {
                let Some((state, attrs)) = self.raw.get_mut(id) else {
                    continue;
                };
                if let Some(plus) = diff.get("+") {
                    if let Some(s) = plus.get("s").and_then(Value::as_str) {
                        *state = s.to_string();
                    }
                    if let Some(more) = plus.get("a").and_then(Value::as_object) {
                        for (k, v) in more {
                            attrs.insert(k.clone(), v.clone());
                        }
                    }
                }
                let gone = diff
                    .get("-")
                    .and_then(|m| m.get("a"))
                    .and_then(Value::as_array);
                for key in gone.into_iter().flatten().filter_map(Value::as_str) {
                    attrs.remove(key);
                }
                if !changed.contains(id) {
                    changed.push(id.clone());
                }
            }
        }
        let removed: Vec<String> = event
            .get("r")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| self.raw.remove(*id).is_some())
            .map(str::to_string)
            .collect();
        let changed = changed
            .into_iter()
            .filter_map(|id| self.get(&id).map(|s| (id, s)))
            .collect();
        (changed, removed)
    }

    pub fn get(&self, id: &str) -> Option<EntityState> {
        let (state, attrs) = self.raw.get(id)?;
        Some(EntityState {
            state: state.clone(),
            name: attrs
                .get("friendly_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            brightness: attrs
                .get("brightness")
                .and_then(Value::as_u64)
                .map(|b| b.min(255) as u8),
            // Groups list their members in `entity_id`.
            members: attrs
                .get("entity_id")
                .and_then(Value::as_array)
                .map_or(0, |m| m.len().min(usize::from(u16::MAX)) as u16),
        })
    }
}

/// Each entity's area name: its own area, else its device's. Takes the results of
/// `config/entity_registry/list_for_display`, `config/device_registry/list` and
/// `config/area_registry/list`; any of them may be `Null` when Ziggy wasn't allowed to read it.
pub fn area_names(entities: &Value, devices: &Value, areas: &Value) -> HashMap<String, String> {
    fn list(v: &Value) -> &[Value] {
        v.as_array().map(Vec::as_slice).unwrap_or(&[])
    }
    let area_name: HashMap<String, String> = list(areas)
        .iter()
        .filter_map(|a| {
            Some((
                a["area_id"].as_str()?.to_string(),
                a["name"].as_str()?.to_string(),
            ))
        })
        .collect();
    let device_area: HashMap<String, String> = list(devices)
        .iter()
        .filter_map(|d| {
            Some((
                d["id"].as_str()?.to_string(),
                d["area_id"].as_str()?.to_string(),
            ))
        })
        .collect();
    list(&entities["entities"])
        .iter()
        .filter_map(|e| {
            let id = e["ei"].as_str()?;
            let area = match e["ai"].as_str() {
                Some(area) => area.to_string(),
                None => device_area.get(e["di"].as_str()?)?.clone(),
            };
            Some((id.to_string(), area_name.get(&area)?.clone()))
        })
        .collect()
}

/// The configured pages, split into tiles and scene buttons. Config checks already refused any
/// other domain.
pub fn pages(config: &crate::config::HomeAssistantConfig) -> Vec<Page> {
    config
        .pages
        .iter()
        .map(|page| {
            let controls: Vec<Control> = page
                .controls
                .iter()
                .filter_map(|c| {
                    Some(Control {
                        entity_id: c.entity().to_string(),
                        domain: Domain::of(c.entity())?,
                        name: c.name().map(str::to_string),
                    })
                })
                .collect();
            let (tiles, scenes) = controls.into_iter().partition(|c| c.domain.is_tile());
            Page {
                name: page.name.clone(),
                tiles,
                scenes,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn domains_come_from_the_entity_id() {
        assert_eq!(Domain::of("light.desk"), Some(Domain::Light));
        assert_eq!(Domain::of("switch.fairy_lights"), Some(Domain::Switch));
        assert_eq!(Domain::of("fan.bedroom"), Some(Domain::Fan));
        assert_eq!(Domain::of("scene.bedtime"), Some(Domain::Scene));
        assert_eq!(Domain::of("script.reading"), Some(Domain::Script));
        assert_eq!(Domain::of("climate.hall"), None);
        assert_eq!(Domain::of("light"), None);
        assert_eq!(Domain::of("light."), None);
        assert_eq!(Domain::of("light.desk-lamp"), None);
        assert_eq!(Domain::of("light.desk lamp"), None);
        assert_eq!(Domain::of("light.Desk"), None);
        assert_eq!(Domain::of("Light.desk"), None);
        assert_eq!(Domain::of("light.desk.lamp"), None);
        assert!(is_entity_id("climate.hall_2") && !is_entity_id(".desk"));
        assert!(Domain::Fan.is_tile() && !Domain::Scene.is_tile());
    }

    #[test]
    fn brightness_converts_both_ways() {
        let on = |b| EntityState {
            state: "on".into(),
            name: None,
            brightness: Some(b),
            members: 0,
        };
        assert_eq!(on(255).percent(), Some(100));
        assert_eq!(on(178).percent(), Some(70));
        assert_eq!(on(1).percent(), Some(1), "a lit light never reads 0%");
        assert_eq!(brightness_of(70), 179);
        assert_eq!(brightness_of(100), 255);
    }

    #[test]
    fn first_event_adds_and_changes_merge() {
        let mut states = States::default();
        let (changed, removed) = states.apply(&json!({"a": {
            "light.desk": {"s": "on", "a": {"friendly_name": "Desk lamp", "brightness": 178}},
            "switch.fan": {"s": "off", "a": {"friendly_name": "Fan"}}
        }}));
        assert_eq!(changed.len(), 2);
        assert!(removed.is_empty());
        let desk = changed.iter().find(|(id, _)| id == "light.desk").unwrap();
        assert_eq!(
            desk.1,
            EntityState {
                state: "on".into(),
                name: Some("Desk lamp".into()),
                brightness: Some(178),
                members: 0,
            }
        );

        // Turning off: the state changes and brightness is taken away.
        let (changed, _) = states.apply(&json!({"c": {
            "light.desk": {"+": {"s": "off"}, "-": {"a": ["brightness"]}}
        }}));
        assert_eq!(
            changed,
            vec![(
                "light.desk".to_string(),
                EntityState {
                    state: "off".into(),
                    name: Some("Desk lamp".into()),
                    brightness: None,
                    members: 0,
                }
            )]
        );

        // Dimming changes only an attribute.
        let (changed, _) = states.apply(&json!({"c": {
            "light.desk": {"+": {"s": "on", "a": {"brightness": 26}}}
        }}));
        assert_eq!(changed[0].1.percent(), Some(10));
    }

    #[test]
    fn removed_entities_are_reported_and_unknown_changes_ignored() {
        let mut states = States::default();
        states.apply(&json!({"a": {"light.desk": {"s": "on", "a": {}}}}));
        let (changed, removed) =
            states.apply(&json!({"c": {"light.other": {"+": {"s": "on"}}}, "r": ["light.desk"]}));
        assert!(changed.is_empty());
        assert_eq!(removed, vec!["light.desk".to_string()]);
        assert_eq!(states.get("light.desk"), None);
    }

    #[test]
    fn areas_come_from_the_entity_or_else_its_device() {
        let entities = json!({"entities": [
            {"ei": "light.desk", "ai": "bedroom", "di": "d1"},
            {"ei": "light.porch", "di": "d2"},
            {"ei": "light.loose"}
        ]});
        let devices =
            json!([{"id": "d1", "area_id": "kitchen"}, {"id": "d2", "area_id": "outside"}]);
        let areas = json!([
            {"area_id": "bedroom", "name": "Bedroom"},
            {"area_id": "outside", "name": "Outside"},
            {"area_id": "kitchen", "name": "Kitchen"}
        ]);
        let names = area_names(&entities, &devices, &areas);
        assert_eq!(names.get("light.desk").map(String::as_str), Some("Bedroom"));
        assert_eq!(
            names.get("light.porch").map(String::as_str),
            Some("Outside")
        );
        assert_eq!(names.get("light.loose"), None);
        // A registry Ziggy wasn't allowed to read gives no areas, not an error.
        assert!(area_names(&Value::Null, &devices, &areas).is_empty());
    }

    #[test]
    fn real_home_assistant_shapes() {
        let mut states = States::default();
        let (changed, _) = states.apply(&json!({"a": {"light.master_bedroom": {
            "s": "on",
            "a": {
                "brightness": 181,
                "friendly_name": "Master Bedroom",
                "color_mode": "brightness",
                "supported_color_modes": ["brightness"]
            },
            "c": "01K0CONTEXT",
            "lc": 1793042900.2,
            "lu": 1793325649.4
        }}}));
        let (id, bedroom) = &changed[0];
        assert_eq!(id, "light.master_bedroom");
        assert_eq!(bedroom.name.as_deref(), Some("Master Bedroom"));
        assert_eq!(bedroom.percent(), Some(71));

        let entities = json!({"entities": [
            {"ei": "light.bay_window", "pl": "hue", "lb": [], "di": "d1", "hn": true}
        ]});
        let devices = json!([{"id": "d1", "area_id": "bay_window"}]);
        let areas = json!([{"area_id": "bay_window", "name": "Bay window"}]);
        let names = area_names(&entities, &devices, &areas);
        assert_eq!(
            names.get("light.bay_window").map(String::as_str),
            Some("Bay window")
        );
    }

    #[test]
    fn repeated_ids_and_unknown_removals() {
        let mut states = States::default();
        let (changed, removed) = states.apply(&json!({
            "a": {"light.desk": {"s": "on", "a": {}}},
            "c": {"light.desk": {"+": {"s": "off"}}},
            "r": ["light.nothing"]
        }));
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1.state, "off");
        assert!(removed.is_empty());
    }

    #[test]
    fn a_group_counts_its_members() {
        let mut states = States::default();
        states.apply(&json!({"a": {
            "light.mud_room": {"s": "on", "a": {
                "friendly_name": "Mud room",
                "is_hue_group": true,
                "hue_type": "room",
                "entity_id": ["light.hue_white_lamp_1", "light.hue_white_lamp_1_2",
                              "light.mud_4", "light.mud_3"]
            }},
            "light.mud_3": {"s": "on", "a": {"friendly_name": "Mud 3"}}
        }}));
        assert_eq!(states.get("light.mud_room").unwrap().members, 4);
        assert_eq!(states.get("light.mud_3").unwrap().members, 0);
    }

    #[test]
    fn unknown_still_counts_as_available() {
        let state = |s: &str| EntityState {
            state: s.into(),
            name: None,
            brightness: None,
            members: 0,
        };
        assert!(state("unknown").available());
        assert!(!state("unavailable").available());
    }
}
