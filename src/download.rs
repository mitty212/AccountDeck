use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};
#[derive(Clone, serde::Serialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub received: u64,
    pub total: Option<u64>,
    pub status: String,
}

pub fn report(
    operation: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2DownloadOperation,
    id: &str,
    proxy: &tao::event_loop::EventLoopProxy<crate::AppEvent>,
) {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    unsafe {
        let mut received = 0;
        let mut total = 0;
        let mut state = COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS;
        let _ = operation.BytesReceived(&mut received);
        let _ = operation.TotalBytesToReceive(&mut total);
        let _ = operation.State(&mut state);
        let status = if state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED {
            "complete"
        } else if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED {
            "interrupted"
        } else {
            "downloading"
        };
        let _ = proxy.send_event(crate::AppEvent::DownloadProgress {
            id: id.into(),
            received: received.max(0) as u64,
            total: if total > 0 { Some(total as u64) } else { None },
            status: status.into(),
        });
    }
}

pub fn default_directory() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(Path::to_path_buf))
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads")))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .join("Downloads")
        })
}
pub fn filename(suggestion: &str) -> String {
    let name = suggestion.rsplit(['/', '\\']).next().unwrap_or("download");
    let clean: String = name
        .chars()
        .take(180)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_matches([' ', '.']);
    let base = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
    if clean.is_empty() {
        "download".into()
    } else if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        format!("_{clean}")
    } else {
        clean.into()
    }
}
/// Reserve a fresh filename so simultaneous downloads never replace existing files.
pub fn reserve(directory: &Path, suggestion: &str) -> Result<PathBuf, String> {
    if !directory.is_absolute() {
        return Err("Choose an absolute download folder.".into());
    }
    fs::create_dir_all(directory).map_err(|e| format!("Cannot open the download folder: {e}"))?;
    let name = filename(suggestion);
    let path = Path::new(&name);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for i in 0..10000 {
        let candidate = directory.join(if i == 0 {
            name.clone()
        } else {
            format!("{stem} ({i}){ext}")
        });
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot save the download: {e}")),
        }
    }
    Err("Too many downloads with this filename. Choose another folder.".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untrusted_names_cannot_escape_or_use_windows_devices() {
        assert_eq!(filename("../../CON.txt"), "_CON.txt");
        assert_eq!(filename("C:\\bad\\a?.txt"), "a_.txt");
        assert_eq!(filename(".."), "download");
    }
    #[test]
    fn concurrent_download_names_preserve_existing_files() {
        let d = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let a = reserve(&d, "report.txt").unwrap();
        fs::write(&a, b"keep").unwrap();
        let b = reserve(&d, "report.txt").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(a).unwrap(), b"keep");
        fs::remove_file(b).unwrap();
        fs::remove_file(d.join("report.txt")).unwrap();
        fs::remove_dir(d).unwrap();
    }
}
