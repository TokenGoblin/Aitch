#!/usr/bin/env bash
#
# Build the Linux packages: a tarball, a .deb and an AppImage.
#
#     packaging/linux/build.sh [--version X.Y.Z] [--skip-build]
#
# Everything lands in target/dist, beside what the Windows script produces.
#
# There is no static build here and there cannot be. PLAN.md Phase 8 asks for
# one "to avoid glibc pain", but winit, wgpu and cosmic-text all dlopen their
# X11, Wayland and Vulkan libraries at run time, and a statically linked musl
# binary cannot dlopen at all. The achievable version is what AppImage assumes
# anyway: build against the oldest glibc worth supporting and let the rest be
# found at run time. The release workflow does that by building on the oldest
# runner image rather than the newest.

set -euo pipefail

version=""
skip_build=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="$2"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$root/packaging/linux"
dist="$root/target/dist"
binary="$root/target/release/aitch"

if [ -z "$version" ]; then
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
fi
[ -n "$version" ] || { echo "could not read the version out of Cargo.toml" >&2; exit 1; }
echo "Packaging Aitch $version"

# The documents that ship beside the binary, the same five the Windows
# installer carries. A missing one is a build failure rather than a silent
# omission; crates/aitch-harness/tests/documentation.rs checks the same list.
docs=(README.md LICENSE docs/guide.md docs/config.md docs/keymap.md docs/third-party.md)
for doc in "${docs[@]}"; do
    [ -f "$root/$doc" ] || { echo "ships $doc, which is not there" >&2; exit 1; }
done

if [ "$skip_build" -eq 0 ]; then
    # Rust writes the absolute path of every source file into panic messages
    # and debug info, so the build machine's home directory and its CARGO_HOME
    # end up in the shipped binary. `trim-paths` would do this in the profile
    # but is not stable in cargo 1.98, so remap by hand -- and note the
    # separator is U+001F, because RUSTFLAGS splits on spaces and a checkout
    # under a path with a space in it would tear a flag in half.
    cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    flags="--remap-path-prefix=$cargo_home=[cargo]"
    flags="$flags"$'\x1f'"--remap-path-prefix=$root=[aitch]"
    flags="$flags"$'\x1f'"--remap-path-prefix=$HOME=[home]"

    # rustc's remapping does not reach the C compiler, and a good deal of this
    # tree is C: every tree-sitter grammar is built by the `cc` crate. Those
    # have to be told separately, or the build machine's paths come back in
    # through the back door.
    c_map="-ffile-prefix-map=$cargo_home=[cargo]"
    c_map="$c_map -ffile-prefix-map=$root=[aitch]"
    c_map="$c_map -ffile-prefix-map=$HOME=[home]"

    (
        cd "$root"
        CARGO_ENCODED_RUSTFLAGS="$flags" \
        CFLAGS="${CFLAGS:-} $c_map" \
        CXXFLAGS="${CXXFLAGS:-} $c_map" \
            cargo build --release -p aitch
    )
fi

[ -f "$binary" ] || { echo "no release binary at $binary; run without --skip-build" >&2; exit 1; }

mkdir -p "$dist"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Everything from here on packages a copy, never the tree's own artifact.
# Stripping target/release/aitch in place would leave cargo none the wiser --
# its fingerprint is unchanged, so the next `cargo build --release` relinks
# nothing and whoever ran this locally is left with a symbol-free binary and
# useless backtraces until something else forces a rebuild.
staged="$work/aitch"
cp "$binary" "$staged"

# Stripped before it is checked or shipped, which is what a Linux package is
# expected to carry anyway -- Debian asks for it -- and which takes the symbol
# and debug tables out along with anything hiding in them. Panic messages are
# unaffected: their file and line come from `file!()`, which is a string in the
# binary rather than debug info, and the remapping above already covers those.
if command -v strip >/dev/null 2>&1; then
    before="$(stat -c%s "$staged")"
    strip --strip-unneeded "$staged"
    after="$(stat -c%s "$staged")"
    echo "Stripped: $((before / 1024)) KiB -> $((after / 1024)) KiB"
fi

# Nothing about this machine goes out in a published binary. A hard failure,
# not a warning: far easier to notice here than after upload.
#
# Written out rather than as one `if` with a pipeline in it, because that
# version failed *open*. `strings | grep | sort -u | head -20` under
# `set -o pipefail` returns 141 once sort's output outruns the pipe buffer and
# head walks away: SIGPIPE, non-zero status, the `&&` short-circuits and the
# check is skipped. So it passed silently in exactly the case it exists for --
# a binary built with no remapping at all, which has tens of thousands of
# paths in it. Truncation happens when reporting, never before the test.
command -v strings >/dev/null 2>&1 || {
    echo "strings is not installed, so the binary cannot be checked" >&2
    exit 1
}
leaks="$(strings -a "$staged" | grep -E '/home/[a-zA-Z0-9_.-]+' | sort -u || true)"
if [ -n "$leaks" ]; then
    echo "the release binary carries paths from the machine that built it:" >&2
    # The whole string, not just the path inside it: "it says /home/runner"
    # does not say which part of the build put it there -- rustc, the C
    # compiler behind the tree-sitter grammars, or something else again.
    echo "$leaks" | head -20 | sed 's/^/    /' >&2
    echo "    ... $(echo "$leaks" | wc -l) distinct paths in total" >&2
    exit 1
fi
echo "Binary carries no build-machine paths"

# What it actually needs at run time, for the record and for the .deb's
# dependencies below. dlopened libraries do not appear here, which is the
# point: they are found or gracefully missed at run time, not required to
# start.
echo "Dynamically linked against:"
ldd "$staged" | sed 's/^/    /'

# -- icons -------------------------------------------------------------------
#
# Generated from the editor's own dark theme, the same drawing Windows gets.
# See docs/screenshots.md.
icon_sizes=(16 24 32 48 64 128 256)
icon_args=()
for size in "${icon_sizes[@]}"; do
    icon_args+=("$work/aitch-$size.png")
done
# The same rustflags as the build above. Cargo keys its fingerprints on
# them, so running this without would rebuild every shared dependency and
# then rebuild the editor again on the next run.
(
    cd "$root"
    CARGO_ENCODED_RUSTFLAGS="${flags:-}" \
        cargo run -q -p aitch-ui --release --example make_icon -- "${icon_args[@]}"
)

# Lay out the shared tree once: both the .deb and the AppImage want the same
# thing under usr/, so build it once and copy it into each.
stage="$work/tree"
mkdir -p "$stage/usr/bin" "$stage/usr/share/applications" "$stage/usr/share/doc/aitch"
install -m 755 "$staged" "$stage/usr/bin/aitch"
install -m 644 "$here/aitch.desktop" "$stage/usr/share/applications/aitch.desktop"
for doc in "${docs[@]}"; do
    install -m 644 "$root/$doc" "$stage/usr/share/doc/aitch/$(basename "$doc")"
done
for size in "${icon_sizes[@]}"; do
    dir="$stage/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    install -m 644 "$work/aitch-$size.png" "$dir/aitch.png"
done

# -- the tarball -------------------------------------------------------------
#
# The counterpart of the Windows zip: nothing to install, no root, no package
# manager. Everything sits under one directory so unpacking it in a downloads
# folder does not scatter files across it.
portable="aitch-$version-x86_64-linux"
mkdir -p "$work/$portable"
install -m 755 "$staged" "$work/$portable/aitch"
for doc in "${docs[@]}"; do
    install -m 644 "$root/$doc" "$work/$portable/$(basename "$doc")"
done
install -m 644 "$work/aitch-256.png" "$work/$portable/aitch.png"
# Deliberately no aitch.desktop here. It says `Exec=aitch` and `TryExec=aitch`,
# which are right for a package that puts the binary on PATH and wrong for an
# archive unpacked anywhere -- TryExec would fail to resolve and a launcher
# would hide the entry outright. A desktop file that silently does nothing is
# worse than none.
tar -czf "$dist/$portable.tar.gz" -C "$work" "$portable"
echo "Built $dist/$portable.tar.gz ($(du -h "$dist/$portable.tar.gz" | cut -f1))"

# -- the .deb ----------------------------------------------------------------
deb="$work/deb"
mkdir -p "$deb/DEBIAN"
cp -r "$stage/usr" "$deb/usr"

# Installed-Size is in kibibytes, and dpkg-deb will not work it out for you.
installed_kib="$(du -ks "$deb/usr" | cut -f1)"

# The glibc version is read out of the binary rather than written down. It is
# built on whatever image the workflow uses -- ubuntu-22.04, so glibc 2.35 --
# and a Rust binary from there needs symbols versioned GLIBC_2.34. Declaring
# something older lets `dpkg -i` succeed on, say, Debian 11, and the editor
# then dies at exec with "version `GLIBC_2.34' not found": a package that
# installs and cannot run, which is worse than one that refuses.
#
# The dynamic symbol table survives stripping -- it has to, for linking -- so
# this still works on the stripped copy.
glibc="$(objdump -T "$staged" \
    | grep -oE 'GLIBC_[0-9]+\.[0-9]+' \
    | sed 's/GLIBC_//' \
    | sort -V \
    | tail -1)"
[ -n "$glibc" ] || { echo "could not read the glibc requirement from the binary" >&2; exit 1; }
echo "Needs glibc >= $glibc"

# Beyond libc, Depends is deliberately short. The graphics libraries are
# dlopened rather than linked, so requiring them would refuse to install on a
# machine that could run the editor perfectly well -- over X11 with no Wayland,
# say. They are Recommends, which apt takes by default and lets you decline.
cat > "$deb/DEBIAN/control" <<CONTROL
Package: aitch
Version: $version
Section: editors
Priority: optional
Architecture: amd64
Maintainer: TokenGoblin <noreply@github.com>
Installed-Size: $installed_kib
Depends: libc6 (>= $glibc)
Recommends: libvulkan1, mesa-vulkan-drivers, libxkbcommon0, fonts-dejavu-core
Suggests: libwayland-client0, libx11-6
Homepage: https://github.com/TokenGoblin/Aitch
Description: A text editor with nano's interaction model and VS Code's project model
 Modeless and keyboard-first. The footer is the interface: two rows of
 context-sensitive shortcuts, always visible, generated from the active
 keymap rather than written out by hand. Prompts happen on a line above
 them. No modal dialogs and no floating windows.
 .
 Opens a folder as a project, with a file tree, fuzzy file open and
 project-wide search. Syntax highlighting runs off the drawing thread, so a
 keystroke costs the same however large the file.
CONTROL

# lintian wants a copyright file, and it is the right place to say that the
# binary carries MPL-2.0 code as well as MIT. See docs/third-party.md.
cat > "$deb/usr/share/doc/aitch/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: aitch
Source: https://github.com/TokenGoblin/Aitch

Files: *
Copyright: TokenGoblin
License: MIT

License: MIT
 Permission is hereby granted, free of charge, to any person obtaining a copy
 of this software and associated documentation files (the "Software"), to deal
 in the Software without restriction, including without limitation the rights
 to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 copies of the Software, and to permit persons to whom the Software is
 furnished to do so, subject to the following conditions:
 .
 The above copyright notice and this permission notice shall be included in
 all copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 SOFTWARE.

Comment: The compiled binary also contains code from its dependencies. All of
 it is permissive except nucleo and nucleo-matcher, the fuzzy matcher behind
 ^T, which are MPL-2.0 -- fine to link and redistribute, with the source
 available on crates.io. docs/third-party.md has the detail.
COPYRIGHT

deb_file="$dist/aitch_${version}_amd64.deb"
dpkg-deb --build --root-owner-group "$deb" "$deb_file" >/dev/null
echo "Built $deb_file ($(du -h "$deb_file" | cut -f1))"

# -- the AppImage ------------------------------------------------------------
#
# The primary Linux artifact per PLAN.md: one file, no install, no root, and
# it runs on anything with a new enough glibc.
appdir="$work/AppDir"
mkdir -p "$appdir"
cp -r "$stage/usr" "$appdir/usr"

# An AppImage wants the desktop file and icon at the top of the AppDir as
# well as in the usual places, and .DirIcon is the one the launcher reads.
install -m 644 "$here/aitch.desktop" "$appdir/aitch.desktop"
install -m 644 "$work/aitch-256.png" "$appdir/aitch.png"
cp "$work/aitch-256.png" "$appdir/.DirIcon"

# AppRun rather than a symlink, so the editor starts in the directory the user
# ran it from and $ARGV0 games do not confuse the argument parser.
cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
# The AppImage entry point. HERE is where the image is mounted.
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/aitch" "$@"
APPRUN
chmod 755 "$appdir/AppRun"

# Pinned to a tagged release and checked, rather than pulled from the rolling
# `continuous` tag. appimagetool's own runtime is concatenated into every
# AppImage this builds, so whatever that URL serves on the day ends up inside
# a published artifact: an upstream change, or a compromise, would ride into a
# signed-off release with nothing in the pipeline noticing.
#
# Nor is a locally installed appimagetool used if there happens to be one. The
# point is that the same input produces the same output wherever this runs.
appimagetool_version=1.9.0
appimagetool_sha256=46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1

tool="$work/appimagetool"
echo "Fetching appimagetool $appimagetool_version"
curl -fsSL -o "$tool" \
    "https://github.com/AppImage/appimagetool/releases/download/$appimagetool_version/appimagetool-x86_64.AppImage"
if ! echo "$appimagetool_sha256  $tool" | sha256sum -c - >/dev/null 2>&1; then
    echo "appimagetool does not match the checksum recorded in this script:" >&2
    echo "    got      $(sha256sum "$tool" | cut -d' ' -f1)" >&2
    echo "    expected $appimagetool_sha256" >&2
    exit 1
fi
chmod 755 "$tool"

# --appimage-extract-and-run because a CI runner has no FUSE, and appimagetool
# is itself an AppImage: without this it fails with a mount error that reads
# like a problem with the package being built rather than with the tool.
appimage="$dist/Aitch-$version-x86_64.AppImage"
ARCH=x86_64 "$tool" --appimage-extract-and-run --no-appstream "$appdir" "$appimage"
chmod 755 "$appimage"
echo "Built $appimage ($(du -h "$appimage" | cut -f1))"

echo "Done. In $dist:"
ls -1 "$dist" | sed 's/^/    /'
