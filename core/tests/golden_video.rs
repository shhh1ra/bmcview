//! Сверка декодера с оригинальным Java-клиентом.
//!
//! Эталоны `golden/video.txt` построены `tools/oracle/gen-video-golden.js`: случайные кадры
//! прогнаны через оригинальный `ImageDecoder.decodeRLEorJPEG1`, для каждого блока записан
//! CRC32 пикселей. Здесь те же кадры декодирует порт, и CRC должны совпасть.

use ibmc_core::video::Decoder;

fn crc32(data: impl IntoIterator<Item = u8>) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Frame {
    line: usize,
    width: usize,
    height: usize,
    quality: usize,
    ok: bool,
    data: Vec<u8>,
    blocks: Vec<(usize, u32)>,
    reset_after: bool,
}

fn load() -> Vec<Frame> {
    let text = include_str!("golden/video.txt");
    let mut frames: Vec<Frame> = Vec::new();
    let (mut width, mut height) = (0, 0);
    for (n, line) in text.lines().enumerate() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["size", w, h] => {
                width = w.parse().unwrap();
                height = h.parse().unwrap();
                // Новая последовательность — новый декодер.
                if let Some(last) = frames.last_mut() {
                    last.reset_after = true;
                }
            }
            ["frame", q, s, hex] => frames.push(Frame {
                line: n + 1,
                width,
                height,
                quality: q.parse().unwrap(),
                ok: *s == "0",
                data: unhex(hex),
                blocks: Vec::new(),
                reset_after: false,
            }),
            ["block", i, crc] => frames
                .last_mut()
                .unwrap()
                .blocks
                .push((i.parse().unwrap(), u32::from_str_radix(crc, 16).unwrap())),
            ["reset"] => frames.last_mut().unwrap().reset_after = true,
            [] => {}
            other => panic!("строка {}: непонятная запись {other:?}", n + 1),
        }
    }
    frames
}

#[test]
fn decoder_matches_original_java_client() {
    let frames = load();
    assert!(frames.len() > 50, "эталонов подозрительно мало: {}", frames.len());
    let mut decoder = Decoder::new();
    let mut checked_blocks = 0;
    for f in &frames {
        let result = decoder.decode(&f.data, f.width, f.height, Some(f.quality));
        match (f.ok, &result) {
            (true, Err(e)) => panic!("строка {}: оригинал декодировал кадр, порт — нет: {e}", f.line),
            (false, Ok(_)) => panic!("строка {}: оригинал выдал исключение, порт кадр принял", f.line),
            _ => {}
        }
        if f.ok {
            for &(index, expected) in &f.blocks {
                let pixels = decoder
                    .block_pixels(index)
                    .unwrap_or_else(|| panic!("строка {}: блок {index} отсутствует", f.line));
                let actual = crc32(pixels.iter().flat_map(|p| p.to_be_bytes()));
                assert_eq!(actual, expected, "строка {}: блок {index} отличается от оригинала", f.line);
                checked_blocks += 1;
            }
        }
        if f.reset_after {
            decoder = Decoder::new();
        }
    }
    assert!(checked_blocks > 300, "сверено блоков: {checked_blocks}");
}
