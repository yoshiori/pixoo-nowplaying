use std::time::Instant;

use crate::metadata::{NowPlaying, PlayStatus};

/// Where a now-playing update came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceId {
    /// MPRIS on this machine, via playerctl.
    Local,
    /// A HomePod on the LAN, via pyatv.
    HomePod,
}

const SOURCE_COUNT: usize = 2;

impl SourceId {
    fn index(self) -> usize {
        match self {
            SourceId::Local => 0,
            SourceId::HomePod => 1,
        }
    }
}

/// Picks which source the Pixoo follows when more than one can play at once.
///
/// The winner is whichever source started playing most recently, and it keeps
/// the screen for as long as it plays — a track change does not restart its
/// claim, so skipping tracks locally never hands the screen to a HomePod that
/// has been playing all along. When nothing is playing anywhere the last
/// winner stays selected, so its pause or stop drives the idle countdown just
/// like a single source would.
pub struct Arbiter {
    states: [Option<NowPlaying>; SOURCE_COUNT],
    /// When each source last went from not-playing to playing.
    playing_since: [Option<Instant>; SOURCE_COUNT],
    winner: usize,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            states: std::array::from_fn(|_| None),
            playing_since: [None; SOURCE_COUNT],
            winner: SourceId::Local.index(),
        }
    }

    pub fn observe(&mut self, source: SourceId, np: Option<NowPlaying>, now: Instant) {
        let i = source.index();
        if matches!(
            np,
            Some(NowPlaying {
                status: PlayStatus::Playing,
                ..
            })
        ) {
            // get_or_insert, not assign: staying in Playing across a track
            // change must not refresh the claim.
            self.playing_since[i].get_or_insert(now);
        } else {
            self.playing_since[i] = None;
        }
        self.states[i] = np;
        if let Some(latest) = self.latest_playing() {
            self.winner = latest;
        }
    }

    /// The playing source that started most recently. Ties keep the incumbent.
    fn latest_playing(&self) -> Option<usize> {
        let mut best: Option<(usize, Instant)> = None;
        for i in 0..SOURCE_COUNT {
            let Some(started) = self.playing_since[i] else {
                continue;
            };
            let wins = match best {
                None => true,
                Some((_, best_started)) => started > best_started,
            };
            if wins {
                best = Some((i, started));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The state to reconcile against, or None when the winning source has no
    /// player at all.
    pub fn winner(&self) -> Option<&NowPlaying> {
        self.states[self.winner].as_ref()
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

    #[test]
    fn reports_nothing_before_any_update() {
        assert_eq!(Arbiter::new().winner(), None);
    }

    #[test]
    fn single_source_passes_its_state_through() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::Local, playing("a"), now);
        assert_eq!(art(&a), Some("a"));
        a.observe(SourceId::Local, paused(), now + SEC);
        assert_eq!(a.winner().map(|np| np.status), Some(PlayStatus::Paused));
        a.observe(SourceId::Local, None, now + SEC * 2);
        assert_eq!(a.winner(), None);
    }

    #[test]
    fn the_source_that_started_playing_later_wins() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        assert_eq!(art(&a), Some("homepod"));
        a.observe(SourceId::Local, playing("local"), now + SEC);
        assert_eq!(art(&a), Some("local"));
    }

    #[test]
    fn a_track_change_does_not_steal_the_screen_back() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod-1"), now);
        a.observe(SourceId::Local, playing("local"), now + SEC);
        // The HomePod moves to its next track: still the older claim.
        a.observe(SourceId::HomePod, playing("homepod-2"), now + SEC * 2);
        assert_eq!(art(&a), Some("local"));
    }

    #[test]
    fn the_other_source_takes_over_when_the_winner_stops() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(SourceId::Local, playing("local"), now + SEC);
        a.observe(SourceId::Local, paused(), now + SEC * 2);
        assert_eq!(art(&a), Some("homepod"));
    }

    #[test]
    fn a_loser_going_quiet_leaves_the_winner_alone() {
        let mut a = Arbiter::new();
        let now = Instant::now();
        a.observe(SourceId::HomePod, playing("homepod"), now);
        a.observe(SourceId::Local, playing("local"), now + SEC);
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
        a.observe(SourceId::Local, playing("local"), now + SEC);
        a.observe(SourceId::HomePod, paused(), now + SEC * 2);
        a.observe(SourceId::HomePod, playing("homepod"), now + SEC * 3);
        assert_eq!(art(&a), Some("homepod"));
    }
}
