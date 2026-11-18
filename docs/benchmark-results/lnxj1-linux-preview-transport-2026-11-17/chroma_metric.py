"""The fit-view chroma metric (TPX3/TPX5/TPX7): chroma PSD of sRGB-encoded values in five radial
bands (cycles/px) after a Hann window; ref() reduces a 16-bit export TIFF to a frame's size with
Lanczos in linear light (ImageMagick) and returns float sRGB."""
import subprocess, numpy as np
from PIL import Image
B = [(0.1, 0.2), (0.2, 0.3), (0.3, 0.4), (0.4, 0.5), (0.5, 0.75)]
def load(p):
    return np.asarray(Image.open(p).convert('RGB'), dtype=np.float64) / 255.0
def lin_srgb(v):
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * np.power(np.clip(v, 0, None), 1 / 2.4) - 0.055)
def ref(tif, w, h):
    raw = subprocess.run(['magick', tif, '-colorspace', 'RGB', '-filter', 'Lanczos', '-resize', f'{w}x{h}!',
                          '-depth', '16', '-endian', 'LSB', 'rgb:-'], check=True, capture_output=True).stdout
    return lin_srgb(np.frombuffer(raw, '<u2').reshape(h, w, 3).astype(np.float64) / 65535.0)
def chroma(a):
    y = a @ np.array([0.2126, 0.7152, 0.0722]); return (a[..., 2] - y) / 1.8556, (a[..., 0] - y) / 1.5748
def psd1(y):
    y = y[8:-8, 8:-8]; y = y - y.mean()
    P = np.abs(np.fft.fft2(y * np.outer(np.hanning(y.shape[0]), np.hanning(y.shape[1])))) ** 2
    r = np.sqrt(np.fft.fftfreq(y.shape[0])[:, None] ** 2 + np.fft.fftfreq(y.shape[1])[None, :] ** 2)
    return np.array([P[(r >= a) & (r < b)].mean() for a, b in B])
def cpsd(a):
    cb, cr = chroma(a); return psd1(cb) + psd1(cr)
