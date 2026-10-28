"""Synthetic test wallpapers for the Control Center renders (render-inputs/wp_*.jpg)."""
import numpy as np, cv2
W, H = 1280, 720
rng = np.random.default_rng(7)
yy, xx = np.mgrid[0:H, 0:W].astype(np.float32)
def save(name, img):
    cv2.imwrite(f"render-inputs/wp_{name}.jpg", cv2.cvtColor(np.clip(img, 0, 255).astype(np.uint8), cv2.COLOR_RGB2BGR), [cv2.IMWRITE_JPEG_QUALITY, 85])
def lerp(a, b, t): return np.array(a, np.float32) * (1 - t[..., None]) + np.array(b, np.float32) * t[..., None]
save("black", np.zeros((H, W, 3)))
save("white", np.full((H, W, 3), 255.0))
save("grey", np.full((H, W, 3), 128.0))
t = (xx / W * 0.7 + yy / H * 0.3)
col = np.where((t < 0.5)[..., None], lerp((255, 64, 160), (255, 140, 40), t / 0.5), lerp((255, 140, 40), (120, 40, 220), (t - 0.5) / 0.5))
save("colorful", col)
# night city: dark gradient, building blocks with lit windows, bokeh lights
img = lerp((10, 12, 30), (40, 20, 50), yy / H)
for _ in range(40):
    x0 = rng.integers(0, W); w = rng.integers(40, 140); h = rng.integers(150, 600)
    img[H - h:, x0:x0 + w] = rng.integers(15, 35)
    for _ in range(int(w * h / 400)):
        wx = rng.integers(x0, min(W, x0 + w)); wy = rng.integers(H - h, H)
        img[wy:wy + 5, wx:wx + 4] = (255, 220, 140) if rng.random() < 0.7 else (140, 200, 255)
for _ in range(120):
    c = (int(rng.integers(0, W)), int(rng.integers(0, H)))
    cv2.circle(img, c, int(rng.integers(6, 30)), tuple(float(v) for v in rng.choice([(255, 80, 60), (255, 200, 80), (80, 160, 255), (255, 255, 255)])), -1)
save("city", cv2.GaussianBlur(img, (0, 0), 1.5))
save("sky", lerp((40, 140, 230), (190, 225, 255), yy / H))
save("navy", lerp((12, 22, 56), (2, 4, 14), (xx / W + yy / H) / 2))
# nature: foliage noise in greens with sunny highlights
n = sum(cv2.resize(rng.random((H // s + 1, W // s + 1)).astype(np.float32), (W, H)) / (i + 1) for i, s in enumerate([160, 60, 20, 6]))
n = (n - n.min()) / (n.max() - n.min())
save("nature", lerp((20, 60, 20), (170, 210, 80), n ** 1.3))
# bloom: deep blue with soft folded light petals (Windows 11 style)
img = lerp((5, 20, 70), (0, 60, 160), yy / H)
for k, a in enumerate(np.linspace(-1.2, 1.2, 7)):
    m = np.zeros((H, W), np.float32)
    cv2.ellipse(m, (W // 2, int(H * 0.62)), (260, 70), float(np.degrees(a)) - 90, 0, 360, 1.0, -1)
    m = cv2.GaussianBlur(m, (0, 0), 25)
    img = img * (1 - m[..., None] * 0.8) + np.array((80 + k * 15, 160 + k * 8, 255), np.float32) * m[..., None] * 0.8
save("bloom", img)
