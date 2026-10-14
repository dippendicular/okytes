# Tauri transitive dependency review

Tauri's CSS parser and directory helpers include five MPL-2.0 crates: cssparser,
cssparser-macros, dtoa-short, option-ext and selectors. These are allowed individually
in cargo-deny. Okytes' own source remains Apache-2.0 OR MIT; MPL-covered source is
not copied into our files. Preserve upstream notices and source access when
distributing binaries. Mozilla explains the separate-file obligations in its
[MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/).
The Apache-2.0 WITH LLVM-exception expression is also allowed for generated
platform bindings; it does not change this project's license.

Tauri 2's Linux GTK3 dependencies include proc-macro-error 1.0.4, covered by
[RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html).
This is an informational unmaintained notice with no patched version, rather than
a reported security vulnerability. A narrowly scoped exception preserves the
required Tauri 2 stack. No vulnerability advisories are suppressed. Review this
exception on every Tauri update and by 2027-01-14. The upstream dependency migration
is tracked in [Tauri's GTK3 issue](https://github.com/tauri-apps/tauri/issues/11928).
Other maintenance advisories and duplicate versions remain visible audit warnings.

The local M2 audit initially failed on these license expressions, the maintenance
notice, and a path dependency lacking a version. The version was pinned to the
workspace version; these reviewed allowances are explicit, rather than disabling
license or advisory checks for the dependency graph.
