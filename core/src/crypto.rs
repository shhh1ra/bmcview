//! Криптография сессии KVM.
//!
//! Порт `com.kvm.AESHandler` и вывода ключей из `KVMApplet.comInKVM` /
//! `BladeThread.distributeConsultation` / `BladeThread.distributeKVMSetKey`.
//!
//! - Шифр: AES-128-CBC без паддинга. Открытый текст дополняется нулями до кратного 16.
//! - KDF: PBKDF2-HMAC-SHA1 или SHA256 (согласуется сообщениями GET_SUITE/SET_SUITE),
//!   по умолчанию SHA1 и 5000 итераций.
//! - После KDF каждое 4-байтовое слово ключа разворачивается (`wordswap`).

use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};

type Enc = cbc::Encryptor<aes::Aes128>;
type Dec = cbc::Decryptor<aes::Aes128>;

const BLOCK: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("decrykey должен быть 64 hex-символа")]
    BadDecryKey,
    #[error("длина шифротекста {0} не кратна 16")]
    BadCiphertextLength(usize),
    #[error("сообщение KVM_KEY_SET должно быть 48 байт, получено {0}")]
    BadKeySetLength(usize),
}

/// Хэш-функция PBKDF2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kdf {
    Sha1,
    Sha256,
}

/// Параметры KDF. Код `alg` в `KVM_SUITE_LIST`/`SET_SUITE`: 2 — SHA1, 3 — SHA256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub kdf: Kdf,
    pub iterations: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            kdf: Kdf::Sha1,
            iterations: 5000,
        }
    }
}

pub fn pbkdf2(params: KdfParams, password: &[u8], salt: &[u8], out: &mut [u8]) {
    match params.kdf {
        Kdf::Sha1 => pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, salt, params.iterations, out),
        Kdf::Sha256 => pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, params.iterations, out),
    }
}

/// Разворот байт в каждом 4-байтовом слове: `b0 b1 b2 b3 → b3 b2 b1 b0`.
pub fn wordswap(bytes: &mut [u8]) {
    for word in bytes.as_chunks_mut::<4>().0 {
        word.reverse();
    }
}

/// Пароль Java (`String` из `char`) → байты, которые получает PBKDF2 в SunJCE (UTF-8).
/// Байты пароля из `KVM_KEY_SET` превращаются в `char` со знаковым расширением
/// (`BladeThread.convert`), поэтому байты ≥ 0x80 становятся символами U+FF80..U+FFFF.
fn java_password_from_bytes(bytes: &[u8]) -> Vec<u8> {
    let s: String = bytes
        .iter()
        .map(|&b| {
            char::from_u32(b as i8 as i16 as u16 as u32).expect("U+FF80..U+FFFF — не суррогаты")
        })
        .collect();
    s.into_bytes()
}

pub fn aes_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let mut buf = plain.to_vec();
    buf.resize(plain.len().div_ceil(BLOCK).max(1) * BLOCK, 0);
    let len = buf.len();
    Enc::new(key.into(), iv.into())
        .encrypt_padded_mut::<NoPadding>(&mut buf, len)
        .expect("длина кратна блоку");
    buf
}

pub fn aes_cbc_decrypt(
    key: &[u8; 16],
    iv: &[u8; 16],
    cipher: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if !cipher.len().is_multiple_of(BLOCK) || cipher.is_empty() {
        return Err(CryptoError::BadCiphertextLength(cipher.len()));
    }
    let mut buf = cipher.to_vec();
    Dec::new(key.into(), iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .expect("длина кратна блоку");
    Ok(buf)
}

/// Ключи из ответа веб-логина: `decrykey` = 64 hex → `user_key` (16) + `user_iv` (16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserKeys {
    pub key: [u8; 16],
    pub iv: [u8; 16],
}

impl UserKeys {
    pub fn from_hex(decrykey: &str) -> Result<Self, CryptoError> {
        let s = decrykey.trim();
        if s.len() != 64 || !s.is_ascii() {
            return Err(CryptoError::BadDecryKey);
        }
        let mut raw = [0u8; 32];
        for (i, byte) in raw.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| CryptoError::BadDecryKey)?;
        }
        let mut key = [0u8; 16];
        let mut iv = [0u8; 16];
        key.copy_from_slice(&raw[..16]);
        iv.copy_from_slice(&raw[16..]);
        Ok(Self { key, iv })
    }

    /// 24-байтный ключ для заголовка CONNECT_BLADE при `compress=1`.
    /// `verify` — `verifyValue` (или `verifyValueExt`, если было согласование KDF).
    pub fn encode_key(&self, verify: &str, params: KdfParams) -> [u8; 24] {
        let mut out = [0u8; 24];
        pbkdf2(params, verify.as_bytes(), &self.iv, &mut out);
        wordswap(&mut out);
        out
    }

    /// Сессионный ключ из тела сообщения `KVM_KEY_SET` (0x40), 48 байт шифротекста.
    pub fn session_key(
        &self,
        key_set: &[u8],
        params: KdfParams,
    ) -> Result<SessionKey, CryptoError> {
        if key_set.len() != 48 {
            return Err(CryptoError::BadKeySetLength(key_set.len()));
        }
        let plain = aes_cbc_decrypt(&self.key, &self.iv, key_set)?;
        let mut key = [0u8; 48];
        pbkdf2(
            params,
            &java_password_from_bytes(&plain[..32]),
            &plain[32..48],
            &mut key,
        );
        wordswap(&mut key);
        Ok(SessionKey(key))
    }

    /// Ключ переподключения: тело `KVM_KEY_SET` длиной 128 байт, расшифрованное user-ключом.
    pub fn reconnect_key(&self, key_set: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aes_cbc_decrypt(&self.key, &self.iv, key_set)
    }
}

/// Сессионный ключ `kvm_key`: `[0..16]` — видео и команды, `[16..32]` — ввод, `[32..48]` — IV.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKey(pub [u8; 48]);

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

impl SessionKey {
    fn part(&self, from: usize) -> &[u8; 16] {
        self.0[from..from + 16].try_into().unwrap()
    }

    /// Клавиатура и мышь (`AESHandler.secure_encry`).
    pub fn encrypt_input(&self, plain: &[u8]) -> Vec<u8> {
        aes_cbc_encrypt(self.part(16), self.part(32), plain)
    }

    /// Команды питания (`AESHandler.kvm_encry`).
    pub fn encrypt_command(&self, plain: &[u8]) -> Vec<u8> {
        aes_cbc_encrypt(self.part(0), self.part(32), plain)
    }

    /// Видеофрагменты и отчёты VMM (`AESHandler.decry`).
    pub fn decrypt(&self, cipher: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aes_cbc_decrypt(self.part(0), self.part(32), cipher)
    }
}

/// Шифрование клавиатуры в режиме `compress=0` (`AESHandler.encry`):
/// ключ `codeKey(BE) 05 06 07 08 01..08`, IV — нули.
pub fn encrypt_legacy(code_key: i32, plain: &[u8]) -> Vec<u8> {
    let mut key = [1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8];
    key[..4].copy_from_slice(&code_key.to_be_bytes());
    aes_cbc_encrypt(&key, &[0; 16], plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordswap_reverses_each_word() {
        let mut v = [1, 2, 3, 4, 5, 6, 7, 8];
        wordswap(&mut v);
        assert_eq!(v, [4, 3, 2, 1, 8, 7, 6, 5]);
    }

    #[test]
    fn user_keys_split_decrykey() {
        let hex = "00112233445566778899aabbccddeeff".repeat(2);
        let k = UserKeys::from_hex(&hex).unwrap();
        assert_eq!(k.key[0], 0x00);
        assert_eq!(k.iv[15], 0xFF);
        assert_eq!(UserKeys::from_hex("abc"), Err(CryptoError::BadDecryKey));
    }

    #[test]
    fn encrypt_pads_with_zeros() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let a = aes_cbc_encrypt(&key, &iv, &[1, 2, 3]);
        let mut padded = [0u8; 16];
        padded[..3].copy_from_slice(&[1, 2, 3]);
        assert_eq!(a, aes_cbc_encrypt(&key, &iv, &padded));
        assert_eq!(aes_cbc_decrypt(&key, &iv, &a).unwrap(), padded);
    }

    #[test]
    fn java_password_sign_extends_high_bytes() {
        assert_eq!(java_password_from_bytes(b"A1"), b"A1");
        // 0x80 → (char)(byte)0x80 = U+FF80 → UTF-8 EF BE 80.
        assert_eq!(java_password_from_bytes(&[0x80]), vec![0xEF, 0xBE, 0x80]);
    }
}
