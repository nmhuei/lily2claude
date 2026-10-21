# LILY2CLAUDE: AGENT RULES & ARCHITECTURAL INVARIANTS
> **BỘ QUY TẮC BẤT DI BẤT DỊCH CHO MỌI AI AGENT & DEVELOPER (NGHIÊM CẤM SỬA BẬY)**
> *Cập nhật ngày 21/10/2026 sau sự cố regression phiên bản 20:00 (commit 5c3fba5 là baseline chuẩn).*

---

## 1. BÀI HỌC SỰ CỐ & NGUYÊN NHÂN TỬ HUYỆT (FATAL INCIDENT)

### Hiện tượng lỗi (Symptom):
Claude Code rơi vào vòng lặp suy nghĩ độc thoại vô tận (*"Brewed for 8m 0s"*), in ra các đoạn văn bản độc thoại như:
> *"Let me check why ds08 is missing from the live pool and read the guest PoW wiring spec..."*  
> *"I'll check why ds08 isn't in the live pool before wiring the remaining piece..."*  
> *"Let me check the store reload behavior and where the pre-login verification-code endpoints are called..."*  

Sau đó dừng lại với `stop_reason: "end_turn"` mà **KHÔNG HỀ GỌI BẤT KỲ TOOL NÀO**. Claude Code kết thúc lượt và trả prompt về cho người dùng: `❯ tiếp tục đi`.

### Nguyên nhân cốt lõi (Root Cause):
Một agent đã tự ý "tối ưu hóa" bằng cách tạo ra `AcpToolMode::Native`, bỏ qua việc inject XML tool calling (`compile_tools` / `[Tools Instruction]`) vào prompt của ACP session, với giả định sai lầm rằng *"OpenCode sẽ tự xử lý native tools"*.

**Sự thật về kiến trúc hệ thống:**
1. **OpenCode bị cấm chạy tool:** Trong `~/.config/opencode/opencode.jsonc`, tất cả native tools của OpenCode đều được cấu hình là `"deny"`. OpenCode **CHỈ LÀM CON MỒI ĐỆM (GATEWAY/BRIDGE)** để lấy completion từ DeepSeek / MiniMax.
2. **Claude Code là bên DUY NHẤT sở hữu và thực thi tools:** Claude Code gửi danh sách tool qua trường `req.tools`. Claude Code yêu cầu model trả về `tool_use` để chính Claude Code thực thi trên máy host.
3. **Khi agent xóa bỏ XML tool injection:** Model upstream (DeepSeek/MiniMax) hoàn toàn **MÙ TOOL** (thấy danh sách tool rỗng 0 tool). Do không biết có tool gì để gọi, model buộc phải độc thoại bằng văn bản thuần (*"Tôi sẽ kiểm tra file X, tôi sẽ chạy lệnh Y..."*) rồi trả về `end_turn`.
4. Claude Code nhận được văn bản thuần không có `tool_use`, tưởng model đã xong việc, ngắt turn và đẩy câu hỏi về cho user!

---

## 2. QUY TẮC BẤT BIẾN (STRICT ARCHITECTURAL INVARIANTS)

### 🔴 QUY TẮC 1: BẮT BUỘC PHẢI COMPILE & INJECT TOOL SCHEMA BẰNG XML
- **TUYỆT ĐỐI KHÔNG ĐƯỢC PHÉP** bỏ `compile_tools` hoặc `[Tools Instruction]` trong `src/protocol/prompt.rs`.
- Mọi request có `req.tools` không rỗng **BẮT BUỘC** phải được compile thành định dạng XML `<tool_call>` / `<antml:invoke>` và inject vào system prompt.
- **NGHIÊM CẤM** tạo các chế độ `AcpToolMode::Native` chỉ truyền tools qua ACP API mà không inject XML vào prompt, trừ khi đã xác minh 100% downstream agent có tool execution = allow (điều này KHÔNG áp dụng cho OpenCode bridge hiện tại).

### 🔴 QUY TẮC 2: CLAUDE CODE NẮM TOÀN QUYỀN TOOL EXECUTION
- Upstream agent (OpenCode) KHÔNG ĐƯỢC PHÉP can thiệp thực thi tool.
- Luồng chuẩn:
  1. Claude Code gửi `req.tools`.
  2. `lily2claude` compile tools vào prompt XML.
  3. Model upstream sinh ra `<tool_call><tool_name>...</tool_name><parameters>...</parameters></tool_call>`.
  4. `lily2claude` phân giải (parse/repair) thẻ XML đó thành `tool_use` block chuẩn của Anthropic Messages API.
  5. Claude Code nhận `tool_use`, tự thực thi lệnh (Bash, Read, Edit, Glob,...) trên máy host.
  6. Claude Code gửi lại `tool_result` cho lượt tiếp theo.

### 🔴 QUY TẮC 3: PHÂN BIỆT RÕ LỖI QUOTA VÀ LỖI IP RATE LIMIT
- **Lỗi `FreeUsageLimitError`**: Đây là lỗi hết hạn ngạch tài khoản (account token quota) trong `auth.json`. **XOAY IP PROXY (WARP) KHÔNG CÓ TÁC DỤNG VỚI LỖI NÀY!** Không được cố xoay IP khi gặp lỗi này; phải dọn sạch token rác hoặc chờ reset hạn ngạch tài khoản.
- **Lỗi HTTP 429 / Cloudflare 403 / `FreeTierError`**: Đây mới là lỗi throttle IP egress. Docker WARP pool (test-warp-1, test-warp-2) sẽ tự động failover và xoay IP trong ~3 giây.

### 🔴 QUY TẮC 4: THƯ MỤC BUILD VÀ QUY TRÌNH DEPLOY
- Do dung lượng phân vùng root (`/`) có thể bị giới hạn, **LUÔN LUÔN** build với:
  ```bash
  CARGO_TARGET_DIR=/home/light/Downloads/ cargo build --release
  ```
- **Không dùng `cp` trực tiếp nếu có tiến trình đang chạy**: Gặp lỗi `Text file busy`. Sử dụng lệnh `install` để thay thế file binary an toàn (atomic inode replace):
  ```bash
  install -m 755 /home/light/Downloads/release/lily2claude /home/light/.cargo/bin/lily2claude
  ```
- Đường dẫn binary chuẩn: `/home/light/.cargo/bin/lily2claude` (symlinks: `~/.local/bin/lily2claude`, `~/.local/bin/l2c`).
- Khởi động lại service:
  ```bash
  lily2claude server restart --port 4001
  ```

### 🔴 QUY TẮC 5: BASELINE ỔN ĐỊNH LÀ COMMIT `5c3fba5`
- Trước khi thêm bất kỳ tính năng nào mới, chạy toàn bộ test suite để đảm bảo không phá vỡ 140+ test cases:
  ```bash
  CARGO_TARGET_DIR=/home/light/Downloads/ cargo test
  ```
- Nếu bất kỳ thay đổi nào làm hỏng test hoặc gây monologue ngắt quãng, **REVERT NGAY LẬP TỨC** về trạng thái clean của commit `5c3fba5`.
