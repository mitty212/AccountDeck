# Changes

## 0.2.0 — 27 November 2026

- Added multiple tabs per profile, sharing the profile's session storage. Popup links and `window.open` use same-profile tabs; opener and close behavior are preserved. Tab layouts survive restart.
- Added an app-controlled download folder and an ask-before-saving toggle. Automatic saves preserve existing files with numbered names.
- Added explicit source/target tab selection for synchronized hover, click, typing, selected keys, scrolling, and popup actions on matching pages. Stop/pause controls and protected-field exclusions are visible in the interface.
- Added a collapsible left navigation pane with persistent state and immediate browser-area resizing.
- Preserved existing profile data and settings through migration. No administrator installation or activation key is required.

## 0.1.0 — 25 November 2026

Initial portable Windows multi-account browser with isolated persistent profiles, account management, mute/sleep controls, optional workspace locking, and a basic common-host blocker.
