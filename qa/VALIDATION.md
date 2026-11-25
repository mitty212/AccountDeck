# Validation record

Validated on 25 November 2026 on the development Windows x64 computer with Rust 1.96.0 and installed WebView2 157.0.4341.60.

Final executable: 1,743,360 bytes. SHA-256: `87e0fa6206740311ebdeb00955f8eb46a519cfc006049aace92f6e28c528e4f4`.

- `cargo test --locked`: 11 passed, 0 failed. Covers URL/ID validation, atomic configuration replacement, transactional metadata import, independent IDs, Argon2 verification, DPAPI encryption/corruption handling, empty session replacement, ad-domain boundaries, and final-save sequencing.
- `cargo build --release --locked`: passed with static CRT and embedded `asInvoker` manifest.
- `cargo build --example persistence_check --locked` followed by `python qa/check_persistence.py`: passed. Results are kept locally in the ignored `qa/results/` folder. Separate seed/verify OS processes; all WebView2 processes for the test workspace exited between phases. A/B profiles retained HttpOnly session cookies, persistent cookies, localStorage and IndexedDB; fresh C had none. Encrypted cookie files did not contain plaintext test tokens. Persistent cookies use WebView2's durable database; session cookies use DPAPI recovery.
- `node qa/check_app.mjs`: passed against the final release executable and an isolated loopback-only fixture. Results are kept locally in the ignored `qa/output/` folder. Exercised native shell startup, DOM account creation through the native IPC bridge, independent duplication, mute, pin, zoom, sleep/resume, password lock/unlock, settings modal, and persisted configuration.
- Inspected native WebView captures `native-empty.png` and `native-settings.png` from that smoke test. Layout rendered correctly. The Windows desktop screenshot helper timed out; CDP captures were used for actual embedded-interface verification. Test-only remote debugging was supplied through the test process's environment; production launch does not enable it.
- PE import/resource inspection: the embedded manifest explicitly requests `asInvoker`. Imported DLLs are Windows components; there is no dynamic VC runtime or WebView2Loader DLL import. Machine-specific evidence is retained locally and excluded from the repository.

The test runner uses fresh local fixture data and never logs into user accounts. Live Facebook, Zalo, Messenger, Telegram, Discord, Instagram, WhatsApp or Google authentication/calling/notification behavior has not been verified. A physical Windows reboot, forced power loss and Windows sign-out were not performed; normal clean-process persistence was verified. Full original-app feature equivalence is not claimed. The ad blocker is a basic common-host filter.

To rerun the UI smoke test, Node.js with built-in `fetch` and `WebSocket` is needed (Node 26.3.0 was used). Node and Python are developer test dependencies only; the shipped app needs neither.
