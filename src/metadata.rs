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
}

/// One MPRIS player's state, keyed by its bus instance name as `playerctl -l`
/// prints it (e.g. `spotify`, `chromium.instance452832`).
pub type PlayerState = (String, NowPlaying);

/// The `--format` template handed to playerctl. Lives next to [`parse_line`]
/// because the two are one contract: change one, change the other.
pub const PLAYERCTL_FORMAT: &str = "{{playerInstance}}\t{{status}}\t{{mpris:artUrl}}";

/// Parses one line of `playerctl metadata --format PLAYERCTL_FORMAT`.
/// Returns None for empty lines (no active player), lines without a player
/// name, or unknown statuses.
pub fn parse_line(line: &str) -> Option<PlayerState> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.splitn(3, '\t');
    let instance = parts.next().filter(|s| !s.is_empty())?;
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
    Some((instance.to_string(), NowPlaying { status, art_url }))
}

/// Parses the output of `playerctl -a metadata --format PLAYERCTL_FORMAT`:
/// one line per player. Lines that don't parse are skipped, so one odd player
/// can't hide the others.
pub fn parse_snapshot(output: &str) -> Vec<PlayerState> {
    output.lines().filter_map(parse_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn np(status: PlayStatus, art: Option<&str>) -> NowPlaying {
        NowPlaying {
            status,
            art_url: art.map(String::from),
        }
    }

    #[test]
    fn parses_playing_with_art_url() {
        let (player, np) = parse_line("spotify\tPlaying\thttps://i.scdn.co/image/abc123").unwrap();
        assert_eq!(player, "spotify");
        assert_eq!(np.status, PlayStatus::Playing);
        assert_eq!(
            np.art_url.as_deref(),
            Some("https://i.scdn.co/image/abc123")
        );
    }

    #[test]
    fn parses_paused_without_art_url() {
        let (player, np) = parse_line("chromium.instance452832\tPaused\t").unwrap();
        assert_eq!(player, "chromium.instance452832");
        assert_eq!(np.status, PlayStatus::Paused);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn parses_stopped_with_missing_field() {
        let (_, np) = parse_line("vlc\tStopped").unwrap();
        assert_eq!(np.status, PlayStatus::Stopped);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn empty_line_means_no_player() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("\n"), None);
    }

    #[test]
    fn line_without_player_name_is_ignored() {
        assert_eq!(parse_line("\tPlaying\tfile:///a.png"), None);
    }

    #[test]
    fn unknown_status_is_ignored() {
        assert_eq!(parse_line("vlc\tBuffering\tfile:///a.png"), None);
    }

    #[test]
    fn trailing_newline_is_stripped() {
        let (_, np) = parse_line("vlc\tPlaying\tfile:///art.jpg\n").unwrap();
        assert_eq!(np.art_url.as_deref(), Some("file:///art.jpg"));
    }

    #[test]
    fn snapshot_lists_every_player() {
        // Shape of the real output on 2026-11-23: a paused browser tab next
        // to a playing Spotify.
        let out = "chromium.instance452832\tPaused\t\n\
                   spotify\tPlaying\thttps://i.scdn.co/image/abc\n";
        assert_eq!(
            parse_snapshot(out),
            vec![
                (
                    "chromium.instance452832".to_string(),
                    np(PlayStatus::Paused, None)
                ),
                (
                    "spotify".to_string(),
                    np(PlayStatus::Playing, Some("https://i.scdn.co/image/abc"))
                ),
            ]
        );
    }

    #[test]
    fn snapshot_skips_unparsable_lines() {
        let out = "vlc\tBuffering\t\nspotify\tPaused\t\n";
        assert_eq!(
            parse_snapshot(out),
            vec![("spotify".to_string(), np(PlayStatus::Paused, None))]
        );
    }

    #[test]
    fn empty_snapshot_means_no_players() {
        assert_eq!(parse_snapshot(""), vec![]);
    }
}
