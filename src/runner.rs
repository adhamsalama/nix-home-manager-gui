//! Runs long commands (build, switch, activation) and streams their output to
//! the UI as `RunEvent::Log` events, followed by one `RunEvent::Done`.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone)]
pub enum RunEvent {
    Log {
        run_id: String,
        /// "stdout", "stderr" or "info".
        stream: &'static str,
        line: String,
    },
    Done {
        run_id: String,
        success: bool,
        code: Option<i32>,
    },
}

/// Where run output goes; called from background tasks.
pub type Sink = Arc<dyn Fn(RunEvent) + Send + Sync>;

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

fn log(sink: &Sink, run_id: &str, stream: &'static str, line: impl Into<String>) {
    sink(RunEvent::Log {
        run_id: run_id.into(),
        stream,
        line: line.into(),
    });
}

fn done(sink: &Sink, run_id: &str, success: bool, code: Option<i32>) {
    sink(RunEvent::Done {
        run_id: run_id.into(),
        success,
        code,
    });
}

/// Spawns `program args`, streaming stdout and stderr line by line.
pub async fn run_process(sink: Sink, run_id: String, program: String, args: Vec<String>) -> bool {
    log(
        &sink,
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
                &sink,
                &run_id,
                "stderr",
                format!("failed to start {program}: {e}"),
            );
            done(&sink, &run_id, false, None);
            return false;
        }
    };

    let stdout = child.stdout.take().map(|s| {
        let (sink, id) = (sink.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log(&sink, &id, "stdout", line);
            }
        })
    });
    let stderr = child.stderr.take().map(|s| {
        let (sink, id) = (sink.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log(&sink, &id, "stderr", line);
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
            log(&sink, &run_id, "stderr", e.to_string());
            (false, None)
        }
    };
    done(&sink, &run_id, success, code);
    success
}

/// Replays canned output with small delays, for mock mode.
pub async fn run_simulated(
    sink: Sink,
    run_id: String,
    lines: Vec<(&'static str, String)>,
    success: bool,
) -> bool {
    for (stream, line) in lines {
        tokio::time::sleep(Duration::from_millis(120)).await;
        log(&sink, &run_id, stream, line);
    }
    done(&sink, &run_id, success, Some(if success { 0 } else { 1 }));
    success
}
