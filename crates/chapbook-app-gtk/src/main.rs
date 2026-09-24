//! The chapbook desktop application — a GTK4 shell, Linux by default.
//!
//! Every decision that is not a widget lives in `chapbook-app`; this crate
//! is GTK plumbing over that model. GTK4 is reached through `gtk4-sys`,
//! which probes for `gtk4.pc` with `pkg-config` — a system library and a
//! system tool — so like the reference viewer this compiles on Linux and,
//! behind the `macos` feature, on a Mac with Homebrew's GTK; elsewhere
//! `gtk4` is a target-gated dependency, and this file is all that is left
//! of the crate.

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
        "chapbook-app-gtk needs GTK4, which this build does not have: it is \
         Linux by default, or macOS with `--features macos` over Homebrew's gtk4."
    );
    std::process::ExitCode::from(2)
}
