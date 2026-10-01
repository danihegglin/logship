<div align="center">

```text
                                      _____
                                     | x x |
                                     |  ^  |
                                     |_===_|
                                        |
                      |                 |                 |
                    __|__             __|__             __|__
                   /     \           /     \           /     \
                  | ERROR |         | WARN  |         | INFO  |
                  |       |         |       |         |       |
                   \_____/           \_____/           \_____/
                      |                 |                 |
         _____________|_________________|_________________|____________
         \    (o)     (o)     (o)     (o)     (o)     (o)     (o)     /
          \   grep    tail    less    awk     sed     cut     jq     /
~~~~~~~~~~~\________________________________________________________/~~~~~~~~~~~
 ~~~~   ~~~~~~   ~~~~~     L   O   G   S   H   I   P     ~~~~~   ~~~~~~   ~
     ~~~~~~   ~~~~   sailing the seven gigabytes at GPU speed   ~~~~   ~~~~
```

**Sail through gigabytes of logs at GPU speed.**

A very fast log file explorer written in Rust on [GPUI](https://www.gpui.rs/), the GPU-accelerated UI framework behind [Zed](https://zed.dev).

![Rust](https://img.shields.io/badge/rust-2024-orange?logo=rust)
![GPUI](https://img.shields.io/badge/ui-GPUI-blue)
![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

<img src="docs/demo.gif" alt="logship demo: scrolling 5M lines, filtering, regex search, live tail, light/dark theme" width="900">

</div>

---

## 🏴‍☠️ Why logship?

Most log viewers run out of steam somewhere around a few hundred megabytes. logship memory-maps the file, indexes it on every core, and draws only the lines on screen with the GPU. Large files open about as quickly as small ones.

| Opening 5.1M lines / 376 MB (Apple Silicon laptop) | Time |
| --- | ---: |
| Open and index the whole file | **61 ms** |
| Full-text search, case-insensitive | **34 ms** |
| Full-text search, case-sensitive | **17 ms** |
| Regex search | **29 ms** |
| Show only `ERROR` lines | **0.6 ms** |

Check these numbers against your own logs with `cargo run --release --example bench -- your.log "query"`.

## ⚓ Features

- **Memory-mapped, parallel indexing.** `memchr` and `rayon` build a line index and detect each line's level in one pass. The file is never copied into memory.
- **Scrolling without limits.** Only the rows on screen are built each frame. The scroll position is stored as a line number plus a pixel offset, so scrolling stays pixel-precise even at line 400,000,000.
- **Search as you type.** Literal or regex, case-sensitive or not. The regex runs over large slices of the file at once, is debounced, and is cancelled when you keep typing.
- **Filter or Highlight.** Filter mode shows only matching lines. Highlight mode keeps every line and lets you jump between matches with `n` / `N`.
- **Level detection.** Recognizes `ERROR` `WARN` `INFO` `DEBUG` `TRACE`, as well as `level=error`, `"level":"warn"` and `[info]`. Lines get a colored stripe, and the toolbar chips show counts and filter by level.
- **Scrollbar overview.** The scrollbar marks where errors, warnings and matches are in the file.
- **Live tail.** Appended lines are indexed as they arrive, and Follow mode keeps the view at the bottom. If the file is truncated or rotated, it is reopened.
- **Line cleanup.** ANSI color codes are stripped, tabs are expanded, invalid UTF-8 is shown safely, and very long lines are cut off before drawing.
- **Dark by default**, with a light theme on ⇧⌘D.

## 🧭 Set sail

```sh
git clone https://github.com/danihegglin/logship && cd logship
cargo run --release -- /var/log/system.log
```

You can also open a file with ⌘O or drag one onto the window.

> **macOS:** gpui is built with `runtime_shaders`, so the Metal shaders compile when the app starts and the build doesn't need Xcode's separate Metal toolchain.

### Prebuilt downloads

Every tagged release publishes builds from GitHub Actions (`.github/workflows/release.yml`):

| Platform | Files |
| --- | --- |
| macOS (Apple Silicon + Intel) | `logship-*-macos-universal.dmg`, per-arch `.tar.gz` |
| Debian, Ubuntu, Mint, Pop!_OS | `logship_*.deb` |
| Fedora, openSUSE, RHEL | `logship-*.rpm` |
| Arch, Manjaro, EndeavourOS | `logship-bin-*.pkg.tar.zst` (`sudo pacman -U …`), plus a `PKGBUILD` for the AUR |
| Any Linux distro | `logship-*.AppImage`, `.tar.gz` |
| Windows | `logship-*-pc-windows-msvc.zip` |

Linux and Windows builds are available for x86_64 and aarch64. The macOS app is ad-hoc signed but not notarized, so the first time you open it, right-click it and choose **Open**.

To cut a release, bump `version` in `Cargo.toml` and push a matching tag (`git tag v0.1.0 && git push origin v0.1.0`). This publishes a GitHub release with every file attached.

## 🗺️ Keys

| Key | Action |
| --- | --- |
| ⌘O | Open a file |
| ⌘F or `/` | Search |
| Enter / ⇧Enter (in search) | Next / previous match |
| `n` / `N`, ⌘G / ⇧⌘G | Next / previous match |
| ↑ ↓ or `j` `k` | Move selection |
| PgUp PgDn / Space | Page up / down |
| Home End or `g` `G` | Top / bottom |
| ← → or `h` `l`, `0` | Scroll sideways, reset |
| Enter (in list) | Leave filter mode, keeping the selected line in context |
| ⌘C / double-click | Copy line |
| `F`, ⇧⌘F | Follow the tail |
| ⌥⌘C / ⌥⌘R / ⌥⌘F | Case-sensitive / regex / filter vs. highlight |
| ⌥-click a level chip | Show only that level |
| ⇧⌘D | Light / dark |

## 🔭 Under the hood

```text
 src/
 ├── index.rs    mmap + parallel line index, level detection, incremental append
 ├── search.rs   chunked parallel regex filter with generation-based cancellation
 ├── view.rs     GPUI view: virtualized rows, custom scrollbar + minimap, input
 ├── theme.rs    dark (default) and light palettes
 └── main.rs     app, menus, key bindings
```

- **Line index:** each line costs one `u64` for its start offset plus one `u8` for its level. A 100M-line file needs about 900 MB of index.
- **Filtering:** results are sorted line numbers. Mapping a row to a line is an array lookup, and mapping a line back to a row is a binary search.
- **Background work:** indexing, filtering, and building the scrollbar overview all run on the background executor, and the UI thread never touches the file in bulk.

## 🦜 Recording the demo

`docs/demo.gif` is made with a script that builds the app, generates a 5M-line log, drives the app with keystrokes and records it with ffmpeg:

```sh
scripts/record-demo.sh
```

It's macOS-only and needs `ffmpeg`. The terminal running it needs **Screen Recording** and **Accessibility** permission (System Settings → Privacy & Security).

To make test logs for your own experiments:

```sh
scripts/gen-log.py big.log --lines 5000000   # a big file
scripts/gen-log.py big.log --live            # keep appending, to try Follow mode
```

## 📜 License

MIT. See [LICENSE](LICENSE).

<div align="center">
<sub>Yo-ho-ho and a bottle of <code>grep</code>.</sub>
</div>
