//! Преобразование цвета для RLE-блоков.
//!
//! Точная копия `com.library.decoder.ColorConverter.ycbcr2rgb`: вычисления в `double`,
//! усечение к нулю (`(int)`), затем ограничение 0..=255. Повторяем побитово, чтобы
//! RLE-блоки совпадали с оригиналом пиксель в пиксель.

/// YCbCr (JFIF, полный диапазон) → `0x00RRGGBB`.
pub fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> u32 {
    let y = y as f64;
    let cb = cb as f64 - 128.0;
    let cr = cr as f64 - 128.0;
    let r = clamp((y + 1.402 * cr) as i32);
    let g = clamp((y - 0.34414 * cb - 0.71414 * cr) as i32);
    let b = clamp((y + 1.772 * cb) as i32);
    (r << 16) | (g << 8) | b
}

fn clamp(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_greys() {
        assert_eq!(ycbcr_to_rgb(0, 128, 128), 0x000000);
        assert_eq!(ycbcr_to_rgb(128, 128, 128), 0x808080);
        assert_eq!(ycbcr_to_rgb(255, 128, 128), 0xFFFFFF);
    }

    #[test]
    fn saturates_like_original() {
        // Y=255, Cr=255: R = 255 + 1.402*127 → 255; B = 255 + 1.772*(-128) = 28.2 → 28.
        assert_eq!(ycbcr_to_rgb(255, 0, 255) & 0xFF, 28);
        assert_eq!(ycbcr_to_rgb(255, 0, 255) >> 16, 255);
    }
}
