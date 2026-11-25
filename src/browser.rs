use crate::{AppEvent, model::Account};
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
    Microsoft::Web::WebView2::Win32::*, NewWindowRequestedEventHandler,
    NotificationReceivedEventHandler, WebResourceRequestedEventHandler, take_pwstr,
};
use windows::{
    Win32::System::Com::IStream,
    core::{Interface, PWSTR, w},
};
use wry::{
    PermissionKind, PermissionResponse, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

pub struct Browser {
    pub view: WebView,
    pub _context: WebContext,
    pub blocking: Arc<AtomicBool>,
    pub notifications: Arc<AtomicBool>,
}

pub fn bounds(window: &Window) -> Rect {
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    Rect {
        position: LogicalPosition::new(260.0, 88.0).into(),
        size: LogicalSize::new((s.width - 260.0).max(1.0), (s.height - 88.0).max(1.0)).into(),
    }
}

pub fn create(
    window: &Window,
    account: &Account,
    directory: &Path,
    proxy: EventLoopProxy<AppEvent>,
) -> Result<Browser, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut context = WebContext::new(Some(directory.join("webview")));
    let blocking = Arc::new(AtomicBool::new(account.blocking));
    let notifications = Arc::new(AtomicBool::new(account.notifications));
    let notices = notifications.clone();
    let id = account.id.clone();
    let p = proxy.clone();
    let title_id = id.clone();
    let title_proxy = proxy.clone();
    // The website webview deliberately has no native IPC bridge.
    let view = WebViewBuilder::new_with_web_context(&mut context)
        .with_bounds(bounds(window))
        .with_visible(false)
        .with_url("about:blank")
        .with_devtools(true)
        .with_browser_accelerator_keys(true)
        .with_navigation_handler(|url| valid_navigation(&url))
        .with_permission_handler(move |kind| {
            if matches!(kind, PermissionKind::Notifications) && !notices.load(Ordering::Relaxed) {
                PermissionResponse::Deny
            } else {
                PermissionResponse::Default
            }
        })
        .with_on_page_load_handler(move |_, url| {
            let _ = p.send_event(AppEvent::Page {
                id: id.clone(),
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
        .build_as_child(window)
        .map_err(|e| e.to_string())?;
    install_hooks(
        &view,
        account.id.clone(),
        blocking.clone(),
        notifications.clone(),
        proxy.clone(),
    )?;
    mute(&view, account.muted)?;
    view.zoom(account.zoom).map_err(|e| e.to_string())?;
    let snapshot = directory.join("session.dpapi");
    if crate::session::restore(&view, &snapshot).is_err() {
        let _ = std::fs::rename(
            &snapshot,
            directory.join(format!("session-unreadable-{}.dpapi", uuid::Uuid::new_v4())),
        );
        let _ = proxy.send_event(AppEvent::Warning("A session-cookie backup could not be recovered. The browser profile was opened with its existing storage; this account may need to sign in again.".into()));
    }
    view.load_url(&account.url).map_err(|e| e.to_string())?;
    Ok(Browser {
        view,
        _context: context,
        blocking,
        notifications,
    })
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
