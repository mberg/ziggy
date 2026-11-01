//! AirPlay: a phone or computer playing to this player takes over Now Playing, and its
//! buttons, until it stops or something is played here from Spotify.

use super::*;

/// Spotify saying it plays this soon after AirPlay started is the pause catching up, not
/// someone picking this player in a Spotify app.
const SPOTIFY_SETTLE_MS: u64 = 3_000;

impl Core {
    pub(super) fn on_airplay(&mut self, update: AirPlayUpdate, now_ms: u64, fx: &mut Vec<Effect>) {
        let air = &mut self.state.airplay;
        if let AirPlayUpdate::Volume { percent } = update {
            air.volume = percent;
            return;
        }
        if update == AirPlayUpdate::Started && air.session.is_none() {
            air.session = Some(AirPlaySession {
                playing: true,
                position_at_ms: now_ms,
                started_ms: now_ms,
                ..AirPlaySession::default()
            });
            // One thing plays at a time.
            let pb = &mut self.state.playback;
            if matches!(pb.status, PlayStatus::Playing | PlayStatus::Loading) {
                pb.status = PlayStatus::Paused;
                fx.push(Effect::Player(PlayerCommand::Pause));
            }
            if self.state.sleep_ends_ms.is_none() {
                self.wake(now_ms, fx);
            }
            self.navigate(Screen::NowPlaying);
            return;
        }
        // The rest describe a session; one Ziggy ended may still say it stopped.
        let Some(s) = &mut air.session else { return };
        match update {
            AirPlayUpdate::Started | AirPlayUpdate::Playing => {
                s.playing = true;
                s.position_at_ms = now_ms;
            }
            AirPlayUpdate::Paused => s.playing = false,
            AirPlayUpdate::Song {
                title,
                artist,
                album,
            } => {
                s.title = title;
                s.artist = artist;
                s.album = album;
            }
            AirPlayUpdate::Cover(url) => s.cover_url = url,
            AirPlayUpdate::Progress {
                position_ms,
                duration_ms,
            } => {
                s.position_ms = position_ms;
                s.duration_ms = duration_ms;
                s.position_at_ms = now_ms;
            }
            AirPlayUpdate::Ended => air.session = None,
            AirPlayUpdate::Volume { .. } => {}
        }
    }

    /// Frees the speaker for Spotify: the phone stops playing here.
    pub(super) fn end_airplay(&mut self, fx: &mut Vec<Effect>) {
        if self.state.airplay.session.take().is_some() {
            fx.push(Effect::AirPlay(AirPlayCommand::End));
        }
    }

    /// Someone picked this player in a Spotify app while AirPlay played.
    pub(super) fn spotify_took_over(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        if self
            .state
            .airplay
            .session
            .as_ref()
            .is_some_and(|s| now_ms.saturating_sub(s.started_ms) >= SPOTIFY_SETTLE_MS)
        {
            self.end_airplay(fx);
        }
    }

    /// While AirPlay plays, the playing buttons control the phone. True when the action was
    /// one of them.
    pub(super) fn airplay_ui(
        &mut self,
        action: &UiAction,
        now_ms: u64,
        fx: &mut Vec<Effect>,
    ) -> bool {
        let air = &mut self.state.airplay;
        let Some(s) = &mut air.session else {
            return false;
        };
        let command = match action {
            UiAction::TogglePlay => {
                s.playing = !s.playing;
                s.position_at_ms = now_ms;
                if s.playing {
                    AirPlayCommand::Play
                } else {
                    AirPlayCommand::Pause
                }
            }
            UiAction::Next => AirPlayCommand::Next,
            UiAction::Previous => AirPlayCommand::Previous,
            UiAction::SetVolume { percent } => {
                air.volume = (*percent).min(100);
                AirPlayCommand::SetVolume {
                    percent: air.volume,
                }
            }
            // AirPlay can't seek, shuffle or repeat from here.
            UiAction::Seek { .. } | UiAction::ToggleShuffle | UiAction::CycleRepeat => {
                return true;
            }
            _ => return false,
        };
        fx.push(Effect::AirPlay(command));
        true
    }

    /// Moves the position on between the phone's reports.
    pub(super) fn airplay_tick(&mut self, now_ms: u64) {
        if let Some(s) = &mut self.state.airplay.session
            && s.playing
        {
            let moved = now_ms.saturating_sub(s.position_at_ms) as u32;
            s.position_ms = s.position_ms.saturating_add(moved);
            if s.duration_ms > 0 {
                s.position_ms = s.position_ms.min(s.duration_ms);
            }
            s.position_at_ms = now_ms;
        }
    }

    pub(super) fn set_airplay(&mut self, enabled: bool, fx: &mut Vec<Effect>) {
        let air = &mut self.state.airplay;
        if !air.available {
            return;
        }
        air.enabled = enabled;
        if !enabled {
            air.session = None;
        }
        self.state.device.saved.airplay = Some(enabled);
        fx.push(Effect::SaveSettings(self.state.device.saved.clone()));
        fx.push(Effect::AirPlay(AirPlayCommand::Enable(enabled)));
    }

    /// Whether something plays: AirPlay while it has the speaker, otherwise Spotify.
    pub(super) fn play_status(&self) -> PlayStatus {
        match &self.state.airplay.session {
            Some(s) if s.playing => PlayStatus::Playing,
            Some(_) => PlayStatus::Paused,
            None => self.state.playback.status,
        }
    }

    /// The volume of whatever has the speaker.
    pub(super) fn volume(&self) -> u8 {
        match &self.state.airplay.session {
            Some(_) => self.state.airplay.volume,
            None => self.state.playback.volume,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{config, track, ui, with_tracks};
    use super::*;
    use crate::view::build;

    const LIST: &str = "spotify:playlist:p1";

    fn core() -> Core {
        let mut cfg = config();
        cfg.airplay = true;
        cfg.saved.airplay = Some(true);
        Core::new(cfg, 0).0
    }

    fn air(core: &mut Core, update: AirPlayUpdate, now_ms: u64) -> Vec<Effect> {
        core.handle(Input::AirPlay(update), now_ms)
    }

    /// Spotify plays a song, then a phone starts AirPlay at 10 s.
    fn airplaying() -> Core {
        let mut c = core();
        with_tracks(&mut c, LIST, 3);
        c.handle(
            ui(UiAction::PlayTrack {
                collection_uri: LIST.into(),
                index: 0,
            }),
            0,
        );
        c.handle(Input::Player(PlayerUpdate::Playing { position_ms: 0 }), 100);
        c.handle(ui(UiAction::Back), 200);
        air(&mut c, AirPlayUpdate::Started, 10_000);
        c
    }

    #[test]
    fn airplay_pauses_spotify_and_takes_over_now_playing() {
        let mut c = core();
        with_tracks(&mut c, LIST, 3);
        c.handle(
            ui(UiAction::PlayTrack {
                collection_uri: LIST.into(),
                index: 0,
            }),
            0,
        );
        c.handle(Input::Player(PlayerUpdate::Playing { position_ms: 0 }), 100);
        c.handle(ui(UiAction::Back), 200);
        let fx = air(&mut c, AirPlayUpdate::Started, 10_000);
        assert_eq!(fx, vec![Effect::Player(PlayerCommand::Pause)]);
        assert_eq!(c.state().screen, Screen::NowPlaying);
        assert_eq!(c.state().playback.status, PlayStatus::Paused);

        air(&mut c, AirPlayUpdate::Volume { percent: 90 }, 10_000);
        air(
            &mut c,
            AirPlayUpdate::Song {
                title: "Episode 4".into(),
                artist: "A Podcast".into(),
                album: "Season 1".into(),
            },
            10_100,
        );
        air(
            &mut c,
            AirPlayUpdate::Cover(Some("airplay-cover:ab".into())),
            10_100,
        );
        air(
            &mut c,
            AirPlayUpdate::Progress {
                position_ms: 60_000,
                duration_ms: 240_000,
            },
            10_100,
        );
        let now = build(c.state()).now;
        assert!(now.airplay);
        assert_eq!(now.title, "Episode 4");
        assert_eq!(now.artists, "A Podcast");
        assert_eq!(now.image_url.as_deref(), Some("airplay-cover:ab"));
        assert_eq!(now.volume, 90);
        assert_eq!(now.position, "1:00");
        assert_eq!(now.duration, "4:00");
        assert!(now.playing);
        assert_eq!(now.track_uri, "", "no liking or adding a phone's song");

        // The position moves on between reports.
        c.handle(Input::Tick, 12_100);
        assert_eq!(build(c.state()).now.position, "1:02");
    }

    #[test]
    fn the_buttons_control_the_phone() {
        let mut c = airplaying();
        let fx = c.handle(ui(UiAction::TogglePlay), 11_000);
        assert_eq!(fx, vec![Effect::AirPlay(AirPlayCommand::Pause)]);
        assert!(!build(c.state()).now.playing);
        assert_eq!(
            c.state().playback.status,
            PlayStatus::Paused,
            "Spotify stays paused"
        );
        let fx = c.handle(ui(UiAction::TogglePlay), 11_500);
        assert_eq!(fx, vec![Effect::AirPlay(AirPlayCommand::Play)]);
        assert_eq!(
            c.handle(ui(UiAction::Next), 12_000),
            vec![Effect::AirPlay(AirPlayCommand::Next)]
        );
        assert_eq!(
            c.handle(ui(UiAction::Previous), 12_000),
            vec![Effect::AirPlay(AirPlayCommand::Previous)]
        );
        assert_eq!(
            c.handle(ui(UiAction::SetVolume { percent: 30 }), 12_000),
            vec![Effect::AirPlay(AirPlayCommand::SetVolume { percent: 30 })]
        );
        assert_eq!(build(c.state()).now.volume, 30);
        assert!(
            c.handle(ui(UiAction::Seek { position_ms: 5 }), 12_000)
                .is_empty()
        );
        assert!(c.handle(ui(UiAction::ToggleShuffle), 12_000).is_empty());
        assert!(!c.state().playback.shuffle);
    }

    #[test]
    fn playing_from_spotify_here_ends_airplay() {
        let mut c = airplaying();
        // Home and the library still work while AirPlay plays.
        c.handle(ui(UiAction::Back), 11_000);
        assert!(c.state().airplay.session.is_some());
        let fx = c.handle(
            ui(UiAction::PlayTrack {
                collection_uri: LIST.into(),
                index: 2,
            }),
            12_000,
        );
        assert!(fx.contains(&Effect::AirPlay(AirPlayCommand::End)), "{fx:?}");
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Player(PlayerCommand::Load { .. })))
        );
        assert_eq!(c.state().airplay.session, None);
        let now = build(c.state()).now;
        assert!(!now.airplay);
        assert_eq!(now.title, track(2).name);
        // The receiver saying so afterwards changes nothing.
        air(&mut c, AirPlayUpdate::Paused, 12_100);
        air(&mut c, AirPlayUpdate::Ended, 22_000);
        assert_eq!(c.state().airplay.session, None);
    }

    #[test]
    fn picking_this_player_in_a_spotify_app_ends_airplay() {
        let mut c = airplaying();
        // Spotify catching up with the pause doesn't count.
        let fx = c.handle(
            Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
            11_000,
        );
        assert!(!fx.contains(&Effect::AirPlay(AirPlayCommand::End)));
        assert!(c.state().airplay.session.is_some());
        let fx = c.handle(
            Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
            20_000,
        );
        assert!(fx.contains(&Effect::AirPlay(AirPlayCommand::End)));
        assert_eq!(c.state().airplay.session, None);
    }

    #[test]
    fn when_the_phone_leaves_spotify_is_back_paused() {
        let mut c = airplaying();
        air(&mut c, AirPlayUpdate::Paused, 20_000);
        assert!(build(c.state()).now.airplay, "a paused phone still shows");
        assert!(air(&mut c, AirPlayUpdate::Ended, 30_000).is_empty());
        let v = build(c.state());
        assert!(!v.now.airplay);
        assert_eq!(v.now.title, track(0).name);
        assert!(!v.now.playing);
    }

    #[test]
    fn a_phone_with_nothing_else_playing_shows_in_the_bottom_bar() {
        let mut c = core();
        let fx = air(&mut c, AirPlayUpdate::Started, 1_000);
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::Player(_))),
            "nothing to pause"
        );
        assert_eq!(
            build(c.state()).now.title,
            "AirPlay",
            "before its details come"
        );
        c.handle(ui(UiAction::Back), 2_000);
        assert!(build(c.state()).mini_visible);
    }

    #[test]
    fn the_sleep_timer_pauses_the_phone() {
        let mut c = airplaying();
        c.handle(ui(UiAction::SetSleepTimer(Some(1))), 11_000);
        let fx = c.handle(Input::Tick, 69_000);
        assert!(
            fx.iter().all(|e| matches!(e, Effect::Display(_))),
            "no fade on the phone's volume: {fx:?}"
        );
        let fx = c.handle(Input::Tick, 71_000);
        assert!(
            fx.contains(&Effect::AirPlay(AirPlayCommand::Pause)),
            "{fx:?}"
        );
        assert_eq!(c.state().sleep_ends_ms, None);
    }

    #[test]
    fn settings_turn_airplay_on_and_off() {
        let mut c = airplaying();
        let fx = c.handle(ui(UiAction::SetAirPlay(false)), 11_000);
        assert!(fx.contains(&Effect::AirPlay(AirPlayCommand::Enable(false))));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::SaveSettings(s) if s.airplay == Some(false)
        )));
        assert_eq!(c.state().airplay.session, None);
        assert!(!build(c.state()).settings.airplay_enabled);

        // Where shairport-sync isn't installed, Settings doesn't offer it.
        let mut plain = Core::new(config(), 0).0;
        assert!(!build(plain.state()).settings.airplay_available);
        assert!(plain.handle(ui(UiAction::SetAirPlay(true)), 0).is_empty());
    }
}
