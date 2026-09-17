#!/usr/bin/env bash
# Presentation helpers shared by scripts/install.sh and scripts/uninstall.sh.
#
# Sourced, never executed. It exists so both scripts report the same way: a
# banner, aligned `key  value` rows, and `✓ / ⚠ / ✗` result lines with a
# `→ fix` line under them — the vocabulary `andler doctor` and the README use.
#
# Degrades by itself: piped output (or NO_COLOR, or TERM=dumb) loses colour,
# and a non-UTF-8 locale falls back to ASCII glyphs, so the scripts stay
# readable in a log file and on a bare console.
#
# Callers must `set -euo pipefail` before sourcing; every function here is
# written to be safe under it (`[[ … ]]` inside `if`, no failing bare commands,
# arithmetic that never evaluates to a failing status).

UI_BOLD=""
UI_DIM=""
UI_RED=""
UI_GREEN=""
UI_YELLOW=""
UI_CYAN=""
UI_RESET=""
UI_UNICODE=0
UI_OK="[ok]"
UI_WARN="[!]"
UI_FAIL="[x]"
UI_ARROW="->"
UI_BULLET="*"
UI_POINT=">"

ui_init() {
    UI_UNICODE=0
    UI_OK="[ok]"
    UI_WARN="[!]"
    UI_FAIL="[x]"
    UI_ARROW="->"
    UI_BULLET="*"
    UI_POINT=">"

    if [[ -z "${NO_COLOR:-}" && "${TERM:-}" != "dumb" ]] && { [[ -t 1 ]] || [[ "${ANDLER_FORCE_COLOR:-}" == "1" ]]; }; then
        UI_BOLD=$'\033[1m'
        UI_DIM=$'\033[2m'
        UI_RED=$'\033[31m'
        UI_GREEN=$'\033[32m'
        UI_YELLOW=$'\033[33m'
        UI_CYAN=$'\033[36m'
        UI_RESET=$'\033[0m'
    fi

    local hint="${LC_ALL:-${LC_CTYPE:-${LANG:-}}}"
    if [[ "$hint" == *[Uu][Tt][Ff]* ]]; then
        UI_UNICODE=1
        UI_OK="✓"
        UI_WARN="⚠"
        UI_FAIL="✗"
        UI_ARROW="→"
        UI_BULLET="·"
        UI_POINT="▸"
    fi
}

# Frame width: the terminal's when there is one, clamped so a narrow window
# wraps nothing and a wide one does not stretch the box across the screen.
ui_width() {
    local cols="${COLUMNS:-0}"
    if [[ "$cols" -eq 0 ]]; then
        cols="$(tput cols 2>/dev/null || true)"
    fi
    if [[ -z "$cols" || "$cols" -eq 0 ]]; then
        cols=72
    fi
    if (( cols > 72 )); then
        cols=72
    fi
    if (( cols < 48 )); then
        cols=48
    fi
    printf '%s' "$cols"
}

ui_repeat() {
    local n="$1" ch="$2" out=""
    local _i
    for ((_i = 0; _i < n; _i++)); do
        out+="$ch"
    done
    printf '%s' "$out"
}

_ui_top() {
    local title="$1" inner="$2" fill
    fill=$(( inner - ${#title} - 3 ))
    (( fill < 0 )) && fill=0
    if (( UI_UNICODE )); then
        printf '%s\n' "${UI_DIM}╭─ ${UI_RESET}${UI_BOLD}${title}${UI_RESET}${UI_DIM} $(ui_repeat "$fill" '─')╮${UI_RESET}"
    else
        printf '+-- %s %s+\n' "$title" "$(ui_repeat "$fill" '-')"
    fi
}

_ui_bottom() {
    local inner="$1"
    if (( UI_UNICODE )); then
        printf '%s\n' "${UI_DIM}╰$(ui_repeat "$inner" '─')╯${UI_RESET}"
    else
        printf '+%s+\n' "$(ui_repeat "$inner" '-')"
    fi
}

_ui_row() {
    local text="$1" inner="$2" pad
    pad=$(( inner - ${#text} - 2 ))
    (( pad < 0 )) && pad=0
    if (( UI_UNICODE )); then
        printf '%s  %s%*s%s\n' "${UI_DIM}│${UI_RESET}" "$text" "$pad" '' "${UI_DIM}│${UI_RESET}"
    else
        printf '|  %s%*s|\n' "$text" "$pad" ''
    fi
}

ui_banner() {
    local width inner
    width="$(ui_width)"
    inner=$(( width - 2 ))
    _ui_top "ANDLER $1" "$inner"
    _ui_row "${2:-}" "$inner"
    _ui_bottom "$inner"
    printf '\n'
}

ui_panel() {
    local title="$1"
    shift
    local width inner line
    width="$(ui_width)"
    inner=$(( width - 2 ))
    _ui_top "$title" "$inner"
    for line in "$@"; do
        _ui_row "$line" "$inner"
    done
    _ui_bottom "$inner"
}

ui_section() {
    printf '\n%s%s %s%s\n' "$UI_BOLD$UI_CYAN" "$UI_POINT" "$1" "$UI_RESET"
}

ui_step() {
    printf '\n%s%s [%s/%s] %s%s\n' "$UI_BOLD$UI_CYAN" "$UI_POINT" "$1" "$2" "$3" "$UI_RESET"
}

_ui_result() {
    local color="$1" glyph="$2" name="$3" detail="${4:-}"
    if [[ -n "$detail" ]]; then
        printf '  %s%s%s %s%s%s %s %s%s\n' \
            "$color" "$glyph" "$UI_RESET" "$UI_BOLD" "$name" "$UI_RESET" "$UI_BULLET" "$detail" "$UI_RESET"
    else
        printf '  %s%s%s %s%s%s\n' "$color" "$glyph" "$UI_RESET" "$UI_BOLD" "$name" "$UI_RESET"
    fi
}

ui_ok() { _ui_result "$UI_GREEN" "$UI_OK" "$1" "${2:-}"; }
ui_warn() { _ui_result "$UI_YELLOW" "$UI_WARN" "$1" "${2:-}"; }
ui_fail() { _ui_result "$UI_RED" "$UI_FAIL" "$1" "${2:-}"; }

# The line under a ⚠ / ✗ result saying what to do about it.
ui_fix() {
    printf '      %s%s %s%s\n' "$UI_DIM" "$UI_ARROW" "$1" "$UI_RESET"
}

ui_note() {
    printf '  %s%s%s\n' "$UI_DIM" "$1" "$UI_RESET"
}

ui_kv() {
    printf '  %s%-11s%s %s\n' "$UI_DIM" "$1" "$UI_RESET" "$2"
}

ui_bullet() {
    printf '  %s%s%s %s\n' "$UI_DIM" "$UI_BULLET" "$UI_RESET" "$1"
}

ui_error() {
    printf '\n  %s%s%s %s%s%s\n' "$UI_RED" "$UI_FAIL" "$UI_RESET" "$UI_BOLD" "$1" "$UI_RESET" >&2
}

ui_hint() {
    printf '      %s%s %s%s\n' "$UI_DIM" "$UI_ARROW" "$1" "$UI_RESET" >&2
}
