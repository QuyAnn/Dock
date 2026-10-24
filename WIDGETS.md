# Widget bổ sung

## Đợt chỉnh giao diện theo ảnh tham chiếu

Đã vẽ lại **44/45 widget** trong dock và gallery bằng Direct2D/DirectWrite.
`gpu` giữ giao diện cũ vì không có ảnh GPU tương ứng. Không thêm tài sản LumaBar;
`.design-ref/` vẫn được gitignore và không được ứng dụng đọc/giải mã/đóng gói.
Ảnh render kiểm tra nằm trong `target/visual-audit/` (96 và 192 DPI, gallery từng nhóm).

| Widget SmartDock | Ảnh mẫu chính (bỏ đuôi `-dark.webp`) |
| --- | --- |
| `clock` | `clock`; khi gộp thời tiết: `clock-and-weather`; kiểu rộng: `today` |
| `date`, `daynum` | `date`, `day-number` |
| `analog`, `flipclock`, `segclock`, `worldclock` | `analog-classic`, `flip-clock`, `segment-clock`, `world-clock` |
| `dayprog`, `monthprog`, `yearprog` | `day-progress`, `month-progress`, `year-progress` |
| `focus`, `pomodoro`, `timer`, `stopwatch` | `pomodoro` (hai bộ tập trung), `timer`, `stopwatch` |
| `git`, `todo`, `notes`, `downloads` | `github-activity`, `todos`, `notes`, `downloads` |
| `cpu`, `ram`, `battery`, `net`, `disk` | `cpu`, `ram`, `battery`, `network`, `storage` |
| `claude`, `codex`, `cursor`, `grok` | `claude-plan-usage`, `chatgpt-usage`, `cursor-usage`, `grok-usage-dark-C-b1PNvn.webp` |
| `media` | `now-playing` |
| `weather`, `sun`, `hourly`, `forecast` | `weather`, `sunrise-and-sunset`, `hourly`, `weather-forecast` |
| `theme`, `brightness`, `recyclebin`, `screenshot`, `recentshots`, `search` | `theme-toggle`, `brightness`, `recycle-bin`, `screenshot-capture`, `recent-screenshots`, `search` |
| `metric`, `currency`, `screentime` | `metric`, `currency`, `screen-time` |
| `cc`, `chevron`, `lang` | `quick-toggles`, `tray`, `language-dark-aFiJF-sI.webp` |

Biểu đồ mạng dùng tối đa 48 số đo thật và ngắt chuỗi khi mất dữ liệu; thanh lưu trữ
dùng dung lượng tổng/trống của ổ C. Đường tiến độ media dùng timeline SMTC nếu có.
Badge Bitcoin hiển thị thay đổi giữa hai lần tải thành công, chưa có lần trước thì `—`;
không giả định đây là biến động 24 giờ. Screen Time dùng các tổng thời gian ứng dụng
đang được theo dõi cục bộ. Popup thời tiết dùng lại cùng bộ vẽ; ảnh chụp popup cũng bo góc.
Không có ảnh nào trong bộ mẫu thể hiện một popup độc lập để đối chiếu toàn bộ thẻ popup.

Các mẫu **không có widget độc lập tương ứng** trong 45 ID hiện tại:

- `7-days`, `commands`, `day-timeline`, `recently-opened`, `wallpaper-gallery`,
  `system-stats` (thẻ CPU/RAM gộp), `temperature`.
- `apps`, `audio-output`, `calendar`: có vùng ứng dụng, âm lượng và lịch trong SmartDock,
  nhưng không có widget riêng với bố cục/nội dung của các mẫu này.
- `datafast-chart`, `dodo-chart`, `dodo-payments`, `dub`, `gumroad-chart`, `gumroad`,
  `lemon-squeezy-chart`, `lemon-squeezy`, `paddle-chart`, `paddle`, `plausible`,
  `polar-chart`, `polar`, `resend`, `shopify-chart`, `shopify`, `stripe-chart`, `stripe`,
  `supabase`, `total-revenue`.
- `deepseek-balance`, `gemini-usage`, `glm-usage`, `opencode-plan-usage`, `opencode-usage`.
- `gmail`, `instagram`, `leetcode-stats`, `linear`, `stock-chart`, `stock`,
  `stock-watchlist`, `tiktok`, `x-profile`, `youtube`.

Biến thể của widget đã có, chưa có lựa chọn kiểu riêng: `analog-classic-dark`
(cả bản dark/light), `analog-minimal`, `claude-usage`, `github`, `github-overview`,
`now-playing-2`, `now-playing-bar`, `now-playing-mini`, `today-and-forecast`,
`weather-details`, `weather-status`, `world-clocks`, `world-time`.

Tệp thay đổi trong đợt này: `src/ui/widget_design.rs` (mới), `src/ui/dock.rs`,
`src/ui/more_widgets.rs`, `src/ui/renderer.rs`, `src/ui/tiles.rs`, `src/ui/mod.rs`,
`src/main.rs`, `src/model.rs`, `src/data/widgets.rs`, `src/data/finance.rs`,
`src/win/system.rs`, `WIDGETS.md`.

Kiểm tra: `cargo build` debug thành công; `cargo test`: **58 passed, 0 failed**, gồm 56 test cũ và 2 test mới
(vẽ 44 widget/gallery ở hai mức DPI; giới hạn/ngắt lịch sử mạng).
Chưa xác nhận khớp từng pixel: trình xem ảnh gốc làm một phần chữ trắng khó đọc;
công cụ ghép nền tối bị bộ duyệt tự động từ chối thực thi do lỗi cấu hình 403
(`codex-auto-review` không khả dụng). Không xử lý lại ảnh tham chiếu qua đường khác.

---

Mở **Thư viện widget…** trong menu chuột phải của dock để xem preview trực tiếp,
bật/tắt hoặc kéo widget sang vị trí khác. 23 widget mới mặc định tắt; cấu hình cũ
và widget cũ được giữ lại. Nhãn và mã nguồn dùng UTF-8.

| ID | Thao tác |
| --- | --- |
| `pomodoro` | Bấm phần giờ để bắt đầu/tạm dừng; nút `»` bỏ qua giai đoạn. 25 phút tập trung / 5 phút nghỉ; tự chuyển vòng. Menu chuột phải có đặt lại. |
| `stopwatch` | Bấm để bắt đầu/tạm dừng; chuột phải ghi vòng và mở popup. Popup có tổng giờ, thời gian từng vòng, phân trang và đặt lại; tối đa 100 vòng. |
| `notes` | Bấm để sửa ghi chú. Enter/nút Lưu để lưu, Shift+Enter xuống dòng, Ctrl+A chọn toàn bộ, Ctrl+V dán; tối đa 1.000 ký tự. Escape đóng, bỏ bản sửa chưa lưu. |
| `dayprog`, `monthprog`, `yearprog` | Tiến độ lịch theo giờ địa phương; năm dùng một chấm mỗi ngày, có xử lý năm nhuận. Bấm mở lịch. |
| `theme` | Bấm đổi cả AppsUseLightTheme và SystemUsesLightTheme trong HKCU Personalize; thông báo thay đổi tới Windows. |
| `brightness` | Bấm mở slider WMI cho màn hình tích hợp. Nếu không có WmiMonitorBrightness hoạt động thì widget bị ẩn trên dock, không thể bật từ gallery. Không dùng giá trị độ sáng giả. |
| `recyclebin` | Số mục và tổng dung lượng qua SHQueryRecycleBinW. Bấm dọn; Windows hiện xác nhận trước khi xóa. |
| `screenshot` | Mở Windows Snipping bằng `ms-screenclip:`. |
| `recentshots` | Ba ảnh mới nhất trong thư mục Screenshots của Windows, gồm đường dẫn Pictures được chuyển sang OneDrive. Bấm thumbnail để mở; chuột phải xem popup lớn. |
| `search` | Nhập từ khóa để tìm web bằng trình duyệt mặc định, hoặc đường dẫn thư mục để mở. Không thực thi chuỗi nhập như lệnh. Nút Windows Search ở giữa dock vẫn giữ nguyên. |
| `worldclock` | Ba thành phố mặc định Tokyo, London, New York. Bấm sửa danh sách theo dạng `Tokyo=9; London=0; New York=-5`; số là UTC offset theo giờ, hỗ trợ 5.5, 5.75. Offset cố định, cần sửa khi đổi DST. |
| `analog`, `flipclock`, `segclock`, `daynum` | Đồng hồ kim, bảng số lật, LED bảy đoạn và lịch ngày, vẽ bằng Direct2D. Bấm mở lịch. |
| `sun`, `hourly`, `forecast` | Vị trí mặt trời trong cung bình minh–hoàng hôn, 6 giờ tiếp theo và 4 ngày tiếp theo. Dùng chung địa điểm, giờ của địa điểm và lần tải Open-Meteo với widget weather. Bấm xem nguồn/thời gian cập nhật. |
| `metric` | BTC/USD từ Coinbase public spot API; tải mỗi 60 giây. Khi lỗi giữ giá cuối và báo dữ liệu cũ. |
| `currency` | Nhập ví dụ `100 USD VND`, hỗ trợ các mã có trong API. Open Access ExchangeRate-API, tải lại mỗi giờ; popup hiện nguồn và thời gian tải. |
| `screentime` | Chỉ theo dõi khi widget bật: thời gian foreground hôm nay theo file exe, bỏ khoảng ngủ/suspend và ngừng tính khi không thao tác quá 5 phút. Bấm xem các app dùng nhiều nhất. |

Thiết lập, ghi chú và cấu hình thành phố/tiền tệ nằm trong
`%APPDATA%\SmartDock\settings.json`. Thời gian ứng dụng nằm riêng tại
`%APPDATA%\SmartDock\screentime.json`, lưu mỗi 15 giây và khi thoát.
Pomodoro, stopwatch và các vòng giữ trong phiên chạy hiện tại, dùng đồng hồ
monotonic; thay đổi giờ Windows không làm sai bộ đếm.

Các thao tác WMI, tải mạng và giải mã thumbnail chạy ở luồng nền. Không có API
trả phí, API key, hình ảnh hoặc mã nguồn từ LumaBar. Kiểm thử thuần chạy bằng
`cargo test`; dùng `cargo build` để tạo bản debug, tránh khóa bản release đang chạy.

## Tệp thay đổi

- `src/main.rs`, `src/settings.rs`
- `src/ui/gallery.rs`, `src/ui/dock.rs`, `src/ui/appmenu.rs`, `src/ui/mod.rs`, `src/ui/more_widgets.rs`
- `src/data/mod.rs`, `src/data/usage.rs`, `src/data/weather.rs`, `src/data/productivity.rs`, `src/data/calendar_progress.rs`, `src/data/finance.rs`, `src/data/screentime.rs`
- `src/win/mod.rs`, `src/win/shellinfo.rs`, `src/win/wallpaper.rs`, `src/win/widget_tools.rs`
- `WIDGETS.md`

## Kiểm chứng

`cargo build` debug thành công; `cargo test`: **56 passed, 0 failed** (41 kiểm thử
cũ và 15 kiểm thử mới). Có kiểm thử vẽ tile/popup qua Direct2D và DirectWrite trong
cửa sổ ẩn, không tạo AppBar. `git diff --check` sạch; mọi tệp thay đổi đọc được
bằng bộ giải mã UTF-8 nghiêm ngặt.

Chưa kiểm chứng API trực tiếp và phần cứng WMI trong phiên này: sandbox không
kết nối được máy chủ API, WMI báo Access denied. Yêu cầu kiểm tra chỉ đọc ngoài
sandbox không được thực thi do bộ duyệt tự động lỗi 403 (model codex-auto-review
không khả dụng). Các nhánh lỗi, parser, lưu cấu hình và hình học hiển thị đã được
kiểm thử ngoại tuyến; chưa thử đổi theme, thay độ sáng, dọn thùng rác hoặc mở Snipping
trên phiên desktop thật của người dùng.
