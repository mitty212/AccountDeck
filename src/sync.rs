use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2,
};
use windows::core::HSTRING;
use wry::{WebView, WebViewExtWindows};

#[derive(Clone, Deserialize, Serialize)]
pub struct Target {
    pub selector: String,
    pub rx: f64,
    pub ry: f64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    Ready {
        enabled: bool,
    },
    Move {
        target: Target,
        modifiers: u8,
    },
    Click {
        target: Target,
        modifiers: u8,
        count: u8,
    },
    Input {
        selector: String,
        value: String,
        editable: bool,
    },
    Change {
        selector: String,
        value: String,
    },
    Scroll {
        selector: String,
        window: bool,
        x: f64,
        y: f64,
    },
    Key {
        selector: String,
        key: String,
        code: String,
        modifiers: u8,
    },
}
#[derive(Deserialize)]
pub struct Packet {
    pub token: String,
    pub page: String,
    pub action: Action,
}
#[derive(Default, Serialize)]
pub struct Selection {
    pub source: Option<String>,
    pub targets: Vec<String>,
    pub include_passwords: bool,
}
pub fn page_key(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    Some(format!("{}{}", u.origin().ascii_serialization(), u.path()))
}
pub fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin() && matches!(a.scheme(), "http" | "https"),
        _ => false,
    }
}
pub fn valid(action: &Action) -> bool {
    match action {
        Action::Ready { .. } => true,
        Action::Move { target, modifiers }
        | Action::Click {
            target, modifiers, ..
        } => {
            !target.selector.is_empty()
                && target.selector.len() <= 8192
                && target.rx.is_finite()
                && target.ry.is_finite()
                && (0.0..=1.0).contains(&target.rx)
                && (0.0..=1.0).contains(&target.ry)
                && *modifiers <= 15
        }
        Action::Input {
            selector, value, ..
        }
        | Action::Change { selector, value } => {
            !selector.is_empty() && selector.len() <= 8192 && value.len() <= 65536
        }
        Action::Scroll { selector, x, y, .. } => {
            selector.len() <= 8192
                && x.is_finite()
                && y.is_finite()
                && x.abs() < 100_000_000.
                && y.abs() < 100_000_000.
        }
        Action::Key {
            selector,
            key,
            code,
            modifiers,
        } => {
            selector.len() <= 8192
                && code.len() < 32
                && *modifiers <= 15
                && matches!(
                    key.as_str(),
                    "Enter"
                        | "Tab"
                        | "Escape"
                        | "ArrowLeft"
                        | "ArrowRight"
                        | "ArrowUp"
                        | "ArrowDown"
                        | "Home"
                        | "End"
                        | "PageUp"
                        | "PageDown"
                )
        }
    }
}
pub fn capture_script(token: &str) -> String {
    include_str!("sync_capture.js").replace("__TOKEN__", &serde_json::to_string(token).unwrap())
}
pub fn configure(view: &WebView, source: bool, passwords: bool) {
    let _ = view.evaluate_script(&format!(
        "window.__accountdeckSync?.configure({source},{passwords})"
    ));
}
type Done = Rc<dyn Fn(Result<(), String>)>;
struct Pending {
    action: Action,
    generation: u64,
    page: String,
    passwords: bool,
    report: Done,
}
#[derive(Clone, Default)]
pub struct Queue {
    pending: Rc<RefCell<VecDeque<Pending>>>,
    busy: Rc<Cell<bool>>,
}
fn pump(core: ICoreWebView2, queue: Queue, epoch: Arc<AtomicU64>) {
    if queue.busy.get() {
        return;
    }
    loop {
        let Some(p) = queue.pending.borrow_mut().pop_front() else {
            return;
        };
        if p.generation != epoch.load(Ordering::Relaxed) {
            continue;
        }
        queue.busy.set(true);
        let (q, c, e) = (queue.clone(), core.clone(), epoch.clone());
        let completed = Cell::new(false);
        let done: Done = Rc::new(move |result| {
            if completed.replace(true) {
                return;
            }
            (p.report)(result);
            q.busy.set(false);
            pump(c.clone(), q.clone(), e.clone());
        });
        execute(
            core,
            p.action,
            epoch,
            p.generation,
            done,
            p.page,
            p.passwords,
            false,
        );
        return;
    }
}
pub fn replay(
    view: &WebView,
    action: Action,
    queue: Queue,
    epoch: Arc<AtomicU64>,
    expected: u64,
    page: String,
    passwords: bool,
    report: Done,
) -> Result<(), String> {
    if !valid(&action) || matches!(action, Action::Ready { .. }) {
        return Err("Invalid synchronized action.".into());
    }
    {
        let mut pending = queue.pending.borrow_mut();
        let coalesce = pending.back().is_some_and(|p| {
            p.generation == expected
                && match (&p.action, &action) {
                    (Action::Move { .. }, Action::Move { .. }) => true,
                    (Action::Input { selector: a, .. }, Action::Input { selector: b, .. }) => {
                        a == b
                    }
                    _ => false,
                }
        });
        if coalesce {
            pending.pop_back();
        }
        if pending.len() >= 512 {
            return Err("Synchronized input queue is full.".into());
        }
        pending.push_back(Pending {
            action,
            generation: expected,
            page,
            passwords,
            report,
        });
    }
    pump(view.webview(), queue, epoch);
    Ok(())
}
// Never return protocol payloads in errors: input commands can contain credentials.
fn cdp(
    core: ICoreWebView2,
    method: &str,
    params: Value,
    callback: impl FnOnce(Result<Value, String>) + 'static,
) {
    let once = Rc::new(RefCell::new(Some(callback)));
    let completed = once.clone();
    let handler =
        CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, response| {
            if let Some(callback) = completed.borrow_mut().take() {
                let parsed = serde_json::from_str::<Value>(&response).ok();
                callback(
                    if status.is_err() || parsed.as_ref().is_none_or(|v| v.get("error").is_some()) {
                        Err("Browser could not apply this action.".into())
                    } else {
                        Ok(parsed.unwrap())
                    },
                );
            }
            Ok(())
        }));
    let result = unsafe {
        core.CallDevToolsProtocolMethod(
            &HSTRING::from(method),
            &HSTRING::from(params.to_string()),
            &handler,
        )
    };
    if result.is_err() {
        if let Some(callback) = once.borrow_mut().take() {
            callback(Err("Browser could not apply this action.".into()));
        }
    }
}
fn matches_page(core: &ICoreWebView2, page: &str) -> bool {
    let mut url = windows::core::PWSTR::null();
    unsafe { core.Source(&mut url) }.is_ok()
        && page_key(&webview2_com::take_pwstr(url)).as_deref() == Some(page)
}
fn call(
    core: ICoreWebView2,
    steps: Arc<Vec<(String, Value)>>,
    index: usize,
    epoch: Arc<AtomicU64>,
    expected: u64,
    done: Done,
    page: Arc<String>,
) {
    if index >= steps.len() {
        done(Ok(()));
        return;
    }
    let (method, params) = steps[index].clone();
    // Always release an already pressed mouse/key even if its handler navigates or sync stops.
    let release = matches!(params["type"].as_str(), Some("mouseReleased" | "keyUp"));
    if !release && (epoch.load(Ordering::Relaxed) != expected || !matches_page(&core, &page)) {
        done(Err(
            "Target page changed before this action finished.".into()
        ));
        return;
    }
    let next = core.clone();
    cdp(core, &method, params, move |result| match result {
        Ok(_) => call(next, steps, index + 1, epoch, expected, done, page),
        Err(error) => {
            // A failed press may still have reached Chromium. Best effort release keeps its input state usable.
            if steps
                .get(index + 1)
                .is_some_and(|(_, v)| matches!(v["type"].as_str(), Some("mouseReleased" | "keyUp")))
            {
                let (method, params) = &steps[index + 1];
                let end = done.clone();
                cdp(next, method, params.clone(), move |_| end(Err(error)));
            } else {
                done(Err(error));
            }
        }
    });
}
/// Resolve matching controls, hit test after hover, and dispatch trusted Chromium input.
fn execute(
    core: ICoreWebView2,
    action: Action,
    epoch: Arc<AtomicU64>,
    expected: u64,
    done: Done,
    page: String,
    passwords: bool,
    hovered: bool,
) {
    if epoch.load(Ordering::Relaxed) != expected {
        done(Err("Sync session stopped.".into()));
        return;
    }
    let Some(key) = page_key(&page) else {
        done(Err("Invalid synchronized page.".into()));
        return;
    };
    let expression = format!(
        "{}({},{},{})",
        include_str!("sync_resolve.js"),
        serde_json::to_string(&action).unwrap(),
        serde_json::to_string(&key).unwrap(),
        passwords
    );
    let next = core.clone();
    cdp(
        core,
        "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true}),
        move |response| {
            let response = match response {
                Ok(v) => v,
                Err(e) => {
                    done(Err(e));
                    return;
                }
            };
            if epoch.load(Ordering::Relaxed) != expected {
                done(Err("Sync session stopped.".into()));
                return;
            }
            let p = &response["result"]["value"];
            if let Some(error) = p["error"].as_str() {
                done(Err(error.into()));
                return;
            }
            if p.is_null() || response.get("exceptionDetails").is_some() {
                done(Err("Matching control could not be prepared.".into()));
                return;
            }
            if p["done"] == true {
                done(Ok(()));
                return;
            }
            if let Action::Click { modifiers, .. } = &action {
                if !hovered {
                    let end = done.clone();
                    let again = next.clone();
                    let modifiers = *modifiers;
                    cdp(
                        next,
                        "Input.dispatchMouseEvent",
                        json!({"type":"mouseMoved","x":p["x"],"y":p["y"],"buttons":0,"modifiers":modifiers}),
                        move |r| match r {
                            Ok(_) => {
                                execute(again, action, epoch, expected, end, page, passwords, true)
                            }
                            Err(e) => end(Err(e)),
                        },
                    );
                    return;
                }
            }
            let mut steps = Vec::<(String, Value)>::new();
            match action {
            Action::Move{modifiers,..}=>steps.push(("Input.dispatchMouseEvent".into(),json!({"type":"mouseMoved","x":p["x"],"y":p["y"],"buttons":0,"modifiers":modifiers}))),
            Action::Click{modifiers,count,..}=>for (kind,buttons) in [("mousePressed",1),("mouseReleased",0)] {
                steps.push(("Input.dispatchMouseEvent".into(),json!({"type":kind,"x":p["x"],"y":p["y"],"button":"left","buttons":buttons,"clickCount":count.clamp(1,2),"modifiers":modifiers})));
            },
            Action::Input{value,..}=>{
                if p["select_all"]==true {for kind in ["rawKeyDown","keyUp"] {steps.push(("Input.dispatchKeyEvent".into(),json!({"type":kind,"key":"a","code":"KeyA","windowsVirtualKeyCode":65,"modifiers":2})));}}
                if value.is_empty(){for kind in ["rawKeyDown","keyUp"]{steps.push(("Input.dispatchKeyEvent".into(),json!({"type":kind,"key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8})));}}
                else {steps.push(("Input.insertText".into(),json!({"text":value})));}
            },
            Action::Key{key,code,modifiers,..}=>{
                let vk=match key.as_str(){"Enter"=>13,"Tab"=>9,"Escape"=>27,"ArrowLeft"=>37,"ArrowUp"=>38,"ArrowRight"=>39,"ArrowDown"=>40,"Home"=>36,"End"=>35,"PageUp"=>33,"PageDown"=>34,_=>0};
                for kind in ["rawKeyDown","keyUp"]{steps.push(("Input.dispatchKeyEvent".into(),json!({"type":kind,"key":key,"code":code,"windowsVirtualKeyCode":vk,"modifiers":modifiers})));}
            },
            _=>{},
        }
            call(
                next,
                Arc::new(steps),
                0,
                epoch,
                expected,
                done,
                Arc::new(key),
            );
        },
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_pages_ignore_query_but_reject_other_origins_or_paths() {
        assert_eq!(
            page_key("https://example.com/work?a=1"),
            page_key("https://example.com/work?a=2#x")
        );
        assert_ne!(
            page_key("https://example.com/work"),
            page_key("https://example.com/other")
        );
        assert!(!same_origin("https://example.com", "http://example.com"));
        assert_eq!(page_key("file:///secret"), None);
    }
    #[test]
    fn packets_cannot_turn_into_arbitrary_javascript_or_input_methods() {
        assert!(!valid(&Action::Key {
            selector: "body".into(),
            key: "Evaluate".into(),
            code: "".into(),
            modifiers: 0
        }));
        assert!(!valid(&Action::Move {
            target: Target {
                selector: "body".into(),
                rx: f64::NAN,
                ry: 0.5
            },
            modifiers: 0
        }));
    }
}
