#!/usr/bin/env bash
# SysPulse Installer
# Builds and installs syspulse so it can be run directly from any terminal.

set -euo pipefail

BOLD="\033[1m"
GREEN="\033[32m"
CYAN="\033[36m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}=== SysPulse Installer ===${RESET}"

# Determine repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

# Check for Rust / Cargo toolchain
if ! command -v cargo &>/dev/null; then
    echo -e "${RED}[ERROR] Cargo (Rust) is not installed or not in PATH.${RESET}"
    echo -e "Please install Rust by running:"
    echo -e "  ${BOLD}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${RESET}"
    exit 1
fi

echo -e "${CYAN}[*] Building syspulse in release mode...${RESET}"
cargo build --release

# Determine installation destination
if [ -n "${1:-}" ]; then
    if [ "$1" = "--system" ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="$1"
    fi
elif [ -d "${HOME}/.cargo/bin" ] && [ -w "${HOME}/.cargo/bin" ]; then
    INSTALL_DIR="${HOME}/.cargo/bin"
elif mkdir -p "${HOME}/.local/bin" 2>/dev/null; then
    INSTALL_DIR="${HOME}/.local/bin"
elif [ -d "${HOME}/.local/bin" ] && [ -w "${HOME}/.local/bin" ]; then
    INSTALL_DIR="${HOME}/.local/bin"
else
    INSTALL_DIR="/usr/local/bin"
fi

if [ ! -w "${INSTALL_DIR}" ] && [ "${EUID:-$(id -u)}" -ne 0 ]; then
    echo -e "${CYAN}[*] Installing to ${BOLD}${INSTALL_DIR}${RESET} requires superuser privileges.${RESET}"
    sudo cp -f target/release/syspulse "${INSTALL_DIR}/syspulse"
    sudo chmod +x "${INSTALL_DIR}/syspulse"
else
    mkdir -p "${INSTALL_DIR}" 2>/dev/null || true
    cp -f target/release/syspulse "${INSTALL_DIR}/syspulse"
    chmod +x "${INSTALL_DIR}/syspulse"
fi

# Verify binary execution
if ! "${INSTALL_DIR}/syspulse" --version &>/dev/null; then
    echo -e "${RED}[ERROR] Installed binary verification failed.${RESET}"
    exit 1
fi

VERSION=$("${INSTALL_DIR}/syspulse" --version)
echo -e "${GREEN}[✔] Successfully installed ${BOLD}${VERSION}${RESET}${GREEN} to ${BOLD}${INSTALL_DIR}/syspulse${RESET}!"

# Ensure INSTALL_DIR is in PATH
PATH_UPDATED=0
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    EXPORT_CMD="export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo -e "${YELLOW}[!] ${INSTALL_DIR} is not currently in your PATH.${RESET}"
    
    # Identify user's shell configuration file
    SHELL_NAME="$(basename "${SHELL:-bash}")"
    TARGET_RC=""
    if [ "${SHELL_NAME}" = "zsh" ] && [ -f "${HOME}/.zshrc" ]; then
        TARGET_RC="${HOME}/.zshrc"
    elif [ -f "${HOME}/.bashrc" ]; then
        TARGET_RC="${HOME}/.bashrc"
    elif [ -f "${HOME}/.profile" ]; then
        TARGET_RC="${HOME}/.profile"
    fi

    if [ -n "${TARGET_RC}" ]; then
        if ! grep -qs "${INSTALL_DIR}" "${TARGET_RC}"; then
            echo "" >> "${TARGET_RC}"
            echo "# Added by SysPulse installer" >> "${TARGET_RC}"
            echo "${EXPORT_CMD}" >> "${TARGET_RC}"
            echo -e "${GREEN}[✔] Added ${INSTALL_DIR} to ${TARGET_RC}.${RESET}"
            PATH_UPDATED=1
        fi
    fi
fi

echo ""
echo -e "${BOLD}${GREEN}SysPulse is ready to use!${RESET}"
if [ ${PATH_UPDATED} -eq 1 ]; then
    echo -e "${YELLOW}Please reload your shell or run:${RESET}"
    echo -e "  ${BOLD}source ${TARGET_RC}${RESET}"
fi
echo -e "To launch SysPulse, simply run:"
echo -e "  ${BOLD}${CYAN}syspulse${RESET}"
echo ""
