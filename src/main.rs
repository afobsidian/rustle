//! Rustle application entry point.

mod autostart;
mod diagnostics;
mod models;
mod notifications;
mod self_test;
mod setup;
mod shutdown;

use rustle_core::{is_hyprland_session, supported_session_label, EventBus, Settings};
use tracing::{error, info, warn};

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run_with_diagnostics());
    // Blocking model inference cannot be cancelled by dropping its async task.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    result
}

async fn run_with_diagnostics() -> anyhow::Result<()> {
    let diagnostics = diagnostics::install();
    info!(
        subsystem = "diagnostics",
        log_path = diagnostics
            .log_path
            .as_ref()
            .map(|path| path.display().to_string()),
        "diagnostics initialised"
    );

    let result = run().await;
    if let Err(error) = &result {
        error!(%error, "Rustle could not complete the requested operation");
        notifications::setup_notice("Rustle needs attention", &error.to_string()).await;
    }
    result
}

async fn run() -> anyhow::Result<()> {
    if let Some(command) = std::env::args().nth(1) {
        match command.as_str() {
            "--version" | "-V" => {
                println!("rustle {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "doctor" => return setup::doctor().await,
            "setup" => {
                let _instance_lock = instance_lock()?;
                setup::prepare().await?;
                self_test::run().await?;
                return setup::mark_ready(&Settings::load().await?).await;
            }
            "self-test" => return self_test::run().await,
            "--help" | "-h" => {
                println!("rustle [doctor --json | setup | self-test | --version]\nNo arguments: start the tray application.");
                return Ok(());
            }
            "--session" => {}
            other => anyhow::bail!("unknown command: {other}"),
        }
    }
    let _instance_lock = instance_lock()?;
    let first_session_start = std::env::args().nth(1).as_deref() == Some("--session")
        && !rustle_core::default_config_path()?.exists();
    let mut settings = load_startup_settings().await?;
    if first_session_start {
        settings.general.start_on_login = true;
        settings.general.start_on_login_method = rustle_core::LoginMethod::Systemd;
        settings.save().await?;
    }
    if std::env::args().nth(1).as_deref() == Some("--session") && !settings.general.start_on_login {
        info!("start-on-login disabled by user");
        return Ok(());
    }
    models::check_cpu()?;
    if std::env::var_os("RUSTLE_SELF_TEST_CHILD").is_none() && !setup::is_ready(&settings).await {
        setup::prepare().await?;
        self_test::run().await?;
        settings = Settings::load().await?;
        setup::mark_ready(&settings).await?;
    }
    if let Err(error) = autostart::reconcile(&settings).await {
        warn!(subsystem = "autostart", %error, "failed to reconcile start-on-login integration");
    }
    warn_if_unsupported_session();

    let event_bus = EventBus::new();
    let sender = event_bus.sender();
    let [notifications_rx, tray_rx, detection_rx, audio_rx, transcription_rx, ai_rx, storage_rx, ui_rx, shutdown_rx] =
        std::array::from_fn::<_, 9, _>(|_| event_bus.subscribe());

    initialise_component(
        "notifications",
        notifications::initialise(Some(notifications_rx)),
    )
    .await?;
    initialise_component(
        "tray",
        rustle_tray::initialise(sender.clone(), Some(tray_rx)),
    )
    .await?;
    initialise_component(
        "detection",
        rustle_detection::initialise(sender.clone(), Some(detection_rx)),
    )
    .await?;
    initialise_component(
        "audio",
        rustle_audio::initialise(sender.clone(), Some(audio_rx)),
    )
    .await?;
    initialise_component(
        "transcription",
        rustle_transcription::initialise(sender.clone(), Some(transcription_rx)),
    )
    .await?;
    initialise_component("ai", rustle_ai::initialise(sender.clone(), Some(ai_rx))).await?;
    initialise_component(
        "storage",
        rustle_storage::initialise(sender.clone(), Some(storage_rx)),
    )
    .await?;
    initialise_component("ui", rustle_ui::initialise(sender, Some(ui_rx))).await?;

    info!("rustle initialised");
    shutdown::wait(event_bus.sender(), shutdown_rx).await?;
    Ok(())
}

async fn load_startup_settings() -> anyhow::Result<Settings> {
    match Settings::load().await {
        Ok(settings) => {
            info!(subsystem = "settings", "startup settings loaded");
            Ok(settings)
        }
        Err(error) => {
            error!(subsystem = "settings", %error, "failed to load startup settings");
            Err(error.into())
        }
    }
}

async fn initialise_component<T>(
    component: &'static str,
    future: impl std::future::Future<Output = Result<(), T>>,
) -> Result<(), T>
where
    T: std::fmt::Display,
{
    info!(subsystem = component, "initialising subsystem");
    match future.await {
        Ok(()) => {
            info!(subsystem = component, "subsystem initialised");
            Ok(())
        }
        Err(error) => {
            error!(subsystem = component, %error, "subsystem initialisation failed");
            Err(error)
        }
    }
}

fn warn_if_unsupported_session() {
    if !is_hyprland_session() {
        warn!(
            expected_session = supported_session_label(),
            "Hyprland session variables are missing; detection will attempt instance discovery"
        );
    }
}

fn instance_lock() -> anyhow::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let directory = rustle_core::default_data_dir()?;
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(directory.join("instance.lock"))?;
    nix::fcntl::flock(
        file.as_raw_fd(),
        nix::fcntl::FlockArg::LockExclusiveNonblock,
    )
    .map_err(|_| anyhow::anyhow!("Rustle is already running for this user/data directory"))?;
    Ok(file)
}
