# Validation record — 0.2.0

Validated on 27 November 2026 on the development Windows x64 computer with Rust 1.96.0 and installed WebView2.

Executable: 2,045,440 bytes. SHA-256: `fc22eb5f7df25d97a9d9de06999d8d68134064f9c245cbe35df1493c164e1cd9`. Embedded manifest: Windows x64, `asInvoker`. The C runtime remains statically linked.

- `cargo test --locked`: **17 passed, 0 failed**, including older-settings migration, tab/download/sidebar persistence, URL and profile-ID validation, atomic configuration writes/imports, download-name traversal and collision handling, sync scope/input validation, DPAPI encryption, and final-save sequencing.
- `cargo build --release --locked`: passed.
- `node qa/check_features.mjs`: **18 feature checks passed** against the release executable and three fresh local profiles. Verified shared storage between same-profile tabs; popup links; OAuth-style opener/close semantics; sidebar resize; custom-folder downloads with exact downloaded bytes and collision protection; the editable save-prompt toggle; trusted hover/click/special-key/Unicode/rich-text mirroring; repeated rich-text replacement; matching controls with different IDs; synchronized background popup tabs; excluded profiles and password fields untouched; Stop sync; and tab layout/settings persistence after clean quit/reopen with sync off.
- `node qa/check_app.mjs`: the **8 existing native-interface checks passed** for account creation/duplication, mute/pin/zoom, sleep/resume, password lock/unlock, settings, and persisted configuration.
- `python qa/check_persistence.py`: passed separate-process cookie/localStorage/IndexedDB isolation tests, with complete WebView2 shutdown between phases. Session-only HttpOnly cookies were recovered from encrypted snapshots; a fresh third profile remained empty.
- Inspected actual embedded-interface captures for settings, tab controls, synchronization, and compact navigation. Test captures/results remain in ignored `qa/output/` and `qa/results/` folders.

The harness uses loopback fixture pages and fresh test workspaces; it does not sign into real accounts. The native folder picker and Save As dialog are not automated by the harness; their editable preferences, persistence, and automatic-save behavior were verified. A physical Windows reboot and live service-specific authentication/calling/notification flows were not exercised.

Sync needs matching top-level page layouts. Password/payment/one-time-code fields and embedded frames are excluded. Complex site-specific editors, drag operations, and browser chrome are outside the tested scope. Sync is opt-in, does not persist as armed across restarts, and keeps action payloads in memory rather than logging them.

Developer-only debugger listeners are enabled only when the integration harness explicitly supplies its debug environment variables. Ordinary launches do not enable them. Node and Python are development-test dependencies; the executable requires neither.
