# Third-party licences

Aitch itself is MIT — see [`LICENSE`](../LICENSE). As of the zero-dependency
rewrite (`PLAN-ZERO-DEP.md`), the binary has **no crate dependencies at
all** — `cargo tree --workspace` shows only the workspace's own four
crates. There is nothing else's licence to account for.

## What used to be here

Before the rewrite, this page listed the 300-plus dependency crates the
old `winit`/`wgpu`/`cosmic-text`/`tree-sitter`/etc. stack pulled in —
nearly all MIT, one MPL-2.0 (`nucleo`), a couple of unmaintained
transitive crates tracked in a now-deleted `deny.toml`. None of that
applies any more; see [`PLAN-ZERO-DEP.md`](../PLAN-ZERO-DEP.md) for what
replaced each one and why.

## Bundled assets

**`crates/aitch-ui/assets/fonts/DejaVuSansMono.ttf`** — DejaVu Sans Mono
2.37, bundled starting with the zero-dependency rewrite so the
hand-written font parser and rasterizer have one known monospace font to
draw, with no system font discovery. DejaVu's changes are public domain;
the underlying Bitstream Vera glyphs are under the Bitstream Vera Fonts
Copyright, a permissive licence with no attribution requirement for a
compiled build. Full text in
`crates/aitch-ui/assets/fonts/DejaVuSansMono-LICENSE.txt`, alongside the
font. Source: <https://github.com/dejavu-fonts/dejavu-fonts>.

## Checking this yourself

`cargo tree --workspace` is the whole check now: it should show exactly
`aitch`, `aitch-core`, `aitch-ui`, `aitch-harness`, and nothing else.
`dependency-budget.txt` is the machine-readable version CI enforces (via
`scripts/check-dependency-budget.ps1`) — it fails the build if
`Cargo.lock` ever grows a package beyond the four workspace crates.
