#!/usr/bin/env bash
# Interactive installer for arctis_chatmix
# Usage:
#   ./install.sh             -> interactive mode
#   ./install.sh --binary ./arctis_chatmix --mode user --udev yes --enable-service yes
set -euo pipefail

# Defaults
BINARY="./arctis_chatmix"
MODE="user"          # user or system
INSTALL_UDEV="yes"   # yes/no
ENABLE_SERVICE="yes" # yes/no
ENABLE_LINGER="no"   # yes/no (only relevant for user mode)

print_help() {
  cat <<'USAGE'
Usage: install.sh [OPTIONS]

Interactive installer for arctis_chatmix.

Options (non-interactive):
  --binary PATH           Path to the arctis_chatmix binary (default: ./arctis_chatmix)
  --mode user|system      Install as a per-user service (default) or system-wide service
  --udev yes|no           Install udev rule (default: yes)
  --enable-service yes|no Enable and start the service immediately (default: yes)
  --enable-linger yes|no  Enable systemd linger for the user (only relevant for --mode user; default: no)
  -h, --help              Show this help and exit

Examples:
  # Interactive:
  ./install.sh

  # Non-interactive:
  ./install.sh --binary ./arctis_chatmix --mode user --udev yes --enable-service yes
USAGE
}

# Parse args (simple parser)
while [[ $# -gt 0 ]]; do
  case "$1" in
  --binary)
    shift
    BINARY="$1"
    shift
    ;;
  --mode)
    shift
    MODE="$1"
    shift
    ;;
  --udev)
    shift
    INSTALL_UDEV="$1"
    shift
    ;;
  --enable-service)
    shift
    ENABLE_SERVICE="$1"
    shift
    ;;
  --enable-linger)
    shift
    ENABLE_LINGER="$1"
    shift
    ;;
  -h | --help)
    print_help
    exit 0
    ;;
  *)
    echo "Unknown argument: $1"
    print_help
    exit 2
    ;;
  esac
done

# helper: yes/no prompt with default
ask_yes_no() {
  local prompt="$1"
  local default="$2"
  local reply
  while true; do
    read -r -p "$prompt [$default] " reply || exit 1
    reply="${reply:-$default}"
    case "${reply,,}" in
    y | yes)
      echo "yes"
      return 0
      ;;
    n | no)
      echo "no"
      return 0
      ;;
    *) echo "Please answer yes or no (y/n)." ;;
    esac
  done
}

# If running fully non-interactive (no TTY) then rely on supplied flags only.
IS_TTY=1
if [[ ! -t 0 ]]; then
  IS_TTY=0
fi

# Interactive prompts (only when TTY)
if [[ $IS_TTY -eq 1 ]]; then
  echo "Arctis ChatMix installer (interactive)"
  read -r -p "Path to binary [${BINARY}]: " input_bin || exit 1
  BINARY="${input_bin:-$BINARY}"

  while [[ ! -f "$BINARY" ]]; do
    echo "Binary not found at '$BINARY'."
    read -r -p "Enter valid path to binary (or press Ctrl+C to abort): " BINARY || exit 1
  done
  echo "Using binary: $BINARY"

  read -r -p "Install mode - user or system [${MODE}]: " input_mode || exit 1
  MODE="${input_mode:-$MODE}"
  if [[ "$MODE" != "user" && "$MODE" != "system" ]]; then
    echo "Invalid mode. Choose 'user' or 'system'."
    exit 2
  fi

  INSTALL_UDEV=$(ask_yes_no "Install udev rule to allow non-root access to the dongle?" "$INSTALL_UDEV")
  ENABLE_SERVICE=$(ask_yes_no "Enable & start the service now?" "$ENABLE_SERVICE")

  if [[ "$MODE" == "user" ]]; then
    ENABLE_LINGER=$(ask_yes_no "Enable lingering so service runs without active session? (loginctl enable-linger)" "$ENABLE_LINGER")
  fi
else
  # non-interactive: validate provided binary path
  if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: binary not found at '$BINARY' (non-interactive mode)."
    exit 2
  fi
fi

# paths
SYSTEM_BIN_DIR="/usr/local/bin"
USER_BIN_DIR="${HOME}/.local/bin"
SERVICE_NAME="arctis_chatmix.service"
USER_UNIT_DIR="${HOME}/.config/systemd/user"
SYSTEM_UNIT_DIR="/etc/systemd/system"
UDEV_RULE_PATH="/etc/udev/rules.d/99-arctis.rules"

install_user() {
  echo "Installing for current user..."

  mkdir -p "${USER_BIN_DIR}"
  cp -f "${BINARY}" "${USER_BIN_DIR}/arctis_chatmix"
  chmod 755 "${USER_BIN_DIR}/arctis_chatmix"
  echo "Binary installed to ${USER_BIN_DIR}/arctis_chatmix"

  mkdir -p "${USER_UNIT_DIR}"
  cat >"${USER_UNIT_DIR}/${SERVICE_NAME}" <<'UNIT'
[Unit]
Description=Arctis Nova 7 ChatMix (virtual-sink mixer)
Wants=pipewire.service
After=pipewire.service

[Service]
Type=simple
ExecStart=%h/.local/bin/arctis_chatmix
Environment=ARCTIS_SIDETONE_DISABLE=1
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
UNIT
  echo "User unit written to ${USER_UNIT_DIR}/${SERVICE_NAME}"

  # reload and enable user service if requested
  if [[ "${ENABLE_SERVICE}" == "yes" ]]; then
    if command -v systemctl >/dev/null 2>&1; then
      echo "Reloading user systemd daemon..."
      systemctl --user daemon-reload || echo "Warning: systemctl --user daemon-reload failed"
      echo "Enabling and starting user service..."
      if ! systemctl --user enable --now arctis_chatmix.service; then
        echo "Warning: Could not enable/start user service in this session."
        echo "You can enable it manually with: systemctl --user enable --now arctis_chatmix.service"
      fi
    else
      echo "Warning: systemctl not available. Please enable the user service manually."
    fi
  fi

  if [[ "${ENABLE_LINGER}" == "yes" ]]; then
    echo "Enabling linger for user (requires sudo)..."
    if [[ $EUID -ne 0 ]]; then
      sudo loginctl enable-linger "$(id -un)"
    else
      loginctl enable-linger "$(id -un)"
    fi
  fi

  echo "User install completed."
}

install_system() {
  echo "Installing system-wide (requires sudo)..."
  if [[ $EUID -ne 0 ]]; then
    sudo install -m 755 "${BINARY}" "${SYSTEM_BIN_DIR}/arctis_chatmix"
  else
    install -m 755 "${BINARY}" "${SYSTEM_BIN_DIR}/arctis_chatmix"
  fi
  echo "Binary installed to ${SYSTEM_BIN_DIR}/arctis_chatmix"

  # write systemd unit
  if [[ $EUID -ne 0 ]]; then
    sudo tee "${SYSTEM_UNIT_DIR}/${SERVICE_NAME}" >/dev/null <<'UNIT'
[Unit]
Description=Arctis Nova 7 ChatMix (virtual-sink mixer)
Wants=pipewire.service
After=pipewire.service

[Service]
Type=simple
ExecStart=/usr/local/bin/arctis_chatmix
Environment=ARCTIS_SIDETONE_DISABLE=1
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
    sudo systemctl daemon-reload
    if [[ "${ENABLE_SERVICE}" == "yes" ]]; then
      sudo systemctl enable --now arctis_chatmix.service
    fi
  else
    tee "${SYSTEM_UNIT_DIR}/${SERVICE_NAME}" >/dev/null <<'UNIT'
[Unit]
Description=Arctis Nova 7 ChatMix (virtual-sink mixer)
Wants=pipewire.service
After=pipewire.service

[Service]
Type=simple
ExecStart=/usr/local/bin/arctis_chatmix
Environment=ARCTIS_SIDETONE_DISABLE=1
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
    systemctl daemon-reload
    if [[ "${ENABLE_SERVICE}" == "yes" ]]; then
      systemctl enable --now arctis_chatmix.service
    fi
  fi

  echo "System-wide install completed."
}

install_udev() {
  if [[ "${INSTALL_UDEV}" != "yes" ]]; then
    echo "Skipping udev rule install (--udev no)"
    return
  fi

  echo "Installing udev rule (requires sudo)..."

  # Supported Product IDs
  PIDS=("2202" "22a1" "227e" "2206" "2258" "229e" "223a" "22a9" "227a")

  UDEV_CONTENT=""
  for pid in "${PIDS[@]}"; do
    UDEV_CONTENT+='ATTRS{idVendor}=="1038", ATTRS{idProduct}=="'"$pid"'", MODE="0660", GROUP="audio", TAG+="uaccess"
KERNEL=="hidraw*", ATTRS{idVendor}=="1038", ATTRS{idProduct}=="'"$pid"'", MODE="0660", GROUP="audio", TAG+="uaccess"
'
  done

  if [[ $EUID -ne 0 ]]; then
    echo "$UDEV_CONTENT" | sudo tee "${UDEV_RULE_PATH}" >/dev/null
    sudo udevadm control --reload
    for pid in "${PIDS[@]}"; do
        sudo udevadm trigger --subsystem-match=usb --attr-match=idVendor=1038 --attr-match=idProduct="$pid" || true
    done
  else
    echo "$UDEV_CONTENT" >"${UDEV_RULE_PATH}"
    udevadm control --reload
    for pid in "${PIDS[@]}"; do
        udevadm trigger --subsystem-match=usb --attr-match=idVendor=1038 --attr-match=idProduct="$pid" || true
    done
  fi

  echo "udev rule installed to ${UDEV_RULE_PATH}"
  echo "Make sure your user is in the 'audio' group (sudo usermod -aG audio <user>) and re-login."
}

echo "== arctis_chatmix installer =="
echo "Mode: ${MODE}"
echo "Binary: ${BINARY}"
echo "Install udev rule: ${INSTALL_UDEV}"
echo "Enable & start service now: ${ENABLE_SERVICE}"
if [[ "${MODE}" == "user" ]]; then
  echo "Enable linger: ${ENABLE_LINGER}"
fi

# Confirm (interactive)
if [[ $IS_TTY -eq 1 ]]; then
  CONFIRM=$(ask_yes_no "Proceed with installation?" "yes")
  if [[ "${CONFIRM}" != "yes" ]]; then
    echo "Aborting."
    exit 0
  fi
fi

# Run installs
if [[ "${MODE}" == "user" ]]; then
  install_user
else
  install_system
fi

install_udev

echo "Installation finished."
if [[ "${MODE}" == "user" ]]; then
  echo "Check user logs with: journalctl --user -u arctis_chatmix.service -f"
else
  echo "Check system logs with: sudo journalctl -u arctis_chatmix.service -f"
fi

echo "Done."
exit 0
