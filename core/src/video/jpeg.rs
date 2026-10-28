//! JPEG-блоки 64×64.
//!
//! BMC передаёт только энтропийно-кодированные данные. Клиент сам дописывает заголовок
//! (`com.library.decoder.JPEGData.createSynHeadData`): SOI+APP0, три DQT по индексу
//! качества, SOF0 64×64 4:4:4, стандартные DHT, DRI, SOS — и хвост `FFD9`.

use super::decoder::{BlockPixels, BLOCK_PIXELS, BLOCK_SIZE};
use super::tables;

/// Число уровней качества (индексы 0..=9 соответствуют качеству 10..=100).
pub const QUALITY_LEVELS: usize = tables::DQT_SETS.len();

/// Собирает полный JPEG-файл из энтропийных данных блока.
/// `None`, если индекс качества вне диапазона (в оригинале — `ArrayIndexOutOfBoundsException`).
pub fn synthesize(quality_index: usize, entropy: &[u8]) -> Option<Vec<u8>> {
    let dqt = tables::DQT_SETS.get(quality_index)?;
    let mut out = Vec::with_capacity(
        tables::HEAD_SYN_SOI_APPO.len()
            + dqt.iter().map(|t| t.len()).sum::<usize>()
            + tables::HEAD_SYN_SOF_444.len()
            + tables::HEAD_SYN_SOF_SOS.len()
            + entropy.len()
            + tables::TAIL.len(),
    );
    out.extend_from_slice(&tables::HEAD_SYN_SOI_APPO);
    for table in dqt {
        out.extend_from_slice(table);
    }
    out.extend_from_slice(&tables::HEAD_SYN_SOF_444);
    out.extend_from_slice(&tables::HEAD_SYN_SOF_SOS);
    out.extend_from_slice(entropy);
    out.extend_from_slice(&tables::TAIL);
    Some(out)
}

#[derive(Debug, thiserror::Error)]
pub enum JpegError {
    #[error("ошибка JPEG-декодера: {0}")]
    Decode(#[from] jpeg_decoder::Error),
    #[error("неожиданный формат блока: {width}×{height}, {format:?}")]
    Format {
        width: u16,
        height: u16,
        format: jpeg_decoder::PixelFormat,
    },
}

/// Декодирует синтезированный JPEG в пиксели блока.
pub fn decode(jpeg: &[u8]) -> Result<Box<BlockPixels>, JpegError> {
    let mut decoder = jpeg_decoder::Decoder::new(jpeg);
    let rgb = decoder.decode()?;
    let info = decoder.info().expect("info доступна после decode()");
    let size = BLOCK_SIZE as u16;
    if info.width != size || info.height != size || info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return Err(JpegError::Format {
            width: info.width,
            height: info.height,
            format: info.pixel_format,
        });
    }
    let mut pixels = Box::new([0u32; BLOCK_PIXELS]);
    for (dst, px) in pixels.iter_mut().zip(rgb.as_chunks::<3>().0) {
        *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
    }
    Ok(pixels)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Энтропийные данные блока, где все коэффициенты нулевые: каждый MCU (4:4:4) —
    /// Y: DC cat0 `00` + EOB `1010`, Cb и Cr: DC cat0 `00` + EOB `00`. Итог — ровно серый 128.
    pub(crate) fn flat_grey_entropy() -> Vec<u8> {
        let mcu = "00101000000000";
        let bits: String = mcu.repeat(BLOCK_PIXELS / 64);
        assert_eq!(bits.len() % 8, 0);
        bits.as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect()
    }

    #[test]
    fn synthesized_header_starts_with_soi_and_ends_with_eoi() {
        let jpeg = synthesize(4, &[0x12]).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9]);
        assert_eq!(jpeg[jpeg.len() - 3], 0x12);
    }

    #[test]
    fn quality_index_out_of_range() {
        assert!(synthesize(QUALITY_LEVELS, &[]).is_none());
    }

    #[test]
    fn decodes_flat_grey_block_at_every_quality() {
        for q in 0..QUALITY_LEVELS {
            let px = decode(&synthesize(q, &flat_grey_entropy()).unwrap()).unwrap();
            for &p in px.iter() {
                for shift in [16, 8, 0] {
                    let c = (p >> shift) & 0xFF;
                    assert!((127..=129).contains(&c), "качество {q}: канал {c}");
                }
            }
        }
    }
}
