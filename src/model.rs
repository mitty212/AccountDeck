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
use unicode_segmentation::UnicodeSegmentation;
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
    pub abbreviation: String,
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
            abbreviation: String::new(),
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
        self.abbreviation = self.abbreviation.trim().to_string();
        if self.abbreviation.graphemes(true).count() > 3
            || self.abbreviation.chars().any(char::is_control)
        {
            return Err("Profile abbreviation must contain at most 3 characters.".into());
        }
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
pub struct ServicePreset {
    pub id: String,
    pub name: String,
    pub url: String,
}
impl ServicePreset {
    pub fn new(name: &str, url: &str) -> Result<Self, String> {
        let mut s = Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            url: url.into(),
        };
        s.validate()?;
        Ok(s)
    }
    fn validate(&mut self) -> Result<(), String> {
        self.id = Uuid::parse_str(&self.id)
            .map_err(|_| "Invalid saved service ID.")?
            .hyphenated()
            .to_string();
        self.name = self.name.trim().to_string();
        if self.name.is_empty()
            || self.name.chars().count() > 60
            || self.name.chars().any(char::is_control)
        {
            return Err("Service name must contain 1–60 characters.".into());
        }
        self.url = normalize_url(&self.url)?;
        Ok(())
    }
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
    #[serde(default)]
    pub top_compact: bool,
    #[serde(default)]
    pub custom_services: Vec<ServicePreset>,
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
            top_compact: false,
            custom_services: Vec::new(),
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
        if self.custom_services.len() > 1000 {
            return Err("Too many saved services.".into());
        }
        let mut service_ids = HashSet::new();
        for service in &mut self.custom_services {
            service.validate()?;
            if !service_ids.insert(service.id.clone()) {
                return Err("Duplicate saved service ID.".into());
            }
        }
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

    /// Drop within/between visible navigation sections, preserving all profile storage IDs.
    pub fn move_profile(&mut self, id: &str, target: &str, after: bool) -> Result<(), String> {
        if id == target {
            return Ok(());
        }
        let from = self
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or("Profile was not found.")?;
        let destination = self
            .accounts
            .iter()
            .find(|a| a.id == target)
            .ok_or("Drop target was not found.")?;
        let group = destination.group.clone();
        let pinned = destination.pinned;
        let mut account = self.accounts.remove(from);
        account.group = group;
        account.pinned = pinned;
        let index = self.accounts.iter().position(|a| a.id == target).unwrap() + usize::from(after);
        self.accounts.insert(index, account);
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
    fn abbreviations_count_display_characters_and_old_profiles_default_empty() {
        let mut a = sample();
        for value in ["FB1", "a\u{301}b👩‍💻", ""] {
            a.abbreviation = value.into();
            a.validate().unwrap();
        }
        a.abbreviation = "ABCD".into();
        assert!(a.validate().is_err());
        a.abbreviation = "A\nB".into();
        assert!(a.validate().is_err());
        let mut old = serde_json::to_value(sample()).unwrap();
        old.as_object_mut().unwrap().remove("abbreviation");
        let loaded: Account = serde_json::from_value(old).unwrap();
        assert_eq!(loaded.abbreviation, "");
    }
    #[test]
    fn services_and_compact_top_survive_reload_with_safe_old_defaults() {
        let service = ServicePreset::new("  Local Chat  ", "example.com/chat").unwrap();
        assert_eq!(service.name, "Local Chat");
        assert_eq!(service.url, "https://example.com/chat");
        assert!(ServicePreset::new("", "example.com").is_err());
        assert!(ServicePreset::new("Bad", "javascript:alert(1)").is_err());
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(!old.top_compact);
        assert!(old.custom_services.is_empty());
        let s = Settings {
            top_compact: true,
            custom_services: vec![service],
            ..Settings::default()
        };
        let mut loaded: Settings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        loaded.validate().unwrap();
        assert!(loaded.top_compact);
        assert_eq!(loaded.custom_services[0].id, s.custom_services[0].id);
    }
    #[test]
    fn moving_profiles_preserves_identity_and_joins_drop_target_group() {
        let mut a = sample();
        a.abbreviation = "ABC".into();
        a.muted = true;
        let b = sample();
        let mut c = sample();
        c.group = "Other".into();
        c.pinned = true;
        let ids = [a.id.clone(), b.id.clone(), c.id.clone()];
        let mut s = Settings {
            accounts: vec![a, b, c],
            ..Settings::default()
        };
        s.move_profile(&ids[0], &ids[1], true).unwrap();
        assert_eq!(
            s.accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            vec![ids[1].clone(), ids[0].clone(), ids[2].clone()]
        );
        assert_eq!(s.accounts[1].abbreviation, "ABC");
        assert!(s.accounts[1].muted);
        s.move_profile(&ids[0], &ids[2], true).unwrap();
        assert_eq!(s.accounts[2].id, ids[0]);
        assert_eq!(s.accounts[2].group, "Other");
        assert!(s.accounts[2].pinned);
        let before = serde_json::to_string(&s).unwrap();
        assert!(s.move_profile(&ids[0], "missing", false).is_err());
        assert_eq!(before, serde_json::to_string(&s).unwrap());
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
