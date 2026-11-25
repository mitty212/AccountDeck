//! Real WebView2 check. Run through qa/check_persistence.py, which owns the test origin.
#[path = "../src/session.rs"]
mod session;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{WebContext, WebView, WebViewBuilder};

#[derive(Debug)]
enum CheckEvent {
    Title(String, String),
    Loaded(String, String),
    Saved(String, Result<(), String>),
}

fn write_result(
    path: &Path,
    phase: &str,
    passed: bool,
    profiles: &HashSet<String>,
    loaded: &HashSet<String>,
    error: Option<&str>,
) {
    let mut profiles: Vec<_> = profiles.iter().collect();
    profiles.sort();
    let mut loaded: Vec<_> = loaded.iter().collect();
    loaded.sort();
    let result = serde_json::json!({"phase": phase, "passed": passed, "profiles": profiles, "loaded": loaded, "error": error});
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(error) = fs::write(path, serde_json::to_vec_pretty(&result).unwrap()) {
        eprintln!("Cannot write check result: {error}");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 || !matches!(args[1].as_str(), "seed" | "verify") {
        return Err("Usage: persistence_check seed|verify DATA_DIR SERVER_URL RESULT_JSON".into());
    }
    let phase = args[1].clone();
    let directory = PathBuf::from(&args[2]);
    let origin = url::Url::parse(&args[3])?;
    if origin.scheme() != "http" || origin.host_str() != Some("127.0.0.1") {
        return Err("Check server must be loopback HTTP.".into());
    }
    let result_path = PathBuf::from(&args[4]);
    fs::create_dir_all(&directory)?;
    let profiles: &[&str] = if phase == "seed" {
        &["A", "B"]
    } else {
        &["A", "B", "C"]
    };
    let event_loop = EventLoopBuilder::<CheckEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("AccountDeck persistence check")
        .with_visible(false)
        .with_inner_size(LogicalSize::new(960., 600.))
        .build(&event_loop)?;
    let mut views = HashMap::<String, WebView>::new();
    let mut contexts = Vec::new();
    // Start the deadline before engine creation, which also performs initialization.
    let deadline = Instant::now() + Duration::from_secs(30);
    let setup = (|| -> Result<(), Box<dyn std::error::Error>> {
        for profile in profiles {
            let profile = profile.to_string();
            let profile_directory = directory.join(&profile);
            fs::create_dir_all(&profile_directory)?;
            let mut context = WebContext::new(Some(profile_directory.join("webview")));
            let title_proxy = proxy.clone();
            let title_profile = profile.clone();
            let load_proxy = proxy.clone();
            let load_profile = profile.clone();
            let view = WebViewBuilder::new_with_web_context(&mut context)
                .with_url("about:blank")
                .with_document_title_changed_handler(move |title| {
                    let _ = title_proxy.send_event(CheckEvent::Title(title_profile.clone(), title));
                })
                .with_on_page_load_handler(move |_event, url| {
                    let _ = load_proxy.send_event(CheckEvent::Loaded(load_profile.clone(), url));
                })
                .build_as_child(&window)?;
            // The first network navigation occurs only after cookie restoration.
            session::restore(&view, &profile_directory.join("session.bin"))?;
            let mut url = origin.join(if phase == "seed" { "seed" } else { "verify" })?;
            url.query_pairs_mut().append_pair("profile", &profile);
            view.load_url(url.as_str())?;
            views.insert(profile, view);
            contexts.push(context);
        }
        Ok(())
    })();
    if let Err(error) = setup {
        write_result(
            &result_path,
            &phase,
            false,
            &HashSet::new(),
            &HashSet::new(),
            Some(&format!("Setup failed: {error}")),
        );
        return Err(error);
    }
    let mut ready = HashSet::<String>::new();
    let mut saved = HashSet::<String>::new();
    let mut loaded = HashSet::<String>::new();
    let mut finished = false;
    event_loop.run(move |event, _, control| {
        *control = ControlFlow::WaitUntil(deadline);
        if finished {
            *control = ControlFlow::Exit;
            return;
        }
        let mut failure = None;
        match event {
            Event::UserEvent(CheckEvent::Loaded(profile, url)) => {
                if url.starts_with(origin.as_str()) {
                    loaded.insert(profile);
                }
            }
            Event::UserEvent(CheckEvent::Title(profile, title)) => {
                if title.starts_with("FAIL:") {
                    failure = Some(format!("{profile}: {title}"));
                }
                let expected = format!(
                    "{}:{profile}",
                    if phase == "seed" { "READY" } else { "PASS" }
                );
                if title == expected && ready.insert(profile.clone()) && phase == "seed" {
                    let snapshot_proxy = proxy.clone();
                    let snapshot_profile = profile.clone();
                    if let Err(error) = session::snapshot(
                        &views[&profile],
                        directory.join(&profile).join("session.bin"),
                        move |result| {
                            let _ = snapshot_proxy
                                .send_event(CheckEvent::Saved(snapshot_profile.clone(), result));
                        },
                    ) {
                        failure = Some(format!("Snapshot submission for {profile}: {error}"));
                    }
                }
            }
            Event::UserEvent(CheckEvent::Saved(profile, result)) => match result {
                Ok(()) => {
                    saved.insert(profile);
                }
                Err(error) => failure = Some(format!("Snapshot for {profile}: {error}")),
            },
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => failure = Some("Check window closed early.".into()),
            _ => {}
        }
        if Instant::now() >= deadline {
            failure = Some("Timed out waiting for browser storage checks.".into());
        }
        let complete =
            ready.len() == profiles.len() && (phase == "verify" || saved.len() == profiles.len());
        if complete || failure.is_some() {
            write_result(
                &result_path,
                &phase,
                complete && failure.is_none(),
                &ready,
                &loaded,
                failure.as_deref(),
            );
            views.clear();
            contexts.clear();
            finished = true;
            *control = ControlFlow::Exit;
        }
    });
}
