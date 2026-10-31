# Home Assistant Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A per-device Home screen on Ziggy that switches and dims Home Assistant lights, toggles
switches and fans, and runs scenes and scripts, as designed in
`docs/superpowers/specs/2026-10-31-home-assistant-design.md`.

**Architecture:** A new `home` module (types, a small WebSocket client, a service that keeps the
connection up) feeds `Input::Home` updates into the app core, which owns every rule (which tap does
what, the brightness bar's timer, optimistic taps) and emits `Effect::Home` requests. The view turns
the core's `HomeState` into rows for a new Slint `HomeScreen` and `HomeBar`. The setup tool learns to
set the address and copy the token.

**Tech Stack:** Rust 2024, tokio, tokio-tungstenite 0.28 (already a dependency), serde_json, Slint
1.18, dialoguer (setup), mdns-sd 0.13 (setup only).

**Branch:** `feat/home-assistant` (the spec is already committed there).

**Conventions in this repo** (read before starting):
- Comments and UI text are short, plain sentences; match the density of the file you're in.
- Every behaviour lives in the app core and is unit-tested there (`crates/ziggy-player/src/app/`).
  Slint files only draw and forward taps.
- Run tests with `cargo test -p ziggy-player` (and `-p ziggy-setup` for setup). Lint with
  `cargo clippy -p ziggy-player`; two "collapsible if" warnings exist on main already.
- Commit messages: `feat(home): …`, ending with the attribution lines from the session.

---

## File map

| File | What it holds |
|---|---|
| Create `crates/ziggy-player/src/home/mod.rs` | Module docs and `pub mod` lines |
| Create `crates/ziggy-player/src/home/types.rs` | `Domain`, `Control`, `Page`, `EntityState`, `States` (compressed-state merging), `area_names`, brightness conversions |
| Create `crates/ziggy-player/src/home/client.rs` | Home Assistant message builders and `parse` for incoming frames; `websocket_url` |
| Create `crates/ziggy-player/src/home/service.rs` | Connection loop with backoff and pings; fake backend for `--fake` |
| Create `crates/ziggy-player/src/app/home.rs` | Core logic: `on_home`, `on_home_ui`, `home_tick`, plus its tests |
| Modify `crates/ziggy-player/src/lib.rs` | `pub mod home;` |
| Modify `crates/ziggy-player/src/config.rs` | `[home_assistant]` section and its checks |
| Modify `crates/ziggy-player/src/model.rs` | `Section::Home` |
| Modify `crates/ziggy-player/src/app/{input,state,mod}.rs` | `HomeUpdate`, `HomeRequest`, `UiAction::Home*`, `Effect::Home`, `HomeState`, `Screen::Home`, wiring |
| Modify `crates/ziggy-player/src/app/tests.rs` | `config()` gains `home: Vec::new()` |
| Modify `crates/ziggy-player/src/runtime.rs` | Spawn the service, dispatch `Effect::Home` |
| Modify `crates/ziggy-player/src/view.rs` | `HomeView` and its tests |
| Modify `crates/ziggy-player/src/ui_bridge.rs` | Home properties, models and callbacks |
| Create `crates/ziggy-player/ui/screens/home.slint` | `HomeScreen`, `HomeTile` |
| Create `crates/ziggy-player/ui/components/home_bar.slint` | The brightness bar |
| Modify `crates/ziggy-player/ui/{types,app}.slint`, `ui/components/rail.slint` | Structs, wiring, rail item |
| Create `crates/ziggy-player/ui/icons/{home,bulb,plug,fan,sun,sparkle}.svg` | Icons |
| Modify `crates/ziggy-setup/src/{devconfig,wizard,install,main}.rs`, `Cargo.toml` | `ziggy config` → Home Assistant |
| Modify `devices/example.toml`, `docs/testing.md`, `README.md` | Docs |

---

### Task 0: Record real Home Assistant messages (manual spike)

This settles the one open question in the spec (can a non-admin user read the registries?) and
gives the parser tests real shapes to check against.

**Files:**
- Create: `crates/ziggy-player/src/home/fixtures/README.md` (notes only; fixtures stay out of git if
  they hold names you'd rather not commit)

- [ ] **Step 1: Make the Home Assistant user and token (in the browser)**

  In Home Assistant at `http://192.168.1.69:8123`: Settings → People → Add person "Ziggy", allow
  login, **not** an administrator. Log in as Ziggy in a private window → profile (bottom left) →
  Security → Long-lived access tokens → Create token "ziggy-micah". Save it to
  `~/.config/ziggy/ha-token-test` (mode 600). Ask the user to do this step if you can't.

- [ ] **Step 2: Record the messages**

  ```bash
  brew install websocat
  TOKEN=$(cat ~/.config/ziggy/ha-token-test)
  LIGHT=light.REPLACE_WITH_A_REAL_LIGHT   # pick one from Settings → Entities
  { echo '{"type":"auth","access_token":"'"$TOKEN"'"}'
    echo '{"id":1,"type":"config/entity_registry/list_for_display"}'
    echo '{"id":2,"type":"config/device_registry/list"}'
    echo '{"id":3,"type":"config/area_registry/list"}'
    echo '{"id":4,"type":"subscribe_entities","entity_ids":["'"$LIGHT"'"]}'
    sleep 20; } | websocat -t ws://192.168.1.69:8123/api/websocket > /tmp/ha-recording.jsonl
  ```
  While it sleeps, switch the light on and off and dim it in the Home Assistant app.

- [ ] **Step 3: Check the shapes**

  Run: `head -c 400 /tmp/ha-recording.jsonl; grep -o '"success":[a-z]*' /tmp/ha-recording.jsonl`
  Expected: `auth_required`, then `auth_ok`, then results for ids 1–4. Write down in
  `fixtures/README.md`:
  - whether ids 1, 2, 3 have `"success":true` (if any is false, area names will be missing for a
    non-admin user; the code below already copes, but note it in the spec's last section);
  - one `"a"` event and one `"c"` event exactly as recorded (trim other entities).
  If the keys differ from those used in Task 2's tests (`a`/`c`/`r`, `s`, `a`, `+`, `-`, `ei`, `ai`,
  `di`), change the tests and code in Task 2 to match what was recorded.

- [ ] **Step 4: Commit the notes**

  ```bash
  git add crates/ziggy-player/src/home/fixtures/README.md
  git commit -m "docs(home): what Home Assistant sends a non-admin token"
  ```

---

### Task 1: `Domain`, `Control`, `Page`

**Files:**
- Create: `crates/ziggy-player/src/home/mod.rs`, `crates/ziggy-player/src/home/types.rs`
- Modify: `crates/ziggy-player/src/lib.rs`

- [ ] **Step 1: Add the module**

  `crates/ziggy-player/src/home/mod.rs`:
  ```rust
  //! Optional Home Assistant controls: lights, switches, fans, scenes and scripts on a Home screen.

  pub mod client;
  pub mod service;
  pub mod types;
  ```
  Add `pub mod home;` to `crates/ziggy-player/src/lib.rs` next to `pub mod audiobooks;`. Create empty
  `client.rs` and `service.rs` (a single `//!` line each) so the crate builds.

- [ ] **Step 2: Write the failing tests** at the end of `home/types.rs`:
  ```rust
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
          let on = |b| EntityState { state: "on".into(), name: None, brightness: Some(b), color: None };
          assert_eq!(on(255).percent(), Some(100));
          assert_eq!(on(178).percent(), Some(70));
          assert_eq!(on(1).percent(), Some(1), "a lit light never reads 0%");
          assert_eq!(brightness_of(70), 179);
          assert_eq!(brightness_of(100), 255);
      }
  }
  ```

- [ ] **Step 3: Run them to see them fail**

  Run: `cargo test -p ziggy-player home::types`
  Expected: compile errors (`Domain` not found).

- [ ] **Step 4: Write the types** at the top of `home/types.rs`:
  ```rust
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
      /// The bulb's colour (`rgb_color`), while a colour or white-tunable light is on.
      #[serde(default)]
      pub color: Option<[u8; 3]>,
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
  ```

- [ ] **Step 5: Run the tests**

  Run: `cargo test -p ziggy-player home::types`
  Expected: 2 passed.

- [ ] **Step 6: Commit**
  ```bash
  git add crates/ziggy-player/src/home crates/ziggy-player/src/lib.rs
  git commit -m "feat(home): entity domains and states"
  ```

---

### Task 2: Merging `subscribe_entities` events and area names

Home Assistant's `subscribe_entities` sends `{"a": {id: {"s": state, "a": attrs}}}` first, then
`{"c": {id: {"+": {"s": …, "a": {…}}, "-": {"a": [keys]}}}}` for changes and `{"r": [ids]}` for
removals. Ziggy keeps the merged form and hands the app core plain `EntityState`s.

**Files:**
- Modify: `crates/ziggy-player/src/home/types.rs`

- [ ] **Step 1: Write the failing tests** (add inside `mod tests`):
  ```rust
  use serde_json::json;

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
      assert_eq!(desk.1, EntityState { state: "on".into(), name: Some("Desk lamp".into()), brightness: Some(178), color: None });

      // Turning off: the state changes and brightness is taken away.
      let (changed, _) = states.apply(&json!({"c": {
          "light.desk": {"+": {"s": "off"}, "-": {"a": ["brightness"]}}
      }}));
      assert_eq!(changed, vec![("light.desk".to_string(), EntityState {
          state: "off".into(), name: Some("Desk lamp".into()), brightness: None, color: None })]);

      // Dimming changes only an attribute.
      let (changed, _) = states.apply(&json!({"c": {
          "light.desk": {"+": {"s": "on", "a": {"brightness": 26}}}
      }}));
      assert_eq!(changed[0].1.percent(), Some(10));
  }

  #[test]
  fn a_bulbs_colour_is_kept() {
      let mut states = States::default();
      let (changed, _) = states.apply(&json!({"a": {"light.desk": {"s": "on", "a": {"rgb_color": [255, 180, 107]}}}}));
      assert_eq!(changed[0].1.color, Some([255, 180, 107]));
  }

  #[test]
  fn removed_entities_are_reported_and_unknown_changes_ignored() {
      let mut states = States::default();
      states.apply(&json!({"a": {"light.desk": {"s": "on", "a": {}}}}));
      let (changed, removed) = states.apply(&json!({"c": {"light.other": {"+": {"s": "on"}}}, "r": ["light.desk"]}));
      assert!(changed.is_empty());
      assert_eq!(removed, vec!["light.desk".to_string()]);
      assert!(states.snapshot().is_empty());
  }

  #[test]
  fn areas_come_from_the_entity_or_else_its_device() {
      let entities = json!({"entities": [
          {"ei": "light.desk", "ai": "micah", "di": "d1"},
          {"ei": "light.porch", "di": "d2"},
          {"ei": "light.loose"}
      ]});
      let devices = json!([{"id": "d1", "area_id": "kitchen"}, {"id": "d2", "area_id": "outside"}]);
      let areas = json!([
          {"area_id": "micah", "name": "Micah's room"},
          {"area_id": "outside", "name": "Outside"},
          {"area_id": "kitchen", "name": "Kitchen"}
      ]);
      let names = area_names(&entities, &devices, &areas);
      assert_eq!(names.get("light.desk").map(String::as_str), Some("Micah's room"));
      assert_eq!(names.get("light.porch").map(String::as_str), Some("Outside"));
      assert_eq!(names.get("light.loose"), None);
      // A registry Ziggy wasn't allowed to read gives no areas, not an error.
      assert!(area_names(&Value::Null, &devices, &areas).is_empty());
  }
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-player home::types`
  Expected: compile errors (`States`, `area_names` not found).

- [ ] **Step 3: Implement** (add to `home/types.rs`, above `#[cfg(test)]`):
  ```rust
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
                  let attrs = v.get("a").and_then(Value::as_object).cloned().unwrap_or_default();
                  self.raw.insert(id.clone(), (state.to_string(), attrs));
                  changed.push(id.clone());
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
                  let gone = diff.get("-").and_then(|m| m.get("a")).and_then(Value::as_array);
                  for key in gone.into_iter().flatten().filter_map(Value::as_str) {
                      attrs.remove(key);
                  }
                  changed.push(id.clone());
              }
          }
          let removed: Vec<String> = event
              .get("r")
              .and_then(Value::as_array)
              .map(|r| r.iter().filter_map(Value::as_str).map(str::to_string).collect())
              .unwrap_or_default();
          for id in &removed {
              self.raw.remove(id);
          }
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
              name: attrs.get("friendly_name").and_then(Value::as_str).map(str::to_string),
              brightness: attrs
                  .get("brightness")
                  .and_then(Value::as_u64)
                  .map(|b| b.min(255) as u8),
              color: attrs.get("rgb_color").and_then(Value::as_array).and_then(|c| {
                  let c: Vec<u8> = c.iter().filter_map(Value::as_u64).map(|v| v.min(255) as u8).collect();
                  <[u8; 3]>::try_from(c).ok()
              }),
          })
      }

      /// Every entity's state, to keep for the next start.
      pub fn snapshot(&self) -> Vec<(String, EntityState)> {
          self.raw.keys().filter_map(|id| self.get(id).map(|s| (id.clone(), s))).collect()
      }
  }

  /// Each entity's area name: its own area, else its device's. Takes the results of
  /// `config/entity_registry/list_for_display`, `config/device_registry/list` and
  /// `config/area_registry/list`; any of them may be `Null` when Ziggy wasn't allowed to read it.
  pub fn area_names(entities: &Value, devices: &Value, areas: &Value) -> HashMap<String, String> {
      let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
      let area_name: HashMap<String, String> = list(areas)
          .iter()
          .filter_map(|a| Some((a["area_id"].as_str()?.to_string(), a["name"].as_str()?.to_string())))
          .collect();
      let device_area: HashMap<String, String> = list(devices)
          .iter()
          .filter_map(|d| Some((d["id"].as_str()?.to_string(), d["area_id"].as_str()?.to_string())))
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
  ```

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p ziggy-player home::types`
  Expected: 6 passed.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/src/home/types.rs
  git commit -m "feat(home): merge Home Assistant's state events and find area names"
  ```

---

### Task 3: The `[home_assistant]` config section

**Files:**
- Modify: `crates/ziggy-player/src/config.rs`, `crates/ziggy-player/src/home/types.rs`

- [ ] **Step 1: Write the failing tests** (add to `config.rs`'s `mod tests`):
  ```rust
  const HA: &str = "device_name = \"Leo\"\nstate_dir = \"/var/lib/ziggy\"\n\
      [home_assistant]\nurl = \"http://192.168.1.69:8123\"\n\
      [[home_assistant.page]]\nname = \"Micah's room\"\n\
      controls = [\"light.desk\", { entity = \"switch.fairy\", name = \"Fairy lights\" }, \"scene.bedtime\"]\n";

  #[test]
  fn home_assistant_pages_parse_with_a_default_token_path() {
      let c = Config::parse(HA).unwrap();
      let ha = c.home_assistant.as_ref().unwrap();
      assert_eq!(c.home_token_file(), Some(PathBuf::from("/var/lib/ziggy/ha-token")));
      let pages = crate::home::types::pages(ha);
      assert_eq!(pages.len(), 1);
      assert_eq!(pages[0].name, "Micah's room");
      assert_eq!(pages[0].tiles.len(), 2);
      assert_eq!(pages[0].tiles[1].name.as_deref(), Some("Fairy lights"));
      assert_eq!(pages[0].scenes[0].entity_id, "scene.bedtime");
  }

  #[test]
  fn home_assistant_is_off_without_the_section() {
      let c = Config::parse("device_name = \"Leo\"\nstate_dir = \"/x\"\n").unwrap();
      assert!(c.home_assistant.is_none());
      assert_eq!(c.home_token_file(), None);
  }

  #[test]
  fn home_assistant_rejects_other_domains_and_https() {
      let climate = HA.replace("\"light.desk\"", "\"climate.hall\"");
      let err = Config::parse(&climate).unwrap_err();
      assert!(err.to_string().contains("climate.hall"), "{err}");
      let https = HA.replace("http://", "https://");
      let err = Config::parse(&https).unwrap_err();
      assert!(err.to_string().contains("http://"), "{err}");
  }

  #[test]
  fn home_assistant_without_pages_is_allowed() {
      let c = Config::parse("device_name = \"Leo\"\nstate_dir = \"/x\"\n[home_assistant]\nurl = \"http://ha.local:8123\"\n").unwrap();
      assert!(crate::home::types::pages(c.home_assistant.as_ref().unwrap()).is_empty());
  }
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-player config::`
  Expected: compile errors (`home_assistant` field missing).

- [ ] **Step 3: Implement**

  In `config.rs`, add the field at the end of `Config`:
  ```rust
      /// Home Assistant controls on a Home screen; Home is on when this has pages.
      #[serde(default)]
      pub home_assistant: Option<HomeAssistantConfig>,
  ```
  and these types after `Config`:
  ```rust
  #[derive(Debug, Clone, PartialEq, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct HomeAssistantConfig {
      /// The local address, e.g. "http://192.168.1.69:8123".
      pub url: String,
      /// A file holding a long-lived access token; `<state_dir>/ha-token` by default.
      #[serde(default)]
      pub token_file: Option<PathBuf>,
      #[serde(default, rename = "page")]
      pub pages: Vec<HomePageConfig>,
  }

  #[derive(Debug, Clone, PartialEq, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct HomePageConfig {
      pub name: String,
      pub controls: Vec<ControlConfig>,
  }

  /// An entity ID, or a table with the name to show instead of Home Assistant's.
  #[derive(Debug, Clone, PartialEq, Deserialize)]
  #[serde(untagged)]
  pub enum ControlConfig {
      Id(String),
      Named {
          entity: String,
          #[serde(default)]
          name: Option<String>,
      },
  }

  impl ControlConfig {
      pub fn entity(&self) -> &str {
          match self {
              ControlConfig::Id(id) => id,
              ControlConfig::Named { entity, .. } => entity,
          }
      }

      pub fn name(&self) -> Option<&str> {
          match self {
              ControlConfig::Id(_) => None,
              ControlConfig::Named { name, .. } => name.as_deref(),
          }
      }
  }
  ```
  In `Config::parse`, before `Ok(config)`:
  ```rust
          if let Some(ha) = &config.home_assistant {
              ensure!(
                  ha.url.starts_with("http://"),
                  "home_assistant.url must be the local http:// address"
              );
              for page in &ha.pages {
                  ensure!(!page.name.trim().is_empty(), "a home_assistant page needs a name");
                  for control in &page.controls {
                      ensure!(
                          crate::home::types::Domain::of(control.entity()).is_some(),
                          "{} isn't a light, switch, fan, scene or script",
                          control.entity()
                      );
                  }
              }
          }
  ```
  In `impl Config`:
  ```rust
      /// The Home Assistant token file, when Home Assistant is set up.
      pub fn home_token_file(&self) -> Option<PathBuf> {
          let ha = self.home_assistant.as_ref()?;
          Some(ha.token_file.clone().unwrap_or_else(|| self.state_dir.join("ha-token")))
      }
  ```
  In `home/types.rs`, add above `#[cfg(test)]`:
  ```rust
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
              Page { name: page.name.clone(), tiles, scenes }
          })
          .collect()
  }
  ```

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p ziggy-player config::`
  Expected: all config tests pass (the existing six plus four new).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/src/config.rs crates/ziggy-player/src/home/types.rs
  git commit -m "feat(home): [home_assistant] pages in the device config"
  ```

---

### Task 4: Home Assistant messages (client)

**Files:**
- Modify: `crates/ziggy-player/src/home/client.rs`

- [ ] **Step 1: Write the failing tests** (`client.rs`):
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use serde_json::{Value, json};

      #[test]
      fn frames_parse() {
          assert_eq!(parse(r#"{"type":"auth_required","ha_version":"2026.2.3"}"#), Frame::AuthRequired);
          assert_eq!(parse(r#"{"type":"auth_ok"}"#), Frame::AuthOk);
          assert_eq!(
              parse(r#"{"type":"auth_invalid","message":"Invalid access token"}"#),
              Frame::AuthInvalid("Invalid access token".into())
          );
          assert_eq!(
              parse(r#"{"id":7,"type":"result","success":false,"error":{"code":"not_found","message":"Entity not found"}}"#),
              Frame::Result { id: 7, ok: false, result: Value::Null, error: "Entity not found".into() }
          );
          assert_eq!(
              parse(r#"{"id":1,"type":"event","event":{"r":["light.x"]}}"#),
              Frame::Event { id: 1, event: json!({"r": ["light.x"]}) }
          );
          assert_eq!(parse(r#"{"id":9,"type":"pong"}"#), Frame::Pong);
          assert_eq!(parse("not json"), Frame::Other);
      }

      #[test]
      fn messages_are_what_home_assistant_expects() {
          let v = |s: String| serde_json::from_str::<Value>(&s).unwrap();
          assert_eq!(v(auth("tok")), json!({"type": "auth", "access_token": "tok"}));
          assert_eq!(
              v(subscribe(1, &["light.a".to_string()])),
              json!({"id": 1, "type": "subscribe_entities", "entity_ids": ["light.a"]})
          );
          assert_eq!(
              v(call(5, "light.desk", "turn_on", Some(70))),
              json!({"id": 5, "type": "call_service", "domain": "light", "service": "turn_on",
                     "target": {"entity_id": "light.desk"}, "service_data": {"brightness_pct": 70}})
          );
          assert_eq!(
              v(call(6, "switch.fan", "toggle", None)),
              json!({"id": 6, "type": "call_service", "domain": "switch", "service": "toggle",
                     "target": {"entity_id": "switch.fan"}})
          );
          assert_eq!(v(command(2, "config/area_registry/list")), json!({"id": 2, "type": "config/area_registry/list"}));
      }

      #[test]
      fn the_websocket_lives_under_the_address() {
          assert_eq!(websocket_url("http://192.168.1.69:8123/"), "ws://192.168.1.69:8123/api/websocket");
          assert_eq!(websocket_url("http://ha.local:8123"), "ws://ha.local:8123/api/websocket");
      }
  }
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-player home::client`
  Expected: compile errors.

- [ ] **Step 3: Implement** (`client.rs`, above the tests):
  ```rust
  //! Home Assistant's WebSocket messages: what Ziggy sends and what it reads back.

  use serde_json::{Value, json};

  #[derive(Debug, Clone, PartialEq)]
  pub enum Frame {
      AuthRequired,
      AuthOk,
      AuthInvalid(String),
      /// The answer to a command; `error` is empty when `ok`.
      Result { id: u64, ok: bool, result: Value, error: String },
      /// An event of a subscription, by the subscribing command's id.
      Event { id: u64, event: Value },
      Pong,
      Other,
  }

  pub fn parse(text: &str) -> Frame {
      let Ok(v) = serde_json::from_str::<Value>(text) else {
          return Frame::Other;
      };
      let id = v["id"].as_u64().unwrap_or(0);
      match v["type"].as_str() {
          Some("auth_required") => Frame::AuthRequired,
          Some("auth_ok") => Frame::AuthOk,
          Some("auth_invalid") => Frame::AuthInvalid(v["message"].as_str().unwrap_or_default().into()),
          Some("result") => Frame::Result {
              id,
              ok: v["success"].as_bool().unwrap_or(false),
              result: v["result"].clone(),
              error: v["error"]["message"].as_str().unwrap_or_default().into(),
          },
          Some("event") => Frame::Event { id, event: v["event"].clone() },
          Some("pong") => Frame::Pong,
          _ => Frame::Other,
      }
  }

  pub fn auth(token: &str) -> String {
      json!({"type": "auth", "access_token": token}).to_string()
  }

  pub fn subscribe(id: u64, entity_ids: &[String]) -> String {
      json!({"id": id, "type": "subscribe_entities", "entity_ids": entity_ids}).to_string()
  }

  /// A command without arguments, such as `config/area_registry/list`.
  pub fn command(id: u64, kind: &str) -> String {
      json!({"id": id, "type": kind}).to_string()
  }

  pub fn ping(id: u64) -> String {
      command(id, "ping")
  }

  /// Calls `<domain>.<service>` on one entity, with a brightness for `light.turn_on`.
  pub fn call(id: u64, entity_id: &str, service: &str, brightness_pct: Option<u8>) -> String {
      let domain = entity_id.split_once('.').map_or(entity_id, |(d, _)| d);
      let mut v = json!({
          "id": id,
          "type": "call_service",
          "domain": domain,
          "service": service,
          "target": {"entity_id": entity_id},
      });
      if let Some(pct) = brightness_pct {
          v["service_data"] = json!({"brightness_pct": pct});
      }
      v.to_string()
  }

  /// Config checks made sure the address is `http://`.
  pub fn websocket_url(base: &str) -> String {
      let host = base.trim_start_matches("http://").trim_end_matches('/');
      format!("ws://{host}/api/websocket")
  }
  ```

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p ziggy-player home::client`
  Expected: 3 passed.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/src/home/client.rs
  git commit -m "feat(home): Home Assistant WebSocket messages"
  ```

---

### Task 5: App-core types (`HomeUpdate`, `HomeRequest`, actions, state)

No behaviour yet; this makes the next task's tests compile.

**Files:**
- Modify: `crates/ziggy-player/src/app/input.rs`, `crates/ziggy-player/src/app/state.rs`,
  `crates/ziggy-player/src/app/mod.rs`, `crates/ziggy-player/src/app/tests.rs`,
  `crates/ziggy-player/src/model.rs`, `crates/ziggy-player/src/runtime.rs`

- [ ] **Step 1: `model.rs` — `Section::Home`**

  Add `/// Home Assistant controls, when set up.` `Home,` to `enum Section`, and to its four
  `match`es: `Section::Home => 8`, `8 => Section::Home`, `Section::Home => "Home"`,
  `Section::Home => "section-home"`.

- [ ] **Step 2: `input.rs`**

  In `enum Input`, after `Books(BooksUpdate),`: `Home(HomeUpdate),`. Add after `BooksUpdate`:
  ```rust
  /// What the Home Assistant service reports.
  #[derive(Debug, Clone, PartialEq)]
  pub enum HomeUpdate {
      Connected,
      Disconnected,
      /// Home Assistant refused the token.
      Refused,
      States(Vec<(String, crate::home::types::EntityState)>),
      Removed(Vec<String>),
      /// Area names by entity ID.
      Areas(HashMap<String, String>),
      CallFailed { entity_id: String, message: String },
  }

  #[derive(Debug, Clone, PartialEq)]
  pub enum HomeRequest {
      /// `<domain>.<service>` on one entity, e.g. `turn_on` with a brightness.
      Call { entity_id: String, service: &'static str, brightness_pct: Option<u8> },
      /// Try now instead of waiting out the retry delay (the Wi-Fi came back).
      Reconnect,
  }
  ```
  (add `use std::collections::HashMap;` at the top if it isn't there.)
  In `enum UiAction`, add:
  ```rust
      /// Home: show the page at this index.
      HomePage(usize),
      /// Home: the main part of a tile, by entity ID.
      HomeTile(String),
      /// Home: a light's brightness strip.
      HomeStrip(String),
      HomeScene(String),
      /// Home: the brightness bar is being dragged, 1-100.
      HomeBrightness(u8),
      /// Home: the finger left the brightness bar at this level.
      HomeBrightnessDone(u8),
      HomeBarToggle,
      HomeBarClose,
  ```
  In `enum Effect`, after `Books(BooksRequest),`: `Home(HomeRequest),`.

- [ ] **Step 3: `state.rs`**

  Add `/// The Home Assistant controls.` `Home,` to `enum Screen`. Add after `BooksState`:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
  pub enum HomeLink {
      #[default]
      Connecting,
      Connected,
      Unreachable,
      /// Home Assistant refused the token.
      Refused,
  }

  /// Home Assistant controls; on when there are pages.
  #[derive(Debug, Clone, PartialEq, Default)]
  pub struct HomeState {
      pub pages: Vec<crate::home::types::Page>,
      pub page: usize,
      pub link: HomeLink,
      pub entities: HashMap<String, crate::home::types::EntityState>,
      /// Area names by entity ID, as far as Home Assistant told us.
      pub areas: HashMap<String, String>,
      /// The light whose brightness the bottom bar shows.
      pub selected: Option<String>,
      /// When the bar goes back to the music.
      pub bar_until_ms: u64,
      /// A scene just run, outlined until this time.
      pub ran: Option<(String, u64)>,
  }

  impl HomeState {
      pub fn enabled(&self) -> bool {
          !self.pages.is_empty()
      }
  }
  ```
  Add `pub home: HomeState,` to `AppState` after `books`.

- [ ] **Step 4: `app/mod.rs`**

  - Add `mod home;` next to `mod voice;`.
  - `CoreConfig` gets `/// Home Assistant pages; none turns Home off.` `pub home: Vec<crate::home::types::Page>,`.
  - In `Core::new`'s `AppState { … }`: `home: HomeState { pages: cfg.home.clone(), ..Default::default() },`.
  - `Core` gets two fields, initialised in `Core::new`:
    ```rust
        /// Entity states from before an optimistic tap, until Home Assistant confirms or refuses it.
        home_undo: HashMap<String, Option<crate::home::types::EntityState>>,
        /// When a dragged brightness was last sent.
        home_sent_ms: u64,
    ```
    (`home_undo: HashMap::new(), home_sent_ms: 0,`)
  - In `handle`: `Input::Home(update) => self.on_home(update, now_ms, &mut fx),` after the Books arm.
  - In `on_ui`, before `UiAction::ShowSection(Section::Settings)`:
    ```rust
            UiAction::ShowSection(Section::Home) => {
                self.state.section = Section::Home;
                self.state.back_stack.clear();
                self.state.screen = Screen::Home;
            }
            UiAction::HomePage(_)
            | UiAction::HomeTile(_)
            | UiAction::HomeStrip(_)
            | UiAction::HomeScene(_)
            | UiAction::HomeBrightness(_)
            | UiAction::HomeBrightnessDone(_)
            | UiAction::HomeBarToggle
            | UiAction::HomeBarClose => self.on_home_ui(action, now_ms, fx),
    ```
  - At the end of `handle`, next to the keyboard rule: leaving Home puts the music back.
    ```rust
        if self.state.screen != Screen::Home {
            self.state.home.selected = None;
        }
    ```
  - In `on_tick`, after `self.voice_tick(now_ms, fx);`: `self.home_tick(now_ms);`.
  - In `on_wifi`, after the existing `if back && !self.state.online { … }`:
    ```rust
        if back && self.state.home.link == HomeLink::Unreachable {
            fx.push(Effect::Home(HomeRequest::Reconnect));
        }
    ```
  - Fix any non-exhaustive `match self.state.screen` the compiler points at (e.g. in
    `view.rs::build`, map `Screen::Home => ScreenView::Home` after adding `Home` to
    `enum ScreenView`, and `ScreenView::Home => ScreenKind::Home` in `ui_bridge.rs` after Task 9
    adds the Slint enum value — until then use `ScreenView::Home => ScreenKind::Grid` with a
    `// Task 9` comment and replace it there).
- [ ] **Step 5: `app/home.rs`** — stubs so it builds:
  ```rust
  //! Home Assistant controls in the core: which tap does what, the brightness bar, optimistic taps.

  use super::*;

  impl Core {
      pub(super) fn on_home(&mut self, _update: HomeUpdate, _now_ms: u64, _fx: &mut Vec<Effect>) {}
      pub(super) fn on_home_ui(&mut self, _action: UiAction, _now_ms: u64, _fx: &mut Vec<Effect>) {}
      pub(super) fn home_tick(&mut self, _now_ms: u64) {}
  }
  ```
- [ ] **Step 6: Callers of `CoreConfig`**

  `app/tests.rs` `config()`: add `home: Vec::new(),`. `runtime.rs` where `CoreConfig { … }` is
  built: add `home: Vec::new(),` for now (Task 8 fills it). `runtime.rs` `dispatch`: add
  `Effect::Home(_) => {}` for now.

- [ ] **Step 7: Build and run all tests**

  Run: `cargo test -p ziggy-player`
  Expected: everything passes as before (234+ tests).

- [ ] **Step 8: Commit**
  ```bash
  git add -A crates/ziggy-player/src
  git commit -m "feat(home): Home screen, section, inputs and effects in the core"
  ```

---

### Task 6: Core rules for taps, the bar and optimistic changes

**Files:**
- Modify: `crates/ziggy-player/src/app/home.rs`

- [ ] **Step 1: Write the failing tests** (end of `app/home.rs`):
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::app::tests::{config, ui};
      use crate::home::types::{Control, Domain, EntityState, Page};

      fn control(id: &str) -> Control {
          Control { entity_id: id.into(), domain: Domain::of(id).unwrap(), name: None }
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
          c.handle(Input::Home(HomeUpdate::States(vec![(id.into(), EntityState {
              state: state.into(), name: None, brightness, color: None })])), 0);
      }

      fn call(id: &str, service: &'static str, pct: Option<u8>) -> Effect {
          Effect::Home(HomeRequest::Call { entity_id: id.into(), service, brightness_pct: pct })
      }

      #[test]
      fn tapping_an_off_light_turns_it_on_without_opening_the_bar() {
          let mut c = home();
          set(&mut c, "light.desk", "off", None);
          let fx = c.handle(ui(UiAction::HomeTile("light.desk".into())), 1_000);
          assert!(fx.contains(&call("light.desk", "turn_on", None)));
          assert!(c.state().home.entities["light.desk"].is_on(), "shown on at once");
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
          assert_eq!(c.state().home.entities["light.desk"].percent(), Some(45), "the bar follows the finger");
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
          c.handle(Input::Tick, 12_000);
          assert!(c.state().home.selected.is_some(), "touching the bar keeps it");
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
          c.handle(Input::Home(HomeUpdate::CallFailed {
              entity_id: "switch.fan".into(), message: "Entity not found".into() }), 1_100);
          assert!(!c.state().home.entities["switch.fan"].is_on());
          assert_eq!(c.state().notice, Some(Notice::Home("Entity not found".into())));
      }

      #[test]
      fn nothing_happens_while_unreachable_or_unavailable() {
          let mut c = home();
          set(&mut c, "switch.fan", "unavailable", None);
          assert!(c.handle(ui(UiAction::HomeTile("switch.fan".into())), 1_000).is_empty());
          set(&mut c, "light.desk", "off", None);
          c.handle(Input::Home(HomeUpdate::Disconnected), 1_000);
          assert!(c.handle(ui(UiAction::HomeTile("light.desk".into())), 1_000).is_empty());
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
          assert!(c.state().home.selected.is_some(), "the bar stays to turn it back on");
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
          let fx = c.handle(Input::Wifi(Some(WifiStatus::Connected {
              ssid: "Home".into(), signal: 70, band: None })), 1_000);
          assert!(fx.contains(&Effect::Home(HomeRequest::Reconnect)));
      }
  }
  ```
  Check `WifiStatus::Connected`'s real fields in `app/input.rs` (around line 37) and adjust the last
  test's literal to match.

- [ ] **Step 2: Add `Notice::Home`**

  In `state.rs` `enum Notice`: `/// Home Assistant refused a change; the tile has been put back.`
  `Home(String),`. In `view.rs` `fn banner`, add
  `Some(Notice::Home(message)) => Some(format!("Home Assistant: {message}")),`.

- [ ] **Step 3: Run the tests to see them fail**

  Run: `cargo test -p ziggy-player app::home`
  Expected: most fail (the stubs do nothing).

- [ ] **Step 4: Implement** — replace the stubs in `app/home.rs`:
  ```rust
  use crate::home::types::{Domain, EntityState, brightness_of};

  /// How long the bottom bar shows a light's brightness after the last touch.
  const BAR_MS: u64 = 8_000;
  /// How often a dragged brightness is sent to Home Assistant.
  const DRAG_SEND_MS: u64 = 250;
  /// How long a scene button stays outlined after a tap.
  const RAN_MS: u64 = 4_000;

  impl Core {
      pub(super) fn on_home(&mut self, update: HomeUpdate, now_ms: u64, _fx: &mut Vec<Effect>) {
          let home = &mut self.state.home;
          match update {
              HomeUpdate::Connected => home.link = HomeLink::Connected,
              HomeUpdate::Disconnected => home.link = HomeLink::Unreachable,
              HomeUpdate::Refused => home.link = HomeLink::Refused,
              HomeUpdate::States(states) => {
                  for (id, state) in states {
                      // Home Assistant's word is final, so the tap needs no undoing.
                      self.home_undo.remove(&id);
                      home.entities.insert(id, state);
                  }
              }
              HomeUpdate::Removed(ids) => {
                  for id in ids {
                      home.entities.remove(&id);
                  }
              }
              HomeUpdate::Areas(areas) => home.areas = areas,
              HomeUpdate::CallFailed { entity_id, message } => {
                  match self.home_undo.remove(&entity_id) {
                      Some(Some(before)) => {
                          self.state.home.entities.insert(entity_id, before);
                      }
                      Some(None) => {
                          self.state.home.entities.remove(&entity_id);
                      }
                      None => {}
                  }
                  self.notify(Notice::Home(message), now_ms);
              }
          }
      }

      pub(super) fn on_home_ui(&mut self, action: UiAction, now_ms: u64, fx: &mut Vec<Effect>) {
          match action {
              UiAction::HomePage(i) => {
                  if i < self.state.home.pages.len() {
                      self.state.home.page = i;
                      self.state.home.selected = None;
                  }
              }
              UiAction::HomeTile(id) => {
                  let Some(state) = self.usable(&id) else { return };
                  match Domain::of(&id) {
                      Some(Domain::Light) if state.is_on() => {
                          self.home_call(&id, "turn_off", None, fx);
                          if self.state.home.selected.as_deref() == Some(id.as_str()) {
                              self.state.home.selected = None;
                          }
                      }
                      Some(Domain::Light) => self.home_call(&id, "turn_on", None, fx),
                      Some(Domain::Switch | Domain::Fan) => self.home_call(&id, "toggle", None, fx),
                      _ => {}
                  }
              }
              UiAction::HomeStrip(id) => {
                  let Some(state) = self.usable(&id) else { return };
                  if state.is_on() {
                      self.state.home.selected = Some(id);
                      self.state.home.bar_until_ms = now_ms + BAR_MS;
                  } else {
                      self.home_call(&id, "turn_on", None, fx);
                  }
              }
              UiAction::HomeScene(id) => {
                  if self.state.home.link != HomeLink::Connected {
                      return;
                  }
                  self.home_call(&id, "turn_on", None, fx);
                  self.state.home.ran = Some((id, now_ms + RAN_MS));
              }
              UiAction::HomeBrightness(pct) | UiAction::HomeBrightnessDone(pct) => {
                  let done = matches!(action, UiAction::HomeBrightnessDone(_));
                  let Some(id) = self.state.home.selected.clone() else { return };
                  if self.usable(&id).is_none() {
                      return;
                  }
                  self.state.home.bar_until_ms = now_ms + BAR_MS;
                  let pct = pct.clamp(1, 100);
                  if done || now_ms.saturating_sub(self.home_sent_ms) >= DRAG_SEND_MS {
                      self.home_sent_ms = now_ms;
                      self.home_call(&id, "turn_on", Some(pct), fx);
                  } else {
                      // Not sent yet, but the bar follows the finger.
                      self.home_show(&id, true, Some(pct));
                  }
              }
              UiAction::HomeBarToggle => {
                  let Some(id) = self.state.home.selected.clone() else { return };
                  let Some(state) = self.usable(&id) else { return };
                  self.state.home.bar_until_ms = now_ms + BAR_MS;
                  let service = if state.is_on() { "turn_off" } else { "turn_on" };
                  self.home_call(&id, service, None, fx);
              }
              UiAction::HomeBarClose => self.state.home.selected = None,
              _ => {}
          }
      }

      pub(super) fn home_tick(&mut self, now_ms: u64) {
          let home = &mut self.state.home;
          if home.selected.is_some() && now_ms >= home.bar_until_ms {
              home.selected = None;
          }
          if home.ran.as_ref().is_some_and(|(_, until)| now_ms >= *until) {
              home.ran = None;
          }
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
      fn home_call(&mut self, id: &str, service: &'static str, pct: Option<u8>, fx: &mut Vec<Effect>) {
          let on = match service {
              "turn_on" => Some(true),
              "turn_off" => Some(false),
              "toggle" => self.state.home.entities.get(id).map(|s| !s.is_on()),
              _ => None,
          };
          if let Some(on) = on {
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
          self.home_undo.entry(id.to_string()).or_insert_with(|| before.clone());
          let mut state = before.unwrap_or(EntityState { state: String::new(), name: None, brightness: None, color: None });
          state.state = if on { "on" } else { "off" }.into();
          state.brightness = match (on, pct) {
              (false, _) => None,
              (true, Some(pct)) => Some(brightness_of(pct)),
              (true, None) => state.brightness.or(Some(255)),
          };
          self.state.home.entities.insert(id.to_string(), state);
      }
  }
  ```
  Remove the now-unused `_fx` underscore where the compiler warns.

- [ ] **Step 5: Run the tests**

  Run: `cargo test -p ziggy-player app::home`
  Expected: 11 passed. Then `cargo test -p ziggy-player` — all pass.

- [ ] **Step 6: Commit**
  ```bash
  git add crates/ziggy-player/src/app crates/ziggy-player/src/view.rs
  git commit -m "feat(home): what each tap does, the brightness bar and optimistic changes"
  ```

---

### Task 7: The Home Assistant service

**Files:**
- Modify: `crates/ziggy-player/src/home/service.rs`

- [ ] **Step 1: Write the failing tests** (`service.rs`):
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn retries_back_off_to_a_minute() {
          let mut d = RETRY_FIRST;
          let mut seen = vec![d.as_secs()];
          for _ in 0..7 {
              d = next_retry(d);
              seen.push(d.as_secs());
          }
          assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60, 60]);
      }

      #[tokio::test]
      async fn the_fake_answers_taps_with_new_states() {
          let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
          let dir = tempfile::tempdir().unwrap();
          let cache = Arc::new(DiskCache::new(dir.path().to_path_buf()).unwrap());
          let requests = spawn_home(Backend::Fake, Vec::new(), cache, tx);
          let mut connected = false;
          while let Some(Input::Home(update)) = rx.recv().await {
              if update == HomeUpdate::Connected {
                  connected = true;
                  break;
              }
          }
          assert!(connected);
          requests.send(HomeRequest::Call {
              entity_id: "light.micah_desk".into(), service: "turn_on", brightness_pct: Some(40) }).unwrap();
          loop {
              if let Some(Input::Home(HomeUpdate::States(states))) = rx.recv().await
                  && let Some((_, s)) = states.iter().find(|(id, _)| id == "light.micah_desk")
                  && s.is_on()
              {
                  assert_eq!(s.percent(), Some(40));
                  break;
              }
          }
      }
  }
  ```
  `DiskCache::new` is the constructor used in `library/cache.rs`; check its signature and adjust.

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-player home::service`
  Expected: compile errors.

- [ ] **Step 3: Implement** (`service.rs`):
  ```rust
  //! Keeps a WebSocket to Home Assistant open: subscribes to the configured entities, reads area
  //! names once, sends calls, and reconnects after drops. `--fake` uses a pretend house instead.

  use std::collections::HashMap;
  use std::sync::Arc;
  use std::time::{Duration, Instant};

  use futures_util::{SinkExt, StreamExt};
  use serde_json::Value;
  use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
  use tokio_tungstenite::tungstenite::Message;

  use super::client::{self, Frame};
  use super::types::{Control, Domain, EntityState, Page, States, area_names, brightness_of};
  use crate::app::{HomeRequest, HomeUpdate, Input};
  use crate::library::cache::DiskCache;

  pub const RETRY_FIRST: Duration = Duration::from_secs(2);
  const RETRY_MOST: Duration = Duration::from_secs(60);
  /// A connection that answers no ping for this long is dead.
  const PING_EVERY: Duration = Duration::from_secs(30);
  const STATES_KEY: &str = "home-states";
  const AREAS_KEY: &str = "home-areas";

  pub enum Backend {
      Real { url: String, token: String },
      /// `--fake`: a pretend house, see `fake_pages`.
      Fake,
  }

  pub fn next_retry(d: Duration) -> Duration {
      (d * 2).min(RETRY_MOST)
  }

  pub fn spawn_home(
      backend: Backend,
      entity_ids: Vec<String>,
      cache: Arc<DiskCache>,
      inputs: UnboundedSender<Input>,
  ) -> UnboundedSender<HomeRequest> {
      let (tx, rx) = mpsc::unbounded_channel();
      match backend {
          Backend::Real { url, token } => {
              tokio::spawn(run(client::websocket_url(&url), token, entity_ids, cache, inputs, rx));
          }
          Backend::Fake => {
              tokio::spawn(run_fake(inputs, rx));
          }
      }
      tx
  }

  fn send(inputs: &UnboundedSender<Input>, update: HomeUpdate) {
      let _ = inputs.send(Input::Home(update));
  }

  enum End {
      RequestsClosed,
      Refused,
  }

  async fn run(
      url: String,
      token: String,
      ids: Vec<String>,
      cache: Arc<DiskCache>,
      inputs: UnboundedSender<Input>,
      mut requests: UnboundedReceiver<HomeRequest>,
  ) {
      // Names and states from last time, so tiles have names before Home Assistant answers.
      if let Some(states) = cache.read::<Vec<(String, EntityState)>>(STATES_KEY) {
          send(&inputs, HomeUpdate::States(states));
      }
      if let Some(areas) = cache.read::<HashMap<String, String>>(AREAS_KEY) {
          send(&inputs, HomeUpdate::Areas(areas));
      }
      let mut retry = RETRY_FIRST;
      loop {
          let started = Instant::now();
          match session(&url, &token, &ids, &cache, &inputs, &mut requests).await {
              Ok(End::RequestsClosed) => return,
              Ok(End::Refused) => {
                  tracing::warn!("Home Assistant refused the token");
                  send(&inputs, HomeUpdate::Refused);
                  // A new token means `ziggy config`, which restarts the player.
                  while let Some(request) = requests.recv().await {
                      refuse(&inputs, request);
                  }
                  return;
              }
              Err(e) => {
                  tracing::warn!("Home Assistant: {e:#}");
                  send(&inputs, HomeUpdate::Disconnected);
              }
          }
          if started.elapsed() > RETRY_MOST {
              retry = RETRY_FIRST;
          }
          // Wait out the delay, or less when asked to try now.
          let wake = tokio::time::sleep(retry);
          tokio::pin!(wake);
          loop {
              tokio::select! {
                  _ = &mut wake => break,
                  request = requests.recv() => match request {
                      None => return,
                      Some(HomeRequest::Reconnect) => break,
                      Some(other) => refuse(&inputs, other),
                  },
              }
          }
          retry = next_retry(retry);
      }
  }

  fn refuse(inputs: &UnboundedSender<Input>, request: HomeRequest) {
      if let HomeRequest::Call { entity_id, .. } = request {
          send(inputs, HomeUpdate::CallFailed {
              entity_id,
              message: "not connected".into(),
          });
      }
  }

  async fn session(
      url: &str,
      token: &str,
      ids: &[String],
      cache: &DiskCache,
      inputs: &UnboundedSender<Input>,
      requests: &mut UnboundedReceiver<HomeRequest>,
  ) -> anyhow::Result<End> {
      let (ws, _) = tokio_tungstenite::connect_async(url).await?;
      let (mut tx, mut rx) = ws.split();
      loop {
          let Some(message) = rx.next().await else {
              anyhow::bail!("closed while signing in");
          };
          let Message::Text(text) = message? else { continue };
          match client::parse(&text) {
              Frame::AuthRequired => tx.send(Message::text(client::auth(token))).await?,
              Frame::AuthOk => break,
              Frame::AuthInvalid(_) => return Ok(End::Refused),
              _ => {}
          }
      }
      send(inputs, HomeUpdate::Connected);
      const SUBSCRIPTION: u64 = 1;
      const REGISTRIES: [(u64, &str); 3] = [
          (2, "config/entity_registry/list_for_display"),
          (3, "config/device_registry/list"),
          (4, "config/area_registry/list"),
      ];
      tx.send(Message::text(client::subscribe(SUBSCRIPTION, ids))).await?;
      for (id, kind) in REGISTRIES {
          tx.send(Message::text(client::command(id, kind))).await?;
      }
      let mut next_id = 5u64;
      let mut states = States::default();
      let mut registries: HashMap<u64, Value> = HashMap::new();
      let mut calls: HashMap<u64, String> = HashMap::new();
      let mut ping = tokio::time::interval(PING_EVERY);
      let mut awaiting_pong = false;
      loop {
          tokio::select! {
              message = rx.next() => {
                  let Some(message) = message else { anyhow::bail!("Home Assistant closed the connection") };
                  let Message::Text(text) = message? else { continue };
                  match client::parse(&text) {
                      Frame::Event { id: SUBSCRIPTION, event } => {
                          let (changed, removed) = states.apply(&event);
                          if !changed.is_empty() {
                              send(inputs, HomeUpdate::States(changed));
                          }
                          if !removed.is_empty() {
                              send(inputs, HomeUpdate::Removed(removed));
                          }
                          let _ = cache.write(STATES_KEY, &states.snapshot());
                      }
                      Frame::Result { id, ok, result, error } if (2..=4).contains(&id) => {
                          if !ok {
                              tracing::info!("Home Assistant didn't list {id}: {error}");
                          }
                          registries.insert(id, if ok { result } else { Value::Null });
                          if registries.len() == REGISTRIES.len() {
                              let areas = area_names(&registries[&2], &registries[&3], &registries[&4]);
                              let _ = cache.write(AREAS_KEY, &areas);
                              send(inputs, HomeUpdate::Areas(areas));
                          }
                      }
                      Frame::Result { id, ok, error, .. } => {
                          if let Some(entity_id) = calls.remove(&id)
                              && !ok
                          {
                              send(inputs, HomeUpdate::CallFailed { entity_id, message: error });
                          }
                      }
                      Frame::Pong => awaiting_pong = false,
                      _ => {}
                  }
              }
              request = requests.recv() => match request {
                  None => return Ok(End::RequestsClosed),
                  Some(HomeRequest::Reconnect) => {}
                  Some(HomeRequest::Call { entity_id, service, brightness_pct }) => {
                      let id = next_id;
                      next_id += 1;
                      tx.send(Message::text(client::call(id, &entity_id, service, brightness_pct))).await?;
                      calls.insert(id, entity_id);
                  }
              },
              _ = ping.tick() => {
                  anyhow::ensure!(!awaiting_pong, "Home Assistant stopped answering");
                  awaiting_pong = true;
                  let id = next_id;
                  next_id += 1;
                  tx.send(Message::text(client::ping(id))).await?;
              }
          }
      }
  }

  /// The pretend house `--fake` shows when the config has no Home Assistant pages.
  pub fn fake_pages() -> Vec<Page> {
      let c = |id: &str| Control { entity_id: id.into(), domain: Domain::of(id).unwrap(), name: None };
      vec![
          Page {
              name: "Micah's room".into(),
              tiles: ["light.micah_ceiling", "light.micah_desk", "light.micah_night", "switch.fairy_lights", "fan.micah"]
                  .map(c).to_vec(),
              scenes: vec![c("scene.bedtime"), c("script.reading")],
          },
          Page {
              name: "Downstairs".into(),
              tiles: ["light.kitchen", "light.sofa_lamp", "switch.tv", "light.porch"].map(c).to_vec(),
              scenes: vec![c("scene.movie")],
          },
      ]
  }

  async fn run_fake(inputs: UnboundedSender<Input>, mut requests: UnboundedReceiver<HomeRequest>) {
      let names = [
          ("light.micah_ceiling", "Ceiling light", "Micah's room", "on", Some(178)),
          ("light.micah_desk", "Desk lamp", "Micah's room", "off", None),
          ("light.micah_night", "Night light", "Micah's room", "on", Some(38)),
          ("switch.fairy_lights", "Fairy lights", "Micah's room", "on", None),
          ("fan.micah", "Fan", "Micah's room", "off", None),
          ("light.kitchen", "Kitchen lights", "Kitchen", "on", Some(230)),
          ("light.sofa_lamp", "Sofa lamp", "Living room", "on", Some(100)),
          ("switch.tv", "TV plug", "Living room", "off", None),
          ("light.porch", "Porch light", "Outside", "off", None),
      ];
      let mut house: HashMap<String, EntityState> = names
          .iter()
          .map(|(id, name, _, state, b)| {
              (id.to_string(), EntityState { state: state.to_string(), name: Some(name.to_string()), brightness: *b, color: None })
          })
          .collect();
      let areas = names.iter().map(|(id, _, area, _, _)| (id.to_string(), area.to_string())).collect();
      send(&inputs, HomeUpdate::Connected);
      send(&inputs, HomeUpdate::Areas(areas));
      send(&inputs, HomeUpdate::States(house.clone().into_iter().collect()));
      while let Some(request) = requests.recv().await {
          let HomeRequest::Call { entity_id, service, brightness_pct } = request else { continue };
          let Some(state) = house.get_mut(&entity_id) else {
              if Domain::of(&entity_id).is_some_and(|d| !d.is_tile()) {
                  continue; // Scenes and scripts just run.
              }
              send(&inputs, HomeUpdate::CallFailed { entity_id, message: "Entity not found".into() });
              continue;
          };
          let on = match service {
              "turn_on" => true,
              "turn_off" => false,
              _ => !state.is_on(),
          };
          state.state = if on { "on" } else { "off" }.into();
          state.brightness = match (on, brightness_pct) {
              (false, _) => None,
              (true, Some(pct)) => Some(brightness_of(pct)),
              (true, None) if entity_id.starts_with("light.") => state.brightness.or(Some(255)),
              (true, None) => None,
          };
          send(&inputs, HomeUpdate::States(vec![(entity_id, state.clone())]));
      }
  }
  ```
  If `DiskCache::read`/`write` take different arguments, follow `audiobooks/service.rs`, which uses
  `cache.read::<T>(key)` and `cache.write(key, &value)`.

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p ziggy-player home::service`
  Expected: 2 passed.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/src/home/service.rs
  git commit -m "feat(home): keep a connection to Home Assistant, and a pretend house for --fake"
  ```

---

### Task 8: Wire the service into the runtime

**Files:**
- Modify: `crates/ziggy-player/src/runtime.rs`

- [ ] **Step 1: Build the pages and spawn the service**

  After the Books service is spawned (around line 115), add:
  ```rust
      // Home Assistant: on when the config lists pages, or a pretend house in `--fake`.
      let home_pages = match &config.home_assistant {
          Some(ha) => crate::home::types::pages(ha),
          None if fake => crate::home::service::fake_pages(),
          None => Vec::new(),
      };
      let home = if home_pages.is_empty() {
          None
      } else if fake && config.home_assistant.is_none() {
          Some(crate::home::service::spawn_home(
              crate::home::service::Backend::Fake,
              Vec::new(),
              cache.clone(),
              inputs.clone(),
          ))
      } else {
          let token_file = config.home_token_file().expect("set with home_assistant");
          match std::fs::read_to_string(&token_file) {
              Ok(token) => {
                  let ids = home_pages
                      .iter()
                      .flat_map(|p| p.tiles.iter().chain(&p.scenes))
                      .map(|c| c.entity_id.clone())
                      .collect();
                  Some(crate::home::service::spawn_home(
                      crate::home::service::Backend::Real {
                          url: config.home_assistant.as_ref().expect("checked").url.clone(),
                          token: token.trim().to_string(),
                      },
                      ids,
                      cache.clone(),
                      inputs.clone(),
                  ))
              }
              Err(e) => {
                  tracing::warn!("no Home Assistant token at {} ({e}); run `ziggy config`", token_file.display());
                  let _ = inputs.send(Input::Home(crate::app::HomeUpdate::Refused));
                  None
              }
          }
      };
  ```
  `cache` is moved into `spawn_library` further down; if the compiler complains, the `.clone()`s
  above are enough because they come first.

- [ ] **Step 2: Pass the pages to the core and the sender to dispatch**

  In the `CoreConfig { … }` literal replace `home: Vec::new(),` with `home: home_pages,`.
  Add `home: Option<UnboundedSender<crate::app::HomeRequest>>,` to `struct Outputs`, set it
  (`home,`) where `Outputs { … }` is built, and in `dispatch` replace `Effect::Home(_) => {}` with:
  ```rust
              Effect::Home(request) => {
                  if let Some(home) = &outputs.home {
                      let _ = home.send(request);
                  }
              }
  ```

- [ ] **Step 3: Build and test**

  Run: `cargo test -p ziggy-player`
  Expected: all pass, including `fake_runtime_loads_library_and_plays`.

- [ ] **Step 4: Commit**
  ```bash
  git add crates/ziggy-player/src/runtime.rs
  git commit -m "feat(home): start the Home Assistant service when pages are configured"
  ```

---

### Task 9: The view

**Files:**
- Modify: `crates/ziggy-player/src/view.rs`

- [ ] **Step 1: Write the failing tests** (in `view.rs`'s `mod tests`):
  ```rust
  mod home_view {
      use super::*;
      use crate::app::{HomeUpdate, Input, UiAction};
      use crate::home::types::{Control, Domain, EntityState, Page};

      fn c(id: &str) -> Control {
          Control { entity_id: id.into(), domain: Domain::of(id).unwrap(), name: None }
      }

      fn core_with(pages: Vec<Page>) -> crate::app::Core {
          let mut cfg = crate::app::tests::config();
          cfg.home = pages;
          let mut core = crate::app::Core::new(cfg, 0).0;
          core.handle(Input::Home(HomeUpdate::Connected), 0);
          core.handle(Input::Ui(UiAction::ShowSection(Section::Home)), 0);
          core
      }

      fn states(core: &mut crate::app::Core, list: &[(&str, &str, &str, Option<u8>)]) {
          core.handle(Input::Home(HomeUpdate::States(list.iter().map(|(id, name, s, b)| {
              (id.to_string(), EntityState { state: s.to_string(), name: Some(name.to_string()), brightness: *b, color: None })
          }).collect())), 0);
      }

      #[test]
      fn a_dim_bulb_glows_darker_than_a_bright_one() {
          let (bright, dark_text) = glow([255, 255, 255], 100);
          assert_eq!((bright, dark_text), ([255, 255, 255], true));
          let (dim, dark_text) = glow([255, 255, 255], 10);
          assert!(dim[0] < 100 && !dark_text);
      }

      #[test]
      fn tiles_show_names_states_and_strips() {
          let mut core = core_with(vec![Page {
              name: "Room".into(),
              tiles: vec![c("light.desk"), c("switch.fan"), c("light.gone")],
              scenes: vec![c("scene.bedtime")],
          }]);
          states(&mut core, &[("light.desk", "Desk lamp", "on", Some(178)), ("switch.fan", "Fan", "off", None)]);
          let v = build(core.state());
          assert!(v.home_enabled);
          assert_eq!(v.home.pages, vec!["Room".to_string()]);
          let desk = &v.home.tiles[0];
          assert_eq!((desk.name.as_str(), desk.on, desk.split, desk.strip.as_str(), desk.sun), ("Desk lamp", true, true, "70%", true));
          assert_eq!(desk.glow, Some(glow(WARM_WHITE, 70).0), "a plain dimmable bulb glows warm white");
          assert_eq!(desk.line, "", "a one-room page doesn't repeat the room");
          let fan = &v.home.tiles[1];
          assert_eq!((fan.split, fan.line.as_str()), (false, "Off"));
          let gone = &v.home.tiles[2];
          assert_eq!((gone.enabled, gone.line.as_str()), (false, "Not in Home Assistant"));
          assert_eq!(v.home.scenes[0].name, "scene.bedtime", "unknown scenes show their ID");
      }

      #[test]
      fn mixed_pages_show_rooms_and_the_bar_shows_the_selected_light() {
          let mut core = core_with(vec![Page { name: "Everyday".into(), tiles: vec![c("light.porch"), c("light.desk")], scenes: vec![] }]);
          states(&mut core, &[("light.porch", "Porch", "off", None), ("light.desk", "Desk", "on", Some(255))]);
          core.handle(Input::Home(HomeUpdate::Areas([("light.porch".to_string(), "Outside".to_string()), ("light.desk".to_string(), "Micah's room".to_string())].into())), 0);
          let v = build(core.state());
          assert_eq!(v.home.tiles[0].line, "Outside");
          assert_eq!(v.home.tiles[0].strip, "Off");
      assert_eq!(v.home.tiles[0].glow, None, "an off light has no glow");
          core.handle(Input::Ui(UiAction::HomeStrip("light.desk".into())), 0);
          let bar = build(core.state()).home.bar.unwrap();
          assert_eq!((bar.name.as_str(), bar.level, bar.level_text.as_str(), bar.on), ("Desk", 100, "100%", true));
      }

      #[test]
      fn unreachable_and_refused_say_so() {
          let mut core = core_with(vec![Page { name: "Room".into(), tiles: vec![c("light.desk")], scenes: vec![] }]);
          core.handle(Input::Home(HomeUpdate::Disconnected), 0);
          let v = build(core.state());
          assert_eq!(v.home.banner, "Can't reach Home Assistant. Trying again…");
          assert!(!v.home.tiles[0].enabled);
          core.handle(Input::Home(HomeUpdate::Refused), 0);
          assert!(build(core.state()).home.banner.contains("ziggy config"));
      }
  }
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-player view::tests::home_view`
  Expected: compile errors.

- [ ] **Step 3: Implement**

  Add to `view.rs` (near `BooksView`):
  ```rust
  #[derive(Debug, Clone, PartialEq, Default)]
  pub struct HomeView {
      pub pages: Vec<String>,
      pub page: usize,
      pub tiles: Vec<HomeTileView>,
      pub scenes: Vec<HomeSceneView>,
      /// Why the controls don't work right now; empty when they do.
      pub banner: String,
      /// The brightness bar, while a light is selected.
      pub bar: Option<HomeBarView>,
  }

  #[derive(Debug, Clone, PartialEq)]
  pub struct HomeTileView {
      pub id: String,
      pub name: String,
      /// Under the name: the room on a mixed page, the state of a switch, or why it can't be used.
      pub line: String,
      pub icon: HomeIcon,
      pub on: bool,
      /// Lights have a brightness strip on the right.
      pub split: bool,
      /// "Off", or the brightness: "70%".
      pub strip: String,
      /// The strip's colour while on: the bulb's colour, darker as it dims.
      pub glow: Option<[u8; 3]>,
      /// The glow is pale, so the strip's icon and text are dark.
      pub glow_dark_text: bool,
      pub sun: bool,
      pub selected: bool,
      pub enabled: bool,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum HomeIcon {
      Light,
      Plug,
      Fan,
  }

  #[derive(Debug, Clone, PartialEq)]
  pub struct HomeSceneView {
      pub id: String,
      pub name: String,
      pub ran: bool,
      pub enabled: bool,
  }

  #[derive(Debug, Clone, PartialEq)]
  pub struct HomeBarView {
      pub name: String,
      /// 0-100; 0 while off.
      pub level: u8,
      pub level_text: String,
      pub on: bool,
  }

  /// About 2700 K.
  const WARM_WHITE: [u8; 3] = [255, 169, 87];
  /// Ziggy's darkest background, which a dim bulb's colour fades toward.
  const GLOW_BASE: [u8; 3] = [20, 18, 31];

  /// A bulb's colour at a brightness, and whether it is pale enough to want dark text on it.
  fn glow(rgb: [u8; 3], percent: u8) -> ([u8; 3], bool) {
      let f = 0.25 + 0.75 * f32::from(percent.min(100)) / 100.0;
      let c: [u8; 3] = std::array::from_fn(|i| {
          let (base, v) = (f32::from(GLOW_BASE[i]), f32::from(rgb[i]));
          (base + (v - base) * f).round() as u8
      });
      let light = (0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])) / 255.0;
      (c, light > 0.5)
  }

  fn home(state: &AppState) -> HomeView {
      use crate::app::HomeLink;
      use crate::home::types::Domain;
      let h = &state.home;
      let Some(page) = h.pages.get(h.page) else {
          return HomeView::default();
      };
      let connected = h.link == HomeLink::Connected;
      let name_of = |c: &crate::home::types::Control| {
          c.name
              .clone()
              .or_else(|| h.entities.get(&c.entity_id).and_then(|s| s.name.clone()))
              .unwrap_or_else(|| c.entity_id.clone())
      };
      let rooms: std::collections::HashSet<&String> =
          page.tiles.iter().filter_map(|c| h.areas.get(&c.entity_id)).collect();
      let mixed = rooms.len() > 1;
      let tiles = page
          .tiles
          .iter()
          .map(|c| {
              let state = h.entities.get(&c.entity_id);
              let on = state.is_some_and(|s| s.is_on());
              let light = c.domain == Domain::Light;
              let room = if mixed { h.areas.get(&c.entity_id).cloned() } else { None };
              let words = if on { "On" } else { "Off" };
              // Bulbs that only dim are drawn warm white.
              let glow = state
                  .filter(|_| on && light && connected)
                  .map(|s| glow(s.color.unwrap_or(WARM_WHITE), s.percent().unwrap_or(100)));
              let (line, enabled) = match state {
                  None if connected => ("Not in Home Assistant".to_string(), false),
                  Some(s) if !s.available() => ("Unavailable".to_string(), false),
                  _ if light => (room.unwrap_or_default(), connected),
                  _ => (room.map_or(words.to_string(), |r| format!("{r} · {words}")), connected),
              };
              HomeTileView {
                  id: c.entity_id.clone(),
                  name: name_of(c),
                  line,
                  icon: match c.domain {
                      Domain::Fan => HomeIcon::Fan,
                      Domain::Switch => HomeIcon::Plug,
                      _ => HomeIcon::Light,
                  },
                  on: on && enabled,
                  split: light,
                  strip: match state.and_then(|s| s.percent()) {
                      Some(pct) if on => format!("{pct}%"),
                      _ if on => "On".into(),
                      _ => "Off".into(),
                  },
                  glow: glow.map(|(rgb, _)| rgb),
                  glow_dark_text: glow.is_some_and(|(_, dark)| dark),
                  sun: on && enabled,
                  selected: h.selected.as_deref() == Some(c.entity_id.as_str()),
                  enabled,
              }
          })
          .collect();
      let scenes = page
          .scenes
          .iter()
          .map(|c| HomeSceneView {
              id: c.entity_id.clone(),
              name: name_of(c),
              ran: h.ran.as_ref().is_some_and(|(id, _)| id == &c.entity_id),
              enabled: connected,
          })
          .collect();
      let bar = h.selected.as_ref().and_then(|id| {
          let s = h.entities.get(id)?;
          let control = page.tiles.iter().find(|c| &c.entity_id == id)?;
          let level = if s.is_on() { s.percent().unwrap_or(100) } else { 0 };
          Some(HomeBarView {
              name: name_of(control),
              level,
              level_text: if s.is_on() { format!("{level}%") } else { "Off".into() },
              on: s.is_on(),
          })
      });
      HomeView {
          pages: h.pages.iter().map(|p| p.name.clone()).collect(),
          page: h.page,
          tiles,
          scenes,
          banner: match h.link {
              HomeLink::Connected | HomeLink::Connecting => String::new(),
              HomeLink::Unreachable => "Can't reach Home Assistant. Trying again…".into(),
              HomeLink::Refused => {
                  "Home Assistant refused Ziggy's sign-in. Run `ziggy config`.".into()
              }
          },
          bar,
      }
  }
  ```
  Add to `struct View`: `/// The rail shows Home.` `pub home_enabled: bool,` and `pub home: HomeView,`,
  and in `build`'s `View { … }`: `home_enabled: state.home.enabled(), home: home(state),`. Make sure
  `ScreenView::Home` exists and `Screen::Home => ScreenView::Home` (from Task 5).

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p ziggy-player view::`
  Expected: all pass.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/src/view.rs
  git commit -m "feat(home): what the Home screen shows"
  ```

---

### Task 10: Icons, Slint types and the rail

**Files:**
- Create: `crates/ziggy-player/ui/icons/{home,bulb,plug,fan,sun,sparkle}.svg`
- Modify: `crates/ziggy-player/ui/types.slint`, `crates/ziggy-player/ui/components/rail.slint`

- [ ] **Step 1: Icons** — white filled 24×24 like the others:
  - `home.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z"/></svg>`
  - `bulb.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="M9 21c0 .55.45 1 1 1h4c.55 0 1-.45 1-1v-1H9v1zm3-19C8.14 2 5 5.14 5 9c0 2.38 1.19 4.47 3 5.74V17c0 .55.45 1 1 1h6c.55 0 1-.45 1-1v-2.26c1.81-1.27 3-3.36 3-5.74 0-3.86-3.14-7-7-7z"/></svg>`
  - `plug.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="M16 7V3h-2v4h-4V3H8v4c-1 0-2 1-2 2v5.5L9.5 18v3h5v-3l3.5-3.5V9c0-1-1-2-2-2z"/></svg>`
  - `fan.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2zm.5-9c4.5 0 4.53 3.5 2.15 4.69-1 .5-1.43 1.56-1.65 2.56.48.19.9.5 1.22.89 3.7-2 7.78-1.15 7.78 2.36 0 4.5-3.5 4.53-4.69 2.15-.51-1.01-1.58-1.43-2.58-1.65-.19.48-.5.9-.89 1.22 2 3.7 1.15 7.78-2.36 7.78-4.5 0-4.53-3.5-2.15-4.69 1-.5 1.42-1.55 1.64-2.55-.49-.19-.91-.5-1.23-.9C4.08 15.85 0 15 0 11.5 0 7 3.5 6.96 4.69 9.35c.51 1.01 1.57 1.42 2.57 1.64.19-.48.5-.9.9-1.22C6.15 6.08 7 2 10.5 2h2z"/></svg>`
  - `sun.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="M6.76 4.84 4.96 3.05 3.55 4.46l1.79 1.79zM4 10.5H1v2h3zm9-9.95h-2V3.5h2zm7.45 3.91-1.41-1.41-1.79 1.79 1.41 1.41zm-3.21 13.7 1.79 1.8 1.41-1.41-1.8-1.79zM20 10.5v2h3v-2zm-8-5c-3.31 0-6 2.69-6 6s2.69 6 6 6 6-2.69 6-6-2.69-6-6-6zm-1 16.95h2V19.5h-2zm-7.45-3.91 1.41 1.41 1.79-1.8-1.41-1.41z"/></svg>`
  - `sparkle.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#ffffff"><path d="m19 9 1.25-2.75L23 5l-2.75-1.25L19 1l-1.25 2.75L15 5l2.75 1.25zm-7.5.5L9 4 6.5 9.5 1 12l5.5 2.5L9 20l2.5-5.5L17 12zM19 15l-1.25 2.75L15 19l2.75 1.25L19 23l1.25-2.75L23 19l-2.75-1.25z"/></svg>`

- [ ] **Step 2: Slint types** — in `types.slint` add `home` to `ScreenKind` (after `book`) and:
  ```slint
  // icon: 0 light, 1 plug, 2 fan.
  export struct HomeTileData {
      id: string,
      name: string,
      line: string,
      icon: int,
      on: bool,
      split: bool,
      strip: string,
      // The strip while on: the bulb's colour at its brightness.
      glow: color,
      glow-dark-text: bool,
      sun: bool,
      selected: bool,
      enabled: bool,
  }

  export struct HomeSceneData {
      id: string,
      name: string,
      ran: bool,
      enabled: bool,
  }
  ```

- [ ] **Step 3: Rail** — in `rail.slint` add `in property <bool> home;` beside `books`, and after the
  Search item:
  ```slint
              if root.home: RailItem { icon: @image-url("../icons/home.svg"); label: "Home"; active: root.selected == 8; clicked => { root.select(8); } }
  ```

- [ ] **Step 4: Build**

  Run: `cargo build -p ziggy-player`
  Expected: builds (the new structs are unused until Task 11).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/ziggy-player/ui
  git commit -m "feat(home): icons, Slint types and the rail item"
  ```

---

### Task 11: `HomeScreen`, `HomeBar` and wiring

**Files:**
- Create: `crates/ziggy-player/ui/screens/home.slint`, `crates/ziggy-player/ui/components/home_bar.slint`
- Modify: `crates/ziggy-player/ui/app.slint`, `crates/ziggy-player/src/ui_bridge.rs`

- [ ] **Step 1: `screens/home.slint`**
  ```slint
  import { Theme } from "../theme.slint";
  import { HomeTileData, HomeSceneData } from "../types.slint";

  component HomeTile inherits Rectangle {
      in property <HomeTileData> data;
      callback tap();
      callback tap-strip();
      property <color> fg: root.data.on ? Theme.accent-text : Theme.text;
      property <color> strip-fg: !root.data.on ? Theme.text : root.data.glow-dark-text ? Theme.accent-text : #ffffff;
      property <length> main-width: root.data.split ? root.width * 0.66 : root.width;
      border-radius: Theme.radius;
      background: root.data.on ? Theme.accent : Theme.surface;
      opacity: root.data.enabled ? 1 : 0.45;
      // Outlined while its brightness is in the bottom bar.
      Rectangle {
          x: -5px;
          y: -5px;
          width: root.width + 10px;
          height: root.height + 10px;
          border-radius: Theme.radius + 5px;
          border-width: 3px;
          border-color: root.data.selected ? Theme.text : transparent;
      }
      VerticalLayout {
          x: 0;
          width: root.main-width;
          padding: 12px;
          padding-left: 14px;
          alignment: space-between;
          Image {
              source: root.data.icon == 1 ? @image-url("../icons/plug.svg") : root.data.icon == 2 ? @image-url("../icons/fan.svg") : @image-url("../icons/bulb.svg");
              width: 30px;
              height: 30px;
              colorize: root.fg;
          }
          VerticalLayout {
              spacing: 2px;
              Text { text: root.data.name; color: root.fg; font-size: 17px; font-weight: 800; overflow: elide; }
              if root.data.line != "": Text {
                  text: root.data.line;
                  color: root.data.on ? Theme.accent-text.transparentize(0.22) : Theme.text-dim;
                  font-size: Theme.font-small;
                  overflow: elide;
              }
          }
      }
      TouchArea {
          x: 0;
          width: root.main-width;
          clicked => { if root.data.enabled { root.tap(); } }
      }
      if root.data.split: Rectangle {
          x: root.main-width;
          width: root.width - root.main-width;
          border-top-right-radius: Theme.radius;
          border-bottom-right-radius: Theme.radius;
          // The bulb's own colour at its brightness.
          background: root.data.on ? root.data.glow : Theme.surface-hi;
          VerticalLayout {
              alignment: center;
              spacing: 6px;
              if root.data.sun: Image { source: @image-url("../icons/sun.svg"); height: 26px; image-fit: contain; colorize: root.strip-fg; }
              Text { text: root.data.strip; horizontal-alignment: center; color: root.strip-fg; font-size: Theme.font-small; font-weight: 800; }
          }
          TouchArea { clicked => { if root.data.enabled { root.tap-strip(); } } }
      }
  }

  export component HomeScreen inherits Rectangle {
      in property <[string]> pages;
      in property <int> page;
      in property <[HomeTileData]> tiles;
      in property <[HomeSceneData]> scenes;
      in property <string> banner;
      callback pick-page(int);
      callback tap-tile(string);
      callback tap-strip(string);
      callback run-scene(string);
      VerticalLayout {
          padding-left: 20px;
          padding-right: 20px;
          padding-top: 14px;
          padding-bottom: 16px;
          spacing: 10px;
          if root.banner != "": Rectangle {
              height: 44px;
              border-radius: Theme.radius;
              background: Theme.surface-hi;
              Text { x: 14px; height: parent.height; vertical-alignment: center; text: root.banner; color: Theme.text; font-size: Theme.font-small; }
          }
          Flickable {
              height: 48px;
              viewport-width: chips.preferred-width;
              chips := HorizontalLayout {
                  spacing: 10px;
                  for name[i] in root.pages: Rectangle {
                      width: label.preferred-width + 40px;
                      height: 48px;
                      border-radius: 24px;
                      background: i == root.page ? Theme.accent : (chip.pressed ? Theme.surface-hi : Theme.surface);
                      label := Text { text: name; color: i == root.page ? Theme.accent-text : Theme.text-dim; font-size: 16px; font-weight: 800; }
                      chip := TouchArea { clicked => { root.pick-page(i); } }
                  }
              }
          }
          grid := Flickable {
              vertical-stretch: 1;
              property <length> tile-width: (self.width - 24px) / 3;
              viewport-height: Math.ceil(root.tiles.length / 3) * 112px;
              for tile[i] in root.tiles: HomeTile {
                  x: Math.mod(i, 3) * (grid.tile-width + 12px);
                  y: Math.floor(i / 3) * 112px + 5px;
                  width: grid.tile-width;
                  height: 100px;
                  data: tile;
                  tap => { root.tap-tile(tile.id); }
                  tap-strip => { root.tap-strip(tile.id); }
              }
          }
          if root.scenes.length > 0: HorizontalLayout {
              height: 64px;
              spacing: 12px;
              for scene in root.scenes: Rectangle {
                  horizontal-stretch: 1;
                  border-radius: Theme.radius;
                  background: Theme.surface-hi;
                  border-width: 2px;
                  border-color: scene.ran ? Theme.accent : transparent;
                  opacity: scene.enabled ? 1 : 0.45;
                  HorizontalLayout {
                      alignment: center;
                      spacing: 10px;
                      Image { source: @image-url("../icons/sparkle.svg"); width: 24px; image-fit: contain; colorize: Theme.accent; }
                      Text { text: scene.name; color: Theme.text; font-size: 17px; font-weight: 800; vertical-alignment: center; overflow: elide; }
                  }
                  TouchArea { clicked => { if scene.enabled { root.run-scene(scene.id); } } }
              }
          }
      }
  }
  ```
  If `Math.mod`/`Math.floor`/`Math.ceil` aren't found, the Slint version wants `mod()`, `floor()`,
  `ceil()`; the compiler error names the right one. `/` on two ints gives a float in Slint, hence
  `floor`.

- [ ] **Step 2: `components/home_bar.slint`**
  ```slint
  import { Theme } from "../theme.slint";
  import { IconButton } from "icon_button.slint";

  // A light's brightness, in the music bar's place.
  export component HomeBar inherits Rectangle {
      in property <string> name;
      in property <string> level-text;
      // 0-100; 0 while off.
      in property <int> level;
      in property <bool> on;
      callback dragging(int);
      callback released(int);
      callback toggle();
      callback close();
      height: Theme.mini-height;
      background: Theme.surface;
      function level-at(x: length, width: length) -> int {
          return Math.clamp(Math.round(x / width * 100), 1, 100);
      }
      HorizontalLayout {
          padding: 10px;
          padding-left: 16px;
          padding-right: 12px;
          spacing: 14px;
          VerticalLayout {
              width: 140px;
              alignment: center;
              spacing: 2px;
              Text { text: root.name; color: Theme.text; font-size: Theme.font-body; font-weight: 800; overflow: elide; }
              Text { text: root.level-text; color: Theme.accent; font-size: Theme.font-small; }
          }
          track := Rectangle {
              horizontal-stretch: 1;
              height: 60px;
              border-radius: 16px;
              clip: true;
              background: Theme.surface-hi;
              Rectangle { x: 0; width: track.width * root.level / 100; background: Theme.accent; }
              Image { x: track.width - 42px; source: @image-url("../icons/sun.svg"); width: 26px; image-fit: contain; colorize: Theme.text; }
              drag := TouchArea {
                  pointer-event(ev) => {
                      if ev.kind == PointerEventKind.down {
                          root.dragging(root.level-at(self.mouse-x, self.width));
                      } else if ev.kind == PointerEventKind.up {
                          root.released(root.level-at(self.mouse-x, self.width));
                      }
                  }
                  moved => { root.dragging(root.level-at(self.mouse-x, self.width)); }
              }
          }
          Rectangle {
              width: 88px;
              height: 56px;
              border-radius: Theme.radius;
              background: switch.pressed ? Theme.key : Theme.surface-hi;
              Text { text: root.on ? "Off" : "On"; color: Theme.text; font-size: Theme.font-body; font-weight: 800; }
              switch := TouchArea { clicked => { root.toggle(); } }
          }
          IconButton {
              size: 56px;
              icon: @image-url("../icons/close.svg");
              clicked => { root.close(); }
          }
      }
  }
  ```

- [ ] **Step 3: `app.slint`**

  Imports: `HomeScreen` from `screens/home.slint`, `HomeBar` from `components/home_bar.slint`,
  `HomeTileData, HomeSceneData` from `types.slint` (also add them to the `export { … }` line).
  Properties and callbacks:
  ```slint
      in property <bool> home-enabled;
      in property <[string]> home-pages;
      in property <int> home-page;
      in property <[HomeTileData]> home-tiles;
      in property <[HomeSceneData]> home-scenes;
      in property <string> home-banner;
      in property <bool> home-bar-visible;
      in property <string> home-bar-name;
      in property <string> home-bar-level-text;
      in property <int> home-bar-level;
      in property <bool> home-bar-on;
      callback home-pick-page(int);
      callback home-tap-tile(string);
      callback home-tap-strip(string);
      callback home-run-scene(string);
      callback home-bar-drag(int);
      callback home-bar-release(int);
      callback home-bar-toggle();
      callback home-bar-close();
  ```
  Change `content-height` to
  `root.mini-visible || root.home-bar-visible ? root.height - Theme.mini-height : root.height`.
  Rail: `home: root.home-enabled;`. Add after `BooksScreen { … }`:
  ```slint
      HomeScreen {
          x: Theme.rail-width;
          y: 0;
          width: root.width - Theme.rail-width;
          height: root.content-height;
          visible: root.screen == ScreenKind.home;
          pages: root.home-pages;
          page: root.home-page;
          tiles: root.home-tiles;
          scenes: root.home-scenes;
          banner: root.home-banner;
          pick-page(i) => { root.home-pick-page(i); }
          tap-tile(id) => { root.home-tap-tile(id); }
          tap-strip(id) => { root.home-tap-strip(id); }
          run-scene(id) => { root.home-run-scene(id); }
      }
  ```
  Change `if root.mini-visible: MiniPlayer {` to `if root.mini-visible && !root.home-bar-visible: MiniPlayer {`
  and add after it:
  ```slint
      if root.home-bar-visible: HomeBar {
          x: Theme.rail-width;
          y: root.height - Theme.mini-height;
          width: root.width - Theme.rail-width;
          name: root.home-bar-name;
          level-text: root.home-bar-level-text;
          level: root.home-bar-level;
          on: root.home-bar-on;
          dragging(l) => { root.home-bar-drag(l); }
          released(l) => { root.home-bar-release(l); }
          toggle => { root.home-bar-toggle(); }
          close => { root.home-bar-close(); }
      }
  ```

- [ ] **Step 4: `ui_bridge.rs`**

  - Import `HomeTileData, HomeSceneData` with the other Slint types.
  - Next to `books_rows`, create `home_tiles: Rc<VecModel<HomeTileData>>`,
    `home_scenes: Rc<VecModel<HomeSceneData>>`, `home_pages: Rc<VecModel<slint::SharedString>>`,
    hand them to the window (`window.set_home_tiles(ModelRc::from(home_tiles.clone()));` etc.) and
    keep them on the struct the same way `books_rows` is kept.
  - Callbacks, next to `on_open_book`:
    ```rust
    window.on_home_pick_page(move |i| s(UiAction::HomePage(i.max(0) as usize)));
    window.on_home_tap_tile(move |id| s(UiAction::HomeTile(id.to_string())));
    window.on_home_tap_strip(move |id| s(UiAction::HomeStrip(id.to_string())));
    window.on_home_run_scene(move |id| s(UiAction::HomeScene(id.to_string())));
    window.on_home_bar_drag(move |l| s(UiAction::HomeBrightness(l.clamp(1, 100) as u8)));
    window.on_home_bar_release(move |l| s(UiAction::HomeBrightnessDone(l.clamp(1, 100) as u8)));
    window.on_home_bar_toggle(move || s(UiAction::HomeBarToggle));
    window.on_home_bar_close(move || s(UiAction::HomeBarClose));
    ```
    (each closure needs its own clone of the sender, as the neighbouring lines do.)
  - In `render`: `ScreenView::Home => ScreenKind::Home` (replace the Task 5 placeholder), and:
    ```rust
        w.set_home_enabled(v.home_enabled);
        w.set_home_page(v.home.page as i32);
        w.set_home_banner(v.home.banner.as_str().into());
        sync(&self.home_pages, v.home.pages.iter().map(|p| p.as_str().into()).collect());
        sync(&self.home_tiles, v.home.tiles.iter().map(|t| HomeTileData {
            id: t.id.as_str().into(),
            name: t.name.as_str().into(),
            line: t.line.as_str().into(),
            icon: match t.icon { HomeIcon::Light => 0, HomeIcon::Plug => 1, HomeIcon::Fan => 2 },
            on: t.on,
            split: t.split,
            strip: t.strip.as_str().into(),
            glow: t.glow.map_or(slint::Color::from_rgb_u8(0, 0, 0), |[r, g, b]| slint::Color::from_rgb_u8(r, g, b)),
            glow_dark_text: t.glow_dark_text,
            sun: t.sun,
            selected: t.selected,
            enabled: t.enabled,
        }).collect());
        sync(&self.home_scenes, v.home.scenes.iter().map(|sc| HomeSceneData {
            id: sc.id.as_str().into(),
            name: sc.name.as_str().into(),
            ran: sc.ran,
            enabled: sc.enabled,
        }).collect());
        w.set_home_bar_visible(v.home.bar.is_some());
        if let Some(bar) = &v.home.bar {
            w.set_home_bar_name(bar.name.as_str().into());
            w.set_home_bar_level_text(bar.level_text.as_str().into());
            w.set_home_bar_level(i32::from(bar.level));
            w.set_home_bar_on(bar.on);
        }
    ```
    (import `HomeIcon` from `crate::view`.)

- [ ] **Step 5: Build and run all tests**

  Run: `cargo test -p ziggy-player`
  Expected: all pass. Fix Slint compile errors as reported (property names, `Math.*`).

- [ ] **Step 6: Try it on the Mac**

  Run: `cargo run -p ziggy-player -- --fake`
  Check by hand, against the mockup (https://claude.ai/artifact/FNYyVRpWjatnuZZY4pRqP5):
  - Home is second in the rail; it opens Micah's room with chips for both pages.
  - Off light: tap anywhere → it lights up, strip shows ☀ and a percentage.
  - On light: left part → off; strip → outline and the bar replaces the music bar.
  - Dragging the bar dims the tile; × or 8 s without a touch brings the music bar back.
  - Downstairs shows room names under the tiles; the TV plug toggles.

- [ ] **Step 7: Commit**
  ```bash
  git add crates/ziggy-player/ui crates/ziggy-player/src/ui_bridge.rs
  git commit -m "feat(home): the Home screen and the brightness bar"
  ```

---

### Task 12: `ziggy config` → Home Assistant

**Files:**
- Modify: `crates/ziggy-setup/Cargo.toml`, `crates/ziggy-setup/src/devconfig.rs`,
  `crates/ziggy-setup/src/install.rs`, `crates/ziggy-setup/src/wizard.rs`

- [ ] **Step 1: Write the failing tests** (`devconfig.rs`'s tests):
  ```rust
  #[test]
  fn home_assistant_url_is_set_and_shown_without_touching_pages() {
      let mut t: Table = toml::from_str(
          "device_name = \"Leo\"\n[home_assistant]\nurl = \"http://old:8123\"\n[[home_assistant.page]]\nname = \"Room\"\ncontrols = [\"light.desk\"]\n",
      ).unwrap();
      assert_eq!(Setting::HomeAssistant.show(&t), "http://old:8123 (1 page)");
      set_home_assistant(&mut t, "http://192.168.1.69:8123");
      assert_eq!(t["home_assistant"]["url"].as_str(), Some("http://192.168.1.69:8123"));
      assert_eq!(t["home_assistant"]["page"].as_array().map(Vec::len), Some(1));
      home_assistant_off(&mut t);
      assert!(!t.contains_key("home_assistant"));
      assert_eq!(Setting::HomeAssistant.show(&t), "off");
  }

  #[test]
  fn home_assistant_must_be_http() {
      let mut t: Table = toml::from_str("device_name = \"Leo\"\n").unwrap();
      set_home_assistant(&mut t, "https://ha.example.com");
      assert!(check(&t).is_err());
  }
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p ziggy-setup devconfig`
  Expected: compile errors.

- [ ] **Step 3: Implement `devconfig.rs`**

  - Add `HomeAssistant` to `enum Setting` after `Audiobookshelf`, to `ALL` (bump the array length
    to 11), `label` → `"Home Assistant"`, `key` → `Setting::HomeAssistant => return None,`, and in
    `show`:
    ```rust
            Setting::HomeAssistant => match t.get("home_assistant").and_then(Value::as_table) {
                None => "off".into(),
                Some(ha) => {
                    let pages = ha.get("page").and_then(Value::as_array).map_or(0, Vec::len);
                    let url = ha.get("url").and_then(Value::as_str).unwrap_or_default();
                    format!("{url} ({pages} page{})", if pages == 1 { "" } else { "s" })
                }
            },
    ```
  - Add:
    ```rust
    pub const HA_TOKEN_PATH: &str = "/var/lib/ziggy/ha-token";

    /// Sets the Home Assistant address, keeping any pages already written.
    pub fn set_home_assistant(t: &mut Table, url: &str) {
        let ha = t
            .entry("home_assistant")
            .or_insert_with(|| Value::Table(Table::new()));
        if let Some(ha) = ha.as_table_mut() {
            ha.insert("url".into(), url.trim().trim_end_matches('/').into());
        }
    }

    pub fn home_assistant_off(t: &mut Table) {
        t.remove("home_assistant");
    }
    ```
  - In `check`, before its final `Ok(())`:
    ```rust
        if let Some(url) = t.get("home_assistant").and_then(|h| h.get("url")).and_then(Value::as_str) {
            anyhow::ensure!(url.starts_with("http://"), "Home Assistant needs its local http:// address");
        }
    ```

- [ ] **Step 4: `install.rs` — copy the token**

  Add `/// A Home Assistant long-lived access token.` `pub ha_token: Option<&'a Path>,` to
  `Secrets`, and in `install_secrets`:
  ```rust
      if let Some(token) = secrets.ha_token {
          let bytes = std::fs::read(token).with_context(|| format!("reading {}", token.display()))?;
          r.upload(&bytes, devconfig::HA_TOKEN_PATH, "600", Some("ziggy-player"))?;
      }
  ```
  Add `ha_token: None,` to every other `Secrets { … }` (run `grep -n "Secrets {" crates/ziggy-setup/src/*.rs`).

- [ ] **Step 5: Finding Home Assistant on the network**

  Add `mdns-sd = "0.13"` to `crates/ziggy-setup/Cargo.toml` `[dependencies]`. In `wizard.rs`:
  ```rust
  /// Home Assistant's own address, if one answers on the local network within `wait`.
  fn find_home_assistant(wait: std::time::Duration) -> Option<String> {
      let mdns = mdns_sd::ServiceDaemon::new().ok()?;
      let events = mdns.browse("_home-assistant._tcp.local.").ok()?;
      let deadline = std::time::Instant::now() + wait;
      let mut found = None;
      while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
          match events.recv_timeout(left) {
              Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                  found = ["internal_url", "base_url"]
                      .iter()
                      .find_map(|k| info.get_property_val_str(k))
                      .filter(|u| u.starts_with("http://"))
                      .map(str::to_string);
                  if found.is_some() {
                      break;
                  }
              }
              Ok(_) => {}
              Err(_) => break,
          }
      }
      let _ = mdns.shutdown();
      found
  }
  ```
  (In mdns-sd 0.13 `ServiceResolved` carries a `ServiceInfo`; if `cargo build` says otherwise,
  follow `cargo doc -p mdns-sd --open` for the version that resolved.)

- [ ] **Step 6: The menu entry in `wizard::config`**

  Add `let mut ha_token: Option<PathBuf> = None;` next to `key_to_copy`, and a match arm before
  `Setting::Volume | …`:
  ```rust
            Setting::HomeAssistant => {
                let on = t.contains_key("home_assistant");
                if on && yes("Turn Home Assistant off?", false)? {
                    devconfig::home_assistant_off(&mut t);
                } else {
                    say("Looking for Home Assistant on the network…");
                    let found = find_home_assistant(std::time::Duration::from_secs(3));
                    let current = t
                        .get("home_assistant")
                        .and_then(|h| h.get("url"))
                        .and_then(|u| u.as_str())
                        .map(str::to_string);
                    let url: String = Input::with_theme(&theme())
                        .with_prompt("Home Assistant address")
                        .with_initial_text(current.or(found).unwrap_or_else(|| "http://".into()))
                        .interact_text()?;
                    devconfig::set_home_assistant(&mut t, &url);
                    say("In Home Assistant, make a non-admin user for Ziggy, sign in as it, and create a\n\
                         long-lived access token under its profile → Security.");
                    let token: String = dialoguer::Password::with_theme(&theme())
                        .with_prompt("Access token (empty keeps the one on the player)")
                        .allow_empty_password(true)
                        .interact()?;
                    if !token.trim().is_empty() {
                        let file = store::device_dir(&ssh.host)?.join("ha-token");
                        std::fs::write(&file, token.trim())?;
                        ha_token = Some(file);
                    }
                    if !t["home_assistant"].as_table().is_some_and(|h| h.contains_key("page")) {
                        say(&format!(
                            "Home shows once {} lists pages, e.g.\n\n\
                             [[home_assistant.page]]\nname = \"Micah's room\"\n\
                             controls = [\"light.ceiling\", \"switch.fairy_lights\", \"scene.bedtime\"]\n",
                            devconfig::CONFIG_PATH
                        ));
                    }
                }
            }
  ```
  After `if let Some(key) = &key_to_copy { … }` add:
  ```rust
    if let Some(token) = &ha_token {
        let dir = store::device_dir(&ssh.host)?;
        install::install_secrets(
            &ssh,
            &Secrets { spotify_dir: &dir.join("none"), soloist_key: None, vertex_key: None, ha_token: Some(token) },
        )?;
    }
  ```
  If `store::device_dir` doesn't create the folder, call `std::fs::create_dir_all` on it first
  (check `store.rs`).

- [ ] **Step 7: Run the tests**

  Run: `cargo test -p ziggy-setup`
  Expected: all pass.

- [ ] **Step 8: Commit**
  ```bash
  git add crates/ziggy-setup
  git commit -m "feat(setup): ziggy config sets up Home Assistant and copies its token"
  ```

---

### Task 13: Docs

**Files:**
- Modify: `devices/example.toml`, `docs/testing.md`, `README.md`

- [ ] **Step 1: `devices/example.toml`** — append:
  ```toml
  # Home Assistant controls on a Home screen. The token (a non-admin user's long-lived access token)
  # is read from <state_dir>/ha-token; `ziggy config` copies it there.
  # [home_assistant]
  # url = "http://192.168.1.69:8123"
  # [[home_assistant.page]]
  # name = "Micah's room"
  # controls = ["light.ceiling", { entity = "light.desk", name = "Desk lamp" }, "switch.fairy_lights", "scene.bedtime"]
  ```
  Run `cargo test -p ziggy-player config::tests::example_device_config_parses` — still passes.

- [ ] **Step 2: `docs/testing.md`** — add a "Home Assistant" checklist in the file's existing style:
  - `ziggy config` → Home Assistant finds the Mac mini, saves the address and token; player restarts.
  - Home shows in the rail second; chips match the config pages.
  - Off light → one tap on; on light → left part off, strip opens the bar; dragging dims the real
    light; the bar returns to the music after 8 s.
  - Change a light in the Home Assistant app → the tile follows within a second.
  - Quit Home Assistant on the Mac mini → banner appears, tiles grey; start it → recovers by itself.
  - A wrong token → the "refused" banner.
  - A device without `[home_assistant]` shows no Home item.

- [ ] **Step 3: `README.md`** — under the setup or features section, a short "Home Assistant"
  paragraph: what it does, the non-admin user and token, `ziggy config`, and the page example.
  Keep the README's tone; no kid/parent framing.

- [ ] **Step 4: Commit**
  ```bash
  git add devices/example.toml docs/testing.md README.md
  git commit -m "docs: Home Assistant controls"
  ```

---

### Task 14: Try it on a Pi

- [ ] **Step 1:** `scripts/build-pi.sh`, then
  `cargo run -p ziggy-setup --bin ziggy -- update ziggy@ziggy-micah.local --player-binary target/pi/release/ziggy-player`.
- [ ] **Step 2:** `cargo run -p ziggy-setup --bin ziggy -- config ziggy@ziggy-micah.local` → Home
  Assistant; then edit `/etc/ziggy/config.toml` on the Pi (`sudo nano`) to add a page with real
  entity IDs, and `sudo systemctl restart ziggy-player`.
- [ ] **Step 3:** Run the `docs/testing.md` Home Assistant checklist. Watch
  `sudo journalctl -u ziggy-player -f | grep -i "home assistant"` while doing it.
- [ ] **Step 4:** Push the branch and open a PR (don't merge until asked):
  ```bash
  git push -u origin feat/home-assistant
  gh pr create --fill
  ```
