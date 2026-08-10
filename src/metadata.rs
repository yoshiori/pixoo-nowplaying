#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    pub status: PlayStatus,
    pub art_url: Option<String>,
    pub player: Option<String>,
}

impl NowPlaying {
    pub fn is_excluded(&self, excluded: &[String]) -> bool {
        self.player
            .as_deref()
            .is_some_and(|name| excluded.iter().any(|entry| player_matches(name, entry)))
    }
}

/// playerctl reports some players with an instance suffix
/// (e.g. `chromium.instance1234`), so an entry also matches `entry.*`.
fn player_matches(name: &str, entry: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let entry = entry.to_ascii_lowercase();
    name == entry || (name.starts_with(&entry) && name.as_bytes()[entry.len()] == b'.')
}

/// Parses one line of
/// `playerctl --follow metadata --format "{{playerName}}\t{{status}}\t{{mpris:artUrl}}"`.
/// Returns None for empty lines (no active player) or unknown statuses.
pub fn parse_line(line: &str) -> Option<NowPlaying> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(3, '\t');
    let player = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let status = match parts.next()? {
        "Playing" => PlayStatus::Playing,
        "Paused" => PlayStatus::Paused,
        "Stopped" => PlayStatus::Stopped,
        _ => return None,
    };
    let art_url = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Some(NowPlaying {
        status,
        art_url,
        player,
    })
}

/// Like [`parse_line`], but drops the artwork of excluded players so the
/// rest of the pipeline treats them as artless playback.
pub fn parse_line_excluding(line: &str, excluded: &[String]) -> Option<NowPlaying> {
    let mut np = parse_line(line)?;
    if np.is_excluded(excluded) {
        np.art_url = None;
    }
    Some(np)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playing_with_art_url() {
        let np = parse_line("spotify\tPlaying\thttps://i.scdn.co/image/abc123").unwrap();
        assert_eq!(np.status, PlayStatus::Playing);
        assert_eq!(
            np.art_url.as_deref(),
            Some("https://i.scdn.co/image/abc123")
        );
    }

    #[test]
    fn parses_paused_without_art_url() {
        let np = parse_line("spotify\tPaused\t").unwrap();
        assert_eq!(np.status, PlayStatus::Paused);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn parses_stopped_with_missing_field() {
        let np = parse_line("spotify\tStopped").unwrap();
        assert_eq!(np.status, PlayStatus::Stopped);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn empty_line_means_no_player() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("\n"), None);
    }

    #[test]
    fn unknown_status_is_ignored() {
        assert_eq!(parse_line("spotify\tBuffering\tfile:///a.png"), None);
    }

    #[test]
    fn trailing_newline_is_stripped() {
        let np = parse_line("spotify\tPlaying\tfile:///art.jpg\n").unwrap();
        assert_eq!(np.art_url.as_deref(), Some("file:///art.jpg"));
    }

    #[test]
    fn parses_player_name() {
        let np = parse_line("chromium.instance1234\tPlaying\t").unwrap();
        assert_eq!(np.player.as_deref(), Some("chromium.instance1234"));
    }

    #[test]
    fn blank_player_name_is_none() {
        let np = parse_line("\tPlaying\tfile:///art.jpg").unwrap();
        assert_eq!(np.player, None);
    }

    fn excluded() -> Vec<String> {
        vec!["chromium".to_string()]
    }

    #[test]
    fn excluded_player_matches_exactly() {
        let np = parse_line("chromium\tPlaying\t").unwrap();
        assert!(np.is_excluded(&excluded()));
    }

    #[test]
    fn exclusion_is_case_insensitive() {
        let np = parse_line("Chromium\tPlaying\t").unwrap();
        assert!(np.is_excluded(&excluded()));
    }

    #[test]
    fn exclusion_matches_instance_suffix() {
        let np = parse_line("chromium.instance1234\tPlaying\t").unwrap();
        assert!(np.is_excluded(&excluded()));
    }

    #[test]
    fn exclusion_does_not_match_name_prefix() {
        for name in ["chromium2", "chromium-beta"] {
            let np = parse_line(&format!("{name}\tPlaying\t")).unwrap();
            assert!(!np.is_excluded(&excluded()), "{name} must not match");
        }
    }

    #[test]
    fn missing_player_is_never_excluded() {
        let np = parse_line("\tPlaying\t").unwrap();
        assert!(!np.is_excluded(&excluded()));
    }

    #[test]
    fn parse_line_excluding_strips_art_for_excluded_player() {
        let np = parse_line_excluding(
            "chromium.instance99\tPlaying\thttps://i.ytimg.com/vi/x.jpg",
            &excluded(),
        )
        .unwrap();
        assert_eq!(np.status, PlayStatus::Playing);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn parse_line_excluding_keeps_art_for_other_players() {
        let np = parse_line_excluding("spotify\tPlaying\thttps://i.scdn.co/image/abc", &excluded())
            .unwrap();
        assert_eq!(np.art_url.as_deref(), Some("https://i.scdn.co/image/abc"));
    }

    #[test]
    fn parse_line_excluding_with_empty_list_is_identity() {
        let line = "chromium\tPlaying\thttps://i.ytimg.com/vi/x.jpg";
        assert_eq!(parse_line_excluding(line, &[]), parse_line(line));
    }

    #[test]
    fn two_field_line_is_rejected() {
        // A line in the pre-playerName format has the status in the player
        // slot, so it must fail the status match rather than misparse.
        assert_eq!(parse_line("Playing\thttps://i.scdn.co/image/abc123"), None);
    }
}
