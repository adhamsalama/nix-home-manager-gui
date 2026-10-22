//! Runs long commands (build, switch, activation) and streams their output to
//! the UI as `run-log` events, followed by one `run-done` event.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub run_id: String,
    pub stream: &'static str,
    pub line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDone {
    pub run_id: String,
    pub success: bool,
    pub code: Option<i32>,
}

/// Guards against two home-manager runs at once.
#[derive(Default)]
pub struct RunLock(AtomicBool);

impl RunLock {
    pub fn try_acquire(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub fn release(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn log(app: &AppHandle, run_id: &str, stream: &'static str, line: impl Into<String>) {
    let _ = app.emit(
        "run-log",
        LogLine {
            run_id: run_id.into(),
            stream,
            line: line.into(),
        },
    );
}

fn done(app: &AppHandle, run_id: &str, success: bool, code: Option<i32>) {
    let _ = app.emit(
        "run-done",
        RunDone {
            run_id: run_id.into(),
            success,
            code,
        },
    );
}

/// Spawns `program args`, streaming stdout and stderr line by line.
pub async fn run_process(
    app: AppHandle,
    run_id: String,
    program: String,
    args: Vec<String>,
) -> bool {
    log(
        &app,
        &run_id,
        "info",
        format!("$ {} {}", program, args.join(" ")),
    );
    let child = crate::nix::command(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            log(
                &app,
                &run_id,
                "stderr",
                format!("failed to start {program}: {e}"),
            );
            done(&app, &run_id, false, None);
            return false;
        }
    };

    let stdout = child.stdout.take().map(|s| {
        let (app, id) = (app.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log(&app, &id, "stdout", line);
            }
        })
    });
    let stderr = child.stderr.take().map(|s| {
        let (app, id) = (app.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log(&app, &id, "stderr", line);
            }
        })
    });

    let status = child.wait().await;
    for task in [stdout, stderr].into_iter().flatten() {
        let _ = task.await;
    }
    let (success, code) = match status {
        Ok(s) => (s.success(), s.code()),
        Err(e) => {
            log(&app, &run_id, "stderr", e.to_string());
            (false, None)
        }
    };
    done(&app, &run_id, success, code);
    success
}

/// Replays canned output with small delays, for mock mode.
pub async fn run_simulated(
    app: AppHandle,
    run_id: String,
    lines: Vec<(&'static str, String)>,
    success: bool,
) -> bool {
    for (stream, line) in lines {
        tokio::time::sleep(Duration::from_millis(120)).await;
        log(&app, &run_id, stream, line);
    }
    done(&app, &run_id, success, Some(if success { 0 } else { 1 }));
    success
}
