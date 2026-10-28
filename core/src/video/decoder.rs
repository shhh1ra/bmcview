//! Декодер кадра: поток блоков 64×64 → пиксели блоков.
//!
//! Порт `com.library.decoder.ImageDecoder.decodeRLEorJPEG1` и `decodeRle`.
//! Цикл двухцветного RLE восстановлен по байткоду: декомпиляция CFR в этом месте
//! неверна (см. `docs/devlog.md`, запись 2026-10-28, и `docs/decodeRle.bytecode.txt`).
//!
//! Заголовок блока — один байт: `zipType = b >> 5`, `rZipType = (b >> 2) & 7`.
//!
//! | zipType | Смысл |
//! |---|---|
//! | 0, 1 | RLE (1 — крайний блок, обрезается по краю экрана) |
//! | 2, 3 | JPEG (3 — крайний): `len(2, BE)` + энтропийные данные |
//! | 4    | блок не изменился |
//! | 5    | копия блока сверху |
//! | 6    | копия блока слева |
//!
//! Крайние блоки хранятся целиком (64×64), обрезку делает отрисовка: рисовать блок
//! в его позиции с отсечением по размеру экрана — ровно то, что делает `ImagePane`.
//!
//! Отличие от оригинала: кадр применяется атомарно. Оригинал при ошибке посреди кадра
//! оставляет уже обновлённые блоки; здесь при ошибке состояние не меняется.

use std::sync::Arc;

use super::color::ycbcr_to_rgb;
use super::jpeg;

pub const BLOCK_SIZE: usize = 64;
pub const BLOCK_PIXELS: usize = BLOCK_SIZE * BLOCK_SIZE;

/// Пиксели блока, `0x00RRGGBB`, построчно.
pub type BlockPixels = [u32; BLOCK_PIXELS];

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("поток блоков оборвался (блок {block}, смещение {offset})")]
    Truncated { block: usize, offset: usize },
    #[error("блоков в потоке больше, чем на экране ({0})")]
    TooManyBlocks(usize),
    #[error("неизвестный тип блока {zip_type}/{rle_type} (блок {block})")]
    UnknownType { block: usize, zip_type: u8, rle_type: u8 },
    #[error("блок {block} ссылается на отсутствующего соседа")]
    MissingNeighbour { block: usize },
    #[error("RLE блока {block} выходит за 64×64")]
    RleOverflow { block: usize },
    #[error("уровень качества JPEG не задан")]
    NoQuality,
    #[error("JPEG блока {block}: {source}")]
    Jpeg { block: usize, source: jpeg::JpegError },
}

/// Состояние одного блока. Пиксели разделяются между блоками при копировании.
#[derive(Debug, Clone)]
struct Block {
    pixels: Arc<BlockPixels>,
    /// RLE-тип (0..=3) для наследования палитры; у JPEG-блоков 0, как в оригинале.
    rle_type: u8,
    /// Палитра (сырые байты YCbCr). У скопированных блоков отсутствует — как в оригинале.
    palette: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct Decoder {
    width: usize,
    height: usize,
    cols: usize,
    blocks: Vec<Option<Block>>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Левый верхний угол блока на экране.
    pub fn block_origin(&self, index: usize) -> (usize, usize) {
        ((index % self.cols) * BLOCK_SIZE, (index / self.cols) * BLOCK_SIZE)
    }

    pub fn block_pixels(&self, index: usize) -> Option<&BlockPixels> {
        self.blocks.get(index)?.as_ref().map(|b| &*b.pixels)
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.cols = width.div_ceil(BLOCK_SIZE);
        let rows = height.div_ceil(BLOCK_SIZE);
        self.blocks = vec![None; self.cols * rows];
    }

    /// Декодирует собранный кадр (`AssembledFrame::data`) и возвращает индексы
    /// изменившихся блоков. При смене разрешения все блоки сбрасываются.
    pub fn decode(
        &mut self,
        data: &[u8],
        width: usize,
        height: usize,
        quality_index: Option<usize>,
    ) -> Result<Vec<usize>, DecodeError> {
        if width != self.width || height != self.height {
            self.resize(width, height);
        }
        let mut next = self.blocks.clone();
        let mut changed = Vec::new();
        let mut pos = 1; // data[0] — diff-флаг
        let mut index = 0;
        while pos < data.len() {
            if index >= next.len() {
                return Err(DecodeError::TooManyBlocks(next.len()));
            }
            let header = data[pos];
            let zip_type = header >> 5;
            let rle_type = (header >> 2) & 7;
            let (consumed, block) = match zip_type {
                0 | 1 => self.decode_rle_block(&next, data, pos, index, rle_type)?,
                2 | 3 => {
                    let len = read_u16(data, pos + 1, index)?;
                    let entropy = slice(data, pos + 3, len, index)?;
                    let quality = quality_index.ok_or(DecodeError::NoQuality)?;
                    let jpeg = jpeg::synthesize(quality, entropy).ok_or(DecodeError::NoQuality)?;
                    let pixels = jpeg::decode(&jpeg).map_err(|source| DecodeError::Jpeg { block: index, source })?;
                    let block = Block { pixels: Arc::from(pixels), rle_type: 0, palette: None };
                    (3 + len, Some(block))
                }
                4 => (1, None),
                5 => (1, Some(copy_of(neighbour(&next, index, self.cols, Above)?))),
                6 => (1, Some(copy_of(neighbour(&next, index, self.cols, Left)?))),
                _ => return Err(DecodeError::UnknownType { block: index, zip_type, rle_type }),
            };
            if let Some(block) = block {
                next[index] = Some(block);
                changed.push(index);
            }
            pos += consumed;
            index += 1;
        }
        self.blocks = next;
        Ok(changed)
    }

    /// zipType 0/1. Возвращает (длину блока в потоке, новый блок).
    fn decode_rle_block(
        &self,
        blocks: &[Option<Block>],
        data: &[u8],
        pos: usize,
        index: usize,
        rle_type: u8,
    ) -> Result<(usize, Option<Block>), DecodeError> {
        let dir = match rle_type {
            0..=3 => return decode_rle(data, pos, index, rle_type, None).map(|(n, b)| (n, Some(b))),
            4 | 5 => Left,
            6 | 7 => Above,
            _ => unreachable!("rle_type — 3 бита"),
        };
        let src = neighbour(blocks, index, self.cols, dir)?;
        let reuse_as_is = rle_type == 4 || rle_type == 6;
        if reuse_as_is && src.rle_type == 0 {
            // «Сплошной» сосед: копируется изображение, палитра не наследуется.
            return Ok((1, Some(copy_of(src))));
        }
        let mut palette = src.palette.clone().ok_or(DecodeError::MissingNeighbour { block: index })?;
        if !reuse_as_is && src.rle_type == 1 {
            // 5/7 для двухцветного соседа: цвета меняются местами, хвост палитры обнуляется.
            if palette.len() < 6 {
                return Err(DecodeError::Truncated { block: index, offset: pos });
            }
            let swapped = [palette[3], palette[4], palette[5], palette[0], palette[1], palette[2]];
            palette.iter_mut().for_each(|b| *b = 0);
            palette[..6].copy_from_slice(&swapped);
        }
        decode_rle(data, pos, index, src.rle_type, Some(palette)).map(|(n, b)| (n, Some(b)))
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Left,
    Above,
}
use Direction::{Above, Left};

fn neighbour(blocks: &[Option<Block>], index: usize, cols: usize, dir: Direction) -> Result<&Block, DecodeError> {
    let src = match dir {
        Left => index.checked_sub(1),
        Above => index.checked_sub(cols),
    };
    src.and_then(|i| blocks.get(i)?.as_ref())
        .ok_or(DecodeError::MissingNeighbour { block: index })
}

/// Копия блока соседа: изображение и RLE-тип, без палитры (как `new ImageBlock()` в оригинале).
fn copy_of(src: &Block) -> Block {
    Block { pixels: Arc::clone(&src.pixels), rle_type: src.rle_type, palette: None }
}

fn read_u16(data: &[u8], offset: usize, block: usize) -> Result<usize, DecodeError> {
    match data.get(offset..offset + 2) {
        Some(b) => Ok(((b[0] as usize) << 8) | b[1] as usize),
        None => Err(DecodeError::Truncated { block, offset }),
    }
}

fn slice(data: &[u8], offset: usize, len: usize, block: usize) -> Result<&[u8], DecodeError> {
    data.get(offset..offset + len).ok_or(DecodeError::Truncated { block, offset })
}

fn color_at(palette: &[u8], i: usize, block: usize) -> Result<u32, DecodeError> {
    match palette.get(i * 3..i * 3 + 3) {
        Some(c) => Ok(ycbcr_to_rgb(c[0], c[1], c[2])),
        None => Err(DecodeError::Truncated { block, offset: i * 3 }),
    }
}

/// `decodeRle`. `palette = None` — палитра читается из потока, иначе наследуется.
fn decode_rle(
    data: &[u8],
    pos: usize,
    block: usize,
    rle_type: u8,
    palette: Option<Vec<u8>>,
) -> Result<(usize, Block), DecodeError> {
    let inherited = palette.is_some();
    let coefficient = if inherited { 0 } else { 1 };
    let palette = match palette {
        Some(p) => p,
        None => {
            let start = if rle_type == 0 { pos + 1 } else { pos + 3 };
            if start > data.len() {
                return Err(DecodeError::Truncated { block, offset: start });
            }
            let len = (data.len() - start).min(12);
            data[start..start + len].to_vec()
        }
    };
    let mut pixels = Box::new([0u32; BLOCK_PIXELS]);

    let consumed = match rle_type {
        0 => {
            pixels.fill(color_at(&palette, 0, block)?);
            4
        }
        1 => {
            let c1 = color_at(&palette, 0, block)?;
            let c2 = color_at(&palette, 1, block)?;
            let len = read_u16(data, pos + 1, block)?;
            let bits = slice(data, pos + 3 + 6 * coefficient, len, block)?;
            decode_two_colour(bits, c1, c2, &mut pixels, block)?;
            3 + 6 * coefficient + len
        }
        2 | 3 => {
            let colours = [
                color_at(&palette, 0, block)?,
                color_at(&palette, 1, block)?,
                color_at(&palette, 2, block)?,
                color_at(&palette, 3, block)?,
            ];
            let palette_len = if rle_type == 2 { 3 } else { 4 };
            let len = read_u16(data, pos + 1, block)?;
            let runs = slice(data, pos + 3 + palette_len * 3 * coefficient, len, block)?;
            let mut filled = 0;
            for &b in runs {
                let run = ((b & 0xFC) >> 2) as usize + 1;
                fill(&mut pixels, &mut filled, run, colours[(b & 3) as usize], block)?;
            }
            3 + palette_len * 3 * coefficient + len
        }
        _ => unreachable!("decode_rle вызывается только для типов 0..=3"),
    };

    Ok((consumed, Block { pixels: Arc::from(pixels), rle_type, palette: Some(palette) }))
}

fn fill(pixels: &mut BlockPixels, filled: &mut usize, run: usize, colour: u32, block: usize) -> Result<(), DecodeError> {
    let end = *filled + run;
    if end > BLOCK_PIXELS {
        return Err(DecodeError::RleOverflow { block });
    }
    pixels[*filled..end].fill(colour);
    *filled = end;
    Ok(())
}

/// Двухцветный RLE. Битовый поток, старшие биты первыми: 6 бит — длина серии минус 1;
/// если серия равна 64, следует ещё бит «не переключать цвет». По умолчанию цвет
/// чередуется c1 ↔ c2.
///
/// Логика окончания повторяет байткод оригинала: когда байты кончились, а в буфере
/// ещё остались биты (`tmpdata != 0` или `lastnum != 0` при незаполненном блоке),
/// цикл продолжает разбирать их без чтения новых байт.
fn decode_two_colour(bits: &[u8], c1: u32, c2: u32, pixels: &mut BlockPixels, block: usize) -> Result<(), DecodeError> {
    let len = bits.len() as i64;
    let mut m: i64 = 0;
    let mut tmp: i32 = 0;
    let mut available: i32 = 0;
    let mut tail = false;
    let mut filled = 0usize;
    let mut colour = c1;

    while m < len {
        let mut cursor = m;
        if !tail && available < 8 {
            tmp = (tmp & 0xFFFF) | ((bits[cursor as usize] as i32) << (8 - available));
            available += 8;
            cursor += 1;
        }
        let run = (((tmp & 0xFC00) >> 10) + 1) as usize;
        tmp = (tmp << 6) & 0xFFFF;
        available -= 6;
        let keep_colour = if run < 64 {
            false
        } else {
            let flag = tmp & 0x8000 != 0;
            tmp = (tmp << 1) & 0xFFFF;
            available -= 1;
            flag
        };
        fill(pixels, &mut filled, run, colour, block)?;
        if !keep_colour {
            colour = if colour == c1 { c2 } else { c1 };
        }
        if tail || cursor >= len {
            if tail {
                cursor += 1;
                tail = false;
            }
            if tmp != 0 || (available != 0 && filled < BLOCK_PIXELS) {
                tail = true;
                cursor -= 1;
            }
        }
        m = cursor;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREY: [u8; 3] = [128, 128, 128];
    const WHITE: [u8; 3] = [255, 128, 128];
    const BLACK: [u8; 3] = [0, 128, 128];

    fn frame(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0u8];
        for b in blocks {
            v.extend_from_slice(b);
        }
        v
    }

    fn solid(c: [u8; 3]) -> Vec<u8> {
        vec![0x00, c[0], c[1], c[2]]
    }

    /// Блок zipType 0, rZipType 2/3: палитра + серии `(run-1)<<2 | idx`.
    fn palette_block(colours: &[[u8; 3]], runs: &[(usize, u8)]) -> Vec<u8> {
        let rle_type = if colours.len() == 3 { 2 } else { 3 };
        let mut body = Vec::new();
        for &(mut run, idx) in runs {
            while run > 0 {
                let r = run.min(64);
                body.push((((r - 1) as u8) << 2) | idx);
                run -= r;
            }
        }
        let mut v = vec![rle_type << 2];
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        for c in colours {
            v.extend_from_slice(c);
        }
        v.extend(body);
        v
    }

    /// Кодировщик двухцветного RLE (обратный к `decode_two_colour`), только для тестов.
    fn two_colour_block(c1: [u8; 3], c2: [u8; 3], runs: &[(usize, bool)]) -> Vec<u8> {
        let mut bits = String::new();
        for &(run, keep) in runs {
            bits.push_str(&format!("{:06b}", run - 1));
            if run == 64 {
                bits.push(if keep { '1' } else { '0' });
            }
        }
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        let body: Vec<u8> = bits.as_bytes().chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        let mut v = vec![1 << 2];
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(&c1);
        v.extend_from_slice(&c2);
        v.extend(body);
        v
    }

    fn rgb(c: [u8; 3]) -> u32 {
        ycbcr_to_rgb(c[0], c[1], c[2])
    }

    #[test]
    fn solid_blocks_and_geometry() {
        let mut d = Decoder::new();
        // 100×70 → 2×2 блока, нижний правый край обрезается отрисовкой.
        let changed = d.decode(&frame(&[solid(GREY), solid(WHITE), solid(BLACK), solid(GREY)]), 100, 70, None).unwrap();
        assert_eq!(changed, vec![0, 1, 2, 3]);
        assert_eq!(d.block_count(), 4);
        assert_eq!(d.block_origin(3), (64, 64));
        assert!(d.block_pixels(1).unwrap().iter().all(|&p| p == 0xFFFFFF));
    }

    #[test]
    fn unchanged_and_copy_blocks() {
        let mut d = Decoder::new();
        d.decode(&frame(&[solid(WHITE), solid(BLACK), solid(GREY), solid(GREY)]), 128, 128, None).unwrap();
        // 0: не изменился, 1: копия слева, 2: копия сверху, 3: не изменился.
        let changed = d.decode(&frame(&[vec![0x80], vec![0xC0], vec![0xA0], vec![0x80]]), 128, 128, None).unwrap();
        assert_eq!(changed, vec![1, 2]);
        assert!(d.block_pixels(1).unwrap().iter().all(|&p| p == 0xFFFFFF));
        assert!(d.block_pixels(2).unwrap().iter().all(|&p| p == 0xFFFFFF));
        assert!(d.block_pixels(3).unwrap().iter().all(|&p| p == rgb(GREY)));
    }

    #[test]
    fn palette_block_runs() {
        let mut d = Decoder::new();
        let block = palette_block(&[WHITE, BLACK, GREY], &[(100, 0), (3000, 1), (996, 2)]);
        d.decode(&frame(&[block]), 64, 64, None).unwrap();
        let px = d.block_pixels(0).unwrap();
        assert_eq!(px[99], 0xFFFFFF);
        assert_eq!(px[100], 0x000000);
        assert_eq!(px[3099], 0x000000);
        assert_eq!(px[3100], rgb(GREY));
        assert_eq!(px[4095], rgb(GREY));
    }

    #[test]
    fn two_colour_block_alternates_and_keeps_on_flag() {
        // 64 (оставить цвет) + 64 → обе серии белые, затем 3968 чёрных
        // (62 серии по 64 с переключением… проще: оставшееся одной цепочкой «оставить цвет»).
        let mut runs = vec![(64, true), (64, false)];
        let mut left = BLOCK_PIXELS - 128;
        while left > 64 {
            runs.push((64, true));
            left -= 64;
        }
        runs.push((left, false));
        let block = two_colour_block(WHITE, BLACK, &runs);
        let mut d = Decoder::new();
        d.decode(&frame(&[block]), 64, 64, None).unwrap();
        let px = d.block_pixels(0).unwrap();
        assert!(px[..128].iter().all(|&p| p == 0xFFFFFF));
        assert!(px[128..].iter().all(|&p| p == 0x000000));
    }

    #[test]
    fn two_colour_short_runs_alternate() {
        let mut runs = Vec::new();
        for _ in 0..(BLOCK_PIXELS / 32) {
            runs.push((16, false));
            runs.push((16, false));
        }
        let block = two_colour_block(WHITE, BLACK, &runs);
        let mut d = Decoder::new();
        d.decode(&frame(&[block]), 64, 64, None).unwrap();
        let px = d.block_pixels(0).unwrap();
        assert_eq!(px[0], 0xFFFFFF);
        assert_eq!(px[16], 0x000000);
        assert_eq!(px[32], 0xFFFFFF);
        assert_eq!(px[4095], 0x000000);
    }

    #[test]
    fn inherited_palette_from_left() {
        let first = palette_block(&[WHITE, BLACK, GREY, GREY], &[(4096, 1)]);
        // rZipType 4: палитра слева, в потоке только длина и серии.
        let mut second = vec![4 << 2, 0, 1];
        second.push(63 << 2); // 64 белых (индекс цвета 0)
        let mut tail = Vec::new();
        for _ in 0..63 {
            tail.push((63 << 2) | 2); // по 64 серых
        }
        second[1..3].copy_from_slice(&((1 + tail.len()) as u16).to_be_bytes());
        second.extend(tail);
        let mut d = Decoder::new();
        d.decode(&frame(&[first, second]), 128, 64, None).unwrap();
        let px = d.block_pixels(1).unwrap();
        assert_eq!(px[0], 0xFFFFFF);
        assert_eq!(px[64], rgb(GREY));
    }

    #[test]
    fn jpeg_block_uses_synthesized_header() {
        let entropy = super::jpeg::tests::flat_grey_entropy();
        let mut block = vec![2 << 5];
        block.extend_from_slice(&(entropy.len() as u16).to_be_bytes());
        block.extend(entropy);
        let mut d = Decoder::new();
        d.decode(&frame(&[block]), 64, 64, Some(4)).unwrap();
        let p = d.block_pixels(0).unwrap()[2000];
        assert!((127..=129).contains(&(p & 0xFF)));
    }

    #[test]
    fn errors_leave_state_untouched() {
        let mut d = Decoder::new();
        d.decode(&frame(&[solid(WHITE), solid(WHITE)]), 128, 64, None).unwrap();
        // Первый блок меняется, второй ссылается на несуществующего соседа сверху.
        let err = d.decode(&frame(&[solid(BLACK), vec![0xA0]]), 128, 64, None).unwrap_err();
        assert!(matches!(err, DecodeError::MissingNeighbour { block: 1 }));
        assert!(d.block_pixels(0).unwrap().iter().all(|&p| p == 0xFFFFFF));
    }

    #[test]
    fn too_many_blocks() {
        let mut d = Decoder::new();
        let err = d.decode(&frame(&[solid(GREY), solid(GREY)]), 64, 64, None).unwrap_err();
        assert!(matches!(err, DecodeError::TooManyBlocks(1)));
    }

    #[test]
    fn jpeg_without_quality_is_an_error() {
        let mut d = Decoder::new();
        let err = d.decode(&frame(&[vec![2 << 5, 0, 1, 0]]), 64, 64, None).unwrap_err();
        assert!(matches!(err, DecodeError::NoQuality));
    }
}
