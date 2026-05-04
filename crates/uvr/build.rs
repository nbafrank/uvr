// Embed a Win32 application manifest into uvr.exe so Windows recognises
// the binary as compatible with Win 7 through Win 11 — issue #74.
//
// v0.3.4 added the manifest via `new_manifest("uvr")` which silently
// includes a `<dependency>` on `Microsoft.Windows.Common-Controls`
// v6.0.0.0 (a SxS assembly used for visual styles in GUI apps). For a
// CLI tool that dependency is dead weight, and on machines where SxS
// activation fails for any reason — corrupt SxS cache, AppLocker /
// WDAC policy, AV interference — Windows refuses to load the binary
// with `ERROR_BAD_EXE_FORMAT`, surfaced in PowerShell as
// "The specified executable is not a valid application for this OS
// platform". v0.3.4 traded one Windows error for another. We strip
// the comctl32 dep so the manifest only declares supportedOS GUIDs,
// asInvoker execution level, long-path-aware, and UTF-8 codepage —
// no SxS activation required.
//
// No-op on non-Windows targets; the build-dep is gated via
// `[target.'cfg(windows)'.build-dependencies]` so non-Windows builds
// don't even compile the crate.

#[cfg(windows)]
fn main() {
    use embed_manifest::{embed_manifest, new_manifest};
    let manifest = new_manifest("uvr").remove_dependency("Microsoft.Windows.Common-Controls");
    embed_manifest(manifest).expect("unable to embed Win32 manifest");
}

#[cfg(not(windows))]
fn main() {
    // Nothing to do on non-Windows targets.
}
