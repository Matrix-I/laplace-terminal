#!/usr/bin/env bash
# ==============================================================================
# LITTY TERMINAL - AUTOMATED PRE-RELEASE VERIFICATION & BENCHMARK SUITE
# ==============================================================================
# Purpose:
#   1. Measure RAM at idle, 1 window 3 tabs, 2 windows 3 tabs each.
#   2. Measure interactive latency (PTY command roundtrip, 4KB/64KB/1MB file reads).
#   3. Stress-test rendering with high-volume logs (5,000 - 20,000 lines), check frame stalls/jitter.
#   4. Verify critical features (FZF history search, regex find, splits, paste security).
#   5. Evaluate against strict SLA standards. Regressions BLOCK RELEASE (exit 1).
# ==============================================================================

set -eo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

REPORT_FILE="$REPO_DIR/scripts/release-test-report.md"
BIN="$REPO_DIR/target/release/litty"

# ANSI Colors
BOLD=$'\033[1m'
DIM=$'\033[2m'
RESET=$'\033[0m'
GREEN=$'\033[32m'
RED=$'\033[31m'
YELLOW=$'\033[33m'
CYAN=$'\033[36m'
MAGENTA=$'\033[35m'

MODE="all"
LOG_LINES=20000

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ram)
            MODE="ram"
            shift
            ;;
        --perf)
            MODE="perf"
            shift
            ;;
        --features)
            MODE="features"
            shift
            ;;
        --quick)
            LOG_LINES=5000
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--ram | --perf | --features | --quick]"
            echo "  --ram      : Run RAM benchmarks only (1w/1t idle, 1w/3t, 2w/6t, cleanup)"
            echo "  --perf     : Run Latency and Render Stress tests only"
            echo "  --features : Run Regression Feature tests only (FZF, Regex, Splits, Paste)"
            echo "  --quick    : Run quick stress mode (5,000 log lines)"
            exit 0
            ;;
        *)
            echo "Invalid option: $1. Use --help for usage."
            exit 1
            ;;
    esac
done

echo ""
echo "${BOLD}${CYAN}╔═══════════════════════════════════════════════════════════════════════╗${RESET}"
echo "${BOLD}${CYAN}║     LITTY TERMINAL — PRE-RELEASE VERIFICATION & BENCHMARK SUITE       ║${RESET}"
echo "${BOLD}${CYAN}╚═══════════════════════════════════════════════════════════════════════╝${RESET}"
echo "${DIM}Platform     : $(uname -s) ($(uname -m)) | Kernel: $(uname -r)${RESET}"
echo "${DIM}Repo root    : $REPO_DIR${RESET}"
echo "${DIM}Timestamp    : $(date '+%Y-%m-%d %H:%M:%S %z')${RESET}"
echo ""

TOTAL_FAILURES=0

record_fail() {
    TOTAL_FAILURES=$((TOTAL_FAILURES + 1))
}

# ==============================================================================
# STEP 1: COMPILE RELEASE BINARY
# ==============================================================================
echo "${BOLD}${YELLOW}▶ [1/5] Compiling Release Binary (cargo build --release)...${RESET}"
if cargo build --release; then
    echo "  ${GREEN}✔ Release build succeeded.${RESET}"
else
    echo "  ${RED}✘ Failed to compile release binary!${RESET}"
    exit 1
fi

# ==============================================================================
# STEP 2: RUN UNIT & INTEGRATION TEST SUITES
# ==============================================================================
if [[ "$MODE" == "all" || "$MODE" == "features" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [2/5] Running Unit & Integration Test Suites (cargo test --release)...${RESET}"
    if cargo test --release -- --nocapture; then
        echo "  ${GREEN}✔ 100% Unit & Integration tests passed.${RESET}"
    else
        echo "  ${RED}✘ One or more cargo tests failed!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# STEP 3: MEASURE RAM PROFILE (IDLE, 1W/3T, 2W/6T, CLEANUP)
# ==============================================================================
RAM_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "ram" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [3/5] Measuring Memory Footprint (Idle, 1w/3t, 2w/6t, Cleanup)...${RESET}"
    echo "  ${DIM}Starting Litty in RAM Benchmark mode...${RESET}"
    
    rm -f /tmp/litty_ram_bench.json
    
    # Run the RAM benchmark
    if "$BIN" +bench-ram; then
        if [[ -f /tmp/litty_ram_bench.json ]]; then
            RAM_JSON=$(cat /tmp/litty_ram_bench.json)
            IS_RAM_PASS=$(echo "$RAM_JSON" | grep -o '"passed":true' || true)
            if [[ -n "$IS_RAM_PASS" ]]; then
                RAM_PASSED="PASS"
                echo "  ${GREEN}✔ RAM profile meets SLA targets.${RESET}"
            else
                RAM_PASSED="FAIL"
                echo "  ${RED}✘ RAM SLA budget violated!${RESET}"
                record_fail
            fi
        else
            RAM_PASSED="FAIL"
            echo "  ${RED}✘ RAM benchmark results not found!${RESET}"
            record_fail
        fi
    else
        RAM_PASSED="FAIL"
        echo "  ${RED}✘ Error executing RAM benchmark!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# STEP 4: MEASURE INTERACTIVE LATENCY & RENDER STRESS (20K LOGS)
# ==============================================================================
PERF_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "perf" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [4/5] Measuring Interactive Latency & Render Stress...${RESET}"
    if "$BIN" +bench-perf; then
        PERF_PASSED="PASS"
        echo "  ${GREEN}✔ Latency and render stress tests meet SLA targets.${RESET}"
    else
        PERF_PASSED="FAIL"
        echo "  ${RED}✘ Latency or render throughput did not meet SLA targets!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# STEP 5: VERIFY CRITICAL FEATURES (FZF, REGEX, SPLITS, PASTE)
# ==============================================================================
FEAT_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "features" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [5/5] Verifying Critical Features (FZF search, Regex find, Splits, Paste security)...${RESET}"
    if "$BIN" +bench-features; then
        FEAT_PASSED="PASS"
        echo "  ${GREEN}✔ All critical features operate correctly and within SLA.${RESET}"
    else
        FEAT_PASSED="FAIL"
        echo "  ${RED}✘ Regression detected in one or more core features!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# SUMMARY AND GENERATE REPORT
# ==============================================================================
echo ""
echo "${BOLD}${CYAN}╔═══════════════════════════════════════════════════════════════════════╗${RESET}"
echo "${BOLD}${CYAN}║                    PRE-RELEASE EVALUATION SUMMARY                     ║${RESET}"
echo "${BOLD}${CYAN}╚═══════════════════════════════════════════════════════════════════════╝${RESET}"

RAM_ICON="[PASS]"
[[ "$RAM_PASSED" == "FAIL" ]] && RAM_ICON="[FAIL]"
[[ "$RAM_PASSED" == "SKIP" ]] && RAM_ICON="[SKIP]"

PERF_ICON="[PASS]"
[[ "$PERF_PASSED" == "FAIL" ]] && PERF_ICON="[FAIL]"
[[ "$PERF_PASSED" == "SKIP" ]] && PERF_ICON="[SKIP]"

FEAT_ICON="[PASS]"
[[ "$FEAT_PASSED" == "FAIL" ]] && FEAT_ICON="[FAIL]"
[[ "$FEAT_PASSED" == "SKIP" ]] && FEAT_ICON="[SKIP]"

echo "  1. RAM Profile (Idle, 1w/3t, 2w/6t, Cleanup) : $RAM_ICON"
echo "  2. Interactive Latency & Render Stress (20k)  : $PERF_ICON"
echo "  3. Critical Features (FZF, Regex, Splits, Sec): $FEAT_ICON"
echo ""

# Write Markdown report
cat <<EOF > "$REPORT_FILE"
# Litty Terminal — Pre-Release Performance & Stability Verification Report

- **Execution Date**: $(date '+%Y-%m-%d %H:%M:%S %z')
- **Platform**: $(uname -s) $(uname -m) (Kernel $(uname -r))
- **Git Commit**: $(git rev-parse --short HEAD 2>/dev/null || echo "N/A")
- **Status**: $([ $TOTAL_FAILURES -eq 0 ] && echo "✅ **READY FOR RELEASE (ALL PASS)**" || echo "❌ **RELEASE BLOCKED (REGRESSIONS/FAILURES DETECTED)**")

---

## 1. Memory Profile (RAM Footprint)
- **1 Window, 1 Tab (Idle Baseline)**: Measures cold start footprint at idle state.
- **1 Window, 3 Tabs**: Measures memory scaling when opening multiple tabs in a single window.
- **2 Windows, 6 Tabs (3 tabs per window)**: Measures multi-window scalability and multi-GPU swapchains.
- **Post-cleanup (Tabs closed)**: Ensures no memory leaks occurred (leak delta $\le$ 8 MB).
- **Status**: **$RAM_PASSED**

## 2. Interactive Latency & I/O
- **Command Execution Roundtrip (PTY Latency)**:
  - Average: $\le$ 1.0 ms
  - P99: $\le$ 3.0 ms
- **File Reading Ingestion**:
  - 4 KB Config: $\le$ 1.0 ms
  - 64 KB Source code: $\le$ 15.0 ms
  - 1 MB Data stream: $\ge$ 50 MB/s
- **Status**: **$PERF_PASSED**

## 3. Large Log Stress Test & Frame Stalls
- **Test Workload**: 20,000 lines of standard ANSI server logs (ISO timestamps, colors, log levels, JSON payloads).
- **Throughput**: $\ge$ 20,000 lines/sec.
- **Frame Stalls (> 16.6 ms)**: Zero severe stalls, Worst stall $\le$ 25 ms.
- **Post-Stress Responsiveness (Unblock Probe)**: $\le$ 1.0 ms (terminal immediately responsive, no lockups/hangs).
- **Status**: **$PERF_PASSED**

## 4. Critical Features Regression
- **FZF Fuzzy History Search**: Query over 10,000 real-world commands ($\le$ 1.5 ms / query, 100% accuracy).
- **Find in Terminal (Regex Search)**: Regex scan across 5,000 scrollback lines ($\le$ 15 ms).
- **Split Panes Engine**: 1,000 vertical/horizontal splits, resize, zoom, and close operations without panicking.
- **Bracketed Paste Security**: Escape injection mitigation (\`\x1b[201~\`) and safe shell quotation.
- **Status**: **$FEAT_PASSED**

---
*Report generated automatically by \`scripts/pre-release-check.sh\`.*
EOF

echo "${BOLD}${CYAN}📄 Test report generated:${RESET} ${BOLD}$REPORT_FILE${RESET}"
echo "   ${DIM}To view: cat scripts/release-test-report.md${RESET}"
echo ""

if [[ $TOTAL_FAILURES -eq 0 ]]; then
    echo "${BOLD}${GREEN}==================================================================${RESET}"
    echo "${BOLD}${GREEN}  ✔ ALL CRITERIA PASSED! BUILD IS READY FOR RELEASE.              ${RESET}"
    echo "${BOLD}${GREEN}==================================================================${RESET}"
    echo ""
    exit 0
else
    echo "${BOLD}${RED}==================================================================${RESET}"
    echo "${BOLD}${RED}  ✘ DETECTED $TOTAL_FAILURES REGRESSIONS / PERFORMANCE FAILURES!      ${RESET}"
    echo "${BOLD}${RED}  MUST BE RESOLVED BEFORE RELEASE. RELEASE BLOCKED!               ${RESET}"
    echo "${BOLD}${RED}==================================================================${RESET}"
    echo ""
    exit 1
fi
