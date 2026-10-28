//! Фрейминг пакетов KVM-сокета.
//!
//! Клиент → BMC (`com.kvm.PackData.makePackData` / `makeEncrypPackData`):
//! ```text
//! FE F6 | len(2, BE) = payload+2 | codeKey(4, BE)  | CRC16(payload)(2, BE) | payload
//! FE F6 | (payload+2)|0x8000     | encodeKey(24)   | CRC16(payload)(2, BE) | payload
//! ```
//!
//! BMC → клиент (`com.kvm.KVMUtil.diviStreamNew`):
//! ```text
//! FE F6 00 | LL (3..=250) | LL байт сообщения
//! ```
//! В сообщении байт `[2]` — тип, `[3]` — номер blade. Назначение `[0..2]` не подтверждено
//! (вероятно, CRC), клиент их не проверяет.

use crate::crc::crc16_xmodem;

pub const MAGIC: [u8; 2] = [0xFE, 0xF6];

/// Обычный пакет с `codeKey` в заголовке.
pub fn pack(code_key: i32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&length_field(payload.len()).to_be_bytes());
    out.extend_from_slice(&code_key.to_be_bytes());
    out.extend_from_slice(&crc16_xmodem(payload).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Пакет с 24-байтным `encodeKey` вместо `codeKey` (только CONNECT_BLADE при `compress=1`).
pub fn pack_with_encode_key(encode_key: &[u8; 24], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(30 + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(length_field(payload.len()) | 0x8000).to_be_bytes());
    out.extend_from_slice(encode_key);
    out.extend_from_slice(&crc16_xmodem(payload).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn length_field(payload_len: usize) -> u16 {
    // Оригинал берёт младшие 16 бит без проверки; длинных пакетов клиент не шлёт.
    debug_assert!(payload_len + 2 <= 0x7FFF);
    (payload_len + 2) as u16
}

/// Сообщение от BMC (тело после `FE F6 00 LL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message(pub Vec<u8>);

impl Message {
    pub fn kind(&self) -> u8 {
        self.0[2]
    }

    pub fn blade(&self) -> Option<u8> {
        self.0.get(3).copied()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Потоковый разборщик входящих сообщений. TCP режет данные произвольно,
/// поэтому состояние сохраняется между вызовами `push`.
#[derive(Debug, Default)]
pub struct Deframer {
    head: u32,
    state: State,
    expected: usize,
    buf: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Header,
    Length,
    Body,
}

const MIN_LEN: usize = 3;
const MAX_LEN: usize = 250;

impl Deframer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Скармливает очередной кусок потока, возвращает все сообщения, которые в нём завершились.
    pub fn push(&mut self, data: &[u8]) -> Vec<Message> {
        let mut out = Vec::new();
        for &byte in data {
            match self.state {
                State::Header => {
                    self.head = (self.head << 8) | byte as u32;
                    if self.head & 0x00FF_FFFF == 0x00FE_F600 {
                        self.state = State::Length;
                    }
                }
                State::Length => {
                    let len = byte as usize;
                    if !(MIN_LEN..=MAX_LEN).contains(&len) {
                        // Как в оригинале: байт длины становится началом поиска следующего заголовка.
                        self.state = State::Header;
                        self.head = byte as u32;
                        continue;
                    }
                    self.expected = len;
                    self.buf = Vec::with_capacity(len);
                    self.state = State::Body;
                }
                State::Body => {
                    self.buf.push(byte);
                    if self.buf.len() == self.expected {
                        out.push(Message(std::mem::take(&mut self.buf)));
                        self.state = State::Header;
                        self.head = 0;
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_layout_matches_original() {
        // REQ_BLADE_PRESENT: payload = [0x0B], codeKey = 0x01020304.
        let p = pack(0x0102_0304, &[0x0B]);
        let crc = crc16_xmodem(&[0x0B]).to_be_bytes();
        assert_eq!(
            p,
            [0xFE, 0xF6, 0x00, 0x03, 0x01, 0x02, 0x03, 0x04, crc[0], crc[1], 0x0B]
        );
    }

    #[test]
    fn pack_negative_code_key_is_twos_complement() {
        let p = pack(-2, &[9, 0]);
        assert_eq!(&p[4..8], &[0xFF, 0xFF, 0xFF, 0xFE]);
    }

    #[test]
    fn pack_with_encode_key_sets_high_bit() {
        let key = [0xAA; 24];
        let payload = [6, 0, 8, 1, 1];
        let p = pack_with_encode_key(&key, &payload);
        assert_eq!(&p[0..4], &[0xFE, 0xF6, 0x80, 0x07]);
        assert_eq!(&p[4..28], &key);
        assert_eq!(&p[28..30], &crc16_xmodem(&payload).to_be_bytes());
        assert_eq!(&p[30..], &payload);
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFE, 0xF6, 0x00, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn deframer_handles_split_chunks() {
        let mut stream = frame(&[0, 0, 4, 0, 3]);
        stream.extend(frame(&[0, 0, 8, 0, 2]));
        let mut d = Deframer::new();
        let mut got = Vec::new();
        for chunk in stream.chunks(3) {
            got.extend(d.push(chunk));
        }
        assert_eq!(
            got,
            vec![Message(vec![0, 0, 4, 0, 3]), Message(vec![0, 0, 8, 0, 2])]
        );
        assert_eq!(got[0].kind(), 4);
        assert_eq!(got[1].blade(), Some(0));
    }

    #[test]
    fn deframer_skips_garbage_and_bad_lengths() {
        let mut stream = vec![0x11, 0xFE, 0x22, 0xFE, 0xF6, 0x00, 0x02]; // длина 2 < 3 — отбрасывается
        stream.extend(frame(&[1, 2, 3]));
        let got = Deframer::new().push(&stream);
        assert_eq!(got, vec![Message(vec![1, 2, 3])]);
    }

    #[test]
    fn deframer_resyncs_when_length_byte_starts_new_header() {
        // Недопустимая длина 0xFE должна стать первым байтом следующего заголовка.
        let mut stream = vec![0xFE, 0xF6, 0x00, 0xFE, 0xF6, 0x00, 0x03];
        stream.extend_from_slice(&[7, 7, 7]);
        let got = Deframer::new().push(&stream);
        assert_eq!(got, vec![Message(vec![7, 7, 7])]);
    }
}
