//! Видео: фрагменты → кадр (`assembler`) → блоки 64×64 (`decoder`).
//!
//! Поток данных:
//! ```text
//! Message(type 0x02) ─fragment_from_message─▶ фрагмент ─Assembler─▶ AssembledFrame ─Decoder─▶ блоки
//! ```

pub mod assembler;
pub mod color;
pub mod decoder;
pub mod jpeg;
#[rustfmt::skip]
#[allow(dead_code)]
mod tables;

pub use assembler::{AssembledFrame, Assembler, IFrameThrottle, PushOutcome};
pub use decoder::{BlockPixels, DecodeError, Decoder, BLOCK_SIZE};

/// Тип сообщения с видеофрагментом.
pub const IMAGE_DATA: u8 = 0x02;

/// Расшифровка фрагмента: шифротекст → открытый текст (`None` при ошибке).
pub type FragmentDecrypt<'a> = &'a dyn Fn(&[u8]) -> Option<Vec<u8>>;

/// Извлекает фрагмент из сообщения типа 0x02 в формате, который ждёт [`Assembler`]:
/// `fragIdx(2, BE) | frameNo(1) | полезные данные`.
///
/// При `compress=1` у всех фрагментов, кроме нулевого, данные зашифрованы: байт `[5]`
/// сообщения (считая от типа) — реальная длина, с `[6]` — шифротекст
/// (`com.kvm.DrawThread.run`). `decrypt` получает шифротекст и возвращает открытый текст.
/// Нулевой фрагмент (заголовок кадра) не шифруется.
pub fn fragment_from_message(msg: &[u8], decrypt: Option<FragmentDecrypt<'_>>) -> Option<Vec<u8>> {
    // msg: [p0, p1, 0x02, blade, fragHi, fragLo, frameNo, ...]
    let body = msg.get(2..)?; // [0x02, blade, fragHi, fragLo, frameNo, ...]
    if body.len() < 5 {
        return None;
    }
    let is_header = body[2] == 0 && body[3] == 0;
    match decrypt {
        Some(decrypt) if !is_header => {
            let real_len = *body.get(5)? as usize;
            let plain = decrypt(body.get(6..)?)?;
            let mut out = body[2..5].to_vec();
            out.extend_from_slice(plain.get(..real_len)?);
            Some(out)
        }
        _ => Some(body[2..].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fragment_strips_prefix_type_and_blade() {
        let msg = [0xAA, 0xBB, 0x02, 0x00, 0x00, 0x01, 0x07, 1, 2, 3];
        assert_eq!(
            fragment_from_message(&msg, None).unwrap(),
            vec![0, 1, 7, 1, 2, 3]
        );
    }

    #[test]
    fn encrypted_fragment_uses_real_length() {
        let msg = [0, 0, 0x02, 0, 0x00, 0x01, 0x07, 2, 0xC0, 0xC1, 0xC2];
        let dec = |c: &[u8]| Some(c.iter().map(|b| b ^ 0xC0).collect::<Vec<u8>>());
        let out = fragment_from_message(&msg, Some(&dec)).unwrap();
        assert_eq!(out, vec![0, 1, 7, 0, 1]);
    }

    #[test]
    fn header_fragment_is_never_decrypted() {
        let msg = [0, 0, 0x02, 0, 0x00, 0x00, 0x07, 9, 9];
        let dec =
            |_: &[u8]| -> Option<Vec<u8>> { panic!("не должно вызываться") };
        assert_eq!(
            fragment_from_message(&msg, Some(&dec)).unwrap(),
            vec![0, 0, 7, 9, 9]
        );
    }
}
