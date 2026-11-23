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

struct Slot {
    source: SourceId,
    state: Option<NowPlaying>,
    /// When this source last went from not-playing to playing.
    playing_since: Option<Instant>,
}

/// Picks which source the Pixoo follows when more than one can play at once.
///
/// A playing source with artwork always beats one without: there is nothing
/// to draw for an artless one, and some players (Spotify's in-app video, an
/// AirPlay stream on a HomePod) never carry artwork. Among sources that tie
/// on that, the winner is whichever started playing most recently, and it keeps
/// the screen for as long as it plays — a track change does not restart its
/// claim, so skipping tracks locally never hands the screen to a HomePod that
/// has been playing all along. When nothing is playing anywhere the last
/// winner stays selected, so its pause or stop drives the idle countdown just
/// like a single source would.
pub struct Arbiter {
    slots: Vec<Slot>,
    winner: Option<SourceId>,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            winner: None,
        }
    }

    pub fn observe(&mut self, source: SourceId, np: Option<NowPlaying>, now: Instant) {
        let playing = matches!(
            np,
            Some(NowPlaying {
                status: PlayStatus::Playing,
                ..
            })
        );
        let slot = match self.slots.iter().position(|s| s.source == source) {
            Some(i) => &mut self.slots[i],
            None => {
                self.slots.push(Slot {
                    source: source.clone(),
                    state: None,
                    playing_since: None,
                });
                self.slots.last_mut().expect("just pushed")
            }
        };
        if playing {
            // get_or_insert, not assign: staying in Playing across a track
            // change must not refresh the claim.
            slot.playing_since.get_or_insert(now);
        } else {
            slot.playing_since = None;
        }
        slot.state = np;
        if let Some(latest) = self.latest_playing() {
            self.winner = Some(latest);
        } else if self.winner.is_none() {
            self.winner = Some(source);
        }
        // Players come and go (a browser gets a new instance name per
        // launch); forget the ones that are gone unless the screen follows
        // them.
        let winner = &self.winner;
        self.slots
            .retain(|s| s.state.is_some() || Some(&s.source) == winner.as_ref());
    }

    /// The playing source to follow: one with artwork first, then the one
    /// that started most recently. Ties keep the earlier slot.
    fn latest_playing(&self) -> Option<SourceId> {
        let mut best: Option<(&SourceId, (bool, Instant))> = None;
        for slot in &self.slots {
            let Some(started) = slot.playing_since else {
                continue;
            };
            let has_art = slot.state.as_ref().is_some_and(|np| np.art_url.is_some());
            let rank = (has_art, started);
            if best.is_none_or(|(_, best_rank)| rank > best_rank) {
                best = Some((&slot.source, rank));
            }
        }
        best.map(|(source, _)| source.clone())
    }

    /// The state to reconcile against, or None when the winning source has no
    /// player at all.
    pub fn winner(&self) -> Option<&NowPlaying> {
        let winner = self.winner.as_ref()?;
        self.slots
            .iter()
            .find(|s| &s.source == winner)
            .and_then(|s| s.state.as_ref())
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
    fn vanished_losers_are_forgotten() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(local("spotify"), playing("a"), now);
        for i in 0..10 {
            let tab = format!("chromium.instance{i}");
            a.observe(local(&tab), paused(), now + SEC);
            a.observe(local(&tab), None, now + SEC * 2);
        }
        assert_eq!(a.slots.len(), 1);
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
        assert_eq!(a.winner().map(|np| np.status), Some(PlayStatus::Paused));
        a.observe(local("spotify"), None, now + SEC * 2);
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
    fn the_last_winner_still_reports_when_nothing_plays() {
        // Otherwise a pause would look like "no player at all" and the idle
        // countdown would lose the state it reconciles against.
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(SourceId::HomePod, paused(), now + SEC);
        assert_eq!(a.winner().map(|np| np.status), Some(PlayStatus::Paused));
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
