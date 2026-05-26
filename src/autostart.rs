use std::path::{Path, PathBuf};

use rustle_core::{LoginMethod, Settings};
use tokio::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutostartPaths {
    config_home: PathBuf,
}

impl AutostartPaths {
    fn from_environment() -> std::io::Result<Self> {
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "HOME or XDG_CONFIG_HOME must be set for start-on-login",
                )
            })?;

        Ok(Self { config_home })
    }

    fn xdg_autostart_path(&self) -> PathBuf {
        self.config_home.join("autostart").join("rustle.desktop")
    }

    fn systemd_unit_path(&self) -> PathBuf {
        self.config_home.join("systemd/user").join("rustle.service")
    }

    fn systemd_wants_dir(&self) -> PathBuf {
        self.config_home.join("systemd/user/default.target.wants")
    }

    fn systemd_wants_path(&self) -> PathBuf {
        self.systemd_wants_dir().join("rustle.service")
    }
}

pub(crate) async fn reconcile(settings: &Settings) -> std::io::Result<()> {
    let paths = AutostartPaths::from_environment()?;
    let executable = std::env::current_exe()?;
    reconcile_at(settings, &paths, &executable).await
}

async fn reconcile_at(
    settings: &Settings,
    paths: &AutostartPaths,
    executable: &Path,
) -> std::io::Result<()> {
    if !settings.general.start_on_login {
        remove_xdg_autostart(paths).await?;
        remove_systemd_startup(paths).await?;
        return Ok(());
    }

    match settings.general.start_on_login_method {
        LoginMethod::Xdg => {
            remove_systemd_startup(paths).await?;
            write_xdg_autostart(paths, executable).await?;
        }
        LoginMethod::Systemd => {
            remove_xdg_autostart(paths).await?;
            write_systemd_unit(paths, executable).await?;
        }
    }

    Ok(())
}

async fn write_xdg_autostart(paths: &AutostartPaths, executable: &Path) -> std::io::Result<()> {
    let path = paths.xdg_autostart_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    fs::write(path, desktop_entry_contents(executable)).await
}

async fn write_systemd_unit(paths: &AutostartPaths, executable: &Path) -> std::io::Result<()> {
    let unit_path = paths.systemd_unit_path();
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&unit_path, systemd_unit_contents(executable)).await?;

    let wants_dir = paths.systemd_wants_dir();
    fs::create_dir_all(&wants_dir).await?;

    let wants_path = paths.systemd_wants_path();
    remove_if_exists(&wants_path).await?;
    create_symlink(&unit_path, &wants_path).await
}

async fn remove_xdg_autostart(paths: &AutostartPaths) -> std::io::Result<()> {
    remove_if_exists(&paths.xdg_autostart_path()).await
}

async fn remove_systemd_startup(paths: &AutostartPaths) -> std::io::Result<()> {
    remove_if_exists(&paths.systemd_wants_path()).await?;
    remove_if_exists(&paths.systemd_unit_path()).await
}

async fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn desktop_entry_contents(executable: &Path) -> String {
    format!(
        concat!(
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Version=1.0\n",
            "Name=Rustle\n",
            "Comment=Tray-first meeting notes app\n",
            "Exec={}\n",
            "Terminal=false\n",
            "X-GNOME-Autostart-enabled=true\n"
        ),
        executable.display()
    )
}

fn systemd_unit_contents(executable: &Path) -> String {
    format!(
        concat!(
            "[Unit]\n",
            "Description=Rustle tray application\n\n",
            "[Service]\n",
            "Type=simple\n",
            "ExecStart={}\n",
            "Restart=on-failure\n",
            "RestartSec=5\n\n",
            "[Install]\n",
            "WantedBy=default.target\n"
        ),
        executable.display()
    )
}

async fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(unix))]
    {
        let _ = target;
        let _ = link;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "systemd user startup symlinks are only supported on Unix",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustle_core::GeneralSettings;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{timestamp}"))
    }

    fn settings(start_on_login: bool, method: LoginMethod) -> Settings {
        Settings {
            general: GeneralSettings {
                start_on_login,
                start_on_login_method: method,
            },
            ..Settings::default()
        }
    }

    #[tokio::test]
    async fn enabling_xdg_autostart_writes_desktop_entry() {
        let temp_root = unique_temp_path("rustle-autostart-test");
        let paths = AutostartPaths {
            config_home: temp_root.join("config"),
        };
        let executable = temp_root.join("bin/rustle");

        reconcile_at(&settings(true, LoginMethod::Xdg), &paths, &executable)
            .await
            .unwrap();

        let desktop_entry = fs::read_to_string(paths.xdg_autostart_path())
            .await
            .unwrap();
        assert!(desktop_entry.contains("Type=Application"));
        assert!(desktop_entry.contains("Name=Rustle"));
        assert!(desktop_entry.contains(&format!("Exec={}", executable.display())));
        assert!(!paths.systemd_unit_path().exists());
        assert!(!paths.systemd_wants_path().exists());
        let _ = fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn enabling_systemd_startup_writes_unit_and_symlink() {
        let temp_root = unique_temp_path("rustle-autostart-test");
        let paths = AutostartPaths {
            config_home: temp_root.join("config"),
        };
        let executable = temp_root.join("bin/rustle");

        reconcile_at(&settings(true, LoginMethod::Systemd), &paths, &executable)
            .await
            .unwrap();

        let unit_file = fs::read_to_string(paths.systemd_unit_path()).await.unwrap();
        assert!(unit_file.contains("[Service]"));
        assert!(unit_file.contains(&format!("ExecStart={}", executable.display())));

        let symlink_target = fs::read_link(paths.systemd_wants_path()).await.unwrap();
        assert_eq!(symlink_target, paths.systemd_unit_path());
        assert!(!paths.xdg_autostart_path().exists());
        let _ = fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn switching_methods_removes_stale_artifacts() {
        let temp_root = unique_temp_path("rustle-autostart-test");
        let paths = AutostartPaths {
            config_home: temp_root.join("config"),
        };
        let executable = temp_root.join("bin/rustle");

        reconcile_at(&settings(true, LoginMethod::Xdg), &paths, &executable)
            .await
            .unwrap();
        assert!(paths.xdg_autostart_path().exists());

        reconcile_at(&settings(true, LoginMethod::Systemd), &paths, &executable)
            .await
            .unwrap();

        assert!(!paths.xdg_autostart_path().exists());
        assert!(paths.systemd_unit_path().exists());
        assert!(paths.systemd_wants_path().exists());
        let _ = fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn disabling_start_on_login_cleans_up_all_artifacts() {
        let temp_root = unique_temp_path("rustle-autostart-test");
        let paths = AutostartPaths {
            config_home: temp_root.join("config"),
        };
        let executable = temp_root.join("bin/rustle");

        reconcile_at(&settings(true, LoginMethod::Systemd), &paths, &executable)
            .await
            .unwrap();
        reconcile_at(&settings(false, LoginMethod::Systemd), &paths, &executable)
            .await
            .unwrap();

        assert!(!paths.xdg_autostart_path().exists());
        assert!(!paths.systemd_unit_path().exists());
        assert!(!paths.systemd_wants_path().exists());
        let _ = fs::remove_dir_all(&temp_root).await;
    }
}
