use std::process::Command;

/// Cross-platform clipboard abstraction.
/// Tries `arboard` first, then falls back to CLI tools on Linux.

pub fn get_text() -> Result<String, String> {
    // Try arboard first
    #[cfg(feature = "arboard")]
    {
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.get_text() {
                Ok(t) => return Ok(t),
                Err(_) => {} // fall through to CLI
            },
            Err(_) => {} // fall through to CLI
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
    // Try tools in order: xclip, xsel, wl-paste, pbpaste (macOS)
    let tools: Vec<(&[&str], &[&str])> = vec![
        (&["xclip", "-selection", "clipboard", "-o"], &[]),
        (&["xsel", "--clipboard", "--output"], &[]),
        (&["wl-paste"], &[]),
        (&["pbpaste"], &[]), // macOS fallback
    ];

    for (args, _env) in &tools {
        let program = args[0];
        if which_exists(program) {
            match Command::new(program).args(&args[1..]).output() {
                Ok(out) if out.status.success() => {
                    return String::from_utf8(out.stdout)
                        .map_err(|e| format!("UTF-8 error: {}", e));
                }
                _ => continue,
            }
        }
    }

    Err("No clipboard tool available. Install xclip, xsel, or wl-clipboard.".to_string())
}

fn cli_set(text: &str) -> Result<(), String> {
    let tools: Vec<&[&str]> = vec![
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
        &["wl-copy"],
        &["pbcopy"], // macOS fallback
    ];

    for args in &tools {
        let program = args[0];
        if which_exists(program) {
            match Command::new(program)
                .args(&args[1..])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(ref mut stdin) = child.stdin {
                        use std::io::Write;
                        stdin.write_all(text.as_bytes()).map_err(|e| format!("Write error: {}", e))?;
                    }
                    child.wait().map_err(|e| format!("Wait error: {}", e))?;
                    return Ok(());
                }
                _ => continue,
            }
        }
    }

    Err("No clipboard tool available. Install xclip, xsel, or wl-clipboard.".to_string())
}

fn which_exists(program: &str) -> bool {
    // Use 'command -v' which is POSIX and available on all Linux distros
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", program))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
