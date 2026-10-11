#!/usr/bin/env bash
# Every gate a rebase of this line is judged by, one command.
#
# usage: tools/verify.sh [--quick]
#
#   fmt       cargo +stable fmt --all -- --check
#   clippy    cargo +stable clippy --workspace --all-targets -- -D warnings
#   tests     cargo +stable test --workspace (a failed upstream async-race test is
#             rerun once before the gate fails — see test_with_flake_retry)
#   parity    cargo +stable xtask parity
#   layers    cargo +stable xtask layers
#   fixtures  the negative-conversion reference table at or better than the values
#             recorded in tools/fixtures.txt for THIS line (each channel RMS of the
#             rendered conversion against the reference must not exceed the recorded
#             value; the rebase may not regress a line's own evidence)
#
# The stable toolchain is mandatory: the default nightly is too old for epaint 0.36.2.
# One cargo command at a time. --quick skips fmt and parity/layers (still runs clippy,
# tests and fixtures) for a fast re-check of an already-verified tree.
#
# Fixture floors are line-specific by design: the canonical line carries the
# reverse-engineered endpoint values, the public line the behavioural ones. Never copy
# one line's tools/fixtures.txt onto the other.
#
# What stays manual: the scratch-merge proof (git diff <scratch-merge> <tip> — see
# REBASE.md), the smoke renders of the user's own files, and the push/tag steps.
set -uo pipefail

line_file="$(dirname "$0")/fixtures.txt"
quick=0
[ "${1:-}" = "--quick" ] && quick=1
[ -f "$line_file" ] || { echo "no tools/fixtures.txt — the fixture gate has no floors" >&2; exit 64; }

c() { # run one cargo gate, print it, stop the script on failure
    echo ""
    echo "== $* =="
    "$@"
    local rc=$?
    if [ $rc -ne 0 ]; then
        echo "FAILED (exit $rc): $*" >&2
        exit $rc
    fi
}

# Upstream's async and preview-race tests (tests_model_setup, tests_preview_limit) lose
# timing races under a fully parallel run roughly one run in four; both files are
# untouched by this line and by recent upstream batches. On a workspace-test failure,
# rerun the failing tests once, still workspace-wide so the feature set matches; a flake
# is reported as such, a repeat failure is a hard failure.
test_with_flake_retry() {
    echo ""
    echo "== cargo +stable test --workspace =="
    local out rc names
    if out="$(NLP_RE_DIR="${NLP_RE_DIR:-$HOME/git/ai/nlp-re}" cargo +stable test --workspace 2>&1)"; then
        echo "$out" | grep "^test result" | awk '{passed+=$4} END {print "workspace tests: all suites pass ("passed" tests)"}'
        return 0
    fi
    echo "$out" | grep -E "^(test result|error)" | head -5
    names="$(echo "$out" | sed -n '/^failures:$/,/^test result/p' | grep '^    ' | sed 's/^ *//' | grep -v '^$' | sort -u)"
    if [ -z "$names" ]; then
        echo "FAILED: workspace tests (no failing test names found)" >&2
        exit 101
    fi
    echo "rerunning the failing tests once (upstream's async tests race under load):"
    for t in $names; do
        if ! NLP_RE_DIR="${NLP_RE_DIR:-$HOME/git/ai/nlp-re}" cargo +stable test --workspace "$t" -- --exact >/dev/null 2>&1; then
            echo "FAILED on rerun: $t" >&2
            exit 101
        fi
        echo "  flake, passes on rerun: $t"
    done
}

if [ $quick -eq 0 ]; then
    c cargo +stable fmt --all -- --check
    c cargo +stable clippy --workspace --all-targets -- -D warnings
fi
test_with_flake_retry
if [ $quick -eq 0 ]; then
    c cargo +stable xtask parity
    c cargo +stable xtask layers
fi

echo ""
echo "== fixtures: negative-conversion reference table =="
out="$(NLP_RE_DIR="${NLP_RE_DIR:-$HOME/git/ai/nlp-re}" cargo +stable test -p lightcraft-cli --test negative_reference -- --nocapture 2>&1)"
rc=$?
echo "$out" | grep -E 'vs (plugin|positive)|skipped|FAILED|test result' | head -20
if [ $rc -ne 0 ]; then echo "FAILED: negative_reference test" >&2; exit $rc; fi
if echo "$out" | grep -q 'skipped (set NLP_RE_DIR'; then
    echo "SKIPPED: the nlp-re fixtures are not available on this machine — the fixture gate ran nothing" >&2
    exit 3
fi

# The table prints one row per fixture:
#   grey_wedge   vs plugin  RMS 1.19/2.08/4.03  max 3/5/7   vs positive RMS ...
# Compare each row's first three RMS channels against the floors in tools/fixtures.txt.
fail=0
while read -r name floor_r floor_g floor_b; do
    row="$(echo "$out" | grep "^$name " | head -1)"
    if [ -z "$row" ]; then
        echo "FAIL: no measured row for $name"; fail=1; continue
    fi
    # The row's first RMS triple: "this conversion vs the plugin's export" when there
    # are two (the row ends with a "vs positive" triple that must not be parsed
    # instead); for rows without a plugin export the only triple is vs the positive.
    rms="$(echo "$row" | grep -oE 'RMS [0-9.]+/[0-9.]+/[0-9.]+' | head -1 | grep -oE '[0-9.]+/[0-9.]+/[0-9.]+' | tr '/' ' ')"
    [ -n "$rms" ] || { echo "FAIL: cannot parse the measured RMS from: $row"; fail=1; continue; }
    set -- $rms
    if awk -v a="$1" -v b="$2" -v c="$3" -v fr="$floor_r" -v fg="$floor_g" -v fb="$floor_b" 'BEGIN { exit !(a>fr || b>fg || c>fb) }'; then
        printf '  FAIL %-14s measured %s exceeds the floor %s/%s/%s\n' "$name" "$rms" "$floor_r" "$floor_g" "$floor_b"
        fail=1
    else
        printf '  ok   %-14s measured %s <= floor %s/%s/%s\n' "$name" "$rms" "$floor_r" "$floor_g" "$floor_b"
    fi
done < <(grep -v '^#' "$line_file")
[ $fail -eq 0 ] || { echo "FIXTURE GATE FAILED — the rebase regressed this line's evidence" >&2; exit 1; }

echo ""
echo "ALL GATES PASS"
