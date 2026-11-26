#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod browser;
mod download;
mod model;
mod session;
mod snapshot_queue;
mod sync;

use model::{Account, Settings};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::{
    Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
    core::{HSTRING, w},
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

enum AppEvent {
    SyncInput {
        tab: String,
        origin: String,
        body: String,
    },
    CloseTab(String),
    Download {
        args: ICoreWebView2DownloadStartingEventArgs,
        deferral: ICoreWebView2Deferral,
    },
    DownloadFinished {
        path: Option<PathBuf>,
        success: bool,
    },
    Warning(String),
    Command(String),
    Page {
        id: String,
        url: String,
        title: Option<String>,
    },
    SnapshotDone {
        id: String,
        result: Result<(), String>,
    },
    Popup {
        id: String,
        args: ICoreWebView2NewWindowRequestedEventArgs,
        deferral: ICoreWebView2Deferral,
        environment: ICoreWebView2Environment,
    },
}
struct App {
    browsers: HashMap<String, browser::Browser>,
    shell: WebView,
    window: Window,
    settings: Settings,
    directory: PathBuf,
    proxy: EventLoopProxy<AppEvent>,
    locked: bool,
    overlay: bool,
    sync_selection: sync::Selection,
    sync_epoch: Arc<AtomicU64>,
    snapshotting: snapshot_queue::SnapshotQueue,
    closing: Option<Instant>,
    sleeping: HashSet<String>,
    last_snapshot: Instant,
    last_unlock: Option<Instant>,
    _shell_context: WebContext,
    _instance_lock: File,
}
impl App {
    fn config_path(&self) -> PathBuf {
        self.directory.join("accounts.json")
    }
    fn width(&self) -> f64 {
        if self.settings.sidebar_collapsed {
            72.
        } else {
            260.
        }
    }
    fn save(&mut self) -> Result<(), String> {
        for (id, b) in &self.browsers {
            self.settings.open_tabs.insert(
                id.clone(),
                b.tabs
                    .iter()
                    .map(|t| model::SavedTab {
                        id: t.id.clone(),
                        url: if t.url == "about:blank" || model::normalize_url(&t.url).is_ok() {
                            t.url.clone()
                        } else {
                            self.settings
                                .accounts
                                .iter()
                                .find(|a| &a.id == id)
                                .map(|a| a.url.clone())
                                .unwrap_or_else(|| "about:blank".into())
                        },
                    })
                    .collect(),
            );
            self.settings
                .active_tabs
                .insert(id.clone(), b.active_tab.clone());
        }
        self.settings.save(&self.config_path())
    }
    fn error(&self, message: &str) {
        let arg = serde_json::to_string(message).unwrap();
        let _ = self
            .shell
            .evaluate_script(&format!("window.showError({arg})"));
    }
    fn send_state(&self) {
        let browser = self
            .settings
            .active
            .as_ref()
            .and_then(|id| self.browsers.get(id));
        let tab = browser.map(|b| b.active());
        let tabs = browser
            .map(|b| {
                b.tabs
                    .iter()
                    .map(|t| json!({"id":t.id,"title":t.title,"url":t.url}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let candidates=self.browsers.iter().flat_map(|(id,b)|b.tabs.iter().map(move|t|json!({"id":t.id,"profile":id,"name":self.settings.accounts.iter().find(|a|&a.id==id).map(|a|&a.name),"title":t.title,"url":t.url}))).collect::<Vec<_>>();
        let state = json!({"accounts":self.settings.accounts,"active":self.settings.active,"theme":self.settings.theme,"locked":self.locked,"has_password":self.settings.password_hash.is_some(),"version":env!("CARGO_PKG_VERSION"),"data_path":self.directory,"loaded":self.browsers.keys().collect::<Vec<_>>(),"current_url":tab.map(|t|&t.url),"current_title":tab.map(|t|&t.title),"tabs":tabs,"active_tab":browser.map(|b|&b.active_tab),"sync":self.sync_selection,"sync_candidates":candidates,"sidebar_collapsed":self.settings.sidebar_collapsed,"downloads":{"directory":self.settings.downloads.directory.clone().unwrap_or_else(download::default_directory),"ask_where_to_save":self.settings.downloads.ask_where_to_save}});
        let _ = self
            .shell
            .evaluate_script(&format!("window.receiveState({state})"));
    }
    fn visibility(&self) {
        for (id, b) in &self.browsers {
            for tab in &b.tabs {
                let _ = tab.view.set_visible(
                    !self.locked
                        && !self.overlay
                        && self.settings.active.as_ref() == Some(id)
                        && tab.id == b.active_tab,
                );
            }
        }
    }
    fn tab(&self, id: &str) -> Option<&browser::Tab> {
        self.browsers
            .values()
            .flat_map(|b| &b.tabs)
            .find(|t| t.id == id)
    }
    fn stop_sync(&mut self) {
        self.sync_epoch.fetch_add(1, Ordering::Relaxed);
        self.sync_selection = sync::Selection::default();
        self.refresh_sync();
    }
    fn refresh_sync(&self) {
        for b in self.browsers.values() {
            for t in &b.tabs {
                sync::configure(
                    &t.view,
                    !self.locked
                        && !self.overlay
                        && self.closing.is_none()
                        && self.sync_selection.source.as_ref() == Some(&t.id),
                );
            }
        }
    }
    fn close_tab(&mut self, id: &str) -> Result<(), String> {
        let profile = self
            .browsers
            .iter()
            .find(|(_, b)| b.tabs.iter().any(|t| t.id == id))
            .map(|(id, _)| id.clone())
            .ok_or("Tab was not found.")?;
        self.stop_sync();
        let a = self
            .settings
            .accounts
            .iter()
            .find(|a| a.id == profile)
            .ok_or("Profile was not found.")?
            .clone();
        let width = self.width();
        let b = self.browsers.get_mut(&profile).unwrap();
        if b.tabs.len() == 1 {
            b.add_tab(
                &self.window,
                &a,
                "about:blank",
                self.proxy.clone(),
                width,
                None,
            )?;
        }
        let index = b.tabs.iter().position(|t| t.id == id).unwrap();
        b.tabs.remove(index);
        if b.active_tab == id {
            b.active_tab = b.tabs[index.min(b.tabs.len() - 1)].id.clone();
        }
        self.visibility();
        self.save()?;
        self.send_state();
        Ok(())
    }
    fn sync_input(&self, id: &str, origin: &str, body: &str) {
        if self.locked
            || self.overlay
            || self.closing.is_some()
            || body.len() > 90000
            || self.sync_selection.source.as_deref() != Some(id)
        {
            return;
        }
        let Some(source) = self.tab(id) else {
            return;
        };
        let active = self
            .settings
            .active
            .as_ref()
            .and_then(|id| self.browsers.get(id))
            .map(|b| b.active_tab.as_str());
        if active != Some(id) {
            return;
        }
        let Ok(packet) = serde_json::from_str::<sync::Packet>(body) else {
            return;
        };
        if packet.token != source.sync_token
            || sync::page_key(origin) != sync::page_key(&packet.page)
            || !sync::same_origin(&source.url, origin)
            || !sync::valid(&packet.action)
        {
            return;
        }
        let key = sync::page_key(&packet.page);
        if key.is_none() {
            return;
        }
        for target in &self.sync_selection.targets {
            if let Some(tab) = self.tab(target) {
                if sync::page_key(&tab.url) == key {
                    let _ = sync::replay(
                        &tab.view,
                        packet.action.clone(),
                        tab.sync_queue.clone(),
                        self.sync_epoch.clone(),
                        self.sync_epoch.load(Ordering::Relaxed),
                        packet.page.clone(),
                    );
                }
            }
        }
    }
    fn download(
        &mut self,
        args: ICoreWebView2DownloadStartingEventArgs,
        deferral: ICoreWebView2Deferral,
    ) -> Result<(), String> {
        let result = (|| {
            if self.locked || self.closing.is_some() {
                unsafe {
                    args.SetCancel(true).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            let mut suggested = windows::core::PWSTR::null();
            unsafe {
                args.ResultFilePath(&mut suggested)
                    .map_err(|e| e.to_string())?;
            }
            let name = download::filename(&webview2_com::take_pwstr(suggested));
            let directory = self
                .settings
                .downloads
                .directory
                .clone()
                .unwrap_or_else(download::default_directory);
            let path = if self.settings.downloads.ask_where_to_save {
                rfd::FileDialog::new()
                    .set_directory(&directory)
                    .set_file_name(&name)
                    .save_file()
            } else {
                Some(download::reserve(&directory, &name)?)
            };
            unsafe {
                if let Some(path) = path {
                    args.SetResultFilePath(&HSTRING::from(path.as_os_str()))
                        .map_err(|e| e.to_string())?;
                    args.SetHandled(true).map_err(|e| e.to_string())?;
                } else {
                    args.SetCancel(true).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            unsafe {
                let _ = args.SetCancel(true);
            }
        }
        unsafe {
            deferral.Complete().map_err(|e| e.to_string())?;
        }
        result
    }
    fn activate(&mut self, id: &str) -> Result<(), String> {
        if self.locked {
            return Err("Unlock the workspace first.".into());
        }
        let a = self
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .ok_or("Account was not found.")?
            .clone();
        self.stop_sync();
        self.sleeping.remove(id);
        if !self.browsers.contains_key(id) {
            let b = browser::create(
                &self.window,
                &a,
                &self.directory.join("profiles").join(id),
                self.proxy.clone(),
                self.settings
                    .open_tabs
                    .get(id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                self.settings.active_tabs.get(id).map(String::as_str),
                self.width(),
            )?;
            self.browsers.insert(id.into(), b);
        }
        self.settings.active = Some(id.into());
        self.visibility();
        self.save()?;
        self.send_state();
        Ok(())
    }
    fn active_view(&self) -> Result<&WebView, String> {
        self.settings
            .active
            .as_ref()
            .and_then(|id| self.browsers.get(id))
            .map(|b| &b.active().view)
            .ok_or_else(|| "Open an account first.".into())
    }
    fn snapshot(&mut self, id: &str, final_save: bool) -> Result<(), String> {
        if !self.snapshotting.request(id, final_save) {
            return Ok(());
        }
        if let Some(b) = self.browsers.get(id) {
            let p = self.proxy.clone();
            let cookie_id = id.to_string();
            let result = session::snapshot(
                &b.active().view,
                self.directory
                    .join("profiles")
                    .join(id)
                    .join("session.dpapi"),
                move |result| {
                    let _ = p.send_event(AppEvent::SnapshotDone {
                        id: cookie_id.clone(),
                        result,
                    });
                },
            );
            if result.is_err() {
                self.snapshotting.completed(id);
            }
            result?;
        } else {
            self.snapshotting.completed(id);
        }
        Ok(())
    }
    fn snapshot_all(&mut self) {
        for id in self.browsers.keys().cloned().collect::<Vec<_>>() {
            if let Err(e) = self.snapshot(&id, self.closing.is_some()) {
                self.closing = None;
                self.error(&e);
            }
        }
        self.last_snapshot = Instant::now();
    }
    fn finish_snapshot(&mut self, id: String, result: Result<(), String>) {
        let again = self.snapshotting.completed(&id);
        if let Err(e) = result {
            self.closing = None;
            if self.sleeping.remove(&id) && self.settings.active.is_none() {
                self.settings.active = Some(id);
            }
            self.visibility();
            self.send_state();
            self.error(&format!(
                "Could not save the browser session. The account remains open: {e}"
            ));
            return;
        }
        if again {
            if let Err(e) = self.snapshot(&id, false) {
                self.finish_snapshot(id, Err(e));
            }
            return;
        }
        if self.sleeping.remove(&id) {
            let _ = self.save();
            self.browsers.remove(&id);
            self.send_state();
        }
    }
    fn lock(&mut self) -> Result<(), String> {
        if self.settings.password_hash.is_none() {
            return Err("Set a workspace password in settings first.".into());
        }
        self.stop_sync();
        self.locked = true;
        self.overlay = false;
        self.visibility();
        self.send_state();
        self.shell.focus().map_err(|e| e.to_string())
    }
    fn command(&mut self, body: &str) -> Result<(), String> {
        if body.len() > 32768 {
            return Err("Command too large.".into());
        }
        let v: Value = serde_json::from_str(body).map_err(|_| "Invalid command.")?;
        let op = v["op"].as_str().ok_or("Missing command.")?;
        let s = |key: &str| v[key].as_str().unwrap_or("");
        if self.closing.is_some() {
            return Ok(());
        }
        if self.locked && !matches!(op, "ready" | "unlock") {
            return Err("Unlock the workspace first.".into());
        }
        match op {
            "quit" => {
                self.stop_sync();
                self.closing = Some(Instant::now());
                self.snapshot_all();
            }
            "ready" => {
                self.send_state();
            }
            "unlock" => {
                if self
                    .last_unlock
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
                {
                    return Err("Please wait a moment before trying again.".into());
                }
                self.last_unlock = Some(Instant::now());
                if !self.settings.verify_password(s("value")) {
                    return Err("The workspace password is incorrect.".into());
                }
                self.locked = false;
                self.overlay = false;
                if let Some(id) = self.settings.active.clone() {
                    self.activate(&id)?;
                }
                self.visibility();
                self.send_state();
            }
            "overlay" => {
                self.overlay = v["visible"].as_bool().unwrap_or(false);
                if self.overlay {
                    self.sync_epoch.fetch_add(1, Ordering::Relaxed);
                }
                self.visibility();
                self.refresh_sync();
            }
            "add" => {
                let a = Account::new(s("name"), s("url"), s("group"), s("color"))?;
                let id = a.id.clone();
                self.settings.accounts.push(a);
                self.save()?;
                self.activate(&id)?;
            }
            "edit" => {
                let index = self
                    .settings
                    .accounts
                    .iter()
                    .position(|a| a.id == s("id"))
                    .ok_or("Account was not found.")?;
                let mut a = self.settings.accounts[index].clone();
                a.name = s("name").into();
                a.url = s("url").into();
                a.group = s("group").into();
                a.color = s("color").into();
                a.blocking = v["blocking"].as_bool().unwrap_or(a.blocking);
                a.notifications = v["notifications"].as_bool().unwrap_or(a.notifications);
                a.validate()?;
                if let Some(b) = self.browsers.get(&a.id) {
                    b.blocking
                        .store(a.blocking, std::sync::atomic::Ordering::Relaxed);
                    b.notifications
                        .store(a.notifications, std::sync::atomic::Ordering::Relaxed);
                }
                self.settings.accounts[index] = a;
            }
            "switch" => self.activate(s("id"))?,
            "duplicate" => {
                let mut a = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.id == s("id"))
                    .ok_or("Account was not found.")?
                    .clone();
                a.id = uuid::Uuid::new_v4().to_string();
                a.name = format!("{} copy", a.name.chars().take(95).collect::<String>());
                a.pinned = false;
                let id = a.id.clone();
                self.settings.accounts.push(a);
                self.save()?;
                self.activate(&id)?;
            }
            "delete" => {
                let id = s("id");
                if self.snapshotting.contains(id) {
                    return Err(
                        "This profile is being saved. Try deleting it again in a moment.".into(),
                    );
                }
                if !self.settings.accounts.iter().any(|a| a.id == id) {
                    return Err("Account was not found.".into());
                }
                self.stop_sync();
                self.browsers.remove(id);
                self.settings.open_tabs.remove(id);
                self.settings.active_tabs.remove(id);
                self.settings.accounts.retain(|a| a.id != id);
                if self.settings.active.as_deref() == Some(id) {
                    self.settings.active = self.settings.accounts.first().map(|a| a.id.clone());
                }
                self.save()?;
                let path = self.directory.join("profiles").join(id);
                if path.exists() {
                    let trash = self.directory.join("deleted");
                    fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
                    if fs::rename(&path, trash.join(id)).is_err() {
                        fs::write(path.join(".deleted"), b"").map_err(|e| e.to_string())?;
                    }
                }
                if let Some(id) = self.settings.active.clone() {
                    self.activate(&id)?;
                }
                self.visibility();
            }
            "sleep" => {
                let id = s("id");
                if !self.browsers.contains_key(id) {
                    return Ok(());
                }
                self.stop_sync();
                self.sleeping.insert(id.into());
                if let Err(e) = self.snapshot(id, true) {
                    self.sleeping.remove(id);
                    return Err(e);
                }
                if self.settings.active.as_deref() == Some(id) {
                    self.settings.active = None;
                }
                self.visibility();
            }
            "pin" | "mute" | "reorder" => {
                let i = self
                    .settings
                    .accounts
                    .iter()
                    .position(|a| a.id == s("id"))
                    .ok_or("Account was not found.")?;
                match op {
                    "pin" => self.settings.accounts[i].pinned = !self.settings.accounts[i].pinned,
                    "mute" => {
                        let a = &mut self.settings.accounts[i];
                        a.muted = !a.muted;
                        if let Some(b) = self.browsers.get(&a.id) {
                            browser::mute(&b.active().view, a.muted)?;
                        }
                    }
                    _ => {
                        let j = if s("direction") == "up" {
                            i.saturating_sub(1)
                        } else {
                            (i + 1).min(self.settings.accounts.len() - 1)
                        };
                        self.settings.accounts.swap(i, j);
                    }
                }
            }
            "tab_new" => {
                self.stop_sync();
                let profile = self
                    .settings
                    .active
                    .clone()
                    .ok_or("Open a profile first.")?;
                let a = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.id == profile)
                    .unwrap()
                    .clone();
                let width = self.width();
                let url = if s("url").is_empty() {
                    a.url.clone()
                } else {
                    model::normalize_url(s("url"))?
                };
                self.browsers
                    .get_mut(&profile)
                    .ok_or("Profile is asleep.")?
                    .add_tab(&self.window, &a, &url, self.proxy.clone(), width, None)?;
                self.visibility();
            }
            "tab_activate" => {
                self.stop_sync();
                let b = self
                    .settings
                    .active
                    .as_ref()
                    .and_then(|id| self.browsers.get_mut(id))
                    .ok_or("Open a profile first.")?;
                if !b.tabs.iter().any(|t| t.id == s("id")) {
                    return Err("Tab was not found.".into());
                }
                b.active_tab = s("id").into();
                self.visibility();
            }
            "tab_close" => self.close_tab(s("id"))?,
            "sidebar" => {
                self.settings.sidebar_collapsed = v["collapsed"]
                    .as_bool()
                    .unwrap_or(!self.settings.sidebar_collapsed);
                let bounds = browser::bounds(&self.window, self.width());
                for b in self.browsers.values() {
                    for t in &b.tabs {
                        let _ = t.view.set_bounds(bounds);
                    }
                }
            }
            "download_choose" => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_directory(
                        self.settings
                            .downloads
                            .directory
                            .clone()
                            .unwrap_or_else(download::default_directory),
                    )
                    .pick_folder()
                {
                    self.settings.downloads.directory = Some(path);
                }
            }
            "download_settings" => {
                let path = PathBuf::from(s("directory"));
                if !path.as_os_str().is_empty() && !path.is_absolute() {
                    return Err("Download folder must be an absolute path.".into());
                }
                self.settings.downloads.directory = if path.as_os_str().is_empty() {
                    None
                } else {
                    Some(path)
                };
                self.settings.downloads.ask_where_to_save = v["ask"].as_bool().unwrap_or(true);
            }
            "sync_stop" => self.stop_sync(),
            "sync_start" => {
                let source = self
                    .settings
                    .active
                    .as_ref()
                    .and_then(|id| self.browsers.get(id))
                    .map(|b| b.active_tab.clone())
                    .ok_or("Open a source tab first.")?;
                let key = sync::page_key(&self.tab(&source).unwrap().url)
                    .ok_or("Sync needs an HTTP or HTTPS page.")?;
                let targets = v["targets"]
                    .as_array()
                    .ok_or("Select target tabs.")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<HashSet<_>>();
                if targets.is_empty() {
                    return Err("Select at least one target tab.".into());
                }
                if targets.iter().any(|id| {
                    id == &source
                        || self
                            .tab(id)
                            .is_none_or(|t| sync::page_key(&t.url).as_ref() != Some(&key))
                }) {
                    return Err("Choose open tabs on the same site and page as the source.".into());
                }
                self.stop_sync();
                self.sync_selection = sync::Selection {
                    source: Some(source),
                    targets: targets.into_iter().collect(),
                };
                self.refresh_sync();
            }
            "navigate" => {
                let url = model::normalize_url(s("url"))?;
                self.active_view()?
                    .load_url(&url)
                    .map_err(|e| e.to_string())?;
            }
            "home" => {
                let a = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| Some(&a.id) == self.settings.active.as_ref())
                    .ok_or("Open an account first.")?;
                self.active_view()?
                    .load_url(&a.url)
                    .map_err(|e| e.to_string())?;
            }
            "back" => unsafe {
                self.active_view()?
                    .webview()
                    .GoBack()
                    .map_err(|e| e.to_string())?;
            },
            "forward" => unsafe {
                self.active_view()?
                    .webview()
                    .GoForward()
                    .map_err(|e| e.to_string())?;
            },
            "reload" => self.active_view()?.reload().map_err(|e| e.to_string())?,
            "zoom" => {
                let zoom = v["value"]
                    .as_f64()
                    .filter(|z| z.is_finite())
                    .ok_or("Invalid zoom.")?
                    .clamp(0.25, 3.0);
                if let Some(b) = self
                    .settings
                    .active
                    .as_ref()
                    .and_then(|id| self.browsers.get(id))
                {
                    for t in &b.tabs {
                        t.view.zoom(zoom).map_err(|e| e.to_string())?;
                    }
                }
                if let Some(a) = self
                    .settings
                    .accounts
                    .iter_mut()
                    .find(|a| Some(&a.id) == self.settings.active.as_ref())
                {
                    a.zoom = zoom;
                }
            }
            "theme" => {
                if !matches!(s("value"), "light" | "dark") {
                    return Err("Invalid theme.".into());
                }
                self.settings.theme = s("value").into();
            }
            "password" => self.settings.set_password(s("value"))?,
            "lock" => self.lock()?,
            "devtools" => self.active_view()?.open_devtools(),
            "export" => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("AccountDeck-accounts.json")
                    .add_filter("Account list", &["json"])
                    .save_file()
                {
                    fs::write(path, self.settings.metadata_json()?).map_err(|e| e.to_string())?;
                }
            }
            "import" => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Account list", &["json"])
                    .pick_file()
                {
                    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
                        return Err("Account list exceeds 4 MB.".into());
                    }
                    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
                    self.settings.import_metadata(&text)?;
                }
            }
            "open_data" => {
                std::process::Command::new("explorer.exe")
                    .arg(&self.directory)
                    .spawn()
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err("Unknown command.".into()),
        }
        if !matches!(op, "ready" | "overlay" | "unlock") {
            self.save()?;
            self.send_state();
        }
        Ok(())
    }
    fn popup(
        &mut self,
        id: String,
        args: ICoreWebView2NewWindowRequestedEventArgs,
        deferral: ICoreWebView2Deferral,
        environment: ICoreWebView2Environment,
    ) -> Result<(), String> {
        let result = (|| {
            if self.locked || self.closing.is_some() {
                return Ok(());
            }
            let syncing = self.sync_selection.source.is_some();
            let a = self
                .settings
                .accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or("Profile was not found.")?
                .clone();
            let width = self.width();
            let b = self.browsers.get_mut(&id).ok_or("Profile is asleep.")?;
            let previous_tab = b.active_tab.clone();
            let tab = b.add_tab(
                &self.window,
                &a,
                "about:blank",
                self.proxy.clone(),
                width,
                Some(environment),
            )?;
            unsafe {
                args.SetNewWindow(&b.tabs.iter().find(|t| t.id == tab).unwrap().view.webview())
                    .map_err(|e| e.to_string())?;
                args.SetHandled(true).map_err(|e| e.to_string())?;
            }
            if syncing {
                b.active_tab = previous_tab;
            }
            self.visibility();
            self.save()?;
            self.send_state();
            Ok(())
        })();
        unsafe {
            deferral.Complete().map_err(|e| e.to_string())?;
        }
        result
    }
}
fn main() {
    if let Err(error) = run() {
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(error),
                w!("AccountDeck"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

fn cleanup_deleted(directory: &std::path::Path, settings: &Settings) {
    // Only UUID-named directories explicitly removed through this application's UI.
    // Never follow a junction/symlink or remove a still-configured profile.
    let Ok(workspace) = fs::canonicalize(directory) else {
        return;
    };
    for root in [directory.join("deleted"), directory.join("profiles")] {
        let Ok(root_metadata) = fs::symlink_metadata(&root) else {
            continue;
        };
        if root_metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        let Ok(resolved_root) = fs::canonicalize(&root) else {
            continue;
        };
        if resolved_root.parent() != Some(workspace.as_path()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if uuid::Uuid::parse_str(&name).is_err()
                || settings.accounts.iter().any(|a| a.id == name)
            {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
            if root.file_name().is_some_and(|n| n == "profiles")
                && !entry.path().join(".deleted").is_file()
            {
                continue;
            }
            let Ok(resolved) = fs::canonicalize(entry.path()) else {
                continue;
            };
            let Ok(expected_parent) = fs::canonicalize(&root) else {
                continue;
            };
            if resolved.parent() == Some(expected_parent.as_path()) {
                let _ = fs::remove_dir_all(resolved);
            }
        }
    }
}
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut directory = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                directory = Some(PathBuf::from(
                    args.next().ok_or("--data-dir needs a folder.")?,
                ))
            }
            "--version" => {
                println!("AccountDeck {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    let directory = directory.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("AccountDeck-data")))
            .unwrap_or_else(|| {
                model::data_dir().unwrap_or_else(|_| PathBuf::from("AccountDeck-data"))
            })
    });
    fs::create_dir_all(&directory).map_err(|e|format!("Cannot create the data folder: {e}. Put AccountDeck in a writable folder or use --data-dir."))?;
    let directory = fs::canonicalize(directory).map_err(|e| e.to_string())?;
    let instance = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(directory.join("instance.lock"))
        .map_err(
            |_| "This AccountDeck workspace is already open, or its data folder is not writable.",
        )?;
    let settings = Settings::load(&directory.join("accounts.json"))?;
    cleanup_deleted(&directory, &settings);
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("AccountDeck")
        .with_inner_size(LogicalSize::new(1280., 840.))
        .with_min_inner_size(LogicalSize::new(900., 600.))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    let mut context = WebContext::new(Some(directory.join("shell")));
    let p = proxy.clone();
    let mut shell_builder = WebViewBuilder::new_with_web_context(&mut context);
    if let Some(port) = browser::qa_port(None) {
        shell_builder = shell_builder.with_additional_browser_args(format!(
            "--remote-debugging-address=127.0.0.1 --remote-debugging-port={port}"
        ));
    }
    let shell = shell_builder
        .with_custom_protocol("accountdeck".into(), |_, request| {
            let allowed = request.uri().path() == "/index.html";
            wry::http::Response::builder()
                .status(if allowed { 200 } else { 404 })
                .header("Content-Type", "text/html; charset=utf-8")
                .body(std::borrow::Cow::Borrowed(if allowed {
                    include_bytes!("shell.html").as_slice()
                } else {
                    &[]
                }))
                .unwrap()
        })
        .with_url("accountdeck://localhost/index.html")
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(|url| {
            url == "http://accountdeck.localhost/index.html" || url == "about:blank"
        })
        .with_ipc_handler(move |request| {
            if request.uri().scheme_str() == Some("http")
                && request.uri().host() == Some("accountdeck.localhost")
                && request.uri().path() == "/index.html"
            {
                let _ = p.send_event(AppEvent::Command(request.body().clone()));
            }
        })
        .build(&window)
        .map_err(|e| format!("Cannot start WebView2: {e}"))?;
    let locked = settings.password_hash.is_some();
    let mut app = App {
        browsers: HashMap::new(),
        shell,
        window,
        settings,
        directory,
        proxy,
        locked,
        overlay: false,
        sync_selection: sync::Selection::default(),
        sync_epoch: Arc::new(AtomicU64::new(0)),
        snapshotting: snapshot_queue::SnapshotQueue::default(),
        closing: None,
        sleeping: HashSet::new(),
        last_snapshot: Instant::now(),
        last_unlock: None,
        _shell_context: context,
        _instance_lock: instance,
    };
    if !locked {
        if let Some(id) = app.settings.active.clone() {
            if let Err(e) = app.activate(&id) {
                app.error(&e);
            }
        }
    }
    event_loop.run(move |event, _target, control| {
        *control = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(1));
        match event {
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if window_id == app.window.id() {
                    if app.closing.is_none() {
                        app.stop_sync();
                        app.closing = Some(Instant::now());
                        app.snapshot_all();
                        let _ = app.save();
                    }
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. },
                ..
            } if window_id == app.window.id() => {
                for b in app.browsers.values() {
                    for tab in &b.tabs{let _ = tab.view.set_bounds(browser::bounds(&app.window,app.width()));}
                }
            }
            Event::UserEvent(AppEvent::Command(body)) => {
                if let Err(e) = app.command(&body) {
                    app.error(&e);
                }
            }
            Event::UserEvent(AppEvent::Warning(message)) => app.error(&message),
            Event::UserEvent(AppEvent::Page{id,url,title})=>{
                for b in app.browsers.values_mut(){if let Some(t)=b.tabs.iter_mut().find(|t|t.id==id){if !url.is_empty()&&url!="about:blank"{t.url=url.clone();}if let Some(title)=title.as_ref(){t.title=title.clone();}break;}}
                app.refresh_sync();app.send_state();
            },
            Event::UserEvent(AppEvent::SyncInput{tab,origin,body})=>app.sync_input(&tab,&origin,&body),
            Event::UserEvent(AppEvent::CloseTab(id))=>{if let Err(e)=app.close_tab(&id){app.error(&e);}},
            Event::UserEvent(AppEvent::Download{args,deferral})=>{if let Err(e)=app.download(args,deferral){app.error(&e);}},
            Event::UserEvent(AppEvent::DownloadFinished{path,success})=>{let message=if success{format!("Download saved: {}",path.map(|p|p.display().to_string()).unwrap_or_default())}else{"Download canceled or interrupted.".into()};let _=app.shell.evaluate_script(&format!("window.showNotice?.({})",serde_json::to_string(&message).unwrap()));},
            Event::UserEvent(AppEvent::SnapshotDone { id, result }) => {
                app.finish_snapshot(id,result);
            }
            Event::UserEvent(AppEvent::Popup {
                id,
                args,
                deferral,
                environment,
            }) => {
                if let Err(e) = app.popup(id, args, deferral, environment) {
                    app.error(&e);
                }
            }
            _ => {}
        }
        if let Some(start) = app.closing {
            if app.snapshotting.is_empty() {
                app.browsers.clear();
                *control = ControlFlow::Exit;
            } else if start.elapsed() > Duration::from_secs(10) {
                app.closing = None;
                app.error("Browser session saving is taking too long. The workspace stays open to protect your logins. Try closing it again in a moment.");
            }
        } else if app.last_snapshot.elapsed() > Duration::from_secs(30) {
            app.snapshot_all();
        }
    });
}
