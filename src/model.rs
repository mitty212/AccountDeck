use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 10_000;
const METADATA_FORMAT: &str = "accountdeck.accounts";

fn default_color() -> String {
    "#31c7af".into()
}
fn default_zoom() -> f64 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    "dark".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub group: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_zoom")]
    pub zoom: f64,
}

impl Account {
    pub fn new(name: &str, url: &str, group: &str, color: &str) -> Result<Self, String> {
        let mut account = Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            url: url.into(),
            group: group.into(),
            color: if color.is_empty() {
                default_color()
            } else {
                color.into()
            },
            muted: false,
            pinned: false,
            blocking: false,
            notifications: true,
            zoom: 1.0,
        };
        account.validate()?;
        Ok(account)
    }

    /// Validate any account before its ID is used as a directory name.
    pub fn validate(&mut self) -> Result<(), String> {
        self.id = Uuid::parse_str(&self.id)
            .map_err(|_| "Account ID is not a valid UUID.".to_string())?
            .hyphenated()
            .to_string();
        self.name = self.name.trim().to_string();
        self.group = self.group.trim().to_string();
        if self.name.is_empty() || self.name.chars().count() > 100 {
            return Err("Account name must contain between 1 and 100 characters.".into());
        }
        if self.group.chars().count() > 100 {
            return Err("Group name must contain at most 100 characters.".into());
        }
        self.url = normalize_url(&self.url)?;
        if self.color.len() != 7
            || !self.color.starts_with('#')
            || !self.color[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Account color must be a six-digit hexadecimal color.".into());
        }
        self.color.make_ascii_lowercase();
        self.zoom = if self.zoom.is_finite() {
            self.zoom.clamp(0.25, 3.0)
        } else {
            1.0
        };
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadSettings {
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub ask_where_to_save: bool,
}
impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            directory: None,
            ask_where_to_save: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub downloads: DownloadSettings,
    #[serde(default)]
    pub open_tabs: HashMap<String, Vec<SavedTab>>,
    #[serde(default)]
    pub active_tabs: HashMap<String, String>,
    #[serde(default)]
    pub sidebar_collapsed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            active: None,
            theme: default_theme(),
            password_hash: None,
            downloads: DownloadSettings::default(),
            open_tabs: HashMap::new(),
            active_tabs: HashMap::new(),
            sidebar_collapsed: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub format: String,
    pub version: u32,
    pub accounts: Vec<Account>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("Cannot read account settings: {error}")),
        };
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read account settings: {error}"))?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("Account settings exceed the 4 MB limit.".into());
        }
        let mut settings: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot parse account settings: {error}"))?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&mut self) -> Result<(), String> {
        if self.accounts.len() > MAX_ACCOUNTS {
            return Err("Too many accounts in settings.".into());
        }
        let mut ids = HashSet::new();
        for account in &mut self.accounts {
            account.validate()?;
            if !ids.insert(account.id.clone()) {
                return Err("Duplicate account ID in settings.".into());
            }
        }
        if self.active.as_ref().is_none_or(|id| !ids.contains(id)) {
            self.active = self.accounts.first().map(|account| account.id.clone());
        }
        if !matches!(self.theme.as_str(), "dark" | "light") {
            self.theme = default_theme();
        }
        if self
            .downloads
            .directory
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err("Download folder must be an absolute path.".into());
        }
        self.open_tabs.retain(|id, _| ids.contains(id));
        self.active_tabs.retain(|id, _| ids.contains(id));
        let mut tab_ids = HashSet::new();
        for tabs in self.open_tabs.values_mut() {
            if tabs.len() > 500 {
                return Err("Too many tabs in one profile.".into());
            }
            for tab in tabs {
                tab.id = Uuid::parse_str(&tab.id)
                    .map_err(|_| "Invalid tab ID.")?
                    .hyphenated()
                    .to_string();
                if !tab_ids.insert(tab.id.clone()) {
                    return Err("Duplicate tab ID.".into());
                }
                if tab.url != "about:blank" {
                    tab.url = normalize_url(&tab.url)?;
                }
            }
        }
        if let Some(hash) = &self.password_hash {
            let parsed =
                PasswordHash::new(hash).map_err(|_| "Invalid saved password hash.".to_string())?;
            if !matches!(
                parsed.algorithm.as_str(),
                "argon2id" | "argon2i" | "argon2d"
            ) || parsed.salt.is_none()
                || parsed.hash.is_none()
            {
                return Err("Invalid saved password hash.".into());
            }
        }
        Ok(())
    }

    /// Write a complete sibling file, flush it, then atomically replace the old file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let mut checked = self.clone();
        checked.validate()?;
        let bytes = serde_json::to_vec_pretty(&checked)
            .map_err(|error| format!("Cannot encode settings: {error}"))?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("Account settings exceed the 4 MB limit.".into());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create settings directory: {error}"))?;
        let temp = parent.join(format!(".accountdeck-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), String> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|error| format!("Cannot create temporary settings: {error}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("Cannot flush settings: {error}"))?;
            drop(file);
            // std::fs::rename uses MoveFileExW with REPLACE_EXISTING on Windows.
            fs::rename(&temp, path).map_err(|error| format!("Cannot replace settings: {error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub fn set_password(&mut self, value: &str) -> Result<(), String> {
        if value.is_empty() {
            self.password_hash = None;
            return Ok(());
        }
        if value.len() > 1024 {
            return Err("Password must contain at most 1024 bytes.".into());
        }
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(value.as_bytes(), &salt)
            .map_err(|error| format!("Cannot protect password: {error}"))?;
        self.password_hash = Some(hash.to_string());
        Ok(())
    }

    pub fn verify_password(&self, value: &str) -> bool {
        if value.len() > 1024 {
            return false;
        }
        let Some(hash) = &self.password_hash else {
            return true;
        };
        PasswordHash::new(hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(value.as_bytes(), &parsed)
                .is_ok()
        })
    }

    /// Export account settings only. Browser cookies, passwords and the app lock are excluded.
    pub fn metadata_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&Metadata {
            format: METADATA_FORMAT.into(),
            version: 1,
            accounts: self.accounts.clone(),
        })
        .map_err(|error| format!("Cannot encode account export: {error}"))
    }

    /// Import into new isolated profiles. Validation finishes before any mutation.
    pub fn import_metadata(&mut self, json: &str) -> Result<usize, String> {
        if json.len() > MAX_CONFIG_BYTES {
            return Err("Import exceeds the 4 MB limit.".into());
        }
        let metadata: Metadata = serde_json::from_str(json)
            .map_err(|error| format!("Cannot parse account export: {error}"))?;
        if metadata.format != METADATA_FORMAT || metadata.version != 1 {
            return Err("Unsupported account export format or version.".into());
        }
        if self.accounts.len().saturating_add(metadata.accounts.len()) > MAX_ACCOUNTS {
            return Err("Import contains too many accounts.".into());
        }
        let mut imported = metadata.accounts;
        for account in &mut imported {
            account.validate()?;
            account.id = Uuid::new_v4().to_string();
        }
        let count = imported.len();
        self.accounts.extend(imported);
        if self.active.is_none() {
            self.active = self.accounts.first().map(|account| account.id.clone());
        }
        Ok(count)
    }
}

pub fn normalize_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty()
        || input.len() > 8192
        || input.starts_with('/')
        || input.contains('\\')
        || input.chars().any(char::is_control)
    {
        return Err("Enter a valid HTTP or HTTPS address.".into());
    }
    let candidate = if input.contains("://") {
        input.to_string()
    } else {
        // A host:port is allowed, but a scheme such as javascript: is not a host.
        let authority = input.split(['/', '?', '#']).next().unwrap_or(input);
        if !authority.starts_with('[') {
            if let Some((_, suffix)) = authority.split_once(':') {
                if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("Only HTTP and HTTPS addresses are supported.".into());
                }
            }
        }
        format!("https://{input}")
    };
    let url =
        url::Url::parse(&candidate).map_err(|_| "Enter a valid website address.".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(
            "Only HTTP and HTTPS addresses without embedded credentials are supported.".into(),
        );
    }
    Ok(url.into())
}

pub fn data_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path).join("AccountDeck"));
    }
    directories::BaseDirs::new()
        .map(|base| base.data_local_dir().join("AccountDeck"))
        .ok_or_else(|| "Cannot locate your Windows user data directory.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Account {
        Account::new("Test account", "example.com", "Work", "#31c7af").unwrap()
    }

    #[test]
    fn older_settings_keep_profiles_and_get_safe_new_defaults() {
        let old = serde_json::json!({"accounts":[sample()],"theme":"light"});
        let path =
            std::env::temp_dir().join(format!("accountdeck-migration-{}.json", Uuid::new_v4()));
        fs::write(&path, old.to_string()).unwrap();
        let loaded = Settings::load(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.theme, "light");
        assert!(loaded.downloads.ask_where_to_save);
        assert!(loaded.open_tabs.is_empty());
        assert!(!loaded.sidebar_collapsed);
    }
    #[test]
    fn tab_layout_and_download_preferences_survive_settings_reload() {
        let a = sample();
        let id = a.id.clone();
        let t = SavedTab {
            id: Uuid::new_v4().to_string(),
            url: "https://example.com/inbox".into(),
        };
        let mut s = Settings::default();
        s.accounts.push(a);
        s.open_tabs.insert(id.clone(), vec![t.clone()]);
        s.active_tabs.insert(id.clone(), t.id.clone());
        s.sidebar_collapsed = true;
        s.downloads.ask_where_to_save = false;
        s.downloads.directory = Some(std::env::temp_dir());
        let path = std::env::temp_dir().join(format!("accountdeck-tabs-{}.json", Uuid::new_v4()));
        s.save(&path).unwrap();
        let result = Settings::load(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(result.open_tabs[&id][0].id, t.id);
        assert!(result.sidebar_collapsed);
        assert!(!result.downloads.ask_where_to_save);
    }
    #[test]
    fn url_normalization_rejects_executable_and_local_addresses() {
        assert_eq!(
            normalize_url(" example.com/path ").unwrap(),
            "https://example.com/path"
        );
        assert_eq!(
            normalize_url("http://localhost:8080").unwrap(),
            "http://localhost:8080/"
        );
        assert_eq!(
            normalize_url("localhost:8080").unwrap(),
            "https://localhost:8080/"
        );
        for invalid in [
            "javascript:alert(1)",
            "file:///C:/Windows",
            "data:text/html,hello",
            "ftp://example.com",
            "https://user:secret@example.com",
            "C:\\Windows",
            "//example.com",
            "",
            "https://",
        ] {
            assert!(normalize_url(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn profile_directory_ids_cannot_traverse_paths() {
        let mut account = sample();
        account.id = "../../outside".into();
        assert!(account.validate().is_err());
    }

    #[test]
    fn metadata_excludes_lock_and_regenerates_profile_ids() {
        let account = sample();
        let original_id = account.id.clone();
        let mut source = Settings {
            accounts: vec![account],
            password_hash: Some("secret-never-exported".into()),
            ..Settings::default()
        };
        let export = source.metadata_json().unwrap();
        assert!(!export.contains("password_hash"));
        assert!(!export.contains("secret-never-exported"));
        let mut target = Settings::default();
        assert_eq!(target.import_metadata(&export).unwrap(), 1);
        assert_ne!(target.accounts[0].id, original_id);
        assert_eq!(target.accounts[0].name, source.accounts[0].name);
        source.accounts[0].id = "../bad".into();
        let bad_export = source.metadata_json().unwrap();
        let before = serde_json::to_string(&target).unwrap();
        assert!(target.import_metadata(&bad_export).is_err());
        assert_eq!(serde_json::to_string(&target).unwrap(), before);
    }

    #[test]
    fn import_is_atomic_when_later_account_is_invalid() {
        let mut metadata = Metadata {
            format: METADATA_FORMAT.into(),
            version: 1,
            accounts: vec![sample(), sample()],
        };
        metadata.accounts[1].url = "javascript:alert(1)".into();
        let mut settings = Settings::default();
        assert!(
            settings
                .import_metadata(&serde_json::to_string(&metadata).unwrap())
                .is_err()
        );
        assert!(settings.accounts.is_empty());
        assert!(settings.active.is_none());
    }

    #[test]
    fn password_hashes_are_salted_and_verify() {
        let mut settings = Settings::default();
        settings.set_password("correct horse").unwrap();
        let first = settings.password_hash.clone();
        assert!(settings.verify_password("correct horse"));
        assert!(!settings.verify_password("incorrect"));
        settings.set_password("correct horse").unwrap();
        assert_ne!(first, settings.password_hash);
        settings.set_password("").unwrap();
        assert!(settings.password_hash.is_none());
        assert!(settings.verify_password(""));
    }

    #[test]
    fn saving_twice_replaces_settings_without_losing_data() {
        let directory =
            std::env::temp_dir().join(format!("accountdeck-model-test-{}", Uuid::new_v4()));
        let path = directory.join("settings.json");
        let mut settings = Settings::default();
        settings.save(&path).unwrap();
        settings.accounts.push(sample());
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().accounts.len(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
