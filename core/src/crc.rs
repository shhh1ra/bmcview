//! CRC пакетов.
//!
//! Оригинал (`com.kvm.CCrc`, режим `CRC_16_H`) — табличный CRC-16 с полиномом 0x1021,
//! начальным значением 0, без отражения и без финального XOR, т.е. CRC-16/XMODEM.

/// CRC-16/XMODEM.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_check_value() {
        // Контрольное значение CRC-16/XMODEM из каталога CRC.
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(crc16_xmodem(&[]), 0);
    }
}
