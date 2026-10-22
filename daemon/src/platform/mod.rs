use crate::DaemonState;
use crate::events::EventTriggers;
use anyhow::Result;
use cfg_if::cfg_if;
use std::path::PathBuf;
use tokio::sync::mpsc;
use which::which;

cfg_if! {
    if #[cfg(windows)] {
        mod windows;

        pub fn perform_preflight() -> Result<()> {
            windows::perform_platform_preflight()
        }

        pub async fn spawn_runtime(state: DaemonState, tx: mpsc::Sender<EventTriggers>) -> Result<()> {
            windows::spawn_platform_runtime(state, tx).await
        }

        pub fn has_autostart() -> bool {
            windows::has_autostart()
        }

        pub fn set_autostart(enabled: bool) -> Result<()> {
            if enabled {
                return windows::enable_autostart();
            }
            windows::disable_autostart()
        }

        pub fn display_error(message: String) {
            windows::display_error(message);
        }
    } else if #[cfg(target_os = "linux")] {
        mod linux;
        mod unix;


        pub fn perform_preflight() -> Result<()> {
            Ok(())
        }

        pub async fn spawn_runtime(state: DaemonState, tx: mpsc::Sender<EventTriggers>) -> Result<()> {
            tokio::spawn(linux::sleep::run(tx.clone(), state.shutdown.clone()));
            unix::spawn_platform_runtime(state, tx).await
        }

        pub fn has_autostart() -> bool {
            linux::autostart::has_autostart()
        }

        pub fn set_autostart(enabled: bool) -> Result<()> {
            if enabled {
                return linux::autostart::create_startup_link();
            }
            linux::autostart::remove_startup_link()
        }

        pub fn display_error(message: String) {
            linux::display_error(message);
        }
    } else if #[cfg(target_os = "macos")] {
        mod macos;

        pub fn perform_preflight() -> Result<()> {
            Ok(())
        }

        pub async fn spawn_runtime(state: DaemonState, tx: mpsc::Sender<EventTriggers>) -> Result<()> {
            macos::runtime::run(tx.clone(), state.shutdown.clone()).await
        }

        pub fn has_autostart() -> bool {
            macos::has_autostart()
        }

        pub fn set_autostart(enabled: bool) -> Result<()> {
            macos::set_autostart(enabled)
        }

         pub fn display_error(message: String) {
            macos::display_error(message);
         }
    } else {
        use anyhow::bail;

        pub fn perform_preflight() -> Result<()> {
            Ok(())
        }

        pub async fn spawn_runtime(_state: DaemonState, _tx: mpsc::Sender<EventTriggers>) -> Result<()> {
            Ok(())
        }

        pub fn has_autostart() -> bool {
            false
        }

        pub fn set_autostart(_enabled: bool) -> Result<()> {
            bail!("Autostart Not Supported on this Platform");
        }

        pub fn display_error(message: String) {}
    }
}

pub fn get_ui_app_path() -> Option<PathBuf> {
    // This simply looks for the GoXLR UI App alongside the daemon binary and returns it..
    let mut path = None;
    let bin_name = get_ui_binary_name();

    // Next to the daemon, then on $PATH. Not in the current directory: it can be any folder
    // (or one that no longer exists), and the launcher doesn't look there either.
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let bin = parent.join(bin_name.clone());
        if bin.exists() {
            path.replace(bin);
        }
    }

    if path.is_none() {
        // Try and locate the binary on $PATH
        if let Ok(which) = which(bin_name) {
            path.replace(which);
        }
    }

    path
}

static UI_NAME: &str = "goxlr-desk-ui";
fn get_ui_binary_name() -> String {
    if cfg!(windows) {
        format!("{UI_NAME}.exe")
    } else {
        String::from(UI_NAME)
    }
}
