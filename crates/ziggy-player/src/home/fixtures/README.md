# What Home Assistant sends a non-admin token

Recorded 2026-10-31 from Home Assistant 2026.2.3 (`ws://192.168.1.69:8123/api/websocket`) with a
long-lived token of a non-admin "Ziggy" user, read-only (nothing was switched).

- `auth_required`, then `auth_ok` after `{"type":"auth","access_token":…}`.
- `config/entity_registry/list_for_display`, `config/device_registry/list` and
  `config/area_registry/list` all answer `"success": true` for the non-admin user, so area names
  are available.
- Entities from `list_for_display` look like
  `{"ei": "light.bay_window", "pl": "hue", "lb": [], "di": "<device id>", "hn": true}`. Hue lights
  carry no `ai`: their area comes from the device (`device_registry/list` → `area_id`).
- Areas look like `{"area_id": "bay_window", "name": "Bay window", …}`.
- `subscribe_entities` answers `{"id": 4, "type": "result", "success": true, "result": null}`, then
  an event with `"a"`:
  `{"light.master_bedroom": {"s": "on", "a": {"brightness": 181, "friendly_name": "Master Bedroom",
  "color_mode": "brightness", …}, "c": "<context>", "lc": 1793042900.2, "lu": 1793325649.4}}`.
  Scenes report state `"unknown"` until run.
- `{"id": 5, "type": "ping"}` → `{"id": 5, "type": "pong"}`.
- No `"c"` (change) event was recorded; its shape (`"+"`/`"-"`) is Home Assistant's documented
  compressed-state format, as used in `types.rs`'s tests.
