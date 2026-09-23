//! Put the application icon into the executable, on Windows.
//!
//! Explorer, Alt-Tab and the Start menu read the icon out of the binary's
//! resources, which is now the only place it comes from: the window class
//! sets no icon of its own. See `docs/screenshots.md`.
//!
//! No crate does this: a hand-written `.rc` naming the icon is compiled by
//! whichever resource compiler the active toolchain provides (`windres` for
//! GNU/MinGW, `rc.exe` for MSVC) and the result is linked straight in.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // The icon lives in packaging/ rather than beside this file, because it is
    // the same file the installer ships to Add/Remove Programs.
    println!("cargo:rerun-if-changed=../../packaging/windows/aitch.ico");

    #[cfg(windows)]
    {
        let icon = "../../packaging/windows/aitch.ico";
        if !Path::new(icon).exists() {
            // Not fatal. A missing icon is a cosmetic problem, and refusing to
            // build the editor over one would be a worse trade. The packaging
            // script checks that a released binary actually got it.
            println!("cargo:warning=no icon at {icon}; building without one");
            return;
        }

        embed_icon(icon);
    }
}

/// Write a `.rc` naming `icon` into `OUT_DIR`, then hand it to whichever
/// resource compiler matches the active toolchain. Every failure here is a
/// warning, never a build error: a machine with no resource compiler must
/// still be able to build and run the editor.
#[cfg(windows)]
fn embed_icon(icon: &str) {
    let Ok(out_dir) = env::var("OUT_DIR") else {
        println!("cargo:warning=OUT_DIR not set; building without an icon");
        return;
    };

    // `.rc` syntax treats `\` inside a quoted string as its own escape
    // character, so a Windows path needs doubled backslashes to survive it.
    // Forward slashes side-step that entirely, and both compilers below
    // accept them fine on Windows.
    let icon_forward = icon.replace('\\', "/");
    let rc_path = Path::new(&out_dir).join("aitch.rc");
    if let Err(e) = std::fs::write(&rc_path, format!("IDI_ICON1 ICON \"{icon_forward}\"\n")) {
        println!("cargo:warning=could not write the resource script: {e}");
        return;
    }

    // Cargo always sets this for a build script: "msvc" or "gnu", matching
    // the target's ABI. That is what decides which resource compiler exists.
    match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("gnu") => compile_with_windres(&rc_path, &out_dir),
        Ok("msvc") => compile_with_rc(&rc_path, &out_dir),
        other => {
            println!(
                "cargo:warning=unrecognized CARGO_CFG_TARGET_ENV {other:?}; building without an icon"
            );
        }
    }
}

/// GNU/MinGW toolchain: `windres` compiles the `.rc` straight to a linkable
/// object file. Tried plain first (WinLibs, MSYS2 and most MinGW
/// distributions put it on `PATH`), then a target-prefixed name some
/// distributions use instead; a deeper filesystem search would be a guess
/// this build script shouldn't make.
#[cfg(windows)]
fn compile_with_windres(rc_path: &Path, out_dir: &str) {
    for windres in ["windres", "x86_64-w64-mingw32-windres"] {
        if Command::new(windres).arg("--version").output().is_err() {
            continue;
        }

        let out_obj = Path::new(out_dir).join("aitch_icon.o");
        match Command::new(windres)
            .args(["-O", "coff", "-o"])
            .arg(&out_obj)
            .arg(rc_path)
            .status()
        {
            Ok(status) if status.success() => {
                println!("cargo:rustc-link-arg-bin=aitch={}", out_obj.display());
            }
            Ok(status) => {
                println!("cargo:warning=windres failed ({status}); building without an icon");
            }
            Err(e) => {
                println!("cargo:warning=could not run windres: {e}");
            }
        }
        return;
    }

    println!("cargo:warning=no working windres found; building without an icon");
}

/// MSVC toolchain: `rc.exe`, part of the Windows SDK, compiles the `.rc` to
/// a `.res` that the MSVC linker accepts as a direct input, same as an
/// object file.
///
/// It is on `PATH` only inside a configured developer environment, which is
/// *not* what CI runs from — a GitHub `windows-latest` runner has the SDK
/// installed and `rc.exe` nowhere on `PATH`. Assuming otherwise is why every
/// release from the zero-dependency rewrite onwards shipped a binary with no
/// icon, until `packaging/windows/build.ps1`'s own check caught it. So look
/// for it properly: `PATH` first, then the environment a developer prompt
/// would have set, then the SDK's own layout.
#[cfg(windows)]
fn compile_with_rc(rc_path: &Path, out_dir: &str) {
    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=no rc.exe found on PATH or in the Windows SDK; building without an icon"
        );
        return;
    };

    let out_res = Path::new(out_dir).join("aitch_icon.res");
    match Command::new(&rc)
        .arg("/fo")
        .arg(&out_res)
        .arg(rc_path)
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bin=aitch={}", out_res.display());
        }
        Ok(status) => {
            println!("cargo:warning=rc.exe failed ({status}); building without an icon");
        }
        Err(e) => {
            println!("cargo:warning=could not run {}: {e}", rc.display());
        }
    }
}

/// Where `rc.exe` actually is, in the order worth trying.
#[cfg(windows)]
fn find_rc() -> Option<PathBuf> {
    // A developer prompt: already on PATH, and whichever one it picked is the
    // one this build should use.
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return Some(PathBuf::from("rc.exe"));
    }

    // `vcvarsall` exports this pointing straight at the versioned bin
    // directory. Honoured before the search below so an explicitly configured
    // SDK always wins over whatever else is installed.
    if let Ok(bin) = env::var("WindowsSdkVerBinPath") {
        for arch in sdk_arch_preference() {
            let candidate = Path::new(&bin).join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // The installed layout: Windows Kits\10\bin\<version>\<arch>\rc.exe.
    // Newest version wins, compared numerically — a string sort puts
    // 10.0.9 after 10.0.10, which would pick an older SDK as releases go on.
    let mut versions: Vec<(Vec<u64>, PathBuf)> = Vec::new();
    for root in sdk_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let parsed: Vec<u64> = name.split('.').filter_map(|p| p.parse().ok()).collect();
            // `bin` also holds unversioned helper directories; a name that is
            // not dotted numbers is one of those, not an SDK.
            if parsed.is_empty() || parsed.len() != name.split('.').count() {
                continue;
            }
            versions.push((parsed, path));
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, dir) in versions {
        for arch in sdk_arch_preference() {
            let candidate = dir.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// The `Windows Kits\10\bin` directories worth looking in. The 32-bit
/// Program Files is where the SDK installs by default even on a 64-bit
/// machine; the other is there for the installs that do not.
#[cfg(windows)]
fn sdk_roots() -> Vec<PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(env::var_os)
        .map(|base| Path::new(&base).join("Windows Kits").join("10").join("bin"))
        .collect()
}

/// Which architecture's `rc.exe` to prefer. It is a host tool, so this
/// follows the machine doing the building rather than the target: an x64
/// `rc.exe` runs fine under emulation on arm64, but picking the native one
/// first avoids paying for that.
#[cfg(windows)]
fn sdk_arch_preference() -> &'static [&'static str] {
    match env::var("HOST").as_deref() {
        Ok(host) if host.starts_with("aarch64") => &["arm64", "x64", "x86"],
        Ok(host) if host.starts_with("i686") => &["x86", "x64"],
        _ => &["x64", "x86", "arm64"],
    }
}
