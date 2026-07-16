use crate::config::app_config_dir;
use crate::error::AgySwitchError;
use std::path::PathBuf;
use std::process::Command;

/// Path to the daemon PID file
pub fn daemon_pid_path() -> PathBuf {
    app_config_dir().join("daemon.pid")
}

/// Path to the stop signal file
pub fn stop_signal_path() -> PathBuf {
    app_config_dir().join("stop.signal")
}

/// Check if a process with the given PID is alive
fn is_process_alive(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::io::RawHandle;
        unsafe extern "system" {
            fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> RawHandle;
            fn CloseHandle(hObject: RawHandle) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let _ = CloseHandle(handle);
            true
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

/// Check if the daemon is currently running
pub async fn is_daemon_running() -> bool {
    let pid_path = daemon_pid_path();
    if !pid_path.exists() {
        return false;
    }
    if let Ok(contents) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            return is_process_alive(pid);
        }
    }
    false
}

/// Spawn the daemon process in the background
pub fn spawn_daemon() -> Result<(), AgySwitchError> {
    let pid_path = daemon_pid_path();
    let stop_path = stop_signal_path();

    // Remove any existing stop signal
    if stop_path.exists() {
        let _ = std::fs::remove_file(&stop_path);
    }

    // Check if daemon is already running
    if pid_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if is_process_alive(pid) {
                    return Ok(()); // Already running, treat as success
                }
                // Stale PID file
                let _ = std::fs::remove_file(&pid_path);
            }
        }
    }

    // Spawn the daemon process
    let current_exe = std::env::current_exe().map_err(AgySwitchError::Io)?;

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let log_path = app_config_dir().join("daemon.log");
        let log_file = std::fs::File::create(&log_path).map_err(AgySwitchError::Io)?;
        let child = Command::new(current_exe)
            .arg("__daemon")
            .creation_flags(CREATE_NO_WINDOW)
            .stderr(std::process::Stdio::from(log_file))
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(AgySwitchError::Io)?;
        std::fs::write(&pid_path, child.id().to_string()).map_err(AgySwitchError::Io)?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Double-fork + setsid to fully detach from parent terminal.
        // Without this, the daemon dies when the TUI closes (SIGHUP from terminal).
        unsafe {
            let pid = libc::fork();
            if pid < 0 {
                return Err(AgySwitchError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "fork failed",
                )));
            }
            if pid > 0 {
                // Parent process: write child PID and exit
                std::fs::write(&pid_path, pid.to_string()).map_err(AgySwitchError::Io)?;
                return Ok(());
            }

            // First child: create new session (detach from terminal)
            libc::setsid();

            // Second fork: prevent the daemon from acquiring a controlling terminal
            let pid2 = libc::fork();
            if pid2 < 0 {
                libc::_exit(1);
            }
            if pid2 > 0 {
                // First child exits, grandchild continues as daemon
                libc::_exit(0);
            }

            // Grandchild (daemon): redirect stdio to /dev/null
            let devnull = b"/dev/null\0".as_ptr() as *const libc::c_char;
            libc::close(libc::STDIN_FILENO);
            libc::close(libc::STDOUT_FILENO);
            libc::close(libc::STDERR_FILENO);
            libc::open(devnull, libc::O_RDWR); // fd 0 = stdin
            libc::dup(0); // fd 1 = stdout
            libc::dup(0); // fd 2 = stderr

            // Write our own PID
            let self_pid = libc::getpid();
            let _ = std::fs::write(&pid_path, self_pid.to_string());

            // Set working directory to root
            let _ = std::env::set_current_dir("/");

            // Exec the daemon binary
            use std::os::unix::process::CommandExt;
            Command::new(current_exe)
                .arg("__daemon")
                .exec();

            // exec failed
            libc::_exit(1);
        }
    }

    Ok(())
}

/// Stop the daemon process
pub fn stop_daemon() -> Result<(), AgySwitchError> {
    let pid_path = daemon_pid_path();
    let stop_path = stop_signal_path();

    if !pid_path.exists() {
        return Ok(()); // Already stopped
    }

    let contents = std::fs::read_to_string(&pid_path).map_err(AgySwitchError::Io)?;
    let pid = contents.trim().parse::<u32>().map_err(|e| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid PID file: {}", e),
        ))
    })?;

    // Create stop signal file
    let _ = std::fs::write(&stop_path, "stop");

    // Give daemon a moment to exit gracefully
    std::thread::sleep(std::time::Duration::from_secs(2));

    // If daemon is still alive, force kill it
    if is_process_alive(pid) {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let _ = Command::new("taskkill")
                .args(&["/PID", &pid.to_string(), "/F"])
                .creation_flags(0x08000000)
                .output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            unsafe { libc::kill(pid as i32, libc::SIGTERM); }
            std::thread::sleep(std::time::Duration::from_secs(1));
            if is_process_alive(pid) {
                unsafe { libc::kill(pid as i32, libc::SIGKILL); }
            }
        }
    }

    // Clean up files
    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(&stop_path);

    Ok(())
}

/// Turn on: start daemon + launch TUI
pub async fn turn_on() -> Result<(), AgySwitchError> {
    // Set state.enabled = true so TUI shows "ON"
    let mut state = crate::config::load_state().await.unwrap_or_default();
    state.enabled = true;
    let _ = crate::config::save_state(&state).await;

    // Start daemon
    match spawn_daemon() {
        Ok(()) => {
            eprintln!("[AGY-SWITCH] Daemon started");
        }
        Err(e) => {
            eprintln!("[AGY-SWITCH] Failed to start daemon: {}", e);
            return Err(e);
        }
    }

    // Launch TUI
    crate::commands::dashboard::run_dashboard().await
}

/// Turn off: stop daemon + exit
pub async fn turn_off() -> Result<(), AgySwitchError> {
    // Set state.enabled = false
    let mut state = crate::config::load_state().await.unwrap_or_default();
    state.enabled = false;
    let _ = crate::config::save_state(&state).await;

    match stop_daemon() {
        Ok(()) => {
            println!("[AGY-SWITCH] Stopped");
        }
        Err(e) => {
            eprintln!("[AGY-SWITCH] Error stopping daemon: {}", e);
            return Err(e);
        }
    }
    Ok(())
}
