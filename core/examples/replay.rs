//! Прогон записанного потока BMC → клиент через ядро: фрейминг, сборка кадров, декодер.
//!
//! Вход — сырые байты TCP-потока от BMC (без заголовков TCP/IP), например выгрузка
//! из дампа. Только `compress=0`: при `compress=1` фрагменты зашифрованы сессионным ключом.
//!
//! ```text
//! cargo run -p ibmc-core --example replay -- stream.bin [last.ppm]
//! ```
//!
//! Печатает статистику по типам сообщений и кадрам. Если указан второй аргумент,
//! сохраняет последнее состояние экрана в PPM. Картинка — содержимое экрана сервера:
//! её место там же, где дамп, а не в репозитории.

use std::collections::BTreeMap;
use std::fs;
use std::process::ExitCode;

use ibmc_core::framing::Deframer;
use ibmc_core::video::{fragment_from_message, Assembler, Decoder, BLOCK_SIZE, IMAGE_DATA};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(input) = args.first() else {
        eprintln!("usage: replay <stream.bin> [out.ppm]");
        return ExitCode::FAILURE;
    };
    let stream = match fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut deframer = Deframer::new();
    let mut assembler = Assembler::new();
    let mut decoder = Decoder::new();
    let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
    let mut prefixes: BTreeMap<[u8; 2], usize> = BTreeMap::new();
    let (mut frames, mut iframes, mut unchanged, mut resync) = (0, 0, 0, 0);
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut sizes: BTreeMap<(u16, u16), usize> = BTreeMap::new();
    let mut qualities: BTreeMap<u8, usize> = BTreeMap::new();
    let mut blocks_updated = 0usize;

    for msg in deframer.push(&stream) {
        let bytes = msg.bytes();
        *kinds.entry(msg.kind()).or_default() += 1;
        *prefixes.entry([bytes[0], bytes[1]]).or_default() += 1;
        if msg.kind() != IMAGE_DATA {
            continue;
        }
        let Some(fragment) = fragment_from_message(bytes, None) else {
            *errors.entry("short fragment".into()).or_default() += 1;
            continue;
        };
        let out = assembler.push(&fragment);
        unchanged += usize::from(out.unchanged);
        resync += usize::from(out.request_iframe);
        let Some(frame) = out.frame else { continue };
        frames += 1;
        iframes += usize::from(frame.is_iframe());
        *sizes.entry((frame.width, frame.height)).or_default() += 1;
        *qualities.entry(frame.info & 0x0F).or_default() += 1;
        match decoder.decode(
            &frame.data,
            frame.width.into(),
            frame.height.into(),
            frame.quality_index(),
        ) {
            Ok(changed) => blocks_updated += changed.len(),
            Err(e) => *errors.entry(format!("{e:?}")).or_default() += 1,
        }
    }

    println!("сообщения по типам:");
    for (k, n) in &kinds {
        println!("  0x{k:02x}: {n}");
    }
    println!("первые два байта сообщений: {prefixes:02x?}");
    println!(
        "кадры: {frames} (I-кадров {iframes}), «не изменился» {unchanged}, рассинхронизаций {resync}"
    );
    println!("разрешения: {sizes:?}");
    println!("качество (младший полубайт info): {qualities:?}");
    println!("обновлено блоков: {blocks_updated}");
    println!("ошибки декодирования: {errors:?}");

    if let Some(path) = args.get(1) {
        let (w, h) = (decoder.width(), decoder.height());
        let mut rgb = vec![0u8; w * h * 3];
        for index in 0..decoder.block_count() {
            let Some(px) = decoder.block_pixels(index) else {
                continue;
            };
            let (bx, by) = decoder.block_origin(index);
            for y in 0..BLOCK_SIZE.min(h.saturating_sub(by)) {
                for x in 0..BLOCK_SIZE.min(w.saturating_sub(bx)) {
                    let c = px[y * BLOCK_SIZE + x];
                    let o = ((by + y) * w + bx + x) * 3;
                    rgb[o..o + 3].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
                }
            }
        }
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        ppm.extend_from_slice(&rgb);
        if let Err(e) = fs::write(path, ppm) {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
        println!("последний кадр: {path} ({w}×{h})");
    }
    ExitCode::SUCCESS
}
