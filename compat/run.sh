#!/bin/sh
# scriptbox real-world compatibility suite.
#
# Two kinds of checks:
#   * TRANSPARENCY - scriptbox must be invisible: `scriptbox <shell> x.sh` must
#     produce the same stdout+exit as `<shell> x.sh`. A divergence is a scriptbox
#     bug (or a case that needs a documented flag). Compared per-shell, so inherent
#     cross-shell differences (array indexing, etc.) don't create false positives.
#   * INSULATION - the whole point: a script that edits itself mid-run must be
#     frozen under scriptbox (the tampered line must NOT run) while a plain shell
#     is vulnerable to it.
#
# Plus $0 (--argv0) behaviour per shell, --subscripts source-freezing, and a
# safe read-only pass (`scriptbox hash`) over real public installer scripts.
#
# Usage: run.sh [path-to-scriptbox]   (defaults to `scriptbox` on PATH)
# Exit non-zero if any transparency divergence or insulation failure is found.

set -u
SB="${1:-scriptbox}"
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ID="$DIR/idioms"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail=0
pass=0
note=0
GRN=''; RED=''; YEL=''; DIM=''; RST=''
if [ -t 1 ]; then GRN=$(printf '\033[32m'); RED=$(printf '\033[31m'); YEL=$(printf '\033[33m'); DIM=$(printf '\033[2m'); RST=$(printf '\033[0m'); fi

has() { command -v "$1" >/dev/null 2>&1 && "$1" -c 'exit 0' >/dev/null 2>&1; }

hdr() { printf '\n%s== %s ==%s\n' "$YEL" "$1" "$RST"; }

# Space-separated, sorted, unique shellcheck codes from stdin.
codes() { grep -oE 'SC[0-9]+' | sort -u | tr '\n' ' '; }

# transparency: plain <shell> vs boxed, same stdout+exit expected.
# args: idiom-file  shell  extra-scriptbox-flags  annotation
trans() {
    idiom=$1; shell=$2; flags=$3; anno=$4
    has "$shell" || return 0
    p_out=$("$shell" "$ID/$idiom" 2>/dev/null); p_rc=$?
    # shellcheck disable=SC2086
    b_out=$("$SB" $flags "$shell" "$ID/$idiom" 2>/dev/null); b_rc=$?
    if [ "$p_out" = "$b_out" ] && [ "$p_rc" = "$b_rc" ]; then
        pass=$((pass+1))
        printf '  %sPASS%s %-16s %-5s %s%s%s\n' "$GRN" "$RST" "$idiom" "$shell" "$DIM" "$anno" "$RST"
    else
        fail=$((fail+1))
        printf '  %sFAIL%s %-16s %-5s %s\n' "$RED" "$RST" "$idiom" "$shell" "$anno"
        printf '       plain(rc=%s): %s\n' "$p_rc" "$(printf '%s' "$p_out" | tr '\n' '|')"
        printf '       boxed(rc=%s): %s\n' "$b_rc" "$(printf '%s' "$b_out" | tr '\n' '|')"
    fi
}

hdr "Transparency: boxed output must match plain shell (per shell)"
for sh in bash zsh dash ksh; do
    trans cmdsub.sh       "$sh" "" "command substitution"
    trans subshell.sh     "$sh" "" "( subshell )"
    trans background.sh   "$sh" "" "background job + wait"
    trans heredoc.sh      "$sh" "" "heredoc"
    trans trap_exit.sh    "$sh" "" "EXIT trap fires"
    trans set_euo.sh      "$sh" "" "set -eu + pipe"
    trans exec_replace.sh "$sh" "" "exec replaces shell"
    trans crlf.sh         "$sh" "" "CRLF line endings (byte-faithful freeze)"
    trans large.sh        "$sh" "" "4000-line script (memfd/unlink path)"
done
# arrays + process substitution: bash/zsh only
for sh in bash zsh; do
    trans arrays.sh  "$sh" "" "arrays (bash/zsh)"
    trans procsub.sh "$sh" "" "process substitution (bash/zsh)"
done

hdr "stdin fidelity: interactive read still works through the frozen fd"
for sh in bash zsh dash ksh; do
    has "$sh" || continue
    p=$(printf 'hello\n' | "$sh" "$ID/stdin_read.sh" 2>/dev/null)
    b=$(printf 'hello\n' | "$SB" "$sh" "$ID/stdin_read.sh" 2>/dev/null)
    if [ "$p" = "$b" ] && [ "$b" = "stdin-got:hello" ]; then
        pass=$((pass+1)); printf '  %sPASS%s stdin_read      %-5s %sread from stdin, not the script fd%s\n' "$GRN" "$RST" "$sh" "$DIM" "$RST"
    else
        fail=$((fail+1)); printf '  %sFAIL%s stdin_read      %-5s plain=[%s] boxed=[%s]\n' "$RED" "$RST" "$sh" "$p" "$b"
    fi
done

hdr "Insulation: a self-editing script must be frozen under scriptbox"
for sh in bash zsh dash ksh; do
    has "$sh" || continue
    cp "$ID/selfedit.tmpl" "$TMP/se_plain.sh"; cp "$ID/selfedit.tmpl" "$TMP/se_boxed.sh"
    p=$("$sh" "$TMP/se_plain.sh" 2>/dev/null)
    b=$(SCRIPTBOX_SOURCE= "$SB" "$sh" "$TMP/se_boxed.sh" 2>/dev/null)
    p_tamper=$(printf '%s' "$p" | grep -c TAMPERED)
    b_tamper=$(printf '%s' "$b" | grep -c TAMPERED)
    if [ "$b_tamper" = "0" ]; then
        pass=$((pass+1))
        extra=""; [ "$p_tamper" != "0" ] && extra="(plain shell was vulnerable, as expected)"
        printf '  %sPASS%s selfedit        %-5s frozen: tampered line did not run %s%s%s\n' "$GRN" "$RST" "$sh" "$DIM" "$extra" "$RST"
    else
        fail=$((fail+1)); printf '  %sFAIL%s selfedit        %-5s boxed ran the tampered line!\n' "$RED" "$RST" "$sh"
    fi
done

hdr "\$0 handling (--argv0) across shells"
for sh in bash zsh dash ksh; do
    has "$sh" || continue
    ns=$("$SB" "$sh" "$ID/argv0.sh" 2>/dev/null)               # no shebang, default mode
    ws=$("$SB" "$sh" "$ID/argv0_shebang.sh" 2>/dev/null)       # shebang present, default mode
    src=$("$SB" --argv0 source "$sh" "$ID/argv0.sh" 2>/dev/null)  # source mode, no shebang
    note=$((note+1))
    printf '  %sNOTE%s argv0 %-5s no-shebang=[%s]\n              with-shebang=[%s]\n              --argv0 source=[%s]\n' \
        "$DIM" "$RST" "$sh" "$ns" "$ws" "$src"
done
printf '       %sThe $0 reset is joined onto the first body line with `;` (issue #1 fix):\n' "$DIM"
printf '       no line is added, so error line numbers stay exact, and the shebang stays\n'
printf '       on line 1, so the served copy is lint-clean. bash>=5/zsh resolve the real\n'
printf '       path with or without a shebang; macOS bash 3.2 has no BASH_ARGV0; dash/ksh\n'
printf '       need --argv0 source; zsh source-mode intentionally keeps the fd path.%s\n' "$RST"

if command -v shellcheck >/dev/null 2>&1; then
    hdr "shellcheck: the served copy (\`scriptbox emit\`) adds no findings the original lacks"
    for name in cmdsub subshell background heredoc trap_exit set_euo exec_replace \
                arrays procsub source_parent argv0 argv0_shebang crlf; do
        f="$ID/$name.sh"
        [ -f "$f" ] || continue
        # Only bash/zsh get the $0 rewrite; dash/ksh emit verbatim (nothing to add).
        for sh in bash zsh; do
            has "$sh" || continue
            o=$(shellcheck -s "$sh" -f gcc "$f" 2>/dev/null | codes)
            e=$("$SB" emit "$sh" "$f" 2>/dev/null | shellcheck -s "$sh" -f gcc - 2>/dev/null | codes)
            added=""
            for c in $e; do case " $o " in *" $c "*) ;; *) added="$added$c ";; esac; done
            if [ -z "$added" ]; then
                pass=$((pass+1))
                printf '  %sPASS%s emit+shellcheck %-14s %-5s %sno added findings (orig: %s)%s\n' \
                    "$GRN" "$RST" "$name" "$sh" "$DIM" "${o:-none}" "$RST"
            else
                fail=$((fail+1))
                printf '  %sFAIL%s emit+shellcheck %-14s %-5s scriptbox ADDED: %s\n' \
                    "$RED" "$RST" "$name" "$sh" "$added"
            fi
        done
    done
else
    printf '\n%s(shellcheck absent - skipped the no-added-findings check)%s\n' "$DIM" "$RST"
fi

hdr "source a sibling (self-location via \$SCRIPTBOX_SOURCE)"
for sh in bash zsh dash ksh; do
    has "$sh" || continue
    b=$("$SB" "$sh" "$ID/source_parent.sh" 2>/dev/null)
    if [ "$b" = "sourced:child-value" ]; then
        pass=$((pass+1)); printf '  %sPASS%s source_parent   %-5s %s\$SCRIPTBOX_SOURCE locates the sibling%s\n' "$GRN" "$RST" "$sh" "$DIM" "$RST"
    else
        fail=$((fail+1)); printf '  %sFAIL%s source_parent   %-5s boxed=[%s]\n' "$RED" "$RST" "$sh" "$b"
    fi
done

if "$SB" --subscripts=report bash "$ID/source_parent.sh" >/dev/null 2>&1; then
    hdr "--subscripts (full-featured build): report + freeze a source include"
    r=$("$SB" --subscripts=report bash "$ID/source_parent.sh" 2>&1 | grep -ci 'child.sh')
    if [ "$r" -ge 1 ]; then
        pass=$((pass+1)); printf '  %sPASS%s subscripts      report detected the child.sh source site\n' "$GRN" "$RST"
    else
        note=$((note+1)); printf '  %sNOTE%s subscripts      report did not flag child.sh (dynamic path?)\n' "$DIM" "$RST"
    fi
    # freeze must wrap an exec'd child while staying transparent to the output
    ep=$(bash "$ID/exec_parent.sh" 2>/dev/null)
    eb=$("$SB" --subscripts=freeze bash "$ID/exec_parent.sh" 2>/dev/null)
    if [ "$ep" = "$eb" ] && printf '%s' "$eb" | grep -q exec-child-ran; then
        pass=$((pass+1)); printf '  %sPASS%s subscripts      freeze wraps the exec'"'"'d child; output matches plain\n' "$GRN" "$RST"
    else
        fail=$((fail+1)); printf '  %sFAIL%s subscripts      freeze exec-child: plain=[%s] boxed=[%s]\n' "$RED" "$RST" "$ep" "$eb"
    fi
else
    printf '\n%s(--subscripts unavailable: lean build. Build with --features subscripts to test it.)%s\n' "$DIM" "$RST"
fi

hdr "Real public installers: safe read-only pass (scriptbox hash), no execution"
if command -v curl >/dev/null 2>&1; then
    for u in \
        "https://sh.rustup.rs" \
        "https://raw.githubusercontent.com/nvm-sh/nvm/master/install.sh" \
        "https://get.docker.com" \
        ; do
        f="$TMP/$(printf '%s' "$u" | tr '/:.' '___').sh"
        if curl -fsSL "$u" -o "$f" 2>/dev/null; then
            if h=$("$SB" hash "$f" 2>/dev/null) && [ -n "$h" ]; then
                pass=$((pass+1)); printf '  %sPASS%s hash            %s %s(%s bytes) %s%s\n' "$GRN" "$RST" "$u" "$DIM" "$(wc -c <"$f" | tr -d ' ')" "$h" "$RST"
            else
                fail=$((fail+1)); printf '  %sFAIL%s hash            %s (scriptbox hash errored)\n' "$RED" "$RST" "$u"
            fi
        else
            note=$((note+1)); printf '  %sNOTE%s fetch failed (offline?) %s\n' "$DIM" "$RST" "$u"
        fi
    done
else
    printf '  %sNOTE%s curl absent - skipped real-installer pass%s\n' "$DIM" "$RST" "$RST"
fi

printf '\n%s== summary ==%s  %sPASS %d%s  %sFAIL %d%s  %sNOTE %d%s\n' \
    "$YEL" "$RST" "$GRN" "$pass" "$RST" "$RED" "$fail" "$RST" "$DIM" "$note" "$RST"
printf '%sNote: this covers the Linux memfd path. The macOS unlink-then-read path\n' "$DIM"
printf 'is NOT exercised by Docker and needs a separate run on real macOS.%s\n' "$RST"
[ "$fail" -eq 0 ]
