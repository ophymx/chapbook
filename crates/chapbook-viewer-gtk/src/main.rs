//! GTK4 reference viewer — a shell for Linux by default.
//!
//! GTK4 is reached through `gtk4-sys`, which probes for `gtk4.pc` with
//! `pkg-config`. That is a system library and a system tool, and neither is
//! present on a stock macOS or Windows box, so a workspace build there used
//! to fail in this crate before it ran a single test. The viewer therefore
//! compiles on Linux and, behind the `macos` feature, on a Mac with
//! Homebrew's GTK; elsewhere `gtk4` is a target-gated dependency, and this
//! file is all that is left of the crate.

#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macos")))]
mod linux;
#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macos")))]
mod page_area;

#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macos")))]
fn main() -> gtk4::glib::ExitCode {
    linux::run()
}

#[cfg(not(any(target_os = "linux", all(target_os = "macos", feature = "macos"))))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "chapbook-viewer-gtk needs GTK4, which this build does not have: it is \
         Linux by default, or macOS with `--features macos` over Homebrew's gtk4. \
         The winit shell (chapbook-viewer) is portable."
    );
    std::process::ExitCode::from(2)
}
