# Windows downloads

AccountDeck v0.2.0 for Windows x64. No installer or administrator access is required. Run from a writable folder and keep its generated `AccountDeck-data` folder to retain sessions. Requires Microsoft Edge WebView2.

- [AccountDeck.exe](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck.exe): standalone executable.
- [AccountDeck-portable.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-portable.zip): executable, guide, licenses, and validation record.
- [AccountDeck-source.zip](https://github.com/mitty212/AccountDeck/raw/refs/heads/main/downloads/AccountDeck-source.zip): source and developer tests, excluding executable downloads and local data.
- [SHA256SUMS.txt](SHA256SUMS.txt): SHA-256 checksums.

The executable SHA-256 is `fc22eb5f7df25d97a9d9de06999d8d68134064f9c245cbe35df1493c164e1cd9`. Seventeen unit tests, eighteen new-feature checks, existing native-interface checks, and clean-process persistence/isolation tests passed. See [validation](../qa/VALIDATION.md) for the tested scope and limits.

Verify a downloaded file in PowerShell:

```powershell
Get-FileHash .\AccountDeck.exe -Algorithm SHA256
```
