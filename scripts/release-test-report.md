# Litty Terminal — Báo Cáo Kiểm Tra Hiệu Năng & Độ Ổn Định Trước Release

- **Ngày chạy kiểm tra**: 2026-10-29 21:47:45 +0700
- **Nền tảng**: Darwin arm64 (Kernel 27.2.0)
- **Commit Git**: 5b7dcda
- **Trạng thái**: ✅ **SẴN SÀNG RELEASE (ALL PASS)**

---

## 1. Kết Quả Đo Bộ Nhớ RAM (Memory Profile)
- **1 Window, 1 Tab (Idle Baseline)**: Đo footprint khi khởi động ở trạng thái nghỉ.
- **1 Window, 3 Tabs**: Đo mức độ mở rộng bộ nhớ khi mở đa tab trên một cửa sổ.
- **2 Windows, 6 Tabs (3 tabs mỗi window)**: Đo khả năng mở rộng đa cửa sổ và đa GPU swapchain.
- **Sau khi đóng tab & Cleanup**: Đảm bảo không có hiện tượng rò rỉ bộ nhớ (leak delta $\le$ 8 MB).
- **Trạng thái**: **PASS**

## 2. Kết Quả Đo Độ Trễ Thao Tác (Interactive Latency & I/O)
- **Độ trễ lệnh PTY (Command Execution Roundtrip)**:
  - Trung bình: $\le$ 1.0 ms
  - P99: $\le$ 3.0 ms
- **Độ trễ đọc file (File Reading Ingestion)**:
  - 4 KB Config: $\le$ 1.0 ms
  - 64 KB Source code: $\le$ 15.0 ms
  - 1 MB Data stream: $\ge$ 50 MB/s
- **Trạng thái**: **PASS**

## 3. Render Dữ Liệu Lớn & Kiểm Tra Giật Lag (Large Log Stress Test)
- **Khối lượng test**: 20,000 dòng log ANSI chuẩn server (ISO timestamps, colors, log levels, JSON payloads).
- **Thông lượng (Throughput)**: $\ge$ 20,000 dòng/giây.
- **Phát hiện giật lag (Frame Stalls > 16.6 ms)**: Không có nghẽn nghiêm trọng, Worst stall $\le$ 25 ms.
- **Phản hồi sau stress (Unblock Probe)**: $\le$ 1.0 ms (terminal phản hồi ngay lập tức, không bị đơ/treo).
- **Trạng thái**: **PASS**

## 4. Kiểm Tra Các Tính Năng Quan Trọng (Critical Features Regression)
- **FZF Fuzzy History Search**: Tìm kiếm trên 10,000 câu lệnh thực tế ($\le$ 1.5 ms / query, 100% chính xác).
- **Find in Terminal (Regex Search)**: Quét biểu thức chính quy trên 5,000 dòng scrollback ($\le$ 15 ms).
- **Split Panes Engine**: 1,000 thao tác chia dọc, chia ngang, điều chỉnh tỷ lệ, zoom và đóng pane không panic.
- **Bảo mật Bracketed Paste**: Triệt tiêu tấn công escape injection (`\x1b[201~`) và bao bọc shell quote an toàn.
- **Trạng thái**: **PASS**

---
*Báo cáo được tạo tự động bởi `scripts/pre-release-check.sh`.*
