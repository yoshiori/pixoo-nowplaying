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

/// Retry gate after a failed HTTP action (draw or restore).
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Minimum grace before artless playback hands the screen back, independent
/// of idle_restore_secs: Spotify emits a transient artless Playing event on
/// track start, and a small user-configured idle timeout must never let that
/// transient trigger a restore.
const ARTLESS_MIN_GRACE: Duration = Duration::from_secs(5);

/// Decides what should be on the Pixoo. `observe` ingests player state,
/// `decide` reconciles it against what is actually drawn, and the outcome
/// callbacks (`shown`/`show_failed`/`restored`/`restore_failed`) feed HTTP
/// results back so failed actions are retried instead of dropped. Time is
/// injected so transitions are testable.
pub struct Tracker {
    idle_after: Duration,
    /// Some(url) iff the latest player state is Playing with artwork.
    playing_art: Option<String>,
    /// The latest player state is Playing but without artwork.
    artless_playing: bool,
    /// When playback stopped being drawable while our artwork was on screen.
    idle_since: Option<Instant>,
    /// Artwork actually on the device (None = the device owns its screen).
    drawn_url: Option<String>,
    /// Earliest time the next attempt may run after a failure.
    next_attempt: Option<Instant>,
}

impl Tracker {
    pub fn new(idle_after: Duration) -> Self {
        Self {
            idle_after,
            playing_art: None,
            artless_playing: false,
            idle_since: None,
            drawn_url: None,
            next_attempt: None,
        }
    }

    pub fn observe(&mut self, np: Option<&NowPlaying>, now: Instant) {
        let (playing_art, artless_playing) = match np {
            Some(NowPlaying {
                status: PlayStatus::Playing,
                art_url: Some(url),
                ..
            }) => (Some(url.clone()), false),
            Some(NowPlaying {
                status: PlayStatus::Playing,
                art_url: None,
                ..
            }) => (None, true),
            _ => (None, false),
        };
        if playing_art != self.playing_art {
            // The goal changed; don't make a stale failure delay it.
            self.next_attempt = None;
        }
        if playing_art.is_some() {
            self.idle_since = None;
        } else if self.drawn_url.is_some() && self.idle_since.is_none() {
            self.idle_since = Some(now);
        }
        self.playing_art = playing_art;
        self.artless_playing = artless_playing;
    }

    pub fn decide(&self, now: Instant) -> Option<Action> {
        if self.next_attempt.is_some_and(|t| now < t) {
            return None;
        }
        match &self.playing_art {
            Some(url) => {
                if self.drawn_url.as_deref() == Some(url) {
                    return None;
                }
                Some(Action::ShowArtwork {
                    url: url.clone(),
                    takes_ownership: self.drawn_url.is_none(),
                })
            }
            None => {
                self.drawn_url.as_ref()?;
                let since = self.idle_since?;
                let grace = if self.artless_playing {
                    self.idle_after.max(ARTLESS_MIN_GRACE)
                } else {
                    self.idle_after
                };
                (now.duration_since(since) >= grace).then_some(Action::RestoreChannel)
            }
        }
    }

    pub fn shown(&mut self, url: &str) {
        self.drawn_url = Some(url.to_string());
        self.next_attempt = None;
    }

    pub fn show_failed(&mut self, now: Instant) {
        self.next_attempt = Some(now + RETRY_DELAY);
    }

    pub fn restored(&mut self) {
        self.drawn_url = None;
        self.idle_since = None;
        self.next_attempt = None;
    }

    pub fn restore_failed(&mut self, now: Instant) {
        self.next_attempt = Some(now + RETRY_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(30);
    const SEC: Duration = Duration::from_secs(1);

    fn np(status: PlayStatus, art: Option<&str>) -> NowPlaying {
        NowPlaying {
            status,
            art_url: art.map(String::from),
            player: None,
        }
    }

    fn playing(url: &str) -> NowPlaying {
        np(PlayStatus::Playing, Some(url))
    }

    fn show(url: &str, takes_ownership: bool) -> Action {
        Action::ShowArtwork {
            url: url.to_string(),
            takes_ownership,
        }
    }

    /// observe + decide + report success, mirroring the main loop's happy path.
    fn play_and_draw(t: &mut Tracker, url: &str, now: Instant) {
        t.observe(Some(&playing(url)), now);
        match t.decide(now) {
            Some(Action::ShowArtwork { url, .. }) => t.shown(&url),
            other => panic!("expected ShowArtwork, got {other:?}"),
        }
    }

    #[test]
    fn first_draw_takes_ownership() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&playing("a")), now);
        assert_eq!(t.decide(now), Some(show("a", true)));
    }

    #[test]
    fn successful_draw_is_not_repeated() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        assert_eq!(t.decide(now + SEC), None);
        t.observe(Some(&playing("a")), now + SEC);
        assert_eq!(t.decide(now + SEC), None);
    }

    #[test]
    fn track_change_redraws_without_taking_ownership() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&playing("b")), now);
        assert_eq!(t.decide(now), Some(show("b", false)));
    }

    #[test]
    fn failed_draw_is_retried_on_tick_after_backoff() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&playing("a")), now);
        assert_eq!(t.decide(now), Some(show("a", true)));
        t.show_failed(now);
        // No new playerctl event needed: pure time passing re-arms the draw.
        assert_eq!(t.decide(now + SEC), None);
        assert_eq!(t.decide(now + RETRY_DELAY), Some(show("a", true)));
    }

    #[test]
    fn failed_first_draw_keeps_ownership_unclaimed() {
        // The retry must still capture the restore channel (takes_ownership
        // stays true) because nothing was actually drawn.
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&playing("a")), now);
        t.decide(now);
        t.show_failed(now);
        assert_eq!(t.decide(now + RETRY_DELAY), Some(show("a", true)));
    }

    #[test]
    fn no_restore_when_nothing_was_ever_drawn() {
        // A failed draw must not lead to a phantom restore bounce.
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&playing("a")), now);
        t.decide(now);
        t.show_failed(now);
        t.observe(Some(&np(PlayStatus::Paused, Some("a"))), now + SEC);
        assert_eq!(t.decide(now + SEC + IDLE * 2), None);
    }

    #[test]
    fn restores_channel_after_idle_timeout() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&np(PlayStatus::Paused, Some("a"))), now);
        assert_eq!(t.decide(now + IDLE - SEC), None);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn failed_restore_is_retried_until_it_succeeds() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(None, now);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
        t.restore_failed(now + IDLE);
        assert_eq!(t.decide(now + IDLE + SEC), None);
        assert_eq!(
            t.decide(now + IDLE + RETRY_DELAY),
            Some(Action::RestoreChannel)
        );
        t.restored();
        assert_eq!(t.decide(now + IDLE * 2), None);
    }

    #[test]
    fn resume_before_timeout_cancels_restore() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&np(PlayStatus::Paused, Some("a"))), now);
        t.observe(Some(&playing("a")), now + Duration::from_secs(5));
        assert_eq!(t.decide(now + IDLE * 2), None);
    }

    #[test]
    fn draw_after_restore_takes_ownership_again() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(None, now);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
        t.restored();
        t.observe(Some(&playing("a")), now + IDLE * 2);
        assert_eq!(t.decide(now + IDLE * 2), Some(show("a", true)));
    }

    #[test]
    fn no_player_starts_idle_countdown() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(None, now);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn sustained_artless_playback_restores_after_timeout() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&np(PlayStatus::Playing, None)), now);
        assert_eq!(t.decide(now + IDLE - SEC), None);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn excluded_playback_restores_after_artless_grace() {
        // An excluded player's line arrives with its artwork already
        // stripped by parse_line_excluding; the tracker must treat it
        // exactly like artless playback and hand the screen back.
        let excluded = vec!["chromium".to_string()];
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        let np = crate::metadata::parse_line_excluding(
            "chromium.instance99\tPlaying\thttps://i.ytimg.com/vi/x.jpg",
            &excluded,
        )
        .unwrap();
        t.observe(Some(&np), now);
        assert_eq!(t.decide(now + IDLE - SEC), None);
        assert_eq!(t.decide(now + IDLE), Some(Action::RestoreChannel));
    }

    #[test]
    fn transient_artless_event_survives_tiny_idle_config() {
        // With idle_restore_secs = 1, the artless transient at track start
        // must still be absorbed by ARTLESS_MIN_GRACE.
        let mut t = Tracker::new(SEC);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&np(PlayStatus::Playing, None)), now);
        assert_eq!(t.decide(now + SEC * 2), None);
        t.observe(Some(&playing("b")), now + SEC * 3);
        assert_eq!(t.decide(now + SEC * 3), Some(show("b", false)));
    }

    #[test]
    fn pause_with_tiny_idle_config_restores_fast() {
        // ARTLESS_MIN_GRACE must not slow down the plain pause path.
        let mut t = Tracker::new(SEC);
        let now = Instant::now();
        play_and_draw(&mut t, "a", now);
        t.observe(Some(&np(PlayStatus::Paused, Some("a"))), now);
        assert_eq!(t.decide(now + SEC), Some(Action::RestoreChannel));
    }

    #[test]
    fn artless_playback_without_prior_drawing_does_nothing() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&np(PlayStatus::Playing, None)), now);
        assert_eq!(t.decide(now + IDLE * 2), None);
    }

    #[test]
    fn track_change_clears_failure_backoff() {
        let mut t = Tracker::new(IDLE);
        let now = Instant::now();
        t.observe(Some(&playing("a")), now);
        t.decide(now);
        t.show_failed(now);
        // A new track should draw immediately, not wait out a's backoff.
        t.observe(Some(&playing("b")), now + SEC);
        assert_eq!(t.decide(now + SEC), Some(show("b", true)));
    }
}
