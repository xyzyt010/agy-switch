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
                if !t.is_empty() {
                    return Ok(t);
                }
            }
        }
    }

    // CLI fallback — detect display server and use correct tools
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let is_x11 = std::env::var("DISPLAY").is_ok();

    // Order depends on display server
    if is_wayland {
        // Wayland: try wl-paste first, then xclip (via XWayland compatibility)
        let tools: Vec<&[&str]> = vec![
            &["wl-paste"],
            &["wl-paste", "-t", "text"],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
            &["pbpaste"],
        ];
        if let Some(text) = try_tools_get(&tools) {
            return Ok(text);
        }
    } else if is_x11 {
        // X11: try xclip/xsel first
        let tools: Vec<&[&str]> = vec![
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
            &["pbpaste"],
        ];
        if let Some(text) = try_tools_get(&tools) {
            return Ok(text);
        }
    } else {
        // No display detected — try all tools anyway (might work over SSH forwarding)
        let tools: Vec<&[&str]> = vec![
            &["pbpaste"],
            &["wl-paste"],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
        ];
        if let Some(text) = try_tools_get(&tools) {
            return Ok(text);
        }
    }

    Err("No clipboard tool available. Install xclip, xsel, or wl-clipboard.".to_string())
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
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let is_x11 = std::env::var("DISPLAY").is_ok();

    if is_wayland {
        let tools: Vec<&[&str]> = vec![
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
            &["pbcopy"],
        ];
        if try_tools_set(&tools, text) {
            return Ok(());
        }
    } else if is_x11 {
        let tools: Vec<&[&str]> = vec![
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
            &["pbcopy"],
        ];
        if try_tools_set(&tools, text) {
            return Ok(());
        }
    } else {
        let tools: Vec<&[&str]> = vec![
            &["pbcopy"],
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ];
        if try_tools_set(&tools, text) {
            return Ok(());
        }
    }

    Err("No clipboard tool available. Install xclip, xsel, or wl-clipboard.".to_string())
}

fn try_tools_get(tools: &[&[&str]]) -> Option<String> {
    for args in tools {
        let program = args[0];
        if !command_exists(program) {
            continue;
        }
        match Command::new(program)
            .args(&args[1..])
            .output()
        {
            Ok(out) if out.status.success() => {
                match String::from_utf8(out.stdout) {
                    Ok(t) if !t.trim().is_empty() => return Some(t),
                    _ => continue,
                }
            }
            _ => continue,
        }
    }
    None
}

fn try_tools_set(tools: &[&[&str]], text: &str) -> bool {
    for args in tools {
        let program = args[0];
        if !command_exists(program) {
            continue;
        }
        if let Ok(mut child) = Command::new(program)
            .args(&args[1..])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn command_exists(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", program))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
