#!/usr/bin/env bash

# Claude Code status line with one JSON parse and priority-based width adaptation.

CYAN="\033[36m"
BOLD_BLUE="\033[1;34m"
RED="\033[31m"
YELLOW="\033[33m"
ORANGE="\033[38;5;208m"
GREEN="\033[32m"
MAGENTA="\033[35m"
PURPLE="\033[38;5;141m"
DIM="\033[2m"
BOLD="\033[1m"
RESET="\033[0m"

input=$(cat)

# Reserve space on the right for Claude Code's notifications.
RIGHT_RESERVE=12
cols=${COLUMNS:-120}
case "$cols" in ''|*[!0-9]*) cols=120 ;; esac
BUDGET=$(( cols - RIGHT_RESERVE ))
[ "$BUDGET" -lt 10 ] && BUDGET=10

# Width cap for name segments (folder / git branch / worktree), proportional to
# the terminal so they can't dominate the whole line on wide terminals. Narrow
# terminals still degrade via each segment's fallback width ladder.
NAME_CAP=$(( BUDGET / 5 ))
[ "$NAME_CAP" -lt 10 ] && NAME_CAP=10
[ "$NAME_CAP" -gt 20 ] && NAME_CAP=20

SEP_TEXT=$' \xe2\x94\x82 '
SEP=$'\033[2m'"$SEP_TEXT"$'\033[0m'
SEP_WIDTH=3

# ---------------------------------------------------------------------------
# Single jq invocation — NUL-delimited fields preserve legitimate newlines in paths and model names.
# Raw paths stay available for filesystem operations; display-only fields are sanitized before they reach the terminal.
# ---------------------------------------------------------------------------
exec 3< <(printf '%s' "$input" | jq -j '
  def field(value): (value // "") | tostring | . + "\u0000";
  def display(value):
    (value // "")
    | tostring
    | gsub("\u001b\\[[0-?]*[ -/]*[@-~]"; "")
    | gsub("\u001b\\][^\u0007]*(\u0007|\u001b\\\\)"; "")
    | gsub("[\u0000-\u001f\u007f-\u009f]"; " ");
  field(.workspace.current_dir // .cwd),
  field(display(.workspace.current_dir // .cwd)),
  field(.workspace.project_dir),
  field(display(.workspace.project_dir)),
  field(.workspace.added_dirs | length),
  field(.session_id),
  field(display(.workspace.git_worktree)),
  field(display(.worktree.name)),
  field(display(.worktree.branch)),
  field(display(.worktree.original_branch)),
  field(.context_window.used_percentage),
  field(.context_window.context_window_size),
  field(.context_window.total_input_tokens),
  field(.context_window.total_output_tokens),
  field(.context_window.current_usage.input_tokens),
  field(.context_window.current_usage.cache_creation_input_tokens),
  field(.context_window.current_usage.cache_read_input_tokens),
  field(.exceeds_200k_tokens),
  field(.cost.total_cost_usd),
  field(display(.effort.level)),
  field(display(.model.display_name // .model.id)),
  field(.rate_limits.five_hour.used_percentage),
  field(.rate_limits.seven_day.used_percentage),
  field(display(.agent.name))
')

{
  IFS= read -r -d '' current_dir
  IFS= read -r -d '' display_current_dir
  IFS= read -r -d '' project_dir
  IFS= read -r -d '' display_project_dir
  IFS= read -r -d '' added_dirs_count
  IFS= read -r -d '' session_id
  IFS= read -r -d '' git_worktree_label
  IFS= read -r -d '' wt_name
  IFS= read -r -d '' wt_branch
  IFS= read -r -d '' wt_orig_branch
  IFS= read -r -d '' used_pct
  IFS= read -r -d '' ctx_size
  IFS= read -r -d '' ctx_in
  IFS= read -r -d '' ctx_out
  IFS= read -r -d '' cur_in
  IFS= read -r -d '' cur_ccw
  IFS= read -r -d '' cur_crd
  IFS= read -r -d '' exceeds_200k
  IFS= read -r -d '' total_cost
  IFS= read -r -d '' effort
  IFS= read -r -d '' model
  IFS= read -r -d '' rl_5h
  IFS= read -r -d '' rl_7d
  IFS= read -r -d '' agent_name
} <&3
exec 3<&-
added_dirs_count=${added_dirs_count:-0}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
format_tokens() {
    local n="$1"
    [ -z "$n" ] || [ "$n" = "null" ] && return
    if [ "$n" -ge 1000000 ]; then
        awk -v n="$n" 'BEGIN { printf "%.1fM", n/1000000 }'
    elif [ "$n" -ge 1000 ]; then
        printf "%dk" $(( n / 1000 ))
    else
        printf "%d" "$n"
    fi
}

pct_color() {
    local p="$1"
    if [ "$p" -ge 90 ]; then echo "$RED"
    elif [ "$p" -ge 70 ]; then echo "$YELLOW"
    else echo "$GREEN"
    fi
}

truncate_to() {
    local s="$1" max="$2"
    local len=${#s}
    if [ "$max" -le 0 ]; then
        return
    fi
    if [ "$len" -le "$max" ]; then
        printf '%s' "$s"
    elif [ "$max" -eq 1 ]; then
        printf '…'
    else
        printf '%s…' "${s:0:$((max-1))}"
    fi
}

byte_width() {
    local LC_ALL=C
    BYTE_WIDTH=${#1}
}

# basename via pure parameter expansion (no fork)
base_name() {
    local p="${1%/}"
    [ -z "$p" ] && { printf '/'; return; }
    printf '%s' "${p##*/}"
}

# ---------------------------------------------------------------------------
# Assembly state and primitives
# ---------------------------------------------------------------------------
output=""
running=0

try_add() {
    # args: visible_width rendered_text
    local vw="$1" rendered="$2" sep_w
    if [ -z "$output" ]; then sep_w=0; else sep_w=$SEP_WIDTH; fi
    if [ $((running + sep_w + vw)) -gt "$BUDGET" ]; then
        return 1
    fi
    [ "$sep_w" -gt 0 ] && output+="$SEP"
    output+="$rendered"
    running=$((running + sep_w + vw))
    return 0
}

force_add() {
    # used by must-show T1 segments when even the minimum doesn't fit
    local vw="$1" rendered="$2" sep_w
    if [ -z "$output" ]; then sep_w=0; else sep_w=$SEP_WIDTH; fi
    [ "$sep_w" -gt 0 ] && output+="$SEP"
    output+="$rendered"
    running=$((running + sep_w + vw))
}

# ---------------------------------------------------------------------------
# Directory & workspace
# ---------------------------------------------------------------------------
dir_basename=$(base_name "$display_current_dir")

dir_drifted=""
if [ -n "$project_dir" ] && [ "$current_dir" != "$project_dir" ]; then
    dir_drifted="≠$(base_name "$display_project_dir")"
fi

# ---------------------------------------------------------------------------
# Git (cached, 5s TTL, keyed on session_id)
# ---------------------------------------------------------------------------
git_branch=""
git_dirty=""

cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/nomad/claude"
(umask 077; mkdir -p "$cache_dir") || exit 1
case "$session_id" in ''|*[!a-zA-Z0-9_-]*) session_id=default ;; esac
cache_file="$cache_dir/git-${session_id}"
[ -L "$cache_file" ] && exit 1
cache_max_age=5

cache_is_stale() {
    [ ! -f "$cache_file" ] && return 0
    local cached_dir
    IFS= read -r -d '' cached_dir < "$cache_file"
    [ "$cached_dir" != "$current_dir" ] && return 0
    [ $(($(date +%s) - $(stat -c %Y "$cache_file" 2>/dev/null || stat -f %m "$cache_file" 2>/dev/null || echo 0))) -gt $cache_max_age ]
}

if cache_is_stale; then
    if git --no-optional-locks -C "$current_dir" rev-parse --git-dir > /dev/null 2>&1; then
        git_branch=$(git --no-optional-locks -C "$current_dir" symbolic-ref --short HEAD 2>/dev/null \
            || git --no-optional-locks -C "$current_dir" rev-parse --short HEAD 2>/dev/null)
        if [ -n "$(git --no-optional-locks -C "$current_dir" status --porcelain 2>/dev/null)" ]; then
            git_dirty="✗"
        fi
    fi
    printf '%s\0%s\0%s\0' "$current_dir" "$git_branch" "$git_dirty" > "$cache_file"
else
    { IFS= read -r -d '' _cached_dir; IFS= read -r -d '' git_branch; IFS= read -r -d '' git_dirty; } < "$cache_file"
fi

# ---------------------------------------------------------------------------
# Context window
# ---------------------------------------------------------------------------
used_pct_int=""
context_color=""
context_light=""
if [ -n "$used_pct" ]; then
    used_pct_int=$(printf "%.0f" "$used_pct")
    context_color=$(pct_color "$used_pct_int")
    # traffic-light glyph mirrors pct_color thresholds
    if [ "$used_pct_int" -ge 90 ]; then context_light="🔴"
    elif [ "$used_pct_int" -ge 70 ]; then context_light="🟡"
    else context_light="🟢"
    fi
fi

ctx_size_label=""
if [ -n "$ctx_size" ] && [ "$ctx_size" != "null" ]; then
    ctx_size_label="/$(format_tokens "$ctx_size")"
fi

token_label=""
if [ -n "$ctx_in" ] && [ "$ctx_in" != "null" ]; then
    in_fmt=$(format_tokens "$ctx_in")
    out_fmt=$(format_tokens "$ctx_out")
    token_label="${in_fmt}↑/${out_fmt}↓"
fi

cache_label=""
if [ -n "$cur_crd" ] && [ "$cur_crd" != "null" ]; then
    total_in=$(( ${cur_in:-0} + ${cur_ccw:-0} + ${cur_crd:-0} ))
    if [ "$total_in" -gt 0 ]; then
        hit=$(( cur_crd * 100 / total_in ))
        cache_label="cache:${hit}%"
    fi
fi

# ---------------------------------------------------------------------------
# Cost / duration
# ---------------------------------------------------------------------------
cost_label=""
cost_color=""
[ -z "$total_cost" ] && total_cost=0
if awk -v c="$total_cost" 'BEGIN { exit !(c > 0) }'; then
    cost=$(awk -v c="$total_cost" 'BEGIN { printf "%.4f", c }')
    cost_int=$(awk -v c="$total_cost" 'BEGIN { printf "%d", c * 100 }')
    if [ "$cost_int" -ge 50 ]; then cost_color="$RED"
    elif [ "$cost_int" -ge 10 ]; then cost_color="$ORANGE"
    else cost_color="$YELLOW"
    fi
    cost_label="$cost"
fi

# Rate limit segment — red + early when >=80% (mode hot), dim + late otherwise
# (mode cold). Promotion mirrors the "near the cap" warning surviving on narrow
# terminals.
add_rate_limit() {
    local label="$1" raw="$2" mode="$3" v s rendered
    [ -z "$raw" ] || [ "$raw" = "null" ] && return
    printf -v v '%.0f' "$raw"
    s="${label}:${v}%"
    if [ "$v" -ge 80 ]; then
        [ "$mode" = hot ] || return
        rendered=$(printf "${RED}${BOLD}%s${RESET}" "$s")
    else
        [ "$mode" = cold ] || return
        rendered=$(printf "${DIM}%s${RESET}" "$s")
    fi
    try_add "${#s}" "$rendered"
}

# ===========================================================================
# Assemble in priority order. emoji prefix counts as 2 cells.
# ===========================================================================

# --- T1 (must): directory --------------------------------------------------
add_dir() {
    local extras=""
    [ -n "$dir_drifted" ] && extras+=" (${dir_drifted})"
    [ "$added_dirs_count" -gt 0 ] && extras+=" +${added_dirs_count}d"
    byte_width "$extras"
    local extras_w=$BYTE_WIDTH

    local top=$(( ${#dir_basename} < NAME_CAP ? ${#dir_basename} : NAME_CAP ))
    local widths=("$top" 16 12 8 4)
    local short rendered vw

    for w in "${widths[@]}"; do
        short=$(truncate_to "$dir_basename" "$w")
        if [ -n "$extras" ]; then
            rendered=$(printf "📂 ${CYAN}%s${RESET}${DIM}%s${RESET}" "$short" "$extras")
            byte_width "$short"
            short_w=$BYTE_WIDTH
            vw=$(( 2 + 1 + short_w + extras_w ))
            try_add "$vw" "$rendered" && return 0
        fi
        rendered=$(printf "📂 ${CYAN}%s${RESET}" "$short")
        byte_width "$short"
        short_w=$BYTE_WIDTH
        vw=$(( 2 + 1 + short_w ))
        try_add "$vw" "$rendered" && return 0
    done

    short=$(truncate_to "$dir_basename" 4)
    rendered=$(printf "📂 ${CYAN}%s${RESET}" "$short")
    byte_width "$short"
    short_w=$BYTE_WIDTH
    vw=$(( 2 + 1 + short_w ))
    force_add "$vw" "$rendered"
}
add_dir

# --- T1 (must, if data): context light + % ---------------------------------
add_context() {
    [ -z "$used_pct_int" ] && return
    local pct_label="${used_pct_int}%"
    local rendered vw

    rendered=$(printf "%s ${context_color}%s${RESET}" "$context_light" "$pct_label")
    vw=$(( 2 + 1 + ${#pct_label} ))
    try_add "$vw" "$rendered" && return 0

    # too tight even for the label — keep just the light
    force_add 2 "$context_light"
}
add_context

# --- T1.5 (promoted): rate limits at/over 80% ------------------------------
add_rate_limit "5h" "$rl_5h" hot
add_rate_limit "7d" "$rl_7d" hot

# --- T2 (high): git branch -------------------------------------------------
add_git_branch() {
    [ -z "$git_branch" ] && return
    local suffix=""
    [ -n "$git_dirty" ] && suffix=" $git_dirty"
    byte_width "$suffix"
    local suffix_w=$BYTE_WIDTH
    local short rendered vw

    local top=$(( ${#git_branch} < NAME_CAP ? ${#git_branch} : NAME_CAP ))
    for w in "$top" 16 12 8; do
        short=$(truncate_to "$git_branch" "$w")
        rendered=$(printf "🌿 ${RED}%s${RESET}${YELLOW}%s${RESET}" "$short" "$suffix")
        byte_width "$short"
        short_w=$BYTE_WIDTH
        vw=$(( 2 + 1 + short_w + suffix_w ))
        try_add "$vw" "$rendered" && return 0
    done
}
add_git_branch

# --- T2 (high): worktree ---------------------------------------------------
add_worktree() {
    local raw emoji color
    if [ -n "$wt_name" ]; then
        raw="$wt_name"
        [ -n "$wt_branch" ] && raw="$raw@$wt_branch"
        [ -n "$wt_orig_branch" ] && raw="${raw}←${wt_orig_branch}"
        emoji="🪵"
    elif [ -n "$git_worktree_label" ]; then
        raw="$git_worktree_label"
        emoji="🌳"
    else
        return
    fi
    color="$MAGENTA"
    local short rendered vw

    local top=$(( ${#raw} < NAME_CAP ? ${#raw} : NAME_CAP ))
    for w in "$top" 16 12 8 6; do
        short=$(truncate_to "$raw" "$w")
        rendered=$(printf "%s ${color}%s${RESET}" "$emoji" "$short")
        byte_width "$short"
        short_w=$BYTE_WIDTH
        vw=$(( 2 + 1 + short_w ))
        try_add "$vw" "$rendered" && return 0
    done
}
add_worktree

# --- T2 (high): model ------------------------------------------------------
add_model() {
    [ -z "$model" ] && return
    local short rendered vw
    for w in "${#model}" 16 10 6; do
        short=$(truncate_to "$model" "$w")
        rendered=$(printf "🧠 ${PURPLE}%s${RESET}" "$short")
        byte_width "$short"
        short_w=$BYTE_WIDTH
        vw=$(( 2 + 1 + short_w ))
        try_add "$vw" "$rendered" && return 0
    done
}
add_model

# --- T2 (high): effort -----------------------------------------------------
if [ -n "$effort" ]; then
    rendered=$(printf "⚡${BOLD_BLUE}%s${RESET}" "$effort")
    byte_width "$effort"
    effort_w=$BYTE_WIDTH
    vw=$(( 2 + effort_w ))
    try_add "$vw" "$rendered"
fi

# --- T3 (med): tokens ------------------------------------------------------
if [ -n "$token_label" ]; then
    combined="${token_label}${ctx_size_label}"
    rendered=$(printf "${DIM}%s${RESET}" "$combined")
    byte_width "$combined"
    try_add "$BYTE_WIDTH" "$rendered"
fi

# --- T3 (med): cache hit ---------------------------------------------------
if [ -n "$cache_label" ]; then
    rendered=$(printf "${DIM}%s${RESET}" "$cache_label")
    byte_width "$cache_label"
    try_add "$BYTE_WIDTH" "$rendered"
fi

# --- T3 (med): >200k flag --------------------------------------------------
if [ "$exceeds_200k" = "true" ]; then
    label=">200k"
    rendered=$(printf "${RED}${BOLD}%s${RESET}" "$label")
    byte_width "$label"
    try_add "$BYTE_WIDTH" "$rendered"
fi

# --- T4 (low): rate limits below 80% ---------------------------------------
add_rate_limit "5h" "$rl_5h" cold
add_rate_limit "7d" "$rl_7d" cold

# --- T4 (low): cost ---------------------------------------------------------
if [ -n "$cost_label" ]; then
    rendered=$(printf "💲${cost_color}%s${RESET}" "$cost_label")
    byte_width "$cost_label"
    cost_w=$BYTE_WIDTH
    vw=$(( 2 + cost_w ))
    try_add "$vw" "$rendered"
fi

# Append the agent only after existing status segments have had their space.
if [ -n "$agent_name" ]; then
    agent_name=${agent_name//\\/／}
    for w in "${#agent_name}" 16 10 6; do
        short=$(truncate_to "$agent_name" "$w")
        rendered=$(printf "🤖 ${PURPLE}%s${RESET}" "$short")
        # UTF-8 byte length is a conservative cell bound for this optional segment.
        # It preserves the notification reserve even for wide characters and emoji.
        byte_width "$short"
        agent_bytes=$BYTE_WIDTH
        vw=$(( 3 + agent_bytes ))
        try_add "$vw" "$rendered" && break
    done
fi

printf '%s\n' "$output"
