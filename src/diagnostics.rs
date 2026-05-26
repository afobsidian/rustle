use std::any::Any;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustle_core::default_data_dir;
use tracing::error;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Clone)]
pub(crate) struct DiagnosticsState {
    pub(crate) log_path: Option<PathBuf>,
}

pub(crate) fn install() -> DiagnosticsState {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,whisper_rs=warn"));

    let log_path = default_log_path().ok();
    let file_writer = log_path
        .as_ref()
        .and_then(|path| open_log_file(path).ok())
        .map(Mutex::new)
        .map(Arc::new);

    let make_writer = {
        let file_writer = file_writer.clone();
        move || TeeWriter::new(file_writer.clone())
    };

    fmt()
        .with_env_filter(filter)
        .with_writer(make_writer)
        .init();
    install_panic_hook(log_path.clone());

    DiagnosticsState { log_path }
}

fn open_log_file(path: &Path) -> io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    OpenOptions::new().create(true).append(true).open(path)
}

fn install_panic_hook(log_path: Option<PathBuf>) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".to_owned());
        let message = panic_payload_message(panic_info.payload());

        error!(
            subsystem = "panic",
            location,
            panic = %message,
            log_path = log_path.as_ref().map(|path| path.display().to_string()),
            "unhandled panic"
        );

        previous_hook(panic_info);
    }));
}

pub(crate) fn default_log_path() -> Result<PathBuf, rustle_core::CoreError> {
    Ok(default_data_dir()?.join("logs").join("rustle.log"))
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }

    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }

    "panic payload is not a string".to_owned()
}

struct TeeWriter {
    file: Option<Arc<Mutex<std::fs::File>>>,
}

impl TeeWriter {
    fn new(file: Option<Arc<Mutex<std::fs::File>>>) -> Self {
        Self { file }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut stderr = io::stderr().lock();
        stderr.write_all(buf)?;

        if let Some(file) = &self.file {
            let mut file = file
                .lock()
                .map_err(|_| io::Error::other("log file lock poisoned"))?;
            file.write_all(buf)?;
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().lock().flush()?;

        if let Some(file) = &self.file {
            let mut file = file
                .lock()
                .map_err(|_| io::Error::other("log file lock poisoned"))?;
            file.flush()?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{default_log_path, panic_payload_message};
    use std::path::PathBuf;

    #[test]
    fn default_log_path_honors_xdg_data_home() {
        let original = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/tmp/rustle-log-test");
        }

        let path = default_log_path().unwrap();

        assert_eq!(
            path,
            PathBuf::from("/tmp/rustle-log-test/rustle/logs/rustle.log")
        );

        match original {
            Some(value) => unsafe {
                std::env::set_var("XDG_DATA_HOME", value);
            },
            None => unsafe {
                std::env::remove_var("XDG_DATA_HOME");
            },
        }
    }

    #[test]
    fn panic_payload_message_formats_strings() {
        assert_eq!(panic_payload_message(&"boom"), "boom");
        assert_eq!(
            panic_payload_message(&String::from("owned boom")),
            "owned boom"
        );
    }

    #[test]
    fn panic_payload_message_handles_non_string_payloads() {
        assert_eq!(
            panic_payload_message(&42_u8),
            "panic payload is not a string"
        );
    }
}
