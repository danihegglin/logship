#!/usr/bin/env python3
"""Builds packaging/macos/logship.icns from packaging/linux/logship.svg.

The artwork is placed on Apple's icon grid (an 824 px body on a 1024 px
canvas, with a soft shadow, so it sits in the Dock like other apps) and
rendered with rsvg-convert at every size an .icns holds. Run it after changing
the icon and commit the result:

    packaging/macos/make-icns.py    # needs rsvg-convert (librsvg)
"""

import re
import struct
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "packaging/linux/logship.svg"
TARGET = ROOT / "packaging/macos/logship.icns"

# .icns entry types holding PNG data, by pixel size (Retina sizes reuse the
# next larger image).
TYPES = [
    ("icp4", 16), ("ic11", 32), ("icp5", 32), ("ic12", 64),
    ("ic07", 128), ("ic13", 256), ("ic08", 256), ("ic14", 512),
    ("ic09", 512), ("ic10", 1024),
]


def mac_svg() -> str:
    source = SOURCE.read_text()
    body = re.search(r"<svg[^>]*>(.*)</svg>", source, re.S).group(1)
    # The 240 px rounded square of the 256 px original becomes Apple's 824 px
    # body, with the corner radius macOS icons use.
    background = '<rect x="8" y="8" width="240" height="240" rx="48"'
    assert background in body, "the icon's background rect changed; update make-icns.py"
    body = body.replace(background, '<rect x="8" y="8" width="240" height="240" rx="54"')
    scale = 824 / 240
    offset = 100 - 8 * scale
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">
      <feDropShadow dx="0" dy="10" stdDeviation="12" flood-color="#000" flood-opacity="0.3"/>
    </filter>
  </defs>
  <g filter="url(#shadow)" transform="translate({offset:.3f} {offset:.3f}) scale({scale:.5f})">{body}</g>
</svg>
"""


def render(svg: str, size: int) -> bytes:
    return subprocess.run(
        ["rsvg-convert", "--width", str(size), "--height", str(size), "--format", "png"],
        input=svg.encode(),
        capture_output=True,
        check=True,
    ).stdout


def main() -> None:
    svg = mac_svg()
    pngs = {size: render(svg, size) for size in {size for _, size in TYPES}}
    entries = b"".join(
        kind.encode() + struct.pack(">I", 8 + len(pngs[size])) + pngs[size] for kind, size in TYPES
    )
    TARGET.write_bytes(b"icns" + struct.pack(">I", 8 + len(entries)) + entries)
    print(f"wrote {TARGET.relative_to(ROOT)} ({TARGET.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
