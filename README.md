# pixoo-nowplaying

Show the currently playing track's artwork on a [Divoom Pixoo 64](https://divoom.com/products/pixoo-64).

Listens to MPRIS (via `playerctl --follow`), so anything that shows up in your
desktop's media controls — Spotify, browser playback, mpv — gets its album art
rendered on the Pixoo. When playback stops, the Pixoo is returned to its
regular channel (clock, weather, ...).

## Requirements

- Linux with a session D-Bus and [`playerctl`](https://github.com/altdesktop/playerctl)
- A Pixoo 64 reachable on your LAN

## Setup

```sh
cargo install --path .
mkdir -p ~/.config/pixoo-nowplaying
cat > ~/.config/pixoo-nowplaying/config.toml <<EOF
pixoo_ip = "192.168.0.153"
EOF
```

Optional config keys (defaults shown):

```toml
restore_channel = 1     # Channel/SetIndex target when playback stops
idle_restore_secs = 30  # how long playback must be stopped before restoring
```

## Run

```sh
pixoo-nowplaying
```

Or as a systemd user service:

```sh
cp systemd/pixoo-nowplaying.service ~/.config/systemd/user/
systemctl --user enable --now pixoo-nowplaying
```
