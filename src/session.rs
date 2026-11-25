//! An encrypted, per-profile snapshot also retains cookies marked "session only".
//! WebView2's normal profile directory retains all other browser storage.

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use webview2_com::{
    GetCookiesCompletedHandler,
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND, ICoreWebView2_2, ICoreWebView2Cookie,
        ICoreWebView2CookieList, ICoreWebView2CookieManager,
    },
    take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    },
    core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR},
};
use wry::{WebView, WebViewExtWindows};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_COOKIES: usize = 100_000;
const MAGIC: &[u8] = b"AccountDeck cookies v1\0";

#[derive(Serialize, Deserialize)]
struct CookieSnapshot {
    version: u32,
    cookies: Vec<SavedCookie>,
}

#[derive(Serialize, Deserialize)]
struct SavedCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    http_only: bool,
    secure: bool,
    same_site: i32,
    expires: f64,
    session: bool,
}

fn com_error(operation: &str, error: windows::core::Error) -> String {
    // Never include cookie contents, URLs, or native error descriptions.
    format!("{operation} failed ({:#010x}).", error.code().0 as u32)
}

fn cookie_manager(view: &WebView) -> Result<ICoreWebView2CookieManager, String> {
    unsafe {
        let core = view
            .controller()
            .CoreWebView2()
            .map_err(|error| com_error("Accessing browser session", error))?;
        core.cast::<ICoreWebView2_2>()
            .map_err(|error| com_error("Accessing browser cookie interface", error))?
            .CookieManager()
            .map_err(|error| com_error("Accessing browser cookies", error))
    }
}

fn cookie_string(
    get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>,
) -> Result<String, String> {
    let mut pointer = PWSTR::null();
    let result = get(&mut pointer);
    let text = take_pwstr(pointer);
    result.map_err(|error| com_error("Reading cookie text", error))?;
    Ok(text)
}

fn read_cookies(list: Option<ICoreWebView2CookieList>) -> Result<Vec<SavedCookie>, String> {
    let list = list.ok_or_else(|| "Browser returned no cookie collection.".to_string())?;
    let mut count = 0;
    unsafe { list.Count(&mut count) }
        .map_err(|error| com_error("Counting browser cookies", error))?;
    if count as usize > MAX_COOKIES {
        return Err("Browser cookie snapshot has too many cookies.".into());
    }
    let mut result = Vec::with_capacity(count as usize);
    let mut estimated_bytes = 128usize;
    for index in 0..count {
        let cookie = unsafe { list.GetValueAtIndex(index) }
            .map_err(|error| com_error("Reading browser cookie", error))?;
        let saved = save_cookie(&cookie)?;
        // Allow for JSON string escaping as well as keys and numeric fields.
        estimated_bytes = estimated_bytes.saturating_add(256).saturating_add(
            saved
                .name
                .len()
                .saturating_add(saved.value.len())
                .saturating_add(saved.domain.len())
                .saturating_add(saved.path.len())
                .saturating_mul(6),
        );
        if estimated_bytes > MAX_BYTES {
            return Err("Browser cookie snapshot exceeds 16 MB.".into());
        }
        result.push(saved);
    }
    Ok(result)
}

fn save_cookie(cookie: &ICoreWebView2Cookie) -> Result<SavedCookie, String> {
    unsafe {
        let name = cookie_string(|pointer| cookie.Name(pointer))?;
        let value = cookie_string(|pointer| cookie.Value(pointer))?;
        let domain = cookie_string(|pointer| cookie.Domain(pointer))?;
        let path = cookie_string(|pointer| cookie.Path(pointer))?;
        let (mut http_only, mut secure, mut session) = (BOOL(0), BOOL(0), BOOL(0));
        let mut same_site = COREWEBVIEW2_COOKIE_SAME_SITE_KIND(0);
        let mut expires = -1.0;
        cookie
            .IsHttpOnly(&mut http_only)
            .map_err(|error| com_error("Reading cookie flags", error))?;
        cookie
            .IsSecure(&mut secure)
            .map_err(|error| com_error("Reading cookie flags", error))?;
        cookie
            .IsSession(&mut session)
            .map_err(|error| com_error("Reading cookie flags", error))?;
        cookie
            .SameSite(&mut same_site)
            .map_err(|error| com_error("Reading cookie SameSite", error))?;
        cookie
            .Expires(&mut expires)
            .map_err(|error| com_error("Reading cookie expiration", error))?;
        let saved = SavedCookie {
            name,
            value,
            domain,
            path,
            http_only: http_only.as_bool(),
            secure: secure.as_bool(),
            same_site: same_site.0,
            expires,
            session: session.as_bool(),
        };
        validate_cookie(&saved)?;
        Ok(saved)
    }
}

fn validate_cookie(cookie: &SavedCookie) -> Result<(), String> {
    if cookie.domain.is_empty()
        || !cookie.path.starts_with('/')
        || !cookie.expires.is_finite()
        || !matches!(cookie.same_site, 0..=2)
        || [&cookie.name, &cookie.value, &cookie.domain, &cookie.path]
            .iter()
            .any(|value| value.contains('\0'))
    {
        return Err("Browser cookie snapshot contains invalid cookie attributes.".into());
    }
    Ok(())
}

/// Restore before the profile's first navigation. A missing snapshot is normal.
/// This must be called on the WebView2 UI thread.
pub fn restore(view: &WebView, path: &Path) -> Result<(), String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Cannot open encrypted browser session.".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read encrypted browser session.".to_string())?;
    if bytes.len() > MAX_BYTES || !bytes.starts_with(MAGIC) {
        return Err("Encrypted browser session is invalid or exceeds 16 MB.".into());
    }
    let mut plaintext = unprotect(&bytes[MAGIC.len()..])?;
    let parsed: Result<CookieSnapshot, _> = serde_json::from_slice(&plaintext);
    plaintext.fill(0);
    let snapshot = parsed.map_err(|_| "Cannot decode encrypted browser session.".to_string())?;
    if snapshot.version != 1 || snapshot.cookies.len() > MAX_COOKIES {
        return Err("Unsupported browser session format or excessive cookie count.".into());
    }
    for cookie in &snapshot.cookies {
        validate_cookie(cookie)?;
    }
    let manager = cookie_manager(view)?;
    for saved in snapshot.cookies {
        // WebView2's durable cookie database is authoritative for persistent
        // cookies. A snapshot can predate a rotation or logout, so restoring a
        // persistent cookie here would overwrite newer data or resurrect it.
        if !saved.session {
            continue;
        }
        unsafe {
            let cookie = manager
                .CreateCookie(
                    &HSTRING::from(&saved.name),
                    &HSTRING::from(&saved.value),
                    &HSTRING::from(&saved.domain),
                    &HSTRING::from(&saved.path),
                )
                .map_err(|error| com_error("Restoring browser cookie", error))?;
            // CreateCookie defaults to a session cookie. Leave its expiry unset.
            cookie
                .SetIsHttpOnly(saved.http_only)
                .map_err(|error| com_error("Restoring cookie flags", error))?;
            cookie
                .SetIsSecure(saved.secure)
                .map_err(|error| com_error("Restoring cookie flags", error))?;
            cookie
                .SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND(saved.same_site))
                .map_err(|error| com_error("Restoring cookie SameSite", error))?;
            manager
                .AddOrUpdateCookie(&cookie)
                .map_err(|error| com_error("Applying browser cookie", error))?;
        }
    }
    Ok(())
}

/// Begin a snapshot on the WebView2 UI thread. Keep the view alive and pump its
/// event loop until `done` runs. Submission errors are returned synchronously;
/// after successful submission `done` receives the eventual result exactly once.
pub fn snapshot(
    view: &WebView,
    path: PathBuf,
    done: impl Fn(Result<(), String>) + 'static,
) -> Result<(), String> {
    let manager = cookie_manager(view)?;
    let handler = GetCookiesCompletedHandler::create(Box::new(move |status, list| {
        let result = (|| {
            status.map_err(|error| com_error("Collecting browser cookies", error))?;
            let cookies = read_cookies(list)?;
            let mut plaintext = serde_json::to_vec(&CookieSnapshot {
                version: 1,
                cookies,
            })
            .map_err(|_| "Cannot encode browser session.".to_string())?;
            if plaintext.len() > MAX_BYTES {
                plaintext.fill(0);
                return Err("Browser cookie snapshot exceeds 16 MB.".into());
            }
            let encrypted = protect(&plaintext);
            plaintext.fill(0);
            let encrypted = encrypted?;
            let mut bytes = Vec::with_capacity(MAGIC.len() + encrypted.len());
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&encrypted);
            if bytes.len() > MAX_BYTES {
                return Err("Encrypted browser session exceeds 16 MB.".into());
            }
            write_atomic(&path, &bytes)
        })();
        done(result);
        Ok(())
    }));
    // A null URI selects cookies from every origin in this profile.
    unsafe { manager.GetCookies(PCWSTR::null(), &handler) }
        .map_err(|error| com_error("Requesting browser cookie snapshot", error))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|_| "Cannot create browser session directory.".to_string())?;
    let temp = parent.join(format!(".session-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| "Cannot create encrypted browser session.".to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot write encrypted browser session.".to_string())?;
        drop(file);
        fs::rename(&temp, path).map_err(|_| "Cannot replace encrypted browser session.".to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn protect(input: &[u8]) -> Result<Vec<u8>, String> {
    crypt(input, false)
}

fn unprotect(input: &[u8]) -> Result<Vec<u8>, String> {
    crypt(input, true)
}

fn crypt(input: &[u8], decrypt: bool) -> Result<Vec<u8>, String> {
    if input.is_empty() || input.len() > MAX_BYTES {
        return Err("Browser session encryption input is invalid.".into());
    }
    let source = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let result = unsafe {
        if decrypt {
            CryptUnprotectData(
                &source,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            // No LOCAL_MACHINE flag: only this Windows user can decrypt it.
            CryptProtectData(
                &source,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if let Err(error) = result {
        if !output.pbData.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
            }
        }
        return Err(com_error("Windows session encryption", error));
    }
    let valid = !output.pbData.is_null() && (output.cbData as usize) <= MAX_BYTES;
    let bytes = if valid {
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() }
    } else {
        Vec::new()
    };
    if !output.pbData.is_null() {
        unsafe {
            if decrypt {
                for offset in 0..output.cbData as usize {
                    std::ptr::write_volatile(output.pbData.add(offset), 0);
                }
            }
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        }
    }
    if !valid {
        return Err("Windows session encryption returned an invalid buffer.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_roundtrip_protects_session_values() {
        let plain = br#"{"version":1,"cookies":[{"value":"example-session-token"}]}"#;
        let encrypted = protect(plain).unwrap();
        assert_ne!(encrypted.as_slice(), plain);
        assert!(
            !encrypted
                .windows(b"example-session-token".len())
                .any(|part| part == b"example-session-token")
        );
        assert_eq!(unprotect(&encrypted).unwrap(), plain);
        let mut corrupted = encrypted;
        let end = corrupted.len() - 1;
        corrupted[end] ^= 0x55;
        assert!(unprotect(&corrupted).is_err());
    }

    #[test]
    fn empty_cookie_snapshots_overwrite_previous_state() {
        let directory =
            std::env::temp_dir().join(format!("accountdeck-session-test-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.bin");
        let encoded = serde_json::to_vec(&CookieSnapshot {
            version: 1,
            cookies: Vec::new(),
        })
        .unwrap();
        let encrypted = protect(&encoded).unwrap();
        write_atomic(&path, b"old-state").unwrap();
        write_atomic(&path, &encrypted).unwrap();
        let stored = fs::read(&path).unwrap();
        let parsed: CookieSnapshot = serde_json::from_slice(&unprotect(&stored).unwrap()).unwrap();
        assert!(parsed.cookies.is_empty());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
