# Windows downloads

AccountDeck v0.1.0 for Windows x64. No installer or administrator access is required. Run from a writable folder and keep its generated `AccountDeck-data` folder to retain sessions. Requires Microsoft Edge WebView2.

- [AccountDeck.exe](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck.exe): standalone executable.
- [AccountDeck-portable.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-portable.zip): executable, guide, licenses, and validation record.
- [AccountDeck-source.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-source.zip): source and developer tests, excluding executable downloads and local data.
- [SHA256SUMS.txt](SHA256SUMS.txt): SHA-256 checksums.

The executable SHA-256 is `87e0fa6206740311ebdeb00955f8eb46a519cfc006049aace92f6e28c528e4f4`. Eleven unit tests and clean-process persistence/isolation and native-interface checks passed. See [validation](../qa/VALIDATION.md) for the tested scope and limits.

Verify a downloaded file in PowerShell:

```powershell
Get-FileHash .\AccountDeck.exe -Algorithm SHA256
```
