# Windows downloads

AccountDeck v0.3.0 for Windows x64. No installer or administrator access is required. Run from a writable folder and keep its generated `AccountDeck-data` folder to retain sessions. Requires Microsoft Edge WebView2.

- [AccountDeck.exe](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck.exe): standalone executable.
- [AccountDeck-portable.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-portable.zip): executable, guide, licenses, and validation record.
- [AccountDeck-source.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-source.zip): source and developer tests, excluding executable downloads and local data.
- [SHA256SUMS.txt](SHA256SUMS.txt): SHA-256 checksums.

The executable SHA-256 is `e7f47e5b2ecbe668a09c837688fa2a6954d7daa7125bb982c9e31967814e72ad`. Twenty unit tests, fourteen usability checks, eighteen tab/sync/download checks, eight native-interface checks, three live Facebook checks with dummy values, and clean-process persistence/isolation tests passed. See [validation](../qa/VALIDATION.md) for the tested scope and limits.

Close the old app and replace the executable, keeping the existing `AccountDeck-data` folder to retain profiles and logins.

Verify a downloaded file in PowerShell:

```powershell
Get-FileHash .\AccountDeck.exe -Algorithm SHA256
```
