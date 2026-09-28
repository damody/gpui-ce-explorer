//! Isolated failure reports for paths that used to abort the process.
//!
//! A window-procedure panic, device-loss panic, or closed stderr pipe used to
//! terminate SuperExplorer. These helpers append the same `error.log` the
//! application diagnostics session uses, then let the caller continue.

use std::{
    fs::{self, OpenOptions},
    io::{Seek as _, SeekFrom, Write as _},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

const MAX_ERROR_LOG_BYTES: u64 = 10 * 1024 * 1024;

pub(crate) fn record_isolated_failure(subsystem: &str, operation: &str, message: &str) {
    log::error!("{subsystem} {operation}: {message}");
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("unnamed");
    let line = format!(
        "timestamp_ms={timestamp_ms} severity=critical subsystem={subsystem:?} operation={operation:?} error={message:?} thread={thread_name:?} version=\"gpui-windows\"\n"
    );
    if append_error_line(&line).is_err() {
        let mut stderr = std::io::stderr();
        let _ = stderr.write_all(line.as_bytes());
        let _ = stderr.flush();
    }
}

pub(crate) fn isolate_window_message(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    handle: impl FnOnce() -> LRESULT,
) -> LRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(handle)) {
        Ok(result) => result,
        Err(payload) => {
            record_isolated_failure("gpui", "window_procedure", &panic_message(&payload));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .map(str::to_owned)
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn append_error_line(line: &str) -> std::io::Result<()> {
    let mut last_error = std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no writable error.log candidate",
    );
    for path in error_log_candidates() {
        match append_line(&path, line) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let current = file.metadata()?.len();
    let incoming = u64::try_from(line.len()).unwrap_or(MAX_ERROR_LOG_BYTES);
    if current > MAX_ERROR_LOG_BYTES
        || current
            .checked_add(incoming)
            .is_none_or(|length| length > MAX_ERROR_LOG_BYTES)
    {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
    } else {
        file.seek(SeekFrom::End(0))?;
    }
    file.write_all(line.as_bytes())?;
    file.flush()
}

fn error_log_candidates() -> Vec<PathBuf> {
    if let Some(directory) = std::env::var_os("EXPLORER_LOG_DIR") {
        return vec![PathBuf::from(directory).join("error.log")];
    }
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("error.log"));
    }
    let local = std::env::var_os("LOCALAPPDATA").map_or_else(
        || std::env::temp_dir().join("RustGpuiExplorer").join("logs"),
        |root| PathBuf::from(root).join("RustGpuiExplorer").join("logs"),
    );
    candidates.push(local.join("error.log"));
    let temporary = std::env::temp_dir()
        .join("RustGpuiExplorer")
        .join("logs")
        .join("error.log");
    if !candidates.contains(&temporary) {
        candidates.push(temporary);
    }
    candidates
}
