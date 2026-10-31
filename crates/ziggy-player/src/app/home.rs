//! Home Assistant controls in the core: which tap does what, the brightness bar, optimistic taps.

use super::*;
use crate::home::types::{Domain, EntityState, brightness_of};

/// How long the bottom bar shows a light's brightness after the last touch.
const BAR_MS: u64 = 8_000;
/// How often a dragged brightness is sent to Home Assistant.
const DRAG_SEND_MS: u64 = 250;
/// How long a scene button stays outlined after a tap.
const RAN_MS: u64 = 4_000;
/// A step for "brighter" and "dimmer", in percent.
const BRIGHTNESS_STEP: u8 = 20;
/// A voice note names at most this many controls, then says how many more.
const NAMED_MOST: usize = 3;

impl Core {
    pub(super) fn on_home(&mut self, update: HomeUpdate, now_ms: u64) {
        let home = &mut self.state.home;
        match update {
            HomeUpdate::Connected => home.link = HomeLink::Connected,
            HomeUpdate::Disconnected => {
                home.link = HomeLink::Unreachable;
                // Unconfirmed taps may never have arrived, so show what was there before.
                for (id, before) in self.home_undo.drain() {
                    match before {
                        Some(before) => home.entities.insert(id, before),
                        None => home.entities.remove(&id),
                    };
                }
            }
            HomeUpdate::Refused => home.link = HomeLink::Refused,
            HomeUpdate::NoToken => home.link = HomeLink::NoToken,
            HomeUpdate::States(states) => {
                for (id, mut state) in states {
                    // Home Assistant's word is final, so the tap needs no undoing.
                    self.home_undo.remove(&id);
                    home.missing.remove(&id);
                    let selected = home.selected.as_deref() == Some(id.as_str());
                    if selected && !state.available() {
                        home.selected = None;
                        self.home_drag = None;
                    }
                    // Answers to earlier drag steps mustn't pull the bar from under the finger.
                    if let Some(pct) = self.home_drag.filter(|_| selected && state.is_on()) {
                        state.brightness = Some(brightness_of(pct));
                    }
                    // Without a finger on the bar, what Home Assistant says wins over what was sent.
                    if self.home_drag.is_none()
                        && self
                            .home_sent_pct
                            .as_ref()
                            .is_some_and(|(sent, _)| *sent == id)
                    {
                        self.home_sent_pct = None;
                    }
                    home.entities.insert(id, state);
                }
            }
            HomeUpdate::Removed(ids) => {
                for id in ids {
                    if home.selected.as_deref() == Some(id.as_str()) {
                        home.selected = None;
                        self.home_drag = None;
                    }
                    self.home_undo.remove(&id);
                    home.entities.remove(&id);
                    home.missing.insert(id);
                }
            }
            HomeUpdate::Areas(areas) => home.areas = areas,
            HomeUpdate::CallFailed { entity_id, message } => {
                match self.home_undo.remove(&entity_id) {
                    Some(Some(before)) => {
                        home.entities.insert(entity_id.clone(), before);
                    }
                    Some(None) => {
                        home.entities.remove(&entity_id);
                    }
                    None => {}
                }
                if home.ran.as_ref().is_some_and(|(id, _)| *id == entity_id) {
                    home.ran = None;
                }
                self.notify(Notice::Home(message), now_ms);
            }
        }
        // The bar's controls do nothing without a connection.
        if self.state.home.link != HomeLink::Connected {
            self.home_deselect();
        }
    }

    pub(super) fn on_home_ui(&mut self, action: UiAction, now_ms: u64, fx: &mut Vec<Effect>) {
        match action {
            UiAction::HomePage(i) => {
                if i < self.state.home.pages.len() {
                    self.state.home.page = i;
                    self.home_deselect();
                }
            }
            UiAction::HomeTile(id) => {
                let Some(state) = self.usable(&id) else {
                    return;
                };
                match Domain::of(&id) {
                    Some(Domain::Light) if state.is_on() => {
                        self.home_call(&id, "turn_off", None, fx);
                        if self.state.home.selected.as_deref() == Some(id.as_str()) {
                            self.home_deselect();
                        }
                    }
                    Some(Domain::Light) => self.home_call(&id, "turn_on", None, fx),
                    Some(Domain::Switch | Domain::Fan) => self.home_call(&id, "toggle", None, fx),
                    _ => {}
                }
            }
            UiAction::HomeStrip(id) => {
                if Domain::of(&id) != Some(Domain::Light) {
                    return;
                }
                let Some(state) = self.usable(&id) else {
                    return;
                };
                if state.is_on() {
                    self.state.home.selected = Some(id);
                    self.state.home.bar_until_ms = now_ms + BAR_MS;
                } else {
                    self.home_call(&id, "turn_on", None, fx);
                }
            }
            UiAction::HomeScene(id) => {
                let home = &self.state.home;
                let unavailable = home.entities.get(&id).is_some_and(|s| !s.available());
                if home.link != HomeLink::Connected || unavailable {
                    return;
                }
                // Scenes have no on or off to show; the outline is the answer.
                fx.push(Effect::Home(HomeRequest::Call {
                    entity_id: id.clone(),
                    service: "turn_on",
                    brightness_pct: None,
                }));
                self.state.home.ran = Some((id, now_ms + RAN_MS));
            }
            UiAction::HomeBrightness(pct) | UiAction::HomeBrightnessDone(pct) => {
                let done = matches!(action, UiAction::HomeBrightnessDone(_));
                // A lifted finger always ends the drag, even when the bar can't be used.
                if done {
                    self.home_drag = None;
                }
                let Some(id) = self.state.home.selected.clone() else {
                    return;
                };
                if self.usable(&id).is_none() {
                    return;
                }
                self.state.home.bar_until_ms = now_ms + BAR_MS;
                let pct = pct.clamp(1, 100);
                self.home_drag = if done { None } else { Some(pct) };
                let sent = self.home_sent_pct.as_ref() == Some(&(id.clone(), pct));
                if done && sent {
                    // A plain tap: the press already sent this level.
                } else if done || now_ms.saturating_sub(self.home_sent_ms) >= DRAG_SEND_MS {
                    self.home_sent_ms = now_ms;
                    self.home_sent_pct = Some((id.clone(), pct));
                    self.home_call(&id, "turn_on", Some(pct), fx);
                } else {
                    // Not sent yet, but the bar follows the finger.
                    self.home_show(&id, true, Some(pct));
                }
            }
            UiAction::HomeBarToggle => {
                let Some(id) = self.state.home.selected.clone() else {
                    return;
                };
                let Some(state) = self.usable(&id) else {
                    return;
                };
                self.state.home.bar_until_ms = now_ms + BAR_MS;
                let service = if state.is_on() { "turn_off" } else { "turn_on" };
                self.home_call(&id, service, None, fx);
            }
            UiAction::HomeBarClose => self.home_deselect(),
            _ => {}
        }
    }

    pub(super) fn home_tick(&mut self, now_ms: u64) {
        let home = &self.state.home;
        // A finger still on the bar keeps it.
        if home.selected.is_some() && now_ms >= home.bar_until_ms && self.home_drag.is_none() {
            self.home_deselect();
        }
        let home = &mut self.state.home;
        if home.ran.as_ref().is_some_and(|(_, until)| now_ms >= *until) {
            home.ran = None;
        }
    }

    /// This player's controls, for Gemini: each once, in page order.
    pub(super) fn home_voice_controls(&self) -> Vec<VoiceHomeControl> {
        let home = &self.state.home;
        let mut out: Vec<VoiceHomeControl> = Vec::new();
        for page in &home.pages {
            for c in page.tiles.iter().chain(&page.scenes) {
                if out.iter().any(|o| o.entity_id == c.entity_id) {
                    continue;
                }
                let state = home.entities.get(&c.entity_id);
                let kind = match c.domain {
                    Domain::Light => match state.map_or(0, |s| s.members) {
                        0 => "light".to_string(),
                        n => format!("light group of {n}"),
                    },
                    Domain::Switch => "switch".into(),
                    Domain::Fan => "fan".into(),
                    Domain::Scene => "scene".into(),
                    Domain::Script => "script".into(),
                };
                let now = match state {
                    _ if !c.domain.is_tile() => String::new(),
                    None => "unknown".into(),
                    Some(s) if !s.available() => "unavailable".into(),
                    Some(s) => match (s.is_on(), s.percent()) {
                        (true, Some(pct)) => format!("on at {pct}%"),
                        (true, None) => "on".into(),
                        (false, _) => "off".into(),
                    },
                };
                out.push(VoiceHomeControl {
                    entity_id: c.entity_id.clone(),
                    kind,
                    name: home.name_of(c),
                    room: home
                        .areas
                        .get(&c.entity_id)
                        .cloned()
                        .unwrap_or_else(|| page.name.clone()),
                    now,
                });
            }
        }
        out
    }

    /// A voice request for Home controls, run as taps would be.
    pub(super) fn voice_home(
        &mut self,
        ids: Vec<String>,
        action: HomeAction,
        now_ms: u64,
        fx: &mut Vec<Effect>,
    ) {
        // Only this player's own controls, whatever Gemini named.
        let mut controls: Vec<crate::home::types::Control> = Vec::new();
        for id in ids {
            let control = self
                .state
                .home
                .pages
                .iter()
                .flat_map(|p| p.tiles.iter().chain(&p.scenes))
                .find(|c| c.entity_id == id);
            if let Some(c) = control
                && !controls.iter().any(|k| k.entity_id == c.entity_id)
            {
                controls.push(c.clone());
            }
        }
        if controls.is_empty() {
            self.notify(Notice::Voice("Didn't catch that".into()), now_ms);
            return;
        }
        if self.state.home.link != HomeLink::Connected {
            self.notify(Notice::Voice("Can't reach Home Assistant".into()), now_ms);
            return;
        }
        let mut done: Vec<(String, Option<u8>)> = Vec::new();
        let mut unavailable: Vec<String> = Vec::new();
        for c in &controls {
            let id = c.entity_id.as_str();
            let name = self.state.home.name_of(c);
            let Some(state) = self.usable(id) else {
                unavailable.push(name);
                continue;
            };
            let light = c.domain == Domain::Light;
            let pct = state.percent().filter(|_| state.is_on());
            let call: Option<(&'static str, Option<u8>)> = match (action, c.domain.is_tile()) {
                (HomeAction::On | HomeAction::Run, _) => Some(("turn_on", None)),
                // A scene or script can only be run.
                (_, false) => None,
                (HomeAction::Off, true) | (HomeAction::Brightness(0), true) => {
                    Some(("turn_off", None))
                }
                (HomeAction::Brightness(p), true) if light => Some(("turn_on", Some(p.min(100)))),
                (HomeAction::Brightness(_), true) => Some(("turn_on", None)),
                (HomeAction::Brighter, true) if light => Some((
                    "turn_on",
                    Some(pct.map_or(BRIGHTNESS_STEP, |p| {
                        p.saturating_add(BRIGHTNESS_STEP).min(100)
                    })),
                )),
                (HomeAction::Dimmer, true) if light => {
                    // A light that's off stays off.
                    pct.map(|p| ("turn_on", Some(p.saturating_sub(BRIGHTNESS_STEP).max(1))))
                }
                (HomeAction::Brighter | HomeAction::Dimmer, true) => None,
            };
            let Some((service, pct)) = call else {
                continue;
            };
            if c.domain.is_tile() {
                self.home_call(id, service, pct, fx);
            } else {
                self.on_home_ui(UiAction::HomeScene(id.to_string()), now_ms, fx);
            }
            done.push((name, pct));
        }
        let text = if done.is_empty() && !unavailable.is_empty() {
            format!("{} unavailable", names(&unavailable))
        } else if done.is_empty() {
            "Didn't catch that".to_string()
        } else {
            let who: Vec<String> = done.iter().map(|(n, _)| n.clone()).collect();
            let who = names(&who);
            let level = done.first().and_then(|(_, p)| *p);
            let same = done.iter().all(|(_, p)| *p == level);
            match (action, level) {
                (HomeAction::Off | HomeAction::Brightness(0), _) => format!("Turned off {who}"),
                (
                    HomeAction::Brightness(_) | HomeAction::Brighter | HomeAction::Dimmer,
                    Some(p),
                ) if same => {
                    format!("{who} at {p}%")
                }
                (HomeAction::Brighter, _) => format!("Turned up {who}"),
                (HomeAction::Dimmer, _) => format!("Turned down {who}"),
                _ if controls.iter().all(|c| !c.domain.is_tile()) => format!("Ran {who}"),
                _ => format!("Turned on {who}"),
            }
        };
        self.notify(Notice::Voice(text), now_ms);
    }

    /// Puts the music back in the bottom bar.
    pub(super) fn home_deselect(&mut self) {
        self.state.home.selected = None;
        self.home_drag = None;
        self.home_sent_pct = None;
    }

    /// The entity's state when it can be used now.
    fn usable(&self, id: &str) -> Option<EntityState> {
        let home = &self.state.home;
        if home.link != HomeLink::Connected {
            return None;
        }
        home.entities.get(id).filter(|s| s.available()).cloned()
    }

    /// Sends a call and shows its result at once; Home Assistant's answer corrects it.
    fn home_call(
        &mut self,
        id: &str,
        service: &'static str,
        pct: Option<u8>,
        fx: &mut Vec<Effect>,
    ) {
        let on = match service {
            "turn_on" => Some(true),
            "turn_off" => Some(false),
            "toggle" => self.state.home.entities.get(id).map(|s| !s.is_on()),
            _ => None,
        };
        // Anything else sent to the light makes the last dragged level stale.
        if pct.is_none() {
            self.home_sent_pct = None;
        }
        let tile = Domain::of(id).is_some_and(Domain::is_tile);
        if let Some(on) = on.filter(|_| tile) {
            self.home_show(id, on, pct);
        }
        fx.push(Effect::Home(HomeRequest::Call {
            entity_id: id.to_string(),
            service,
            brightness_pct: pct,
        }));
    }

    /// Changes what the screen shows for an entity, remembering what it was.
    fn home_show(&mut self, id: &str, on: bool, pct: Option<u8>) {
        let before = self.state.home.entities.get(id).cloned();
        self.home_undo
            .entry(id.to_string())
            .or_insert_with(|| before.clone());
        let mut state = before.unwrap_or(EntityState {
            state: String::new(),
            name: None,
            brightness: None,
            members: 0,
        });
        state.state = if on { "on" } else { "off" }.into();
        state.brightness = match (Domain::of(id) == Some(Domain::Light) && on, pct) {
            (false, _) => None,
            (true, Some(pct)) => Some(brightness_of(pct)),
            (true, None) => state.brightness.or(Some(255)),
        };
        self.state.home.entities.insert(id.to_string(), state);
    }
}

/// "MR", "MR and LR", "MR, LR and Lamp", "MR, LR, Lamp and 2 more".
fn names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        _ if names.len() <= NAMED_MOST => {
            let (last, rest) = names.split_last().expect("not empty");
            format!("{} and {last}", rest.join(", "))
        }
        _ => format!(
            "{} and {} more",
            names[..NAMED_MOST].join(", "),
            names.len() - NAMED_MOST
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{config, ui};
    use crate::home::types::{Control, Domain, EntityState, Page};

    fn control(id: &str) -> Control {
        Control {
            entity_id: id.into(),
            domain: Domain::of(id).unwrap(),
            name: None,
        }
    }

    fn home() -> Core {
        let mut cfg = config();
        cfg.home = vec![Page {
            name: "Room".into(),
            tiles: vec![control("light.desk"), control("switch.fan")],
            scenes: vec![control("scene.bedtime")],
        }];
        let mut c = Core::new(cfg, 0).0;
        c.handle(Input::Home(HomeUpdate::Connected), 0);
        c.handle(ui(UiAction::ShowSection(Section::Home)), 0);
        c
    }

    fn set(c: &mut Core, id: &str, state: &str, brightness: Option<u8>) {
        c.handle(
            Input::Home(HomeUpdate::States(vec![(
                id.into(),
                EntityState {
                    state: state.into(),
                    name: None,
                    brightness,
                    members: 0,
                },
            )])),
            0,
        );
    }

    fn call(id: &str, service: &'static str, pct: Option<u8>) -> Effect {
        Effect::Home(HomeRequest::Call {
            entity_id: id.into(),
            service,
            brightness_pct: pct,
        })
    }

    #[test]
    fn tapping_an_off_light_turns_it_on_without_opening_the_bar() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        let fx = c.handle(ui(UiAction::HomeTile("light.desk".into())), 1_000);
        assert!(fx.contains(&call("light.desk", "turn_on", None)));
        assert!(
            c.state().home.entities["light.desk"].is_on(),
            "shown on at once"
        );
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn tapping_an_on_light_turns_it_off_and_its_strip_opens_the_bar() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(178));
        let fx = c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        assert!(fx.is_empty(), "the strip only selects");
        assert_eq!(c.state().home.selected.as_deref(), Some("light.desk"));
        let fx = c.handle(ui(UiAction::HomeTile("light.desk".into())), 2_000);
        assert!(fx.contains(&call("light.desk", "turn_off", None)));
        assert_eq!(c.state().home.selected, None, "an off light has no bar");
    }

    #[test]
    fn the_strip_of_an_off_light_turns_it_on() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        let fx = c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        assert!(fx.contains(&call("light.desk", "turn_on", None)));
    }

    #[test]
    fn switches_and_fans_toggle() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        let fx = c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000);
        assert!(fx.contains(&call("switch.fan", "toggle", None)));
        assert!(c.state().home.entities["switch.fan"].is_on());
    }

    #[test]
    fn dragging_sends_at_most_every_250_ms_and_always_on_release() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        let sent = |fx: &[Effect]| fx.iter().filter(|e| matches!(e, Effect::Home(_))).count();
        assert_eq!(sent(&c.handle(ui(UiAction::HomeBrightness(40)), 1_000)), 1);
        assert_eq!(sent(&c.handle(ui(UiAction::HomeBrightness(45)), 1_100)), 0);
        assert_eq!(
            c.state().home.entities["light.desk"].percent(),
            Some(45),
            "the bar follows the finger"
        );
        assert_eq!(sent(&c.handle(ui(UiAction::HomeBrightness(50)), 1_300)), 1);
        let fx = c.handle(ui(UiAction::HomeBrightnessDone(52)), 1_350);
        assert!(fx.contains(&call("light.desk", "turn_on", Some(52))));
    }

    #[test]
    fn the_bar_goes_back_to_the_music_after_8_seconds_or_on_leaving() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(30)), 5_000);
        c.handle(ui(UiAction::HomeBrightnessDone(30)), 5_000);
        c.handle(Input::Tick, 12_000);
        assert!(
            c.state().home.selected.is_some(),
            "touching the bar keeps it"
        );
        c.handle(Input::Tick, 13_000);
        assert_eq!(c.state().home.selected, None);
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 14_000);
        c.handle(ui(UiAction::ShowSection(Section::Albums)), 15_000);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn a_refused_call_puts_the_tile_back_and_says_why() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000);
        c.handle(
            Input::Home(HomeUpdate::CallFailed {
                entity_id: "switch.fan".into(),
                message: "Entity not found".into(),
            }),
            1_100,
        );
        assert!(!c.state().home.entities["switch.fan"].is_on());
        assert_eq!(
            c.state().notice,
            Some(Notice::Home("Entity not found".into()))
        );
    }

    #[test]
    fn nothing_happens_while_unreachable_or_unavailable() {
        let mut c = home();
        set(&mut c, "switch.fan", "unavailable", None);
        assert!(
            c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000)
                .is_empty()
        );
        set(&mut c, "light.desk", "off", None);
        c.handle(Input::Home(HomeUpdate::Disconnected), 1_000);
        assert!(
            c.handle(ui(UiAction::HomeTile("light.desk".into())), 1_000)
                .is_empty()
        );
    }

    #[test]
    fn scenes_run_and_stay_outlined_for_4_seconds() {
        let mut c = home();
        let fx = c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_000);
        assert!(fx.contains(&call("scene.bedtime", "turn_on", None)));
        assert!(c.state().home.ran.is_some());
        c.handle(Input::Tick, 5_000);
        assert_eq!(c.state().home.ran, None);
    }

    #[test]
    fn the_bar_toggles_and_closes() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        let fx = c.handle(ui(UiAction::HomeBarToggle), 1_100);
        assert!(fx.contains(&call("light.desk", "turn_off", None)));
        assert!(
            c.state().home.selected.is_some(),
            "the bar stays to turn it back on"
        );
        let fx = c.handle(ui(UiAction::HomeBarToggle), 1_200);
        assert!(fx.contains(&call("light.desk", "turn_on", None)));
        c.handle(ui(UiAction::HomeBarClose), 1_300);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn wifi_coming_back_reconnects_home_assistant() {
        let mut c = home();
        c.handle(Input::Wifi(Some(WifiStatus::Disconnected)), 0);
        c.handle(Input::Home(HomeUpdate::Disconnected), 0);
        let fx = c.handle(
            Input::Wifi(Some(WifiStatus::Connected {
                network: "Home".into(),
                band: "5 GHz".into(),
                signal: 70,
            })),
            1_000,
        );
        assert!(fx.contains(&Effect::Home(HomeRequest::Reconnect)));
    }

    #[test]
    fn a_scene_tap_changes_no_entity() {
        let mut c = home();
        set(&mut c, "scene.bedtime", "unknown", None);
        let before = c.state().home.entities.clone();
        let fx = c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_000);
        assert!(fx.contains(&call("scene.bedtime", "turn_on", None)));
        assert_eq!(c.state().home.entities, before);
        assert!(c.home_undo.is_empty());
    }

    #[test]
    fn a_refused_scene_loses_its_outline() {
        let mut c = home();
        c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_000);
        c.handle(
            Input::Home(HomeUpdate::CallFailed {
                entity_id: "scene.bedtime".into(),
                message: "Not found".into(),
            }),
            1_100,
        );
        assert_eq!(c.state().home.ran, None);
    }

    #[test]
    fn scenes_are_blocked_while_unreachable_or_unavailable() {
        let mut c = home();
        set(&mut c, "scene.bedtime", "unavailable", None);
        assert!(
            home_fx(&c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_000)).is_empty()
        );
        set(&mut c, "scene.bedtime", "unknown", None);
        c.handle(Input::Home(HomeUpdate::Disconnected), 1_000);
        assert!(
            home_fx(&c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_000)).is_empty()
        );
        assert_eq!(c.state().home.ran, None);
    }

    fn home_fx(fx: &[Effect]) -> Vec<&Effect> {
        fx.iter().filter(|e| matches!(e, Effect::Home(_))).collect()
    }

    #[test]
    fn answers_to_earlier_drag_steps_dont_move_the_bar() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 1_000);
        c.handle(ui(UiAction::HomeBrightness(45)), 1_100);
        set(&mut c, "light.desk", "on", Some(brightness_of(40)));
        assert_eq!(c.state().home.entities["light.desk"].percent(), Some(45));
        c.handle(ui(UiAction::HomeBrightnessDone(45)), 1_200);
        set(&mut c, "light.desk", "on", Some(brightness_of(30)));
        assert_eq!(
            c.state().home.entities["light.desk"].percent(),
            Some(30),
            "after release it follows Home Assistant"
        );
    }

    #[test]
    fn a_finger_on_the_bar_keeps_it_past_the_timeout() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 1_000);
        c.handle(Input::Tick, 20_000);
        assert!(c.state().home.selected.is_some());
        c.handle(ui(UiAction::HomeBrightnessDone(40)), 20_500);
        c.handle(Input::Tick, 29_000);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn clearing_the_selection_ends_a_drag() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 1_000);
        c.handle(ui(UiAction::HomeBarClose), 1_100);
        assert_eq!(c.home_drag, None);
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 2_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 2_000);
        c.handle(ui(UiAction::HomeTile("light.desk".into())), 2_100);
        assert_eq!(c.home_drag, None);
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 3_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 3_000);
        c.handle(ui(UiAction::ShowSection(Section::Albums)), 3_100);
        assert_eq!(c.home_drag, None);
    }

    #[test]
    fn a_drag_ends_on_release_even_after_the_connection_drops() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 1_000);
        c.handle(Input::Home(HomeUpdate::Disconnected), 1_100);
        c.handle(ui(UiAction::HomeBrightnessDone(40)), 1_200);
        assert_eq!(c.home_drag, None);
        c.handle(Input::Tick, 10_000);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn losing_the_link_closes_the_bar() {
        for update in [
            HomeUpdate::Disconnected,
            HomeUpdate::Refused,
            HomeUpdate::NoToken,
        ] {
            let mut c = home();
            set(&mut c, "light.desk", "on", Some(100));
            c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
            c.handle(Input::Home(update.clone()), 1_100);
            assert_eq!(c.state().home.selected, None, "{update:?}");
        }
    }

    #[test]
    fn removing_the_selected_light_closes_the_bar() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomeBrightness(40)), 1_000);
        c.handle(
            Input::Home(HomeUpdate::Removed(vec!["light.desk".into()])),
            1_100,
        );
        assert_eq!(c.state().home.selected, None);
        assert_eq!(c.home_drag, None);
    }

    #[test]
    fn a_tap_on_the_bar_sends_its_level_once() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        assert_eq!(
            home_fx(&c.handle(ui(UiAction::HomeBrightness(50)), 2_000)).len(),
            1
        );
        assert_eq!(
            home_fx(&c.handle(ui(UiAction::HomeBrightness(50)), 2_010)).len(),
            0
        );
        assert!(
            home_fx(&c.handle(ui(UiAction::HomeBrightnessDone(50)), 2_020)).is_empty(),
            "the press already sent 50"
        );
        // Switched off between two quick taps: the same level is sent again.
        c.handle(ui(UiAction::HomeBrightness(50)), 2_500);
        c.handle(ui(UiAction::HomeBarToggle), 2_510);
        assert!(home_fx(&c.handle(ui(UiAction::HomeBrightness(50)), 2_600)).is_empty());
        let fx = c.handle(ui(UiAction::HomeBrightnessDone(50)), 2_610);
        assert!(fx.contains(&call("light.desk", "turn_on", Some(50))));
        // A different level on release is still sent.
        c.handle(ui(UiAction::HomeBrightness(60)), 3_000);
        let fx = c.handle(ui(UiAction::HomeBrightnessDone(64)), 3_010);
        assert!(fx.contains(&call("light.desk", "turn_on", Some(64))));
    }

    #[test]
    fn a_removed_entity_has_nothing_to_undo() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000);
        c.handle(
            Input::Home(HomeUpdate::Removed(vec!["switch.fan".into()])),
            1_100,
        );
        assert!(c.home_undo.is_empty());
        assert!(!c.state().home.entities.contains_key("switch.fan"));
    }

    #[test]
    fn losing_the_connection_puts_unconfirmed_taps_back() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000);
        c.handle(Input::Home(HomeUpdate::Disconnected), 1_100);
        assert!(!c.state().home.entities["switch.fan"].is_on());
        assert!(c.home_undo.is_empty());
    }

    #[test]
    fn only_lights_have_a_strip_or_a_brightness() {
        let mut c = home();
        set(&mut c, "switch.fan", "on", None);
        let fx = c.handle(ui(UiAction::HomeStrip("switch.fan".into())), 1_000);
        assert!(home_fx(&fx).is_empty());
        assert_eq!(c.state().home.selected, None);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_100);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_200);
        assert!(c.state().home.entities["switch.fan"].is_on());
        assert_eq!(c.state().home.entities["switch.fan"].brightness, None);
    }

    #[test]
    fn home_cannot_open_without_pages() {
        let mut c = Core::new(config(), 0).0;
        c.handle(ui(UiAction::ShowSection(Section::Home)), 0);
        assert_ne!(c.state().screen, Screen::Home);
        assert_ne!(c.state().section, Section::Home);
    }

    #[test]
    fn a_light_going_unavailable_closes_its_bar() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        set(&mut c, "light.desk", "unavailable", None);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn switching_pages_closes_the_bar_and_ignores_missing_pages() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(100));
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        c.handle(ui(UiAction::HomePage(3)), 1_100);
        assert!(c.state().home.selected.is_some(), "no such page");
        assert_eq!(c.state().home.page, 0);
        c.handle(ui(UiAction::HomePage(0)), 1_200);
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn the_strip_of_an_off_light_doesnt_select_it() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        c.handle(ui(UiAction::HomeStrip("light.desk".into())), 1_000);
        assert!(c.state().home.entities["light.desk"].is_on());
        assert_eq!(c.state().home.selected, None);
    }

    #[test]
    fn a_failure_after_home_assistant_confirmed_changes_nothing() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000);
        set(&mut c, "switch.fan", "on", None);
        c.handle(
            Input::Home(HomeUpdate::CallFailed {
                entity_id: "switch.fan".into(),
                message: "Timed out".into(),
            }),
            1_200,
        );
        assert!(c.state().home.entities["switch.fan"].is_on());
    }

    #[test]
    fn removed_entities_are_missing_until_a_state_arrives() {
        let mut c = home();
        c.handle(
            Input::Home(HomeUpdate::Removed(vec!["light.desk".into()])),
            0,
        );
        assert!(c.state().home.missing.contains("light.desk"));
        set(&mut c, "light.desk", "on", None);
        assert!(c.state().home.missing.is_empty());
    }

    #[test]
    fn no_token_blocks_taps() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(Input::Home(HomeUpdate::NoToken), 1_000);
        assert_eq!(c.state().home.link, HomeLink::NoToken);
        assert!(home_fx(&c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_100)).is_empty());
    }

    #[test]
    fn a_refused_token_blocks_taps() {
        let mut c = home();
        set(&mut c, "switch.fan", "off", None);
        c.handle(Input::Home(HomeUpdate::Refused), 1_000);
        assert!(home_fx(&c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_100)).is_empty());
        assert!(
            home_fx(&c.handle(ui(UiAction::HomeScene("scene.bedtime".into())), 1_200)).is_empty()
        );
    }

    fn say(c: &mut Core, ids: &[&str], action: HomeAction) -> Vec<Effect> {
        let ids = ids.iter().map(|id| id.to_string()).collect();
        c.handle(
            Input::Voice(VoiceUpdate::Command(VoiceCommand::Home { ids, action })),
            1_000,
        )
    }

    fn said(c: &Core) -> Option<String> {
        match &c.state().notice {
            Some(Notice::Voice(text)) => Some(text.clone()),
            _ => None,
        }
    }

    #[test]
    fn gemini_is_told_each_control_once_with_its_state() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(178));
        set(&mut c, "switch.fan", "unavailable", None);
        let controls = c.voice_context().home;
        let rows: Vec<(&str, &str, &str, &str)> = controls
            .iter()
            .map(|h| {
                (
                    h.entity_id.as_str(),
                    h.kind.as_str(),
                    h.room.as_str(),
                    h.now.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("light.desk", "light", "Room", "on at 70%"),
                ("switch.fan", "switch", "Room", "unavailable"),
                ("scene.bedtime", "scene", "Room", ""),
            ]
        );
        assert_eq!(controls[0].name, "Desk");
    }

    #[test]
    fn a_player_without_home_tells_gemini_of_none() {
        let c = Core::new(config(), 0).0;
        assert!(c.voice_context().home.is_empty());
    }

    #[test]
    fn voice_switches_several_controls_at_once() {
        let mut c = home();
        set(&mut c, "light.desk", "on", Some(178));
        set(&mut c, "switch.fan", "on", None);
        let fx = say(&mut c, &["light.desk", "switch.fan"], HomeAction::Off);
        assert!(fx.contains(&call("light.desk", "turn_off", None)));
        assert!(fx.contains(&call("switch.fan", "turn_off", None)));
        assert!(
            !c.state().home.entities["light.desk"].is_on(),
            "shown at once"
        );
        assert_eq!(said(&c).as_deref(), Some("Turned off Desk and Fan"));
    }

    #[test]
    fn voice_sets_and_steps_brightness() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        let fx = say(&mut c, &["light.desk"], HomeAction::Brightness(40));
        assert!(fx.contains(&call("light.desk", "turn_on", Some(40))));
        assert_eq!(said(&c).as_deref(), Some("Desk at 40%"));
        set(&mut c, "light.desk", "on", Some(brightness_of(90)));
        let fx = say(&mut c, &["light.desk"], HomeAction::Brighter);
        assert!(fx.contains(&call("light.desk", "turn_on", Some(100))));
        let fx = say(&mut c, &["light.desk"], HomeAction::Dimmer);
        assert!(
            fx.contains(&call("light.desk", "turn_on", Some(80))),
            "{fx:?}"
        );
        let fx = say(&mut c, &["light.desk"], HomeAction::Brightness(0));
        assert!(fx.contains(&call("light.desk", "turn_off", None)));
        assert_eq!(said(&c).as_deref(), Some("Turned off Desk"));
    }

    #[test]
    fn dimmer_leaves_a_light_that_is_off_alone() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        let fx = say(&mut c, &["light.desk"], HomeAction::Dimmer);
        assert!(home_fx(&fx).is_empty());
        assert_eq!(said(&c).as_deref(), Some("Didn't catch that"));
    }

    #[test]
    fn voice_runs_scenes() {
        let mut c = home();
        set(&mut c, "scene.bedtime", "unknown", None);
        let fx = say(&mut c, &["scene.bedtime"], HomeAction::Run);
        assert!(fx.contains(&call("scene.bedtime", "turn_on", None)));
        assert_eq!(said(&c).as_deref(), Some("Ran Bedtime"));
        // Turning a scene off means nothing.
        let fx = say(&mut c, &["scene.bedtime"], HomeAction::Off);
        assert!(home_fx(&fx).is_empty());
    }

    #[test]
    fn voice_never_touches_controls_the_player_doesnt_have() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        let fx = say(
            &mut c,
            &["light.kitchen", "lock.front_door"],
            HomeAction::On,
        );
        assert!(home_fx(&fx).is_empty());
        assert_eq!(said(&c).as_deref(), Some("Didn't catch that"));
        let fx = say(&mut c, &["light.kitchen", "light.desk"], HomeAction::On);
        assert_eq!(home_fx(&fx), vec![&call("light.desk", "turn_on", None)]);
    }

    #[test]
    fn voice_says_when_home_assistant_cant_be_reached() {
        let mut c = home();
        set(&mut c, "light.desk", "off", None);
        c.handle(Input::Home(HomeUpdate::Disconnected), 0);
        let fx = say(&mut c, &["light.desk"], HomeAction::On);
        assert!(home_fx(&fx).is_empty());
        assert_eq!(said(&c).as_deref(), Some("Can't reach Home Assistant"));
        let mut c = home();
        set(&mut c, "light.desk", "unavailable", None);
        say(&mut c, &["light.desk"], HomeAction::On);
        assert_eq!(said(&c).as_deref(), Some("Desk unavailable"));
    }

    #[test]
    fn long_lists_of_names_are_cut_short() {
        let n = |list: &[&str]| names(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["MR"]), "MR");
        assert_eq!(n(&["MR", "LR", "Lamp"]), "MR, LR and Lamp");
        assert_eq!(
            n(&["MR", "LR", "Lamp", "Hall", "Porch"]),
            "MR, LR, Lamp and 2 more"
        );
    }
}
