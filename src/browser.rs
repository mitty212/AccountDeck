use crate::{
    AppEvent,
    model::{Account, SavedTab},
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event_loop::EventLoopProxy,
    window::Window,
};
use webview2_com::{
    DownloadStartingEventHandler, Microsoft::Web::WebView2::Win32::*,
    NewWindowRequestedEventHandler, NotificationReceivedEventHandler,
    WebResourceRequestedEventHandler, WindowCloseRequestedEventHandler, take_pwstr,
};
use windows::{
    Win32::System::Com::IStream,
    core::{Interface, PWSTR, w},
};
use wry::{
    PermissionKind, PermissionResponse, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

pub struct Tab {
    pub id: String,
    pub view: WebView,
    pub url: String,
    pub title: String,
    pub sync_token: String,
    pub sync_queue: crate::sync::Queue,
}
pub struct Browser {
    pub tabs: Vec<Tab>,
    pub active_tab: String,
    pub _context: WebContext,
    pub blocking: Arc<AtomicBool>,
    pub notifications: Arc<AtomicBool>,
}
// Opt-in local debugger endpoints for the isolated integration harness only.
// Normal launches leave this unset and expose no debugging listener.
pub fn qa_port(profile: Option<&str>) -> Option<u16> {
    let base = std::env::var("ACCOUNTDECK_QA_CDP_BASE")
        .ok()?
        .parse::<u16>()
        .ok()?;
    if !(10000..=40000).contains(&base) {
        return None;
    }
    Some(
        base + profile
            .map(|id| u32::from_str_radix(&id[..8], 16).unwrap_or(0) % 10000 + 1)
            .unwrap_or(0) as u16,
    )
}
impl Browser {
    pub fn active(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|t| t.id == self.active_tab)
            .unwrap_or(&self.tabs[0])
    }
    pub fn add_tab(
        &mut self,
        window: &Window,
        account: &Account,
        url: &str,
        proxy: EventLoopProxy<AppEvent>,
        width: f64,
        environment: Option<ICoreWebView2Environment>,
    ) -> Result<String, String> {
        let env = environment.unwrap_or_else(|| self.active().view.environment());
        let id = uuid::Uuid::new_v4().to_string();
        let tab = build_tab(
            window,
            account,
            &mut self._context,
            Some(env),
            id.clone(),
            url,
            self.blocking.clone(),
            self.notifications.clone(),
            proxy,
            width,
        )?;
        if url != "about:blank" {
            tab.view.load_url(url).map_err(|e| e.to_string())?;
        }
        self.tabs.push(tab);
        self.active_tab = id.clone();
        Ok(id)
    }
}
pub fn bounds(window: &Window, width: f64) -> Rect {
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    Rect {
        position: LogicalPosition::new(width, 128.).into(),
        size: LogicalSize::new((s.width - width).max(1.), (s.height - 128.).max(1.)).into(),
    }
}
fn build_tab(
    window: &Window,
    account: &Account,
    context: &mut WebContext,
    environment: Option<ICoreWebView2Environment>,
    id: String,
    url: &str,
    blocking: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
    proxy: EventLoopProxy<AppEvent>,
    width: f64,
) -> Result<Tab, String> {
    let notices = notifications.clone();
    let p = proxy.clone();
    let load_id = id.clone();
    let title_id = id.clone();
    let title_proxy = proxy.clone();
    let input_id = id.clone();
    let input_proxy = proxy.clone();
    let token = uuid::Uuid::new_v4().to_string();
    let mut builder = WebViewBuilder::new_with_web_context(context);
    if let Some(port) = qa_port(Some(&account.id)) {
        builder = builder.with_additional_browser_args(format!(
            "--remote-debugging-address=127.0.0.1 --remote-debugging-port={port}"
        ));
    }
    if let Some(env) = environment {
        builder = builder.with_environment(env);
    }
    // This bridge handles only authenticated input-sync packets; it cannot execute application commands.
    let view = builder
        .with_bounds(bounds(window, width))
        .with_visible(false)
        .with_focused(false)
        .with_url("about:blank")
        .with_devtools(true)
        .with_browser_accelerator_keys(true)
        .with_navigation_handler(|url| valid_navigation(&url))
        .with_initialization_script(&crate::sync::capture_script(&token))
        .with_ipc_handler(move |request| {
            let _ = input_proxy.send_event(AppEvent::SyncInput {
                tab: input_id.clone(),
                origin: request.uri().to_string(),
                body: request.body().clone(),
            });
        })
        .with_permission_handler(move |kind| {
            if matches!(kind, PermissionKind::Notifications) && !notices.load(Ordering::Relaxed) {
                PermissionResponse::Deny
            } else {
                PermissionResponse::Default
            }
        })
        .with_on_page_load_handler(move |_, url| {
            let _ = p.send_event(AppEvent::Page {
                id: load_id.clone(),
                url,
                title: None,
            });
        })
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(AppEvent::Page {
                id: title_id.clone(),
                url: String::new(),
                title: Some(title),
            });
        })
        .with_download_completed_handler({
            let p = proxy.clone();
            move |_, path, success| {
                let _ = p.send_event(AppEvent::DownloadFinished { path, success });
            }
        })
        .build_as_child(window)
        .map_err(|e| e.to_string())?;
    install_hooks(
        &view,
        account.id.clone(),
        id.clone(),
        blocking,
        notifications,
        proxy,
    )?;
    mute(&view, account.muted)?;
    view.zoom(account.zoom).map_err(|e| e.to_string())?;
    Ok(Tab {
        id,
        view,
        url: url.into(),
        title: String::new(),
        sync_token: token,
        sync_queue: crate::sync::Queue::default(),
    })
}
pub fn create(
    window: &Window,
    account: &Account,
    directory: &Path,
    proxy: EventLoopProxy<AppEvent>,
    saved: &[SavedTab],
    active: Option<&str>,
    width: f64,
) -> Result<Browser, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut context = WebContext::new(Some(directory.join("webview")));
    let blocking = Arc::new(AtomicBool::new(account.blocking));
    let notifications = Arc::new(AtomicBool::new(account.notifications));
    let initial = saved.first().cloned().unwrap_or(SavedTab {
        id: uuid::Uuid::new_v4().to_string(),
        url: account.url.clone(),
    });
    let tab = build_tab(
        window,
        account,
        &mut context,
        None,
        initial.id.clone(),
        &initial.url,
        blocking.clone(),
        notifications.clone(),
        proxy.clone(),
        width,
    )?;
    let snapshot = directory.join("session.dpapi");
    if crate::session::restore(&tab.view, &snapshot).is_err() {
        let _ = std::fs::rename(
            &snapshot,
            directory.join(format!("session-unreadable-{}.dpapi", uuid::Uuid::new_v4())),
        );
        let _=proxy.send_event(AppEvent::Warning("A session-cookie backup could not be recovered. Existing browser storage was retained; this profile may need to sign in again.".into()));
    }
    tab.view.load_url(&initial.url).map_err(|e| e.to_string())?;
    let mut browser = Browser {
        tabs: vec![tab],
        active_tab: initial.id,
        _context: context,
        blocking,
        notifications,
    };
    for item in saved.iter().skip(1) {
        let env = browser.active().view.environment();
        let tab = build_tab(
            window,
            account,
            &mut browser._context,
            Some(env),
            item.id.clone(),
            &item.url,
            browser.blocking.clone(),
            browser.notifications.clone(),
            proxy.clone(),
            width,
        )?;
        tab.view.load_url(&item.url).map_err(|e| e.to_string())?;
        browser.tabs.push(tab);
    }
    if let Some(id) = active {
        if browser.tabs.iter().any(|t| t.id == id) {
            browser.active_tab = id.into();
        }
    }
    Ok(browser)
}

pub fn valid_navigation(url: &str) -> bool {
    if url == "about:blank" {
        return true;
    }
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "blob"))
}

pub fn blocked(url: &str) -> bool {
    const DOMAINS: &[&str] = &[
        "doubleclick.net",
        "googlesyndication.com",
        "googleadservices.com",
        "google-analytics.com",
        "googletagmanager.com",
        "adservice.google.com",
        "ads.twitter.com",
        "ads.linkedin.com",
        "adsrvr.org",
        "adnxs.com",
        "scorecardresearch.com",
        "hotjar.com",
        "clarity.ms",
        "taboola.com",
        "outbrain.com",
        "criteo.com",
        "criteo.net",
        "demdex.net",
        "quantserve.com",
        "zedo.com",
        "advertising.com",
    ];
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|host| {
            DOMAINS
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        })
}

pub fn mute(view: &WebView, muted: bool) -> Result<(), String> {
    unsafe {
        view.webview()
            .cast::<ICoreWebView2_8>()
            .and_then(|v| v.SetIsMuted(muted))
            .map_err(|e| e.to_string())
    }
}

pub fn install_hooks(
    view: &WebView,
    id: String,
    tab_id: String,
    blocking: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
    proxy: EventLoopProxy<AppEvent>,
) -> Result<(), String> {
    unsafe {
        let core = view.webview();
        // Password saving is browser-managed; credentials never enter the application UI.
        if let Ok(settings) = core
            .Settings()
            .and_then(|s| s.cast::<ICoreWebView2Settings4>())
        {
            let _ = settings.SetIsPasswordAutosaveEnabled(true);
        }
        core.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
            .map_err(|e| e.to_string())?;
        let env = view.environment();
        let mut token = 0;
        core.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                if !blocking.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Some(args) = args {
                    let request = args.Request()?;
                    let mut uri = PWSTR::null();
                    request.Uri(&mut uri)?;
                    let url = take_pwstr(uri);
                    if blocked(&url) {
                        let response = env.CreateWebResourceResponse(
                            None::<&IStream>,
                            204,
                            w!("Blocked"),
                            w!("Content-Length: 0\r\n"),
                        )?;
                        args.SetResponse(&response)?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )
        .map_err(|e| e.to_string())?;
        if let Ok(v) = core.cast::<ICoreWebView2_24>() {
            v.add_NotificationReceived(
                &NotificationReceivedEventHandler::create(Box::new(move |_, args| {
                    if !notifications.load(Ordering::Relaxed) {
                        if let Some(a) = args {
                            a.SetHandled(true)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| e.to_string())?;
        }
        let download_proxy = proxy.clone();
        core.cast::<ICoreWebView2_4>()
            .map_err(|e| e.to_string())?
            .add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                        let deferral = args.GetDeferral()?;
                        if download_proxy
                            .send_event(AppEvent::Download {
                                args: args.clone(),
                                deferral: deferral.clone(),
                            })
                            .is_err()
                        {
                            args.SetCancel(true)?;
                            deferral.Complete()?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| e.to_string())?;
        let close_proxy = proxy.clone();
        core.add_WindowCloseRequested(
            &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                let _ = close_proxy.send_event(AppEvent::CloseTab(tab_id.clone()));
                Ok(())
            })),
            &mut token,
        )
        .map_err(|e| e.to_string())?;
        let env = view.environment();
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                    let deferral = args.GetDeferral()?;
                    if proxy
                        .send_event(AppEvent::Popup {
                            id: id.clone(),
                            args: args.clone(),
                            deferral: deferral.clone(),
                            environment: env.clone(),
                        })
                        .is_err()
                    {
                        deferral.Complete()?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_host_boundaries_without_breaking_social_origins() {
        assert!(blocked("https://a.doubleclick.net/x"));
        assert!(!blocked("https://notdoubleclick.net/x"));
        assert!(!blocked("https://www.facebook.com/"));
        assert!(!blocked("https://chat.zalo.me/"));
        assert!(!valid_navigation("file:///C:/private"));
        assert!(!valid_navigation("javascript:alert(1)"));
    }
}
