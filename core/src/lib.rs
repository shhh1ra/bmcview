//! Ядро клиента KVM для Huawei iBMC (RH1288 V3).
//!
//! Крейт ничего не знает про UI, сеть и ОС: на вход принимает байты, на выход отдаёт
//! байты и структуры. Благодаря этому всё здесь тестируется на записанных дампах.
//!
//! Описание протокола — `docs/protocol.md`, решения — `docs/architecture.md`.

pub mod crc;
pub mod crypto;
pub mod framing;
pub mod video;
