# agy-switch

Antigravity account quota monitor, auto-switcher, and TUI dashboard.

## Features

- **Real-time TUI dashboard** — monitor all account quotas at a glance
- **Sorted display** — accounts sorted by quota percentage (100% → 0%), A-Z within tiers
- **Auto-switch** — automatically switches to the next best account when one is exhausted or rate-limited
- **Rate limit detection** — detects HTTP 429 responses, marks accounts, shows red in TUI
- **Daemon mode** — runs in background, checks quotas every 10 seconds
- **JSON import/export** — manage accounts via JSON files
- **Clipboard import/export** — paste accounts JSON for quick review before importing
- **Remove account** — remove accounts directly from the TUI
- **OAuth login** — login via browser-based OAuth flow
- **Cross-platform** — Windows x64, Windows ARM64, Linux x64, Linux ARM64, macOS ARM64

## Install

Download the latest binary for your platform from [Releases](https://github.com/xyzyt010/agy-switch/releases).

### Windows

```powershell
# x64 (Intel/AMD)
iwr -Uri "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-windows-x64.exe" -OutFile "$env:LOCALAPPDATA\agy\bin\agy-switch.exe"

# ARM64 (Surface Pro X, Snapdragon, etc.)
iwr -Uri "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-windows-arm64.exe" -OutFile "$env:LOCALAPPDATA\agy\bin\agy-switch.exe"

# Add to PATH (run once)
$currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
[Environment]::SetEnvironmentVariable("PATH", "$currentPath;$env:LOCALAPPDATA\agy\bin", "User")
```

### Linux (any distro — static musl binary, no dependencies)

```bash
# x64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-linux-x64"
chmod +x agy-switch-linux-x64
sudo mv agy-switch-linux-x64 /usr/local/bin/agy-switch

# ARM64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-linux-arm64"
chmod +x agy-switch-linux-arm64
sudo mv agy-switch-linux-arm64 /usr/local/bin/agy-switch
```

### macOS (Apple Silicon)

```bash
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-macos-arm64"
chmod +x agy-switch-macos-arm64
sudo mv agy-switch-macos-arm64 /usr/local/bin/agy-switch
```

## Update

Re-run the same install command — it downloads the latest release and overwrites the old binary.

## Usage

```bash
agy-switch              # Show TUI or prompt to start daemon
agy-switch on           # Start daemon + TUI
agy-switch off          # Stop daemon + exit
agy-switch restart      # Restart daemon
```

## Building from Source

```bash
git clone https://github.com/xyzyt010/agy-switch.git
cd agy-switch
cargo build --release
```

## Platforms

| Platform | Binary | Notes |
|---|---|---|
| Windows x64 | `agy-switch-windows-x64.exe` | Native MSVC build |
| Windows ARM64 | `agy-switch-windows-arm64.exe` | Surface Pro X, Snapdragon |
| Linux x64 | `agy-switch-linux-x64` | Static musl binary, no dependencies |
| Linux ARM64 | `agy-switch-linux-arm64` | Static musl binary, no dependencies |
| macOS ARM64 | `agy-switch-macos-arm64` | Apple Silicon (M1/M2/M3/M4) |

## License

MIT
