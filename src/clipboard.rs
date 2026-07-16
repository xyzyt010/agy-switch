use std::process::Command;

/// Cross-platform clipboard abstraction.
/// Tries `arboard` first, then falls back to CLI tools on Linux.
/// Detects X11 vs Wayland and uses the correct tools.

pub fn get_text() -> Result<String, String> {
    // Try arboard first (works on Windows, macOS, and Linux with libs)
    #[cfg(feature = "arboard")]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(t) = cb.get_text() {
                if !t.trim().is_empty() {
                    return Ok(t);
                }
            }
        }
    }

    // CLI fallback
    cli_get()
}

pub fn set_text(text: &str) -> Result<(), String> {
    // Try arboard first
    #[cfg(feature = "arboard")]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if cb.set_text(text).is_ok() {
                return Ok(());
            }
        }
    }

    // CLI fallback
    cli_set(text)
}

fn cli_get() -> Result<String, String> {
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let is_x11 = std::env::var("DISPLAY").is_ok();

    let mut tried = Vec::new();

    if is_wayland {
        // Wayland: wl-paste first, then xclip via XWayland
        for args in [
            &["wl-paste"] as &[&str],
            &["wl-paste", "-t", "text"],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
        ] {
            tried.push(args[0].to_string());
            if let Some(text) = try_get(args) {
                return Ok(text);
            }
        }
    } else if is_x11 {
        // X11: xclip/xsel first
        for args in [
            &["xclip", "-selection", "clipboard", "-o"] as &[&str],
            &["xsel", "--clipboard", "--output"],
        ] {
            tried.push(args[0].to_string());
            if let Some(text) = try_get(args) {
                return Ok(text);
            }
        }
    } else {
        // No display — try all
        for args in [
            &["wl-paste"] as &[&str],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
        ] {
            tried.push(args[0].to_string());
            if let Some(text) = try_get(args) {
                return Ok(text);
            }
        }
    }

    let display_type = if is_wayland {
        "Wayland"
    } else if is_x11 {
        "X11"
    } else {
        "none"
    };
    Err(format!(
        "Clipboard not available (display: {}). Tried: {}. Install wl-clipboard (Wayland) or xclip/xsel (X11).",
        display_type,
        tried.join(", ")
    ))
}

fn cli_set(text: &str) -> Result<(), String> {
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let is_x11 = std::env::var("DISPLAY").is_ok();

    let mut tried = Vec::new();

    if is_wayland {
        for args in [
            &["wl-copy"] as &[&str],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ] {
            tried.push(args[0].to_string());
            if try_set(args, text) {
                return Ok(());
            }
        }
    } else if is_x11 {
        for args in [
            &["xclip", "-selection", "clipboard"] as &[&str],
            &["xsel", "--clipboard", "--input"],
        ] {
            tried.push(args[0].to_string());
            if try_set(args, text) {
                return Ok(());
            }
        }
    } else {
        for args in [
            &["wl-copy"] as &[&str],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ] {
            tried.push(args[0].to_string());
            if try_set(args, text) {
                return Ok(());
            }
        }
    }

    let display_type = if is_wayland {
        "Wayland"
    } else if is_x11 {
        "X11"
    } else {
        "none"
    };
    Err(format!(
        "Clipboard not available (display: {}). Tried: {}. Install wl-clipboard (Wayland) or xclip/xsel (X11).",
        display_type,
        tried.join(", ")
    ))
}

fn try_get(args: &[&str]) -> Option<String> {
    let program = args[0];
    if !command_exists(program) {
        return None;
    }
    let output = Command::new(program)
        .args(&args[1..])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

fn try_set(args: &[&str], text: &str) -> bool {
    let program = args[0];
    if !command_exists(program) {
        return false;
    }
    let mut child = match Command::new(program)
        .args(&args[1..])
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(ref mut stdin) = child.stdin {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn command_exists(program: &str) -> bool {
    // Try 'command -v' (POSIX), fall back to 'which'
    if Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} 2>/dev/null", program))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }
    Command::new("which")
        .arg(program)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
