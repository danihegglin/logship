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
- **Search as you type.** Literal or regex, case-sensitive or not. A line matches when the pattern matches within that line, as with grep. The regex runs over large slices of the file at once, in time proportional to the file size whatever the pattern, is debounced, and is cancelled when you keep typing.
- **Filter or Highlight.** Filter mode shows only matching lines. Highlight mode keeps every line and lets you jump between matches with `n` / `N`.
- **Level detection.** Recognizes `ERROR` `WARN` `INFO` `DEBUG` `TRACE`, as well as `level=error`, `"level":"warn"` and `[info]`. Lines get a colored stripe, and the toolbar chips show counts and filter by level.
- **Scrollbar overview.** The scrollbar marks where errors, warnings and matches are in the file.
- **Live tail.** Appended lines are indexed as they arrive, and Follow mode keeps the view on the newest lines. If the file is truncated or rotated, it is reopened.
- **Newest first.** Flip the order so the latest lines are at the top (next to Follow, or ⇧⌘R). Follow then stays at the top, and when you've scrolled away new lines arrive without moving what you're reading. The choice is remembered.
- **Line cleanup.** ANSI color codes are stripped, tabs are expanded, invalid UTF-8 is shown safely, and very long lines are cut off before drawing.
- **Exact copy.** Select text with the mouse, character by character, by word (double-click) or by line (triple-click), across as many lines as you like; dragging past the edge scrolls. Copying gives the text exactly as it is in the file, tabs included and long lines not cut short, minus the color codes and invisible control characters, so a pasted log line can't run anything hidden in it. With a filter on, only the lines you see are copied. A single copy takes at most 64 MB. On Linux the selection is also pasteable with the middle button.
- **Tabs.** Open several logs side by side. Open tabs and recently opened files are remembered and come back on the next start.
- **Settings** (⚙ or ⌘,). Pick the font and size and a dark or light theme. [Nordwand Mono](https://github.com/tywr/Nordwand-Mono) is built in and the default; installed monospace fonts like JetBrains Mono or Fira Code are offered too.

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

Linux and Windows builds are available for x86_64 and aarch64. The Linux builds need glibc 2.17 or newer (RHEL/CentOS 7, Ubuntu 18.04 and later) and a Vulkan driver. The macOS app is ad-hoc signed but not notarized, so the first time you open it, right-click it and choose **Open**.

Every push to `main` is released automatically as the next patch version (`.github/workflows/auto-tag.yml`): if the newest tag is `v0.2.1`, the push is tagged `v0.2.2` and a GitHub release with every file attached is published. Put `[skip ci]` in the commit message to push without releasing. If a push to `main` didn't start a release (GitHub occasionally drops the event), open the **Auto tag** workflow in the Actions tab and click **Run workflow** on `main`. For a new minor or major version, push a tag yourself (`git tag v0.3.0 && git push origin v0.3.0`); later pushes continue from it (`v0.3.1`, …). The tag sets the version, so `Cargo.toml` doesn't need a bump first, and a short tag like `v0.2` becomes `0.2.0`. Pull requests only build and test, without packaging.

## 🗺️ Keys

On Linux and Windows, use Ctrl where the table says ⌘.

| Key | Action |
| --- | --- |
| ⌘O | Open files (each in its own tab) |
| ⌘T / ⌘W | New tab / close tab |
| ⌃Tab / ⌃⇧Tab, ⇧⌘] / ⇧⌘[ | Next / previous tab |
| ⌘, | Settings |
| ⌘F or `/` | Search |
| Enter / ⇧Enter (in search) | Next / previous match |
| `n` / `N`, ⌘G / ⇧⌘G | Next / previous match |
| ↑ ↓ or `j` `k` | Move selection |
| PgUp PgDn / Space | Page up / down |
| Home End or `g` `G` | Top / bottom |
| ← → or `h` `l`, `0` | Scroll sideways, reset |
| Enter (in list) | Leave filter mode, keeping the selected line in context |
| Drag, double-click, triple-click | Select text, a word, a line (⇧-click extends) |
| ⌘C | Copy the selection, or the selected line if nothing is selected |
| ⌘A | Select all shown lines |
| `F`, ⇧⌘F | Follow the tail |
| ⇧⌘R | Newest first / oldest first |
| ⌥⌘C / ⌥⌘R / ⌥⌘F | Case-sensitive / regex / filter vs. highlight |
| ⌥-click a level chip | Show only that level |
| ⇧⌘D | Light / dark |

## 🔭 Under the hood

```text
 src/
 ├── index.rs    mmap + parallel line index, level detection, incremental append
 ├── search.rs   chunked parallel regex filter with generation-based cancellation
 ├── view.rs     GPUI view: virtualized rows, scrollbar + minimap, text selection, input
 ├── workspace.rs  window root: tabs, settings panel, open/close, drag and drop
 ├── settings.rs   font, size and theme; the bundled Nordwand Mono
 ├── session.rs    saves settings, open tabs and recent files between runs
 ├── theme.rs    dark (default) and light palettes
 ├── decorations.rs  own title bar where the desktop draws none (GNOME on Wayland)
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

## 🛡️ Security

Please report vulnerabilities privately, not in public issues. See [SECURITY.md](SECURITY.md) for how, and for how to verify downloads.

## 📜 License

MIT. See [LICENSE](LICENSE).

The bundled [Nordwand Mono](https://github.com/tywr/Nordwand-Mono) font is © Tanguy Vivier, under the SIL Open Font License 1.1 ([assets/fonts/LICENSE-NordwandMono.txt](assets/fonts/LICENSE-NordwandMono.txt)).

<div align="center">
<sub>Yo-ho-ho and a bottle of <code>grep</code>.</sub>
</div>
