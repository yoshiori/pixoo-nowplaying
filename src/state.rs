use std::time::{Duration, Instant};

use crate::metadata::{NowPlaying, PlayStatus};

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// `takes_ownership` is true when this draw takes the screen over from
    /// the device's own channel (as opposed to replacing our previous
    /// artwork) — the moment to capture which channel to restore later.
    ShowArtwork {
        url: String,
        takes_ownership: bool,
    },
    RestoreChannel,
}

/// Decides when to draw artwork and when to give the screen back to the
/// Pixoo's own channel. Time is injected so transitions are testable.
pub struct Tracker {
    idle_after: Duration,
    shown_art: Option<String>,
    idle_since: Option<Instant>,
    owning: bool,
}

impl Tracker {
    pub fn new(idle_after: Duration) -> Self {
        Self {
            idle_after,
            shown_art: None,
            idle_since: None,
            owning: false,
        }
    }

    pub fn on_update(&mut self, np: Option<&NowPlaying>, now: Instant) -> Option<Action> {
        match np {
            Some(NowPlaying {
                status: PlayStatus::Playing,
                art_url: Some(url),
            }) => {
                self.idle_since = None;
                if self.owning && self.shown_art.as_deref() == Some(url) {
                    return None;
                }
                let takes_ownership = !self.owning;
                self.owning = true;
                self.shown_art = Some(url.clone());
                Some(Action::ShowArtwork {
                    url: url.clone(),
                    takes_ownership,
                })
            }
            // Playing without an artUrl falls through to the idle countdown:
            // sustained artless playback (radio streams) hands the screen
            // back, while the transient artless event Spotify emits on track
            // start is absorbed by the grace period.
            _ => {
                if self.owning && self.idle_since.is_none() {
                    self.idle_since = Some(now);
                }
                self.tick(now)
            }
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        let idle_since = self.idle_since?;
        if self.owning && now.duration_since(idle_since) >= self.idle_after {
            self.owning = false;
            self.idle_since = None;
            self.shown_art = None;
            return Some(Action::RestoreChannel);
        }
        None
    }

    /// Called when drawing failed, so the same URL is retried on the next update.
    pub fn forget_shown_art(&mut self) {
        self.shown_art = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(30);

    fn playing(url: &str) -> NowPlaying {
        NowPlaying {
            status: PlayStatus::Playing,
            art_url: Some(url.to_string()),
        }
    }

    fn paused() -> NowPlaying {
        NowPlaying {
            status: PlayStatus::Paused,
            art_url: Some("x".to_string()),
        }
    }

    fn playing_no_art() -> NowPlaying {
        NowPlaying {
            status: PlayStatus::Playing,
            art_url: None,
        }
    }

    fn show(url: &str, takes_ownership: bool) -> Action {
        Action::ShowArtwork {
            url: url.to_string(),
            takes_ownership,
        }
    }

    #[test]
    fn first_draw_takes_ownership() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        assert_eq!(t.on_update(Some(&playing("a")), now), Some(show("a", true)));
    }

    #[test]
    fn does_not_redraw_same_artwork() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.on_update(Some(&playing("a")), now);
        assert_eq!(t.on_update(Some(&playing("a")), now), None);
    }

    #[test]
    fn track_change_redraws_without_taking_ownership() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.on_update(Some(&playing("a")), now);
        assert_eq!(
            t.on_update(Some(&playing("b")), now),
            Some(show("b", false))
        );
    }

    #[test]
    fn restores_channel_after_idle_timeout() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        assert_eq!(t.on_update(Some(&paused()), start), None);
        assert_eq!(t.tick(start + IDLE - Duration::from_secs(1)), None);
        assert_eq!(t.tick(start + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn resume_before_timeout_cancels_restore() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        t.on_update(Some(&paused()), start);
        assert_eq!(
            t.on_update(Some(&playing("a")), start + Duration::from_secs(5)),
            None
        );
        assert_eq!(t.tick(start + IDLE * 2), None);
    }

    #[test]
    fn draw_after_restore_takes_ownership_again() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        t.on_update(Some(&paused()), start);
        t.tick(start + IDLE);
        assert_eq!(
            t.on_update(Some(&playing("a")), start + IDLE * 2),
            Some(show("a", true))
        );
    }

    #[test]
    fn no_restore_when_nothing_was_drawn() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        assert_eq!(t.on_update(None, start), None);
        assert_eq!(t.tick(start + IDLE * 2), None);
    }

    #[test]
    fn no_player_line_starts_idle_countdown() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        assert_eq!(t.on_update(None, start), None);
        assert_eq!(t.tick(start + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn sustained_artless_playback_restores_after_timeout() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        assert_eq!(t.on_update(Some(&playing_no_art()), start), None);
        assert_eq!(t.tick(start + IDLE - Duration::from_secs(1)), None);
        assert_eq!(t.tick(start + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn transient_artless_event_does_not_restore_when_art_follows() {
        // Spotify sends a Playing event without artUrl for an instant on
        // track start; the idle grace period must absorb it.
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        t.on_update(Some(&playing("a")), start);
        assert_eq!(t.on_update(Some(&playing_no_art()), start), None);
        assert_eq!(
            t.on_update(Some(&playing("b")), start + Duration::from_secs(1)),
            Some(show("b", false))
        );
        assert_eq!(t.tick(start + IDLE * 2), None);
    }

    #[test]
    fn artless_playback_without_prior_drawing_does_nothing() {
        let mut t = Tracker::new(IDLE);
        let start = Instant::now();
        assert_eq!(t.on_update(Some(&playing_no_art()), start), None);
        assert_eq!(t.tick(start + IDLE * 2), None);
    }

    #[test]
    fn forget_shown_art_forces_redraw_without_reclaiming_ownership() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.on_update(Some(&playing("a")), now);
        t.forget_shown_art();
        assert_eq!(
            t.on_update(Some(&playing("a")), now),
            Some(show("a", false))
        );
    }
}
