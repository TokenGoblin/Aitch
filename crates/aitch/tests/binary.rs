//! Properties of the built binary itself, rather than of the code in it.
//!
//! Both of these have been wrong in a shipped release, and neither shows up
//! in a unit test: the first is a flag in the executable header, and the
//! second only fails once the program is a separate process.

use std::process::Command;

/// The binary cargo just built for this test.
const AITCH: &str = env!("CARGO_BIN_EXE_aitch");

/// Windows decides whether to hand a program a console from the subsystem in
/// its header, so a GUI program has to say so there and nowhere else.
///
/// 0.1.0 and 0.1.1 shipped as console programs, and every launch from the
/// Start menu opened an empty console window beside the editor.
#[test]
#[cfg(windows)]
fn the_binary_is_a_gui_program() {
    const WINDOWS_GUI: u16 = 2;

    let image = std::fs::read(AITCH).expect("the built binary");
    let pe = u32::from_le_bytes(image[0x3c..0x40].try_into().expect("e_lfanew")) as usize;
    assert_eq!(&image[pe..pe + 4], b"PE\0\0", "not a PE image");

    // COFF header is 20 bytes; Subsystem is 68 bytes into the optional
    // header, in both the 32- and 64-bit layouts.
    let subsystem_at = pe + 4 + 20 + 68;
    let subsystem = u16::from_le_bytes(
        image[subsystem_at..subsystem_at + 2]
            .try_into()
            .expect("subsystem"),
    );

    assert_eq!(
        subsystem, WINDOWS_GUI,
        "subsystem {subsystem} means Windows opens a console window beside the editor"
    );
}

/// Being a GUI program must not cost the command line.
///
/// `--version` and `--help` print, a bad argument explains itself on stderr
/// and exits non-zero. On Windows this only works because the program attaches
/// to whatever console started it.
#[test]
fn the_command_line_still_answers() {
    let version = Command::new(AITCH)
        .arg("--version")
        .output()
        .expect("run --version");
    assert!(version.status.success());
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version said {text:?}"
    );

    let help = Command::new(AITCH)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(help.status.success());
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("Usage:"),
        "--help printed no usage"
    );

    let bad = Command::new(AITCH)
        .arg("--nonsense")
        .output()
        .expect("run a bad argument");
    assert!(!bad.status.success(), "a bad argument should fail");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("nonsense"),
        "it should say what it did not understand"
    );
}
