//! Put the application icon into the executable, on Windows.
//!
//! Explorer, Alt-Tab and the Start menu read the icon out of the binary's
//! resources; the window's own icon is a separate thing, set from raw pixels
//! in `aitch-ui`. Both come from the same drawing — see
//! `crates/aitch-ui/examples/make_icon.rs` and `docs/screenshots.md`.

fn main() {
    // The icon lives in packaging/ rather than beside this file, because it is
    // the same file the installer ships to Add/Remove Programs.
    println!("cargo:rerun-if-changed=../../packaging/windows/aitch.ico");

    #[cfg(windows)]
    {
        let icon = "../../packaging/windows/aitch.ico";
        if !std::path::Path::new(icon).exists() {
            // Not fatal. A missing icon is a cosmetic problem, and refusing to
            // build the editor over one would be a worse trade. The packaging
            // script checks that a released binary actually got it.
            println!("cargo:warning=no icon at {icon}; building without one");
            return;
        }

        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon);
        if let Err(e) = resource.compile() {
            // Same reasoning: a machine with no resource compiler can still
            // build and run the editor.
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
