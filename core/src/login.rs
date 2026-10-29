//! Веб-логин: тело запроса `processparameter.php` и разбор ответа.
//!
//! Порт `ClientSubmitLoginIofo.getParaFromWeb`, `FindParaFromMsg` и `KVMApplet.comInKVM`.
//! Сеть здесь не открывается: вызывающий отправляет [`form_body`] POST-запросом на
//! [`LOGIN_PATH`] и передаёт первую строку ответа в [`parse_response`].
//!
//! Ответ разбирается так же, как в оригинале, — по позициям скобок, без имён полей:
//! значения по порядку стоят в `[...]` (код ошибки, verifyValue, "decrykey", privilege,
//! compress, vmm_compress, kvmPort, vmmPort), дополнительные — в `<...>`
//! (SerialNumber, verifyValueExt).

use crate::crypto::{CryptoError, UserKeys};

pub const LOGIN_PATH: &str = "/bmc/php/processparameter.php";

/// Оригинал читает ответ до первого `\n`, не длиннее 1024 символов.
pub const MAX_RESPONSE_CHARS: usize = 1024;

/// Режим KVM: общий или монопольный (`KvmMode` в запросе).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmMode {
    Shared,
    Exclusive,
}

/// Тело POST-запроса (`application/x-www-form-urlencoded`).
///
/// Оригинал кодирует только пароль; имя пользователя здесь тоже кодируется — для обычных
/// имён результат тот же, а с `&` или `=` в имени запрос не ломается.
pub fn form_body(user: &str, password: &str, mode: KvmMode) -> String {
    let mode = match mode {
        KvmMode::Shared => "0",
        KvmMode::Exclusive => "1",
    };
    format!(
        "check_pwd={}&logtype=1&user_name={}&func=DirectKVM&IsKvmApp=1&KvmMode={mode}",
        url_encode(password),
        url_encode(user),
    )
}

/// `java.net.URLEncoder.encode(s, "UTF-8")`: пробел → `+`, без изменений `A-Za-z0-9.-*_`.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Данные сессии из ответа логина.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginInfo {
    /// `verifyValue` → `codeKey`, кладётся в заголовок каждого пакета.
    pub code_key: i32,
    /// `verifyValue` строкой — пароль KDF для encodeKey (`compress=1` без согласования).
    pub verify_value: String,
    /// Из `decrykey`. `None`, если поле пустое или битое: тогда недоступны `compress=1`
    /// и переподключение по reconnKey.
    pub user_keys: Option<UserKeys>,
    pub privilege: String,
    pub compress: bool,
    pub vmm_compress: bool,
    pub kvm_port: u16,
    pub vmm_port: u16,
    pub serial_number: String,
    /// Непустое значение включает согласование KDF (GET_SUITE/SET_SUITE).
    pub verify_value_ext: String,
}

impl std::fmt::Debug for LoginInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginInfo")
            .field("compress", &self.compress)
            .field("kvm_port", &self.kvm_port)
            .field("privilege", &self.privilege)
            .field("negotiate_kdf", &!self.verify_value_ext.is_empty())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LoginError {
    #[error("пользователь заблокирован")]
    UserLocked,
    #[error("у пользователя нет прав на KVM")]
    NoPermission,
    #[error("срок действия пароля истёк")]
    PasswordExpired,
    #[error("вход ограничен политикой BMC")]
    LoginRestricted,
    #[error("неверное имя пользователя или пароль (код {0})")]
    Failed(String),
    #[error("ответ BMC не распознан")]
    Malformed,
    #[error("поле {0} не распознано")]
    BadField(&'static str),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Разбор первой строки ответа.
pub fn parse_response(response: &str) -> Result<LoginInfo, LoginError> {
    let s: String = response
        .chars()
        .take_while(|&c| c != '\n')
        .take(MAX_RESPONSE_CHARS)
        .collect();
    let square = brackets(&s, '[', ']');
    let angle = brackets(&s, '<', '>');
    let field = |i: usize| -> Result<&str, LoginError> {
        let &(start, end) = square.get(i).ok_or(LoginError::Malformed)?;
        s.get(start + 1..end).ok_or(LoginError::Malformed)
    };

    match field(0)?.trim() {
        "0" => {}
        "131" => return Err(LoginError::UserLocked),
        "136" => return Err(LoginError::NoPermission),
        "137" => return Err(LoginError::PasswordExpired),
        "144" => return Err(LoginError::LoginRestricted),
        other => return Err(LoginError::Failed(other.to_owned())),
    }

    let verify_value = field(1)?.trim().to_owned();
    // KVMApplet: Long.parseLong, затем младшие 32 бита (константа `i` там равна 0).
    let code_key = verify_value
        .parse::<i64>()
        .map_err(|_| LoginError::BadField("verifyValue"))? as i32;
    // `["<64 hex>"]` — оригинал отрезает кавычки (substring(start + 2, end - 1)).
    let user_keys = UserKeys::from_hex(field(2)?.trim().trim_matches('"')).ok();
    let flag = |i: usize, name| -> Result<bool, LoginError> {
        match field(i)?.trim() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(LoginError::BadField(name)),
        }
    };
    let port = |i: usize, name| -> Result<u16, LoginError> {
        field(i)?
            .trim()
            .parse()
            .map_err(|_| LoginError::BadField(name))
    };
    let extra = |i: usize| -> String {
        angle
            .get(i)
            .and_then(|&(start, end)| s.get(start + 1..end))
            .unwrap_or_default()
            .to_owned()
    };

    Ok(LoginInfo {
        code_key,
        verify_value,
        user_keys,
        privilege: field(3)?.trim().to_owned(),
        compress: flag(4, "compress")?,
        vmm_compress: flag(5, "vmm_compress")?,
        kvm_port: port(6, "kvmPort")?,
        vmm_port: port(7, "vmmPort")?,
        serial_number: extra(0),
        verify_value_ext: extra(1),
    })
}

/// Пары позиций открывающих и закрывающих скобок по порядку — как массивы
/// `indexStartArray`/`indexEndArray` в `FindParaFromMsg`.
fn brackets(s: &str, open: char, close: char) -> Vec<(usize, usize)> {
    let starts = s.char_indices().filter(|&(_, c)| c == open).map(|(i, _)| i);
    let ends = s
        .char_indices()
        .filter(|&(_, c)| c == close)
        .map(|(i, _)| i);
    starts.zip(ends).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn response(code: &str, compress: &str, ext: &str) -> String {
        format!(
            "{{\"a\":[{code}],\"b\":[3123456789],\"c\":[\"{KEY}\"],\"d\":[4],\"e\":[{compress}],\
             \"f\":[0],\"g\":[2198],\"h\":[8208],\"i\":\"<2102310YPY10J1000123>\",\"j\":\"<{ext}>\"}}\n"
        )
    }

    #[test]
    fn parses_fields_by_position() {
        let info = parse_response(&response("0", "0", "987654321")).unwrap();
        assert_eq!(info.code_key, (3123456789u64 as u32) as i32);
        assert_eq!(info.verify_value, "3123456789");
        assert_eq!(info.user_keys, Some(UserKeys::from_hex(KEY).unwrap()));
        assert_eq!(info.privilege, "4");
        assert!(!info.compress);
        assert!(!info.vmm_compress);
        assert_eq!(info.kvm_port, 2198);
        assert_eq!(info.vmm_port, 8208);
        assert_eq!(info.serial_number, "2102310YPY10J1000123");
        assert_eq!(info.verify_value_ext, "987654321");
    }

    #[test]
    fn extras_are_optional() {
        let s = "{[0],[42],[\"\"],[4],[1],[1],[2198],[8208]}";
        let info = parse_response(s).unwrap();
        assert_eq!(info.code_key, 42);
        assert!(info.compress);
        assert_eq!(info.user_keys, None);
        assert_eq!(info.verify_value_ext, "");
    }

    #[test]
    fn maps_error_codes() {
        assert_eq!(
            parse_response(&response("131", "0", "")),
            Err(LoginError::UserLocked)
        );
        assert_eq!(
            parse_response(&response("137", "0", "")),
            Err(LoginError::PasswordExpired)
        );
        assert_eq!(
            parse_response(&response("5", "0", "")),
            Err(LoginError::Failed("5".into()))
        );
        assert_eq!(parse_response("<html>"), Err(LoginError::Malformed));
    }

    #[test]
    fn only_first_line_counts() {
        let s = "{[0],[1],[\"\"],[4],[0],[0]}\n[2198],[8208]";
        assert_eq!(parse_response(s), Err(LoginError::Malformed));
    }

    #[test]
    fn form_body_encodes_like_java() {
        assert_eq!(
            form_body("Administrator", "p@ss word&=ä", KvmMode::Shared),
            "check_pwd=p%40ss+word%26%3D%C3%A4&logtype=1&user_name=Administrator\
             &func=DirectKVM&IsKvmApp=1&KvmMode=0"
        );
        assert!(form_body("u", "p", KvmMode::Exclusive).ends_with("KvmMode=1"));
    }
}
