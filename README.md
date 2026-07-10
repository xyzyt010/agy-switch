# agy-switch

Antigravity account quota monitor, auto-switcher, and TUI dashboard.

## Features

- **Real-time TUI dashboard** — monitor all account quotas at a glance
- **Sorted display** — accounts sorted by quota percentage (100% → 0%), A-Z within tiers
- **Auto-switch** — automatically switches to the next best account when one is exhausted or rate-limited
- **Rate limit detection** — detects HTTP 429 responses, marks accounts, shows red in TUI
- **Daemon mode** — runs in background, checks quotas every 10 seconds
- **JSON import/export** — manage accounts via JSON files
- **OAuth login** — login via browser-based OAuth flow
- **Cross-platform** — Windows, Linux, macOS

## Install

### Windows

```powershell
# x64 (Intel/AMD)
iwr -Uri "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-windows-x64.exe" -OutFile "$env:LOCALAPPDATA\bin\agy-switch.exe"

# ARM64 (Surface Pro X, Snapdragon, etc.)
iwr -Uri "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-windows-arm64.exe" -OutFile "$env:LOCALAPPDATA\bin\agy-switch.exe"
```

Or download manually from [Releases](https://github.com/xyzyt010/agy-switch/releases).

### Linux (Debian/Ubuntu)

```bash
# ARM64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch_0.1.0_arm64.deb"
sudo dpkg -i agy-switch_0.1.0_arm64.deb

# x64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch_0.1.0_amd64.deb"
sudo dpkg -i agy-switch_0.1.0_amd64.deb
```

### Linux (Fedora/RHEL)

```bash
# ARM64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-0.1.0-1.aarch64.rpm"
sudo rpm -i agy-switch-0.1.0-1.aarch64.rpm

# x64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-0.1.0-1.x86_64.rpm"
sudo rpm -i agy-switch-0.1.0-1.x86_64.rpm
```

### Linux (raw binary — any distro with glibc ≥ 2.28)

```bash
# ARM64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-linux-arm64"
chmod +x agy-switch-linux-arm64
sudo mv agy-switch-linux-arm64 /usr/local/bin/agy-switch

# x64
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-linux-x64"
chmod +x agy-switch-linux-x64
sudo mv agy-switch-linux-x64 /usr/local/bin/agy-switch
```

### macOS (Apple Silicon)

```bash
curl -LO "https://github.com/xyzyt010/agy-switch/releases/latest/download/agy-switch-macos-arm64"
chmod +x agy-switch-macos-arm64
sudo mv agy-switch-macos-arm64 /usr/local/bin/agy-switch
```

## Usage

```bash
agy-switch              # Show TUI or prompt to start daemon
agy-switch on           # Start daemon + TUI
agy-switch off          # Stop daemon + exit
agy-switch restart      # Restart daemon
```

## Dependencies (Linux)

Linux binaries require GTK3 and Wayland client libraries:

```bash
# Debian/Ubuntu
sudo apt install libgtk-3-0 libwayland-client0

# Fedora
sudo dnf install gtk3 libwayland-client
```

## Building from Source

```bash
git clone https://github.com/xyzyt010/agy-switch.git
cd agy-switch
cargo build --release
```

## License

MIT
