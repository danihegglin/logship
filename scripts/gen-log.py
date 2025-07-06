#!/usr/bin/env python3
"""Generate a realistic-looking log file for demos and benchmarks.

    scripts/gen-log.py demo.log                 # 5M lines (~375 MB)
    scripts/gen-log.py demo.log --lines 100000  # smaller
    scripts/gen-log.py demo.log --live          # keep appending ~20 lines/s (for tail/follow)
"""

import argparse
import random
import sys
import time
from datetime import datetime, timedelta, timezone

LEVELS = ["INFO"] * 70 + ["DEBUG"] * 18 + ["WARN"] * 7 + ["ERROR"] * 3 + ["TRACE"] * 2
SERVICES = ["api", "db", "auth", "worker", "cache", "gateway", "crew", "cannon"]
MESSAGES = [
    "request completed status=200 path=/v1/users/{a} dur={b}ms",
    "cache miss key=user:{a} ttl={b}s",
    "connection reset by peer id={a} retry={b}",
    "slow query took {b}ms rows={a}",
    "token refreshed for user {a} exp={b}",
    "job {a} finished in {b}ms",
    "cargo manifest {a} loaded crates={b}",
    "spotted ship on the horizon bearing={b} id={a}",
]
STACK = (
    "\tat com.example.Handler.process(Handler.java:42)\n"
    "\tat com.example.Server.run(Server.java:117)\n"
)


def line(ts: datetime, rng: random.Random) -> str:
    level = rng.choice(LEVELS)
    msg = rng.choice(MESSAGES).format(a=rng.randint(1, 99999), b=rng.randint(1, 999))
    out = f"{ts.isoformat(timespec='milliseconds')[:-6]}Z {level:5} [{rng.choice(SERVICES)}] {msg}\n"
    if level == "ERROR" and rng.random() < 0.3:
        out += STACK
    return out


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("path")
    p.add_argument("--lines", type=int, default=5_000_000)
    p.add_argument("--live", action="store_true", help="append lines forever instead of writing a file")
    p.add_argument("--rate", type=float, default=20, help="lines per second in --live mode")
    args = p.parse_args()

    rng = random.Random(1)
    if args.live:
        with open(args.path, "a", buffering=1) as f:
            while True:
                f.write(line(datetime.now(timezone.utc), rng))
                time.sleep(1 / args.rate)

    ts = datetime(2026, 9, 30, 8, 0, tzinfo=timezone.utc)
    with open(args.path, "w", buffering=1 << 20) as f:
        for _ in range(args.lines):
            ts += timedelta(milliseconds=rng.randint(0, 40))
            f.write(line(ts, rng))
    print(f"wrote {args.lines:,} lines to {args.path}", file=sys.stderr)


if __name__ == "__main__":
    main()
