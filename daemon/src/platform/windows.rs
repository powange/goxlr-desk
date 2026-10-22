use crate::DaemonState;
use crate::events::EventTriggers;
use anyhow::{Result, bail};
use log::{debug, error, info, warn};
use mslnk::ShellLink;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::{env, fs};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::{select, time};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winrt_toast_reborn::content::audio::Sound;
use winrt_toast_reborn::{Audio, Toast, ToastDuration, ToastManager};

const GOXLR_APP_NAME: &str = "GoXLR App.exe";
const GOXLR_BETA_APP_NAME: &str = "GoXLR Beta App.exe";
const AUTOSTART_FILENAME: &str = "GoXLR Desk.lnk";

static STARTUP_PATH: LazyLock<Option<PathBuf>> = LazyLock::new(get_startup_dir);

pub fn perform_platform_preflight() -> Result<()> {
    if !locate_goxlr_driver() {
        error!("Driver not found, Failing Preflight.");
        bail!("The GoXLR Driver was not found, please install it and try again.");
    }

    if get_official_app_count(&mut System::new()) > 0 {
        error!("Detected Official GoXLR Application Running, Failing Preflight.");
        bail!(
            "The official GoXLR Application is currently running, Please close it before running the Utility"
        );
    }

    if get_utility_count() > 1 {
        error!("Daemon Process already running, Failing Preflight");
        bail!("GoXLR Desk (or GoXLR Utility) is already running, please stop it and try again.");
    }

    Ok(())
}

pub fn display_error(message: String) {
    let message = HSTRING::from(message);

    unsafe {
        MessageBoxW(None, &message, w!("GoXLR Desk"), MB_OK | MB_ICONERROR);
    }
}

/// Refreshes the process list, only the names are needed so nothing else is read (reading
/// everything for every process each second was costly, especially while a session starts).
fn refresh_process_names(system: &mut System) {
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
}

fn get_official_app_count(system: &mut System) -> usize {
    refresh_process_names(system);

    let stable = OsStr::new(GOXLR_APP_NAME);
    let beta = OsStr::new(GOXLR_BETA_APP_NAME);
    system.processes_by_exact_name(stable).count() + system.processes_by_exact_name(beta).count()
}

fn get_utility_count() -> usize {
    if let Ok(exe) = env::current_exe()
        && let Some(file_name) = exe.file_name()
    {
        let mut system = System::new();
        refresh_process_names(&mut system);

        return system.processes_by_exact_name(file_name).count();
    }

    0
}

pub async fn spawn_platform_runtime(
    state: DaemonState,
    tx: mpsc::Sender<EventTriggers>,
) -> Result<()> {
    // Move an old Startup shortcut to the logon task (it runs PowerShell, so not on this thread)
    tokio::task::spawn_blocking(migrate_autostart);

    // Grab an async shutdown event..
    let mut shutdown = state.shutdown.clone();
    let mut duration = time::interval(Duration::from_millis(1000));
    let mut system = System::new();

    let mut ctrl_break = ctrl_break()?;
    let mut ctrl_close = ctrl_close()?;
    let mut ctrl_shutdown = ctrl_shutdown()?;
    let mut ctrl_logoff = ctrl_logoff()?;

    loop {
        select! {
            _ = duration.tick() => {
                let count = get_official_app_count(&mut system);
                if count > 0 {
                    throw_notification();
                    // We're calling 'DevicesStopped' here to force an end to the util, we can't use
                    // the regular Stop because it may attempt to load profiles, which isn't possible
                    // in a situation where the official app is running.
                    tx.send(EventTriggers::DevicesStopped).await?;
                    break;
                }
            },
            Some(_) = ctrl_break.recv() => {
                tx.send(EventTriggers::Stop(false)).await?;
            },
            Some(_) = ctrl_close.recv() => {
                debug!("Hit Ctrl+Close");
                tx.send(EventTriggers::Stop(false)).await?;
            }
            Some(_) = ctrl_shutdown.recv() => {
                debug!("Hit Ctrl+Shutdown");
                tx.send(EventTriggers::Stop(false)).await?;
            }
            Some(_) = ctrl_logoff.recv() => {
                debug!("Hit Ctrl+Logoff");
                tx.send(EventTriggers::Stop(false)).await?;
            }
            //Some(_) = ctrl_
            () = shutdown.recv() => {
                debug!("Shutting down Platform Runtime..");
                break;
            }
        };
    }

    Ok(())
}

fn throw_notification() {
    let manager = ToastManager::new(ToastManager::POWERSHELL_AUM_ID);

    let mut toast = Toast::new();
    toast.text1("GoXLR Desk Stopped");
    toast.text2("Please stop the official app before using GoXLR Desk");
    toast.audio(Audio::new(Sound::SMS));
    toast.duration(ToastDuration::Short);

    let _ = manager.show(&toast);
}

// Autostart is a scheduled task run at logon: Windows runs the Startup folder's shortcuts minutes
// after logon (5 to 6 measured), a logon task starts right away. The shortcut is the fallback.
const AUTOSTART_TASK: &str = "GoXLR Desk";

// No console window flashes when PowerShell or schtasks run
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hidden_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn has_autostart_task() -> bool {
    hidden_command("schtasks")
        .args(["/Query", "/TN", AUTOSTART_TASK])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Registers the logon task for this user: no time limit, normal priority, one instance.
fn create_autostart_task() -> Result<()> {
    let executable = env::current_exe()?;
    let executable = dunce::simplified(&executable).to_path_buf();
    let directory = executable
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let quote = |value: &std::path::Path| value.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         $user = \"$env:USERDOMAIN\\$env:USERNAME\"; \
         $action = New-ScheduledTaskAction -Execute '{exe}' -WorkingDirectory '{dir}'; \
         $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user; \
         $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited; \
         $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -Priority 4 \
           -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew; \
         Register-ScheduledTask -TaskName '{task}' -Action $action -Trigger $trigger \
           -Principal $principal -Settings $settings -Description 'Starts GoXLR Desk at logon.' -Force | Out-Null",
        exe = quote(&executable),
        dir = quote(&directory),
        task = AUTOSTART_TASK,
    );
    let output = hidden_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()?;
    if !output.status.success() {
        bail!(
            "Unable to create the logon task: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn remove_autostart_task() {
    if has_autostart_task() {
        let _ = hidden_command("schtasks")
            .args(["/Delete", "/TN", AUTOSTART_TASK, "/F"])
            .output();
    }
}

pub fn has_autostart() -> bool {
    if let Some(path) = &*STARTUP_PATH
        && path.join(AUTOSTART_FILENAME).exists()
    {
        return true;
    }
    has_autostart_task()
}

/// Turns autostart on: the logon task, or the Startup shortcut when the task can't be made.
pub fn enable_autostart() -> Result<()> {
    match create_autostart_task() {
        Ok(()) => {
            // Only one way to start, or the Utility would be started twice
            remove_startup_link()
        }
        Err(e) => {
            warn!("{}, using the Startup folder instead", e);
            create_startup_link()
        }
    }
}

pub fn disable_autostart() -> Result<()> {
    remove_autostart_task();
    remove_startup_link()
}

/// Earlier versions used the Startup shortcut: move it to the logon task.
pub fn migrate_autostart() {
    let Some(path) = &*STARTUP_PATH else {
        return;
    };
    if path.join(AUTOSTART_FILENAME).exists() && !has_autostart_task() {
        match create_autostart_task() {
            Ok(()) => {
                info!("Autostart moved from the Startup folder to a logon task");
                let _ = remove_startup_link();
            }
            Err(e) => warn!("Keeping the Startup shortcut: {}", e),
        }
    }
}

pub fn remove_startup_link() -> Result<()> {
    if let Some(path) = &*STARTUP_PATH {
        let file = path.join(AUTOSTART_FILENAME);
        if !file.exists() {
            debug!("Attempted to remove link on non-existent file");
            return Ok(());
        }

        // Remove the file.
        fs::remove_file(file)?;
    }
    Ok(())
}

pub fn create_startup_link() -> Result<()> {
    if let Some(path) = &*STARTUP_PATH {
        let file = path.join(AUTOSTART_FILENAME);
        if file.exists() {
            // File already exists, we're done?
            return Ok(());
        }

        // Get our executable filename..
        let executable = env::current_exe()?;

        // Remove any UNC Prefix from the executable path when safe..
        let executable = dunce::simplified(&executable);

        // Create the Symlink to our current path..
        let link = ShellLink::new(executable)?;
        link.create_lnk(file)?;
        return Ok(());
    }
    bail!("Error Finding Startup Path, unable to create link");
}

fn get_startup_dir() -> Option<PathBuf> {
    let reg_path = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\User Shell Folders";

    // Get %USERPROFILE% from the ENV..
    if let Ok(profile) = env::var("USERPROFILE") {
        let local_user = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(folders) = local_user.open_subkey(reg_path)
            && let Ok(startup) = folders.get_value::<String, &str>("Startup")
        {
            let full_path = startup.replace("%USERPROFILE%", &profile);
            let path_buf = PathBuf::from(&full_path);

            if path_buf.exists() {
                debug!("Setting Startup Path: {:?}", path_buf);
                return Some(path_buf);
            }
        }
    }

    None
}

fn locate_goxlr_driver() -> bool {
    // HKLM only: HKEY_CLASSES_ROOT also reads the user's own classes (HKCU), which any program
    // run by the user can change to load another DLL here. And only a DLL under Program Files,
    // which needs an administrator to write.
    let regpath =
        "SOFTWARE\\Classes\\CLSID\\{024D0372-641F-4B7B-8140-F4DFE458C982}\\InprocServer32\\";
    let program_files: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    let trusted = |path: &PathBuf| {
        path.canonicalize()
            .is_ok_and(|real| program_files.iter().any(|base| real.starts_with(base)))
    };
    let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(folders) = machine.open_subkey(regpath)
        // Name is blank because we need the default key
        && let Ok(api) = folders.get_value::<String, &str>("")
    {
        let path = PathBuf::from(&api);
        if path.exists() && trusted(&path) {
            return true;
        }
    }
    // If we get here, we didn't find it, return a default and hope for the best!
    let path = String::from(
        "C:/Program Files/TC-HELICON/GoXLR_Audio_Driver/W10_x64/goxlr_audioapi_x64.dll",
    );
    if PathBuf::from(&path).exists() {
        return true;
    }
    false
}
