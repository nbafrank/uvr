// Embed a Win32 application manifest into the uvr.exe PE so Windows
// recognises the binary as compatible with Win 7 through Win 11.
// Without it, naked MSVC binaries built on recent toolchains (the
// `windows-latest` GH runner currently linker 14.44+) are rejected on
// some Win 11 builds with "This version of … uvr.exe is not compatible
// with the version of Windows you're running" — issue #74.
//
// `new_manifest("uvr")` produces a manifest declaring supportedOS GUIDs
// for Vista, 7, 8, 8.1, 10, 11 and asInvoker execution level.
//
// This file is a no-op on every non-Windows target; the build-dep is
// also gated by `[target.'cfg(windows)'.build-dependencies]` so non-
// Windows builds don't even pull the crate.

#[cfg(windows)]
fn main() {
    use embed_manifest::{embed_manifest, new_manifest};
    embed_manifest(new_manifest("uvr")).expect("unable to embed Win32 manifest");
}

#[cfg(not(windows))]
fn main() {
    // Nothing to do on non-Windows targets.
}
