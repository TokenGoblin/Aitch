#!/usr/bin/env python3
"""Turn Cargo.lock into the source list a Flatpak build can use offline.

    packaging/linux/flatpak/generate-cargo-sources.py > cargo-sources.json

A Flathub build has no network. Cargo very much wants one, so every crate has
to arrive as a declared source that flatpak-builder fetches and verifies up
front, and cargo has to be pointed at those instead of at crates.io.

Everything needed is already in Cargo.lock: each registry crate records its
name, version and sha256. Nothing is downloaded to write this file, and the
checksums are the lock file's own, so what a Flatpak build compiles is exactly
what `cargo build` compiles here.

The output is not committed. It is a few thousand lines that would need
regenerating on every dependency change, and a stale copy is worse than none;
the manifest's README says to run this.

Only crates.io dependencies are handled, which is all this project has --
`cargo deny check sources` enforces that, so a git dependency would fail there
before it reached here. One turning up is an error rather than a silent
omission, because a missing crate surfaces as a compile failure inside the
sandbox with nothing to say why.
"""

import json
import re
import sys
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"

# What cargo is told once the crates are unpacked: use the vendor directory
# and never look at the network.
CARGO_CONFIG = """\
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "cargo/vendor"
"""


def packages(lock_text):
    """Every [[package]] in the lock file, as a dict."""
    for block in lock_text.split("[[package]]")[1:]:
        entry = {}
        for key in ("name", "version", "source", "checksum"):
            match = re.search(rf'^{key} = "(.*)"$', block, re.MULTILINE)
            if match:
                entry[key] = match.group(1)
        yield entry


def main():
    root = Path(__file__).resolve().parents[3]
    lock = root / "Cargo.lock"
    entries = list(packages(lock.read_text(encoding="utf-8")))
    if not entries:
        sys.exit(f"no packages found in {lock}")

    sources = []
    vendored = 0
    for entry in entries:
        source = entry.get("source")

        # No source at all means a path dependency: this workspace's own
        # crates, which are built rather than fetched.
        if source is None:
            continue

        if source != CRATES_IO:
            sys.exit(
                f"{entry['name']} {entry['version']} comes from {source!r}, "
                "which this generator does not handle. Only crates.io "
                "dependencies can be vendored this way."
            )

        checksum = entry.get("checksum")
        if not checksum:
            sys.exit(f"{entry['name']} {entry['version']} has no checksum in Cargo.lock")

        name, version = entry["name"], entry["version"]
        dest = f"cargo/vendor/{name}-{version}"

        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )

        # cargo refuses to use a vendored crate that cannot prove what it is.
        # The file list is empty on purpose: that tells cargo the directory
        # was not modified after unpacking, which is the case here and is what
        # `cargo vendor` itself writes for a registry crate.
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
        vendored += 1

    sources.append(
        {
            "type": "inline",
            "contents": CARGO_CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )

    json.dump(sources, sys.stdout, indent=4)
    sys.stdout.write("\n")
    print(f"{vendored} crates vendored", file=sys.stderr)


if __name__ == "__main__":
    main()
