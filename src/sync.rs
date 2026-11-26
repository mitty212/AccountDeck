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
        Action::Move { target, modifiers }
        | Action::Click {
            target, modifiers, ..
        } => {
            !target.selector.is_empty()
                && target.selector.len() <= 2048
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
            !selector.is_empty() && selector.len() <= 2048 && value.len() <= 65536
        }
        Action::Scroll { selector, x, y, .. } => {
            selector.len() <= 2048
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
            selector.len() <= 2048
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
pub fn configure(view: &WebView, source: bool) {
    let _ = view.evaluate_script(&format!("window.__accountdeckSync?.configure({source})"));
}

#[derive(Clone, Default)]
pub struct Queue {
    pending: Rc<RefCell<VecDeque<(Action, u64, String)>>>,
    busy: Rc<Cell<bool>>,
}
fn pump(core: ICoreWebView2, queue: Queue, epoch: Arc<AtomicU64>) {
    if queue.busy.get() {
        return;
    }
    loop {
        let Some((action, expected, page)) = queue.pending.borrow_mut().pop_front() else {
            return;
        };
        if expected != epoch.load(Ordering::Relaxed) {
            continue;
        }
        queue.busy.set(true);
        let q = queue.clone();
        let c = core.clone();
        let e = epoch.clone();
        let done: Rc<dyn Fn()> = Rc::new(move || {
            q.busy.set(false);
            pump(c.clone(), q.clone(), e.clone());
        });
        if execute(core.clone(), action, epoch, expected, done.clone(), page).is_err() {
            done();
        }
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
) -> Result<(), String> {
    if !valid(&action) {
        return Err("Invalid synchronized action.".into());
    }
    // Keep actions in order so rapid typing cannot race selection/replacement.
    {
        let mut pending = queue.pending.borrow_mut();
        let coalesce = match (pending.back(), &action) {
            (Some((Action::Move { .. }, generation, _)), Action::Move { .. }) => {
                *generation == expected
            }
            (
                Some((
                    Action::Input {
                        selector: previous, ..
                    },
                    generation,
                    _,
                )),
                Action::Input { selector, .. },
            ) => *generation == expected && previous == selector,
            _ => false,
        };
        if coalesce {
            pending.pop_back();
        }
        if pending.len() >= 512 {
            return Err("Synchronized input queue is full.".into());
        }
        pending.push_back((action, expected, page));
    }
    pump(view.webview(), queue, epoch);
    Ok(())
}
fn call(
    core: ICoreWebView2,
    steps: Arc<Vec<(String, Value)>>,
    index: usize,
    epoch: Arc<AtomicU64>,
    expected: u64,
    done: Rc<dyn Fn()>,
    page: Arc<String>,
) {
    if epoch.load(Ordering::Relaxed) != expected || index >= steps.len() {
        done();
        return;
    }
    let mut url = windows::core::PWSTR::null();
    let matches = unsafe { core.Source(&mut url) }.is_ok()
        && page_key(&webview2_com::take_pwstr(url)).as_ref() == Some(page.as_ref());
    if !matches {
        done();
        return;
    }
    let next = core.clone();
    let rest = steps.clone();
    let guard = epoch.clone();
    let next_done = done.clone();
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, _| {
        if status.is_ok() {
            call(
                next,
                rest,
                index + 1,
                guard,
                expected,
                next_done.clone(),
                page,
            );
        } else {
            next_done();
        }
        Ok(())
    }));
    unsafe {
        if core
            .CallDevToolsProtocolMethod(
                &HSTRING::from(&steps[index].0),
                &HSTRING::from(steps[index].1.to_string()),
                &handler,
            )
            .is_err()
        {
            done();
        }
    }
}
/// Prepare the matching DOM element, then dispatch real Chromium input to it.
/// The shared epoch cancels queued commands as soon as the user stops sync.
fn execute(
    core: ICoreWebView2,
    action: Action,
    epoch: Arc<AtomicU64>,
    expected: u64,
    done: Rc<dyn Fn()>,
    page: String,
) -> Result<(), String> {
    if !valid(&action) {
        return Err("Invalid synchronized action.".into());
    }
    let payload = serde_json::to_string(&action).map_err(|e| e.to_string())?;
    let key = page_key(&page).ok_or("Invalid synchronized page.")?;
    let expected_key = serde_json::to_string(&key).unwrap();
    let expression = format!(
        r#"(()=>{{
      if(location.origin+location.pathname!=={expected_key})return null;
      const a={payload};let el;
      function find(s){{let options;try{{options=JSON.parse(s);}}catch{{options=[s];}}if(!Array.isArray(options))return null;for(const candidate of options){{try{{const e=document.querySelector(candidate);if(e)return e;}}catch{{}}}}return null;}}
      try{{el=a.kind==='move'||a.kind==='click'?find(a.target.selector):a.selector?find(a.selector):document.documentElement;}}catch{{return null;}}
      if(!el||el.closest('input[type=password],[autocomplete^="cc-"],[autocomplete="one-time-code"]'))return null;
      if(a.kind==='move'||a.kind==='click'){{const r=el.getBoundingClientRect();const x=r.left+r.width*a.target.rx,y=r.top+r.height*a.target.ry;if(x<0||y<0||x>=innerWidth||y>=innerHeight)return null;return {{x,y}};}}
      if(a.kind==='scroll'){{if(a.window)window.scrollTo(a.x,a.y);else el.scrollTo(a.x,a.y);return {{done:true}};}}
      if(a.kind==='change'){{if(!(el instanceof HTMLSelectElement))return null;el.value=a.value;el.dispatchEvent(new Event('input',{{bubbles:true}}));el.dispatchEvent(new Event('change',{{bubbles:true}}));return {{done:true}};}}
      el.focus({{preventScroll:true}});
      if(a.kind==='input'){{
        if(a.editable){{if(!el.isContentEditable)return null;const r=document.createRange();r.selectNodeContents(el);const s=getSelection();s.removeAllRanges();s.addRange(r);}}
        else{{if(!(el instanceof HTMLInputElement||el instanceof HTMLTextAreaElement))return null;
          if(el instanceof HTMLInputElement&&!['text','search','email','url','tel','number','range'].includes(el.type))return null;
          try{{el.setSelectionRange(0,el.value.length);}}catch{{const prototype=el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(prototype,'value').set.call(el,a.value);el.dispatchEvent(new InputEvent('input',{{bubbles:true,inputType:'insertReplacementText',data:a.value}}));return {{done:true}};}}
        }}
      }}
      return {{ready:true}};
    }})()"#
    );
    let next = core.clone();
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
        move |status, response| {
            if status.is_err() || epoch.load(Ordering::Relaxed) != expected {
                done();
                return Ok(());
            }
            let raw = response;
            let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let p = &value["result"]["value"];
            if p.is_null() || p["done"] == true {
                done();
                return Ok(());
            }
            let mut steps = Vec::<(String, Value)>::new();
            match action {
                Action::Move { modifiers, .. } => {
                    steps.push((
                        "Input.dispatchMouseEvent".into(),
                        json!({"type":"mouseMoved","x":p["x"],"y":p["y"],"modifiers":modifiers}),
                    ));
                }
                Action::Click {
                    modifiers, count, ..
                } => {
                    for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
                        steps.push(("Input.dispatchMouseEvent".into(),json!({"type":kind,"x":p["x"],"y":p["y"],"button":"left","clickCount":count.clamp(1,2),"modifiers":modifiers})));
                    }
                }
                Action::Input { value, .. } => {
                    if value.is_empty() {
                        for kind in ["rawKeyDown", "keyUp"] {
                            steps.push(("Input.dispatchKeyEvent".into(),json!({"type":kind,"key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8})));
                        }
                    } else {
                        steps.push(("Input.insertText".into(), json!({"text":value})));
                    }
                }
                Action::Key {
                    key,
                    code,
                    modifiers,
                    ..
                } => {
                    let vk = match key.as_str() {
                        "Enter" => 13,
                        "Tab" => 9,
                        "Escape" => 27,
                        "ArrowLeft" => 37,
                        "ArrowUp" => 38,
                        "ArrowRight" => 39,
                        "ArrowDown" => 40,
                        "Home" => 36,
                        "End" => 35,
                        "PageUp" => 33,
                        "PageDown" => 34,
                        _ => 0,
                    };
                    for kind in ["rawKeyDown", "keyUp"] {
                        steps.push(("Input.dispatchKeyEvent".into(),json!({"type":kind,"key":key,"code":code,"windowsVirtualKeyCode":vk,"modifiers":modifiers})));
                    }
                }
                _ => {}
            }
            call(
                next,
                Arc::new(steps),
                0,
                epoch,
                expected,
                done.clone(),
                Arc::new(key),
            );
            Ok(())
        },
    ));
    unsafe {
        core.CallDevToolsProtocolMethod(
            &HSTRING::from("Runtime.evaluate"),
            &HSTRING::from(json!({"expression":expression,"returnByValue":true}).to_string()),
            &handler,
        )
        .map_err(|e| e.to_string())
    }
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
