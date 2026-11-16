# Contributing to moth

Thanks for your interest in moth! Bug reports, ideas and pull requests are all welcome. Please follow the
[Code of Conduct](CODE_OF_CONDUCT.md) in all project spaces.

## Reporting bugs and requesting features

Open an issue on [GitHub](https://github.com/sortedcord/moth/issues). For bugs, include:

- what you did, what you expected, and what happened instead;
- your distribution, compositor and GPU (moth is Wayland-only and leans on VA-API and Vulkan);
- the video URL, if it's specific to one video;
- moth's terminal output. For playback problems, the player debug panel (`F8` on the watch page) and
  `moth status` output help a lot.

For larger changes, please open an issue to discuss the approach before writing a lot of code.

## Development setup

Follow the [installation steps](README.md#installation) to get the system dependencies and the vendored gpui
(`./scripts/fetch-vendor.sh`). Then use the debug build while developing:

```sh
cargo build
./target/debug/moth
cargo test
```

The dev profile compiles dependencies with optimizations, so the debug build is fast enough to judge
performance, while moth's own crate stays quick to rebuild.

### Debug tools

| | |
|---|---|
| `MOTH_OPEN=<home card index \| URL \| video ID>` | Open a video at startup |
| `MOTH_CARDS=<n>` | Fill the home grid with n cards (stress test) |
| `MOTH_FRAME_STATS=1` / `F12` | gpui's frame-time overlay |
| `F9` | Outline where each moving grid card is heading |
| `F10` | 5× slow motion for layout animations |
| `F8` / `MOTH_PLAYER_STATS=0\|1` | Player debug panel (a setting, on by default): clock, caps, dropped frames, queues, A/V offset, network |
| `MOTH_FORMAT=<yt-dlp format>` | Override the yt-dlp stream selection (e.g. to force AV1) |
| `moth status --watch` | Follow the running instance's state as JSON |
| `playerctl -p moth …` | Drive playback without the keyboard (play-pause, `position 30+`, `open <url>`) |

Position discontinuities during playback are logged to stderr as `moth: playback jumped …`.

## Architecture

moth is one crate in three layers, with dependencies pointing one way: `ui` → `backend`, `ui` → `desktop`.

- **`backend/`** knows about sites and media, never about gpui. The UI reaches sites only through `Backend`
  and the neutral types in `backend/media.rs`; it never imports RustyPipe or GStreamer.
- **`desktop/`** (socket, `moth status`, MPRIS, native host) doesn't know about gpui either: the UI publishes a
  `NowPlaying` to the `Hub` and receives `Command`s.
- **`ui/`** is gpui. `AppView` owns navigation; `PlaybackSession` owns playback state and the player; pages with
  state of their own (the watch page) are entities. Shortcuts are gpui actions listed once in `ui/actions.rs`.

### Adding a video source

Implement `backend::source::VideoSource` (resolve a link, details, streams, optionally alternate URLs), register it
in `Backend::new`, and add its URL check to `source::claims_url` so the browser extension's host accepts its links.
Sources return raw values (counts, durations, dates); formatting is the UI's job (`ui/format.rs`).

### Conventions

- **Opinions are settings.** If a behavior is a matter of taste (a default, a step size, a preference order),
  put it in `Settings` with a sensible default and expose it on the settings page, rather than a constant.
- **Async work belongs to whoever shows its result.** Keep the `Task` in the entity (dropping it cancels the
  work) instead of checking generation counters on completion.
- **No side effects in paint**, beyond reporting where something was drawn (see `VideoAnchor`).
- Use `Duration` for time and enums for states; don't pass meaning around in strings.

## Pull requests

- Keep each pull request focused on one change, and describe what it does and why.
- Make sure `cargo build` has no new warnings and `cargo test` passes. Add tests for logic that can be tested
  without a window (see `src/backend/selection.rs` and `src/ui/layout.rs`).
- Try your change in the running app. For UI changes, include a screenshot or short recording; for playback
  changes, mention the videos and hardware you tested with.
- Update the README when you change user-facing behavior (keys, CLI, socket protocol, MPRIS).
- Changes to gpui go in `patches/` as a patch against the pinned revision in `scripts/fetch-vendor.sh`, not as
  edits under `vendor/` (which isn't tracked).

### Code style

Run `cargo fmt` (see `rustfmt.toml`) and keep `cargo clippy --all-targets` free of warnings. Prefer clear names
over comments, and use comments to explain *why*: the YouTube, GStreamer and Wayland quirks moth works around are
easy to undo by accident, so note them where they're handled.

## License

moth is licensed under the [GNU General Public License v3.0](LICENSE). By contributing, you agree that your
contributions are licensed under the same terms.
