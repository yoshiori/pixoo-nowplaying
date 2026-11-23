use std::time::Instant;

use crate::metadata::{NowPlaying, PlayStatus};

/// Where a now-playing update came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceId {
    /// One MPRIS player on this machine, by its playerctl instance name.
    /// Players are separate sources so that a paused one updating its
    /// metadata cannot hide another one that is playing.
    Local(String),
    /// A HomePod on the LAN, via pyatv.
    HomePod,
}

/// A source that is currently playing.
struct Claim {
    source: SourceId,
    state: NowPlaying,
    /// When the source went from not-playing to playing. A track change
    /// keeps it, so skipping tracks never steals the screen back.
    since: Instant,
}

impl Claim {
    /// Higher wins. There is nothing to draw for an artless source, and some
    /// never carry artwork (Spotify's in-app video, an AirPlay stream on a
    /// HomePod), so artwork comes first; then the one that started later.
    fn rank(&self) -> (bool, Instant) {
        (self.state.art_url.is_some(), self.since)
    }
}

/// Picks which source the Pixoo follows when more than one can play at once.
/// Only playing sources are kept: the Tracker handles a pause and "no player"
/// the same way, so nothing else needs remembering.
pub struct Arbiter {
    playing: Vec<Claim>,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            playing: Vec::new(),
        }
    }

    pub fn observe(&mut self, source: SourceId, np: Option<NowPlaying>, now: Instant) {
        let existing = self.playing.iter().position(|c| c.source == source);
        match (np, existing) {
            (Some(state), Some(i)) if state.status == PlayStatus::Playing => {
                self.playing[i].state = state;
            }
            (Some(state), None) if state.status == PlayStatus::Playing => {
                self.playing.push(Claim {
                    source,
                    state,
                    since: now,
                });
            }
            (_, Some(i)) => {
                self.playing.remove(i);
            }
            (_, None) => {}
        }
    }

    /// The playing source to follow, or None when nothing is playing. Ties
    /// keep the one that was observed first.
    pub fn winner(&self) -> Option<&NowPlaying> {
        self.playing
            .iter()
            .reduce(|best, c| if c.rank() > best.rank() { c } else { best })
            .map(|c| &c.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SEC: Duration = Duration::from_secs(1);

    fn playing(url: &str) -> Option<NowPlaying> {
        Some(NowPlaying {
            status: PlayStatus::Playing,
            art_url: Some(url.to_string()),
        })
    }

    fn paused() -> Option<NowPlaying> {
        Some(NowPlaying {
            status: PlayStatus::Paused,
            art_url: None,
        })
    }

    fn art(a: &Arbiter) -> Option<&str> {
        a.winner().and_then(|np| np.art_url.as_deref())
    }

    fn local(instance: &str) -> SourceId {
        SourceId::Local(instance.to_string())
    }

    fn playing_artless() -> Option<NowPlaying> {
        Some(NowPlaying {
            status: PlayStatus::Playing,
            art_url: None,
        })
    }

    fn is_artless_playing(a: &Arbiter) -> bool {
        a.winner() == playing_artless().as_ref()
    }

    #[test]
    fn a_later_artless_player_does_not_take_the_screen() {
        // Spotify's desktop app also exposes an artless chromium.instanceNNN
        // player for its in-app video; it went Playing on 2026-11-23 and
        // would otherwise win by starting later.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing("track"), now);
        a.observe(
            local("chromium.instance452832"),
            playing_artless(),
            now + SEC,
        );
        assert_eq!(art(&a), Some("track"));
    }

    #[test]
    fn an_artless_player_wins_once_the_one_with_art_stops() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("chromium.instance1"), playing_artless(), now);
        a.observe(local("spotify"), playing("track"), now + SEC);
        a.observe(local("spotify"), paused(), now + SEC * 2);
        assert!(is_artless_playing(&a));
    }

    #[test]
    fn among_artless_players_the_later_one_wins() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("radio"), playing_artless(), now);
        a.observe(SourceId::HomePod, playing_artless(), now + SEC);
        a.observe(local("radio"), paused(), now + SEC * 2);
        assert!(is_artless_playing(&a));
        a.observe(local("radio"), playing_artless(), now + SEC * 3);
        a.observe(SourceId::HomePod, paused(), now + SEC * 4);
        assert!(is_artless_playing(&a));
    }

    #[test]
    fn artwork_arriving_late_takes_the_screen() {
        // Spotify emits an artless Playing event at track start before the
        // artUrl shows up; once it does, it must beat an artless player that
        // started in between.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing_artless(), now);
        a.observe(local("chromium.instance1"), playing_artless(), now + SEC);
        a.observe(local("spotify"), playing("track"), now + SEC * 2);
        assert_eq!(art(&a), Some("track"));
    }

    #[test]
    fn a_paused_player_updating_does_not_hide_a_playing_one() {
        // 2026-11-23: a paused Chromium tab mirroring Spotify refreshed its
        // metadata right after every Spotify track change, and the artwork
        // was handed back to the clock 30 s later.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("chromium.instance452832"), paused(), now);
        a.observe(local("spotify"), playing("track-1"), now + SEC);
        a.observe(local("spotify"), playing("track-2"), now + SEC * 2);
        a.observe(local("chromium.instance452832"), paused(), now + SEC * 2);
        assert_eq!(art(&a), Some("track-2"));
    }

    #[test]
    fn the_local_player_that_started_later_wins() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing("spotify"), now);
        a.observe(local("vlc"), playing("vlc"), now + SEC);
        assert_eq!(art(&a), Some("vlc"));
        a.observe(local("vlc"), paused(), now + SEC * 2);
        assert_eq!(art(&a), Some("spotify"));
    }

    #[test]
    fn a_winner_that_vanishes_reports_no_player() {
        // The player quit while playing: nothing to follow any more, so the
        // tracker can start its idle countdown.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("chromium.instance1"), paused(), now);
        a.observe(local("spotify"), playing("a"), now + SEC);
        a.observe(local("spotify"), None, now + SEC * 2);
        assert_eq!(a.winner(), None);
    }

    #[test]
    fn only_playing_sources_are_kept() {
        // Browsers get a new instance name per launch; stopped ones must not
        // pile up.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing("a"), now);
        for i in 0..10 {
            let tab = format!("chromium.instance{i}");
            a.observe(local(&tab), paused(), now + SEC);
            a.observe(local(&tab), None, now + SEC * 2);
        }
        assert_eq!(a.playing.len(), 1);
        assert_eq!(art(&a), Some("a"));
    }

    #[test]
    fn reports_nothing_before_any_update() {
        assert_eq!(Arbiter::new().winner(), None);
    }

    #[test]
    fn single_source_passes_its_state_through() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing("a"), now);
        assert_eq!(art(&a), Some("a"));
        a.observe(local("spotify"), paused(), now + SEC);
        assert_eq!(a.winner(), None);
        a.observe(local("spotify"), playing("b"), now + SEC * 2);
        assert_eq!(art(&a), Some("b"));
        a.observe(local("spotify"), None, now + SEC * 3);
        assert_eq!(a.winner(), None);
    }

    #[test]
    fn the_source_that_started_playing_later_wins() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        assert_eq!(art(&a), Some("homepod"));
        a.observe(local("spotify"), playing("local"), now + SEC);
        assert_eq!(art(&a), Some("local"));
    }

    #[test]
    fn a_track_change_does_not_steal_the_screen_back() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod-1"), now);
        a.observe(local("spotify"), playing("local"), now + SEC);
        // The HomePod moves to its next track: still the older claim.
        a.observe(SourceId::HomePod, playing("homepod-2"), now + SEC * 2);
        assert_eq!(art(&a), Some("local"));
    }

    #[test]
    fn the_other_source_takes_over_when_the_winner_stops() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(local("spotify"), playing("local"), now + SEC);
        a.observe(local("spotify"), paused(), now + SEC * 2);
        assert_eq!(art(&a), Some("homepod"));
    }

    #[test]
    fn a_loser_going_quiet_leaves_the_winner_alone() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(local("spotify"), playing("local"), now + SEC);
        a.observe(SourceId::HomePod, None, now + SEC * 2);
        assert_eq!(art(&a), Some("local"));
    }

    #[test]
    fn nothing_playing_reports_nothing() {
        // The Tracker treats a pause and "no player" alike, so there is no
        // need to remember who was paused last.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(local("spotify"), paused(), now + SEC);
        a.observe(SourceId::HomePod, paused(), now + SEC * 2);
        assert_eq!(a.winner(), None);
    }

    #[test]
    fn a_source_that_resumes_last_wins_again() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(local("spotify"), playing("local"), now + SEC);
        a.observe(SourceId::HomePod, paused(), now + SEC * 2);
        a.observe(SourceId::HomePod, playing("homepod"), now + SEC * 3);
        assert_eq!(art(&a), Some("homepod"));
    }
}
