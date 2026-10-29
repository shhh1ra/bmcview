//! Веб-логин к BMC: POST `processparameter.php` по HTTPS с привязкой сертификата.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::Request;
use hyper_util::rt::TokioIo;
use ibmc_core::login::{self, KvmMode, LoginError, LoginInfo};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::tls;

pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Адрес BMC: `host`, `host:port`, `[v6]` или `[v6]:port`. Порт по умолчанию — 443.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    pub port: u16,
}

impl Address {
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        let s = s
            .strip_prefix("https://")
            .unwrap_or(s)
            .trim_end_matches('/');
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (host, tail) = rest.split_once(']')?;
            let port = match tail.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None if tail.is_empty() => 443,
                None => return None,
            };
            (host, port)
        } else {
            match s.split_once(':') {
                Some((host, port)) if !port.contains(':') => (host, port.parse().ok()?),
                Some(_) => (s, 443), // IPv6 без скобок
                None => (s, 443),
            }
        };
        (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| Self {
            host: host.to_owned(),
            port,
        })
    }

    /// Ключ в списке известных сертификатов.
    pub fn key(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebLoginError {
    #[error("сертификат BMC изменился")]
    CertificateChanged { expected: String, actual: String },
    #[error("BMC не ответил за {} с", TIMEOUT.as_secs())]
    Timeout,
    #[error("нет связи с BMC: {0}")]
    Network(String),
    #[error("ошибка TLS: {0}")]
    Tls(String),
    #[error("BMC ответил HTTP {0}")]
    Http(u16),
    #[error("{0}")]
    Login(#[from] LoginError),
}

pub struct LoginResult {
    pub info: LoginInfo,
    /// Отпечаток сертификата, с которым прошёл логин.
    pub fingerprint: String,
}

pub async fn login(
    address: &Address,
    user: &str,
    password: &str,
    mode: KvmMode,
    expected_fingerprint: Option<String>,
) -> Result<LoginResult, WebLoginError> {
    tokio::time::timeout(
        TIMEOUT,
        login_inner(address, user, password, mode, expected_fingerprint),
    )
    .await
    .map_err(|_| WebLoginError::Timeout)?
}

async fn login_inner(
    address: &Address,
    user: &str,
    password: &str,
    mode: KvmMode,
    expected: Option<String>,
) -> Result<LoginResult, WebLoginError> {
    let tcp = TcpStream::connect((address.host.as_str(), address.port))
        .await
        .map_err(|e| WebLoginError::Network(e.to_string()))?;
    let (config, verifier) = tls::client_config(expected.clone());
    let server_name = ServerName::try_from(address.host.clone())
        .map_err(|e| WebLoginError::Tls(e.to_string()))?;
    let stream = match TlsConnector::from(Arc::new(config))
        .connect(server_name, tcp)
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            return Err(match (expected, verifier.seen()) {
                (Some(expected), Some(actual)) if expected != actual => {
                    WebLoginError::CertificateChanged { expected, actual }
                }
                _ => WebLoginError::Tls(e.to_string()),
            })
        }
    };
    let fingerprint = verifier.seen().unwrap_or_default();

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| WebLoginError::Network(e.to_string()))?;
    tokio::spawn(connection);

    let request = Request::post(login::LOGIN_PATH)
        .header(HOST, address.key())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Full::new(Bytes::from(login::form_body(
            user, password, mode,
        ))))
        .map_err(|e| WebLoginError::Network(e.to_string()))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| WebLoginError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(WebLoginError::Http(response.status().as_u16()));
    }
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| WebLoginError::Network(e.to_string()))?
        .to_bytes();
    let info = login::parse_response(&String::from_utf8_lossy(&body))?;
    Ok(LoginResult { info, fingerprint })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(host: &str, port: u16) -> Option<Address> {
        Some(Address {
            host: host.into(),
            port,
        })
    }

    #[test]
    fn parses_addresses() {
        assert_eq!(Address::parse("10.10.12.8"), addr("10.10.12.8", 443));
        assert_eq!(
            Address::parse(" 10.10.12.8:8443 "),
            addr("10.10.12.8", 8443)
        );
        assert_eq!(Address::parse("https://bmc.local/"), addr("bmc.local", 443));
        assert_eq!(Address::parse("[fe80::1]:444"), addr("fe80::1", 444));
        assert_eq!(Address::parse("fe80::1"), addr("fe80::1", 443));
        assert_eq!(Address::parse("host:notaport"), None);
        assert_eq!(Address::parse(""), None);
        assert_eq!(
            Address::parse("[fe80::1]:444").unwrap().key(),
            "[fe80::1]:444"
        );
    }
}
