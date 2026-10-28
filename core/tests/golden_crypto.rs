//! Сверка криптографии с оригинальным `com.kvm.AESHandler`.
//! Эталоны — `golden/crypto.txt`, генератор — `tools/oracle/gen-crypto-golden.js`.

use ibmc_core::crypto::{encrypt_legacy, Kdf, KdfParams, SessionKey, UserKeys};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn arr<const N: usize>(s: &str) -> [u8; N] {
    unhex(s).try_into().unwrap()
}

fn kdf(name: &str, iterations: &str) -> KdfParams {
    let kdf = match name {
        "PBKDF2WithHmacSHA1" => Kdf::Sha1,
        "PBKDF2WithHmacSHA256" => Kdf::Sha256,
        other => panic!("неизвестный KDF {other}"),
    };
    KdfParams { kdf, iterations: iterations.parse().unwrap() }
}

#[test]
fn crypto_matches_original_java_client() {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for (n, line) in include_str!("golden/crypto.txt").lines().enumerate() {
        let mut parts = line.split(' ');
        let (name, args, expected) = (parts.next().unwrap(), parts.next().unwrap(), unhex(parts.next().unwrap()));
        let a: Vec<&str> = args.split(':').collect();
        let actual = match name {
            "encode_key" => {
                let verify = String::from_utf8(unhex(a[2])).unwrap();
                let keys = UserKeys { key: [0; 16], iv: arr(a[3]) };
                keys.encode_key(&verify, kdf(a[0], a[1])).to_vec()
            }
            "kvm_key" => {
                let keys = UserKeys { key: arr(a[2]), iv: arr(a[3]) };
                keys.session_key(&unhex(a[4]), kdf(a[0], a[1])).unwrap().0.to_vec()
            }
            "secure_encry" => SessionKey(arr(a[0])).encrypt_input(&unhex(a[1])),
            "kvm_encry" => SessionKey(arr(a[0])).encrypt_command(&unhex(a[1])),
            "decry" => SessionKey(arr(a[0])).decrypt(&unhex(a[1])).unwrap(),
            "encry" => encrypt_legacy(u32::from_str_radix(a[0], 16).unwrap() as i32, &unhex(a[1])),
            other => panic!("строка {}: неизвестный случай {other}", n + 1),
        };
        assert_eq!(actual, expected, "строка {}: {name} отличается от оригинала", n + 1);
        *counts.entry(name).or_default() += 1;
    }
    assert_eq!(counts.len(), 6, "проверены не все виды случаев: {counts:?}");
}
