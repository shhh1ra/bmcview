//! TLS к веб-интерфейсу BMC с привязкой к отпечатку сертификата (trust on first use).
//!
//! У BMC самоподписанный сертификат, и оригинальный клиент принимает любой. Здесь при первом
//! подключении запоминается SHA-256 сертификата, а при следующих рукопожатие обрывается,
//! если сертификат другой, — до того, как отправлен пароль. Подпись рукопожатия
//! проверяется как обычно: сервер доказывает, что владеет ключом сертификата.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// SHA-256 сертификата в виде `AB:CD:…`, как его показывают браузеры.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug)]
pub struct PinnedVerifier {
    expected: Option<String>,
    seen: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    /// Отпечаток сертификата, который предъявил сервер (после рукопожатия или его отказа).
    pub fn seen(&self) -> Option<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = fingerprint(end_entity.as_ref());
        *self.seen.lock().unwrap() = Some(actual.clone());
        match &self.expected {
            Some(expected) if *expected != actual => {
                Err(rustls::Error::General("сертификат BMC изменился".into()))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Конфигурация клиента: `expected` — запомненный отпечаток или `None` при первом подключении.
pub fn client_config(expected: Option<String>) -> (ClientConfig, Arc<PinnedVerifier>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedVerifier {
        expected,
        seen: Mutex::new(None),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring поддерживает TLS 1.2 и 1.3")
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    (config, verifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(
            fp,
            "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
        );
    }
}
