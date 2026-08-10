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

/// The `--format` template handed to playerctl. Lives next to [`parse_line`]
/// because the two are one contract: change one, change the other.
pub const PLAYERCTL_FORMAT: &str = "{{status}}\t{{mpris:artUrl}}";

/// Parses one line of `playerctl --follow metadata --format PLAYERCTL_FORMAT`.
/// Returns None for empty lines (no active player) or unknown statuses.
pub fn parse_line(line: &str) -> Option<NowPlaying> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(2, '\t');
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
    Some(NowPlaying { status, art_url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playing_with_art_url() {
        let np = parse_line("Playing\thttps://i.scdn.co/image/abc123").unwrap();
        assert_eq!(np.status, PlayStatus::Playing);
        assert_eq!(
            np.art_url.as_deref(),
            Some("https://i.scdn.co/image/abc123")
        );
    }

    #[test]
    fn parses_paused_without_art_url() {
        let np = parse_line("Paused\t").unwrap();
        assert_eq!(np.status, PlayStatus::Paused);
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn parses_stopped_with_missing_field() {
        let np = parse_line("Stopped").unwrap();
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
        assert_eq!(parse_line("Buffering\tfile:///a.png"), None);
    }

    #[test]
    fn trailing_newline_is_stripped() {
        let np = parse_line("Playing\tfile:///art.jpg\n").unwrap();
        assert_eq!(np.art_url.as_deref(), Some("file:///art.jpg"));
    }
}
