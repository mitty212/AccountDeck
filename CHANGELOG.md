# Changes

## 0.3.0 — 28 November 2026

- Added profile icon abbreviations of up to three displayed characters and drag/drop profile ordering, with persistent IDs and login storage.
- Added a reusable custom-service tile editor beside the built-in services. Service Name and Home URL persist across restarts.
- Added a live download indicator and progress panel with received/total bytes and completion/interruption status.
- Added compact top-pane mode that hides the title/tab rows, retains the address controls, and resizes browser content immediately.
- Reworked sync control matching, offscreen scrolling, hit testing, trusted email replacement, pointer button state, and release handling during navigation. Sync reports skipped actions instead of silently ignoring them.
- Added an explicit per-session password-field synchronization option, off by default and never persisted. Card and one-time-code fields remain excluded.

## 0.2.0 — 27 November 2026

- Added multiple tabs per profile, sharing the profile's session storage. Popup links and `window.open` use same-profile tabs; opener and close behavior are preserved. Tab layouts survive restart.
- Added an app-controlled download folder and an ask-before-saving toggle. Automatic saves preserve existing files with numbered names.
- Added explicit source/target tab selection for synchronized hover, click, typing, selected keys, scrolling, and popup actions on matching pages. Stop/pause controls and protected-field exclusions are visible in the interface.
- Added a collapsible left navigation pane with persistent state and immediate browser-area resizing.
- Preserved existing profile data and settings through migration. No administrator installation or activation key is required.

## 0.1.0 — 25 November 2026

Initial portable Windows multi-account browser with isolated persistent profiles, account management, mute/sleep controls, optional workspace locking, and a basic common-host blocker.
