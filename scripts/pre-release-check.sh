#!/usr/bin/env bash
# ==============================================================================
# LITTY TERMINAL - AUTOMATED PRE-RELEASE VERIFICATION & BENCHMARK SUITE
# ==============================================================================
# Mục đích:
#   1. Đo RAM khi idle, khi có 1 window 3 tabs, 2 windows mỗi window 3 tabs.
#   2. Đo độ trễ khi thao tác (chạy lệnh PTY, đọc file 4KB/64KB/1MB).
#   3. Check render với dữ liệu log lớn (5,000 - 20,000 dòng), kiểm tra nghẽn / giật lag.
#   4. Kiểm tra các feature quan trọng (FZF history search, regex find, splits, paste security).
#   5. Đánh giá theo tiêu chuẩn SLA khắt khe. Nếu có hồi quy (regression) -> CHẶN RELEASE (exit 1).
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
            echo "  --ram      : Chỉ chạy đo RAM (1w/1t idle, 1w/3t, 2w/6t, cleanup)"
            echo "  --perf     : Chỉ chạy đo Latency và Render Stress Test"
            echo "  --features : Chỉ chạy kiểm tra Regression Features (FZF, Regex, Splits, Paste)"
            echo "  --quick    : Chạy chế độ nhanh (5,000 dòng log stress)"
            exit 0
            ;;
        *)
            echo "Lựa chọn không hợp lệ: $1. Dùng --help để xem hướng dẫn."
            exit 1
            ;;
    esac
done

echo ""
echo "${BOLD}${CYAN}╔═══════════════════════════════════════════════════════════════════════╗${RESET}"
echo "${BOLD}${CYAN}║     LITTY TERMINAL — PRE-RELEASE VERIFICATION & BENCHMARK SUITE       ║${RESET}"
echo "${BOLD}${CYAN}╚═══════════════════════════════════════════════════════════════════════╝${RESET}"
echo "${DIM}Hệ điều hành : $(uname -s) ($(uname -m)) | Kernel: $(uname -r)${RESET}"
echo "${DIM}Thư mục repo : $REPO_DIR${RESET}"
echo "${DIM}Thời gian    : $(date '+%Y-%m-%d %H:%M:%S %z')${RESET}"
echo ""

TOTAL_FAILURES=0

record_fail() {
    TOTAL_FAILURES=$((TOTAL_FAILURES + 1))
}

# ==============================================================================
# BƯỚC 1: BIÊN DỊCH RELEASE BINARY
# ==============================================================================
echo "${BOLD}${YELLOW}▶ [1/5] Biên dịch Release Binary (cargo build --release)...${RESET}"
if cargo build --release; then
    echo "  ${GREEN}✔ Build release thành công.${RESET}"
else
    echo "  ${RED}✘ Lỗi biên dịch release binary!${RESET}"
    exit 1
fi

# ==============================================================================
# BƯỚC 2: CHẠY UNIT & INTEGRATION TEST SUITES
# ==============================================================================
if [[ "$MODE" == "all" || "$MODE" == "features" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [2/5] Chạy Unit & Integration Test Suites (cargo test --release)...${RESET}"
    if cargo test --release -- --nocapture; then
        echo "  ${GREEN}✔ 100% Unit & Integration tests đã vượt qua.${RESET}"
    else
        echo "  ${RED}✘ Có test trong bộ cargo test bị thất bại!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# BƯỚC 3: ĐO BỘ NHỚ RAM (IDLE, 1W/3T, 2W/6T, CLEANUP)
# ==============================================================================
RAM_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "ram" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [3/5] Đo lường Bộ nhớ RAM (Idle, 1w/3t, 2w/6t, Cleanup)...${RESET}"
    echo "  ${DIM}Đang khởi động Litty ở chế độ RAM Benchmark...${RESET}"
    
    rm -f /tmp/litty_ram_bench.json
    
    # Run the RAM benchmark
    if "$BIN" +bench-ram; then
        if [[ -f /tmp/litty_ram_bench.json ]]; then
            RAM_JSON=$(cat /tmp/litty_ram_bench.json)
            IS_RAM_PASS=$(echo "$RAM_JSON" | grep -o '"passed":true' || true)
            if [[ -n "$IS_RAM_PASS" ]]; then
                RAM_PASSED="PASS"
                echo "  ${GREEN}✔ Kiểm tra RAM đạt chuẩn SLA.${RESET}"
            else
                RAM_PASSED="FAIL"
                echo "  ${RED}✘ Vi phạm ngân sách RAM SLA!${RESET}"
                record_fail
            fi
        else
            RAM_PASSED="FAIL"
            echo "  ${RED}✘ Không tìm thấy kết quả đo RAM!${RESET}"
            record_fail
        fi
    else
        RAM_PASSED="FAIL"
        echo "  ${RED}✘ Lỗi khi thực thi RAM benchmark!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# BƯỚC 4: ĐO ĐỘ TRỄ THAO TÁC & RENDER DỮ LIỆU LỚN (STRESS TEST)
# ==============================================================================
PERF_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "perf" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [4/5] Đo Độ trễ thao tác & Render Log Lớn (Latency & Stress)...${RESET}"
    if "$BIN" +bench-perf; then
        PERF_PASSED="PASS"
        echo "  ${GREEN}✔ Độ trễ và Render stress test đạt chuẩn SLA.${RESET}"
    else
        PERF_PASSED="FAIL"
        echo "  ${RED}✘ Độ trễ hoặc Throughput render không đạt chuẩn SLA!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# BƯỚC 5: KIỂM TRA CÁC FEATURE QUAN TRỌNG (FZF, REGEX, SPLITS, PASTE)
# ==============================================================================
FEAT_PASSED="SKIP"
if [[ "$MODE" == "all" || "$MODE" == "features" ]]; then
    echo ""
    echo "${BOLD}${YELLOW}▶ [5/5] Kiểm tra Features quan trọng (FZF search, Regex find, Splits, Security)...${RESET}"
    if "$BIN" +bench-features; then
        FEAT_PASSED="PASS"
        echo "  ${GREEN}✔ Toàn bộ các features quan trọng hoạt động chính xác và đạt SLA.${RESET}"
    else
        FEAT_PASSED="FAIL"
        echo "  ${RED}✘ Phát hiện regression ở một trong các feature cốt lõi!${RESET}"
        record_fail
    fi
fi

# ==============================================================================
# TỔNG HỢP VÀ XUẤT BÁO CÁO (RELEASE TEST REPORT)
# ==============================================================================
echo ""
echo "${BOLD}${CYAN}╔═══════════════════════════════════════════════════════════════════════╗${RESET}"
echo "${BOLD}${CYAN}║                  KẾT QUẢ ĐÁNH GIÁ TRƯỚC RELEASE                       ║${RESET}"
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
echo "  2. Độ trễ thao tác & Render Stress (20k logs) : $PERF_ICON"
echo "  3. Critical Features (FZF, Regex, Splits, Sec) : $FEAT_ICON"
echo ""

# Write Markdown report
cat <<EOF > "$REPORT_FILE"
# Litty Terminal — Báo Cáo Kiểm Tra Hiệu Năng & Độ Ổn Định Trước Release

- **Ngày chạy kiểm tra**: $(date '+%Y-%m-%d %H:%M:%S %z')
- **Nền tảng**: $(uname -s) $(uname -m) (Kernel $(uname -r))
- **Commit Git**: $(git rev-parse --short HEAD 2>/dev/null || echo "N/A")
- **Trạng thái**: $([ $TOTAL_FAILURES -eq 0 ] && echo "✅ **SẴN SÀNG RELEASE (ALL PASS)**" || echo "❌ **CHẶN RELEASE (CÓ LỖI/REGRESSION)**")

---

## 1. Kết Quả Đo Bộ Nhớ RAM (Memory Profile)
- **1 Window, 1 Tab (Idle Baseline)**: Đo footprint khi khởi động ở trạng thái nghỉ.
- **1 Window, 3 Tabs**: Đo mức độ mở rộng bộ nhớ khi mở đa tab trên một cửa sổ.
- **2 Windows, 6 Tabs (3 tabs mỗi window)**: Đo khả năng mở rộng đa cửa sổ và đa GPU swapchain.
- **Sau khi đóng tab & Cleanup**: Đảm bảo không có hiện tượng rò rỉ bộ nhớ (leak delta $\le$ 8 MB).
- **Trạng thái**: **$RAM_PASSED**

## 2. Kết Quả Đo Độ Trễ Thao Tác (Interactive Latency & I/O)
- **Độ trễ lệnh PTY (Command Execution Roundtrip)**:
  - Trung bình: $\le$ 1.0 ms
  - P99: $\le$ 3.0 ms
- **Độ trễ đọc file (File Reading Ingestion)**:
  - 4 KB Config: $\le$ 1.0 ms
  - 64 KB Source code: $\le$ 15.0 ms
  - 1 MB Data stream: $\ge$ 50 MB/s
- **Trạng thái**: **$PERF_PASSED**

## 3. Render Dữ Liệu Lớn & Kiểm Tra Giật Lag (Large Log Stress Test)
- **Khối lượng test**: 20,000 dòng log ANSI chuẩn server (ISO timestamps, colors, log levels, JSON payloads).
- **Thông lượng (Throughput)**: $\ge$ 20,000 dòng/giây.
- **Phát hiện giật lag (Frame Stalls > 16.6 ms)**: Không có nghẽn nghiêm trọng, Worst stall $\le$ 25 ms.
- **Phản hồi sau stress (Unblock Probe)**: $\le$ 1.0 ms (terminal phản hồi ngay lập tức, không bị đơ/treo).
- **Trạng thái**: **$PERF_PASSED**

## 4. Kiểm Tra Các Tính Năng Quan Trọng (Critical Features Regression)
- **FZF Fuzzy History Search**: Tìm kiếm trên 10,000 câu lệnh thực tế ($\le$ 1.5 ms / query, 100% chính xác).
- **Find in Terminal (Regex Search)**: Quét biểu thức chính quy trên 5,000 dòng scrollback ($\le$ 15 ms).
- **Split Panes Engine**: 1,000 thao tác chia dọc, chia ngang, điều chỉnh tỷ lệ, zoom và đóng pane không panic.
- **Bảo mật Bracketed Paste**: Triệt tiêu tấn công escape injection (\`\x1b[201~\`) và bao bọc shell quote an toàn.
- **Trạng thái**: **$FEAT_PASSED**

---
*Báo cáo được tạo tự động bởi \`scripts/pre-release-check.sh\`.*
EOF

echo "${DIM}Báo cáo chi tiết đã được lưu tại: ${RESET}${BOLD}$REPORT_FILE${RESET}"
echo ""

if [[ $TOTAL_FAILURES -eq 0 ]]; then
    echo "${BOLD}${GREEN}==================================================================${RESET}"
    echo "${BOLD}${GREEN}  ✔ TẤT CẢ TIÊU CHÍ ĐÃ ĐẠT CHUẨN! BẢN BUILD SẴN SÀNG ĐỂ RELEASE.  ${RESET}"
    echo "${BOLD}${GREEN}==================================================================${RESET}"
    echo ""
    exit 0
else
    echo "${BOLD}${RED}==================================================================${RESET}"
    echo "${BOLD}${RED}  ✘ PHÁT HIỆN $TOTAL_FAILURES LỖI / REGRESSION HIỆU NĂNG!             ${RESET}"
    echo "${BOLD}${RED}  CẦN ĐƯỢC FIX NGAY TRƯỚC KHI RELEASE. CHẶN TIẾN TRÌNH RELEASE!   ${RESET}"
    echo "${BOLD}${RED}==================================================================${RESET}"
    echo ""
    exit 1
fi
