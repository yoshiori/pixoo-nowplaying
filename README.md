# pixoo-nowplaying

[![CI](https://github.com/yoshiori/pixoo-nowplaying/actions/workflows/ci.yml/badge.svg)](https://github.com/yoshiori/pixoo-nowplaying/actions/workflows/ci.yml)

Show the currently playing track's artwork on a [Divoom Pixoo 64](https://divoom.com/products/pixoo-64).

Listens to MPRIS (via `playerctl --follow`), so anything that shows up in your
desktop's media controls — Spotify, browser playback, mpv — gets its album art
rendered on the Pixoo. When playback stops, the Pixoo is returned to its
regular channel (clock, weather, ...).

It can also follow a HomePod on the same network, so music started from a
phone or by Siri shows up too. When both are playing, the one that started
more recently owns the screen.

## Requirements

- Linux with a session D-Bus and [`playerctl`](https://github.com/altdesktop/playerctl)
- A Pixoo 64 reachable on your LAN
- For the HomePod source: [pyatv](https://pyatv.dev/)'s `atvscript` and
  `atvremote` on your `PATH` (`uv tool install pyatv`)

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
idle_restore_secs = 30  # how long playback must be stopped before restoring
# restore_channel = 1   # unset: restore to whatever channel was showing
                        # when artwork took over (Channel/GetIndex)
# excluded_players = ["chromium", "firefox"]
                        # players the daemon never follows (e.g. YouTube
                        # thumbnails from a browser). Passed to playerctl as
                        # --ignore-player, so other players keep working while
                        # an excluded one is active. Use names as reported by
                        # `playerctl -l`; a base name also covers its
                        # ".instanceNNN" variants
```

## Following a HomePod

Find the device with `atvremote scan` and take its identifier:

```
       Name: Living Room
   Model/SW: HomePod Mini, tvOS 26.6
    Address: 192.168.0.141
        MAC: 46:B5:0E:8C:4A:17
Services:
 - Protocol: AirPlay, Port: 7000, Credentials: None, Pairing: NotNeeded
```

Then add a `[homepod]` table at the *end* of the config file (a TOML table
swallows every key after it):

```toml
[homepod]
identifier = "46:B5:0E:8C:4A:17"
# address = "192.168.0.141"  # optional: skips pyatv's mDNS discovery, which
                             # is worth doing on a host with many interfaces
```

No pairing is needed — a HomePod accepts pyatv's transient AirPlay pairing.

Artwork is only as good as what the HomePod exposes: Apple Music playback
carries its cover art, but a third-party app streaming in over AirPlay
[does not](https://github.com/postlund/pyatv/issues/2891), and those tracks
show up as playback without artwork.

## Run

```sh
pixoo-nowplaying
```

Or as a systemd user service:

```sh
cp systemd/pixoo-nowplaying.service ~/.config/systemd/user/
systemctl --user enable --now pixoo-nowplaying
```
