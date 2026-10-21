#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Result, bail};
use std::path::PathBuf;

use goxlr_ipc::client::Client;
use goxlr_ipc::clients::ipc::ipc_client::IPCClient;
use goxlr_ipc::clients::ipc::ipc_socket::Socket;
use goxlr_ipc::location::{NAMED_PIPE, socket_candidates, socket_path};
use goxlr_ipc::{DaemonCommand, DaemonRequest, DaemonResponse};
use interprocess::local_socket::tokio::prelude::LocalSocketStream;
use interprocess::local_socket::traits::tokio::Stream;
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ToFsName, ToNsName};
use which::which;

static DAEMON_NAME: &str = "goxlr-daemon";

#[tokio::main]
async fn main() -> Result<()> {
    // First thing to do, is check to see if the Daemon is running..
    if !daemon_answers().await {
        launch_daemon()?;
    }

    open_ui().await?;
    Ok(())
}

async fn get_connection() -> Result<LocalSocketStream> {
    // The named pipe on Windows, otherwise this user's socket, then the legacy shared one.
    if !cfg!(windows) {
        for candidate in socket_candidates() {
            if let Ok(name) = candidate.to_fs_name::<GenericFilePath>()
                && let Ok(connection) = LocalSocketStream::connect(name).await
            {
                return Ok(connection);
            }
        }
    }
    let path = if cfg!(windows) {
        NAMED_PIPE.to_ns_name::<GenericNamespaced>()
    } else {
        socket_path().to_fs_name::<GenericFilePath>()
    };

    let path = match path {
        Ok(path) => path,
        Err(e) => {
            bail!("Unable to Process Path {}", e);
        }
    };

    LocalSocketStream::connect(path)
        .await
        .map_err(anyhow::Error::msg)
}

#[cfg(unix)]
fn launch_daemon() -> Result<()> {
    use nix::unistd::execve;
    use std::env;
    use std::ffi::CString;

    if let Some(path) = locate_daemon_binary() {
        // Use execve to replace this process with the daemon..
        let c_path = CString::new(path.to_string_lossy().as_bytes())?;
        let c_daemon = CString::new(get_daemon_binary_name())?;
        let c_start_ui = CString::new("--start-ui")?;

        // TO-CONSIDER: Pass all env::args() through to the daemon?
        let c_params = vec![c_daemon, c_start_ui];

        // Copy all environment variables for this into the new process..
        let mut c_env = vec![];
        for (key, value) in env::vars() {
            c_env.push(CString::new(format!("{key}={value}"))?);
        }

        execve::<CString, CString>(&c_path, c_params.as_slice(), c_env.as_slice())?;
    }
    bail!("Unable to Locate GoXLR Daemon Binary");
}

/// Whether this user's daemon is running: it answers on its socket. (Looking for a process of
/// that name would also find another user's daemon.)
async fn daemon_answers() -> bool {
    let Ok(connection) = get_connection().await else {
        return false;
    };
    let mut socket: Socket<DaemonResponse, DaemonRequest> = Socket::new(connection);
    if socket.send(DaemonRequest::Ping).await.is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), socket.read()).await,
        Ok(Some(Ok(_)))
    )
}

#[cfg(windows)]
fn launch_daemon() -> Result<()> {
    use std::process::{Command, Stdio, exit};

    // Ok, try a simple spawn and exit..
    if let Some(path) = locate_daemon_binary() {
        let mut command = Command::new(&path);
        command.arg("--start-ui");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());

        if let Some(parent) = path.parent() {
            command.current_dir(parent);
        }

        command.spawn().expect("Unable to Launch Child Process");
        exit(0);
    }

    bail!("Unable to Locate GoXLR Daemon Binary");
}

async fn open_ui() -> Result<()> {
    // We kinda have to hope for the best here..
    let mut usable_connection = None;

    if let Ok(connection) = get_connection().await {
        usable_connection.replace(connection);
    }

    if let Some(connection) = usable_connection {
        let socket: Socket<DaemonResponse, DaemonRequest> = Socket::new(connection);
        let mut client = IPCClient::new(socket);
        client
            .send(DaemonRequest::Daemon(DaemonCommand::Activate))
            .await?;
        return Ok(());
    }
    bail!("Unable to make a connection with the Daemon");
}

fn locate_daemon_binary() -> Option<PathBuf> {
    let mut binary_path = None;
    let bin_name = get_daemon_binary_name();

    // Next to this program, then on $PATH. Not the current folder: starting the launcher from
    // Downloads or a clone would run whatever daemon sits there.
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let bin = parent.join(bin_name.clone());
        if bin.exists() {
            binary_path.replace(bin);
        }
    }

    if binary_path.is_none() {
        // Try and locate the binary on $PATH
        if let Ok(path) = which(bin_name) {
            binary_path.replace(path);
        }
    }

    binary_path
}

fn get_daemon_binary_name() -> String {
    if cfg!(windows) {
        format!("{DAEMON_NAME}.exe")
    } else {
        String::from(DAEMON_NAME)
    }
}
