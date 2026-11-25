#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod browser;
mod model;
mod session;
mod snapshot_queue;

use model::{Account, Settings};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    window::{Window, WindowBuilder, WindowId},
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::{
    Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
    core::{HSTRING, w},
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

enum AppEvent {
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
struct Popup {
    view: WebView,
    window: Window,
    account: String,
}
struct App {
    browsers: HashMap<String, browser::Browser>,
    popups: HashMap<WindowId, Popup>,
    shell: WebView,
    window: Window,
    settings: Settings,
    directory: PathBuf,
    proxy: EventLoopProxy<AppEvent>,
    locked: bool,
    overlay: bool,
    titles: HashMap<String, String>,
    urls: HashMap<String, String>,
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
    fn save(&self) -> Result<(), String> {
        self.settings.save(&self.config_path())
    }
    fn error(&self, message: &str) {
        let arg = serde_json::to_string(message).unwrap();
        let _ = self
            .shell
            .evaluate_script(&format!("window.showError({arg})"));
    }
    fn send_state(&self) {
        let id = self.settings.active.as_deref().unwrap_or("");
        let state = json!({"accounts":self.settings.accounts,"active":self.settings.active,"theme":self.settings.theme,"locked":self.locked,"has_password":self.settings.password_hash.is_some(),"version":env!("CARGO_PKG_VERSION"),"data_path":self.directory,"loaded":self.browsers.keys().collect::<Vec<_>>(),"current_url":self.urls.get(id).or_else(||self.settings.accounts.iter().find(|a|a.id==id).map(|a|&a.url)),"current_title":self.titles.get(id)});
        let _ = self
            .shell
            .evaluate_script(&format!("window.receiveState({state})"));
    }
    fn visibility(&self) {
        for (id, b) in &self.browsers {
            let _ = b.view.set_visible(
                !self.locked && !self.overlay && self.settings.active.as_ref() == Some(id),
            );
        }
        for p in self.popups.values() {
            p.window.set_visible(!self.locked);
        }
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
        self.sleeping.remove(id);
        if !self.browsers.contains_key(id) {
            let b = browser::create(
                &self.window,
                &a,
                &self.directory.join("profiles").join(id),
                self.proxy.clone(),
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
            .map(|b| &b.view)
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
                &b.view,
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
            self.popups.retain(|_, p| p.account != id);
            self.browsers.remove(&id);
            self.send_state();
        }
    }
    fn lock(&mut self) -> Result<(), String> {
        if self.settings.password_hash.is_none() {
            return Err("Set a workspace password in settings first.".into());
        }
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
                self.visibility();
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
                self.popups.retain(|_, p| p.account != id);
                self.browsers.remove(id);
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
                            browser::mute(&b.view, a.muted)?;
                        }
                        for p in self.popups.values().filter(|p| p.account == a.id) {
                            browser::mute(&p.view, a.muted)?;
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
                self.active_view()?.zoom(zoom).map_err(|e| e.to_string())?;
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
        target: &EventLoopWindowTarget<AppEvent>,
    ) -> Result<(), String> {
        let result = (|| {
            if self.locked || self.closing.is_some() {
                return Ok(());
            }
            let a = self
                .settings
                .accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or("Account was not found.")?;
            let b = self.browsers.get(&id).ok_or("Account is asleep.")?;
            let window = WindowBuilder::new()
                .with_title(format!("{} — AccountDeck", a.name))
                .with_inner_size(LogicalSize::new(1000., 760.))
                .build(target)
                .map_err(|e| e.to_string())?;
            let view = WebViewBuilder::new()
                .with_environment(environment)
                .with_devtools(true)
                .with_browser_accelerator_keys(true)
                .with_navigation_handler(|url| browser::valid_navigation(&url))
                .build(&window)
                .map_err(|e| e.to_string())?;
            browser::install_hooks(
                &view,
                id.clone(),
                b.blocking.clone(),
                b.notifications.clone(),
                self.proxy.clone(),
            )?;
            browser::mute(&view, a.muted)?;
            unsafe {
                args.SetNewWindow(&view.webview())
                    .map_err(|e| e.to_string())?;
                args.SetHandled(true).map_err(|e| e.to_string())?;
            }
            self.popups.insert(
                window.id(),
                Popup {
                    view,
                    window,
                    account: id,
                },
            );
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
    let shell = WebViewBuilder::new_with_web_context(&mut context)
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
        popups: HashMap::new(),
        shell,
        window,
        settings,
        directory,
        proxy,
        locked,
        overlay: false,
        titles: HashMap::new(),
        urls: HashMap::new(),
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
    event_loop.run(move |event, target, control| {
        *control = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(1));
        match event {
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if window_id == app.window.id() {
                    if app.closing.is_none() {
                        app.closing = Some(Instant::now());
                        app.snapshot_all();
                        let _ = app.save();
                    }
                } else {
                    app.popups.remove(&window_id);
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. },
                ..
            } if window_id == app.window.id() => {
                for b in app.browsers.values() {
                    let _ = b.view.set_bounds(browser::bounds(&app.window));
                }
            }
            Event::UserEvent(AppEvent::Command(body)) => {
                if let Err(e) = app.command(&body) {
                    app.error(&e);
                }
            }
            Event::UserEvent(AppEvent::Warning(message)) => app.error(&message),
            Event::UserEvent(AppEvent::Page { id, url, title }) => {
                if !url.is_empty() && url != "about:blank" {
                    app.urls.insert(id.clone(), url);
                }
                if let Some(t) = title {
                    app.titles.insert(id, t);
                }
                app.send_state();
            }
            Event::UserEvent(AppEvent::SnapshotDone { id, result }) => {
                app.finish_snapshot(id,result);
            }
            Event::UserEvent(AppEvent::Popup {
                id,
                args,
                deferral,
                environment,
            }) => {
                if let Err(e) = app.popup(id, args, deferral, environment, target) {
                    app.error(&e);
                }
            }
            _ => {}
        }
        if let Some(start) = app.closing {
            if app.snapshotting.is_empty() {
                app.popups.clear();
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
