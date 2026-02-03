# Arctis Nova 7 ChatMix

Lightweight helper that creates two PipeWire virtual sinks (Arctis_Game and Arctis_Chat), links them to your SteelSeries Arctis Nova 7 dongle, and exposes the headset's hardware ChatMix HID controls to set volumes for each virtual sink. The program watches for the dongle being unplugged and will automatically reconnect, relink the virtual sinks and move existing audio streams so playback continues without restarting apps.

This repository contains:
- Rust implementation of the controller (`src/main.rs`)
- Convenience installer script (`install.sh`) that can install as a per-user systemd service or system-wide service and optionally install a udev rule
- Packaging helpers for Arch/AUR (optional)

Supported (tested) environment:
- Linux with PipeWire (pactl, pw-link / pw-cli available)
- libusb for HID reads
- The SteelSeries Arctis Nova 7 dongle (vendor: 0x1038)
  - Arctis Nova 7 (0x2202)
  - Arctis Nova 7 Gen 2 (Feb 2026 update) (0x22A1)
  - Arctis Nova 7 Wireless Gen 2 (0x227e)
  - Arctis Nova 7x (0x2206)
  - Arctis Nova 7x v2 (0x2258, 0x229e)
  - Arctis Nova 7 Diablo IV (0x223a, 0x22a9)
  - Arctis Nova 7 WoW Edition (0x227a)

Features
- Creates two virtual sinks:
  - Arctis_Game — intended for game audio
  - Arctis_Chat — intended for voice/chat audio
- Links virtual sinks to the physical headset playback ports (pw-link)
- Reads the dongle HID ChatMix reports and maps the physical Game/Chat knob values to the two virtual sinks
- Detects unplug/replug and:
  - reclaims the USB interface (tries libusb auto-detach and manual detach)
  - relinks virtual sinks to the current physical device node
  - sets Arctis_Game as the default sink
  - moves existing sink-inputs (clients) to Arctis_Game so audio continues without restarting applications
- Clean shutdown sets the original default sink back and destroys the virtual sink nodes

## Installation

### Quick Start
1. Clone the repository:
   ```bash
   git clone https://github.com/m24ih/Arctis-Nova-7-Chatmix.git
   cd Arctis-Nova-7-Chatmix
   ```



2. Run the installer:
   ```bash
   chmod +x install.sh
   ./install.sh
   # Select the pre-built 'arctis_chatmix' binary when prompted (default)
   ```
   - The script will guide you through the process (user/system service, udev rules, etc.).

### Non-interactive example


- Per-user install:
  ./install.sh --binary ./arctis_chatmix --mode user --udev yes --enable-service yes --enable-linger no

- System install (requires sudo):
  sudo ./install.sh --binary ./arctis_chatmix --mode system --udev yes --enable-service yes

Files the installer writes
- User mode:
  - Binary -> ~/.local/bin/arctis_chatmix
  - Systemd unit -> ~/.config/systemd/user/arctis_chatmix.service
- System mode:
  - Binary -> /usr/local/bin/arctis_chatmix
  - Systemd unit -> /etc/systemd/system/arctis_chatmix.service
- Optional udev rule:
  - /etc/udev/rules.d/99-arctis.rules

udev rule (recommended for non-root installs)
The provided udev rule grants the active session user and audio group access to the dongle. After installing the rule:
- Ensure your user is in the `audio` group (sudo usermod -aG audio $USER) and re-login.
- Reload rules:
  sudo udevadm control --reload
  sudo udevadm trigger --subsystem-match=usb --attr-match=idVendor=1038 --attr-match=idProduct=2202

Running and logs
- Per-user service logs:
  journalctl --user -u arctis_chatmix.service -f
- System service logs:
  sudo journalctl -u arctis_chatmix.service -f

Troubleshooting
- Confirm PipeWire sees the physical sink:
  pactl list short sinks
- Confirm virtual sinks exist:
  pactl list short sinks | grep Arctis
- Confirm sink inputs (clients):
  pactl list short sink-inputs
- If clients don't hear audio after reconnect:
  - Check logs (journalctl)
  - Verify the installer's udev rule applied (ls -l /dev/hidraw* or ls -l /dev/bus/usb/*/*)
  - Manually set default sink and move clients:
    pactl set-default-sink Arctis_Game
    pactl move-sink-input <index> Arctis_Game

Security & permissions
- The process needs permission to access the USB device (via libusb). The udev rule + membership in `audio` is the recommended approach to avoid running the service as root.
- If detach/claim fails repeatedly, running as root will usually work, but it's less desirable for interacting with a user PipeWire session.



License
- This project is provided under the MIT license — see the included LICENSE file.

## Contributing

We welcome contributions! Here's how to get started:

1. **Fork** the repository on GitHub.
2. **Clone** your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/Arctis-Nova-7-Chatmix.git
   cd Arctis-Nova-7-Chatmix
   ```
3. Create a new **feature branch**:
   ```bash
   git checkout -b feature/amazing-feature
   ```
4. Make your changes and verify they build:
   ```bash
   cd source
   cargo build
   ```
5. Commit and push your changes:
   ```bash
   git add .
   git commit -m "feat: Add amazing feature"
   git push origin feature/amazing-feature
   ```
6. Open a **Pull Request** on GitHub against the `master` branch.
