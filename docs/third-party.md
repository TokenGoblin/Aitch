# Third-party licences

Aitch itself is MIT — see [`LICENSE`](../LICENSE). The binary it builds also
contains code from its dependencies, and this is the part of that worth
knowing about.

## The one that is not permissive

**`nucleo` and `nucleo-matcher` are MPL-2.0.** They are the fuzzy matcher
behind `^T`, and they offer no permissive alternative, so the shipped binary
contains MPL-2.0 code. That is allowed and needs nothing from you: the Mozilla
Public Licence is weak copyleft per *file*, so linking it into an MIT-licensed
program and redistributing the result is fine. The obligation it does carry is
that the source of those files stays available and that changes to them are
published. Nothing here changes them, and the source is on
[crates.io](https://crates.io/crates/nucleo).

## Everything else

Permissive, and near enough all of it MIT: 338 crates of MIT, then Zlib,
Unlicense, ISC, BSD-2-Clause, BSD-3-Clause, Apache-2.0, BSL-1.0, CC0-1.0,
Unicode-3.0 and 0BSD.

Two crates offer a copyleft licence *alongside* permissive ones, and this
project takes the permissive side, which is what the licence lets you do:

| Crate | Offered as | Taken as |
|---|---|---|
| `self_cell` | `Apache-2.0 OR GPL-2.0-only` | Apache-2.0 |
| `r-efi` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | MIT |

## Checking this yourself

[`deny.toml`](../deny.toml) is the machine-readable version, and CI runs it on
every push:

```
cargo deny check
```

It fails on a security advisory, a yanked crate, a licence not on the list, a
wildcard version, or anything from outside crates.io. The full breakdown of
which crate carries which licence:

```
cargo deny list
```

## Known unmaintained crates

Neither is a vulnerability, and both are recorded in `deny.toml` rather than
silently allowed.

- **`ttf-parser`** ([RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192))
  — the author has stated it will receive no further fixes, and the advisory
  says no safe upgrade exists. It arrives twice, through
  `cosmic-text → fontdb` and through `winit → sctk-adwaita → ab_glyph`, so it
  leaves when those move to `skrifa` and not before. Worth keeping an eye on:
  it parses fonts, which is untrusted input in a complex binary format.
- **`paste`** ([RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436))
  — a proc-macro reached through `wgpu-hal → metal`, wgpu's macOS backend. It
  is in the lock file because a lock file covers every target; it is compiled
  on neither platform this ships to.
