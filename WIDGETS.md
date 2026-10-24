# Widget bổ sung

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
