//! Сессия KVM как конечный автомат без ввода-вывода.
//!
//! Вход — байты от BMC ([`Session::receive`]), действия пользователя и текущее время
//! ([`Session::poll`]). Выход — байты для отправки ([`Session::take_outgoing`]) и события.
//! Сокет, таймеры и поток держит вызывающий: [`Session::next_deadline`] говорит,
//! когда снова вызвать `poll`.
//!
//! Порядок подключения повторяет оригинал (`KVMUtil.connectBlade` и др.), сверен с записью
//! реальной сессии (`docs/protocol.md` §7):
//!
//! 1. `HEART_BEAT`, затем каждые 5 с;
//! 2. при непустом `verifyValueExt` — `GET_SUITE`, ответ `0x43` → `SET_SUITE`;
//! 3. через 1 с после `GET_SUITE` (или сразу без согласования) — `CONNECT_BLADE`,
//!    `FRAME_COMM(35)`, `MOUSE_MODE_SET(2)`;
//! 4. дальше видео, индикаторы, ввод.

use std::time::{Duration, Instant};

use crate::crypto::{encrypt_legacy, Kdf, KdfParams, SessionKey};
use crate::framing::{pack, pack_with_encode_key, Deframer};
use crate::input::{self, KeyReport, Keyboard};
use crate::login::LoginInfo;
use crate::video::{fragment_from_message, Assembler, Decoder, IFrameThrottle, IMAGE_DATA};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Пауза между `GET_SUITE` и `CONNECT_BLADE` (`Thread.sleep(1000)` в оригинале).
pub const CONNECT_DELAY: Duration = Duration::from_secs(1);
/// Частота кадров, которую клиент запрашивает при старте.
pub const DEFAULT_FPS: u8 = 35;

/// Стоечный сервер — один «blade» с номером 1.
const BLADE: u8 = 1;

mod cmd {
    pub const KEY: u8 = 0x03;
    pub const MOUSE: u8 = 0x05;
    pub const CONNECT_BLADE: u8 = 0x06;
    pub const I_REQ: u8 = 0x08;
    pub const HEART_BEAT: u8 = 0x09;
    pub const FRAME_COMM: u8 = 0x1C;
    pub const MOUSE_MODE_SET: u8 = 0x24;
    pub const DQT_MODE_SET: u8 = 0x27;
    pub const POWER_ENCRYPTED: u8 = 0x33;
    pub const GET_SUITE: u8 = 0x42;
    pub const SET_SUITE: u8 = 0x44;
}

mod msg {
    pub const LED: u8 = 0x04;
    pub const CONNECT_STATE: u8 = 0x08;
    pub const BLADE_CLOSED: u8 = 0x21;
    pub const MOUSE_MODE: u8 = 0x25;
    pub const QUALITY: u8 = 0x28;
    pub const KEY_SET: u8 = 0x40;
    pub const SUITE_LIST: u8 = 0x43;
}

/// Управление питанием (меню «Питание» оригинала).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    /// «Forced Power Off».
    ForceOff = 0x20,
    PowerOn = 0x21,
    /// «Cold Reset».
    ColdReset = 0x22,
    /// «Graceful Reboot».
    GracefulReboot = 0x23,
    /// «Graceful Power Off».
    GracefulOff = 0x25,
    UsbReset = 0x30,
}

/// Почему BMC закрыл сессию (`0x08`, `BladeThread.distributeConnectStateData`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    TooManyUsers,
    BadCredentials,
    SignalOutOfRange,
    ServerDisabled,
    UserDeleted,
    SessionTimeout,
    BladeClosed,
    Other(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Кадр декодирован, перечислены изменившиеся блоки ([`Session::decoder`]).
    Frame {
        width: usize,
        height: usize,
        changed: Vec<usize>,
    },
    /// Индикаторы: бит0 Num, бит1 Caps, бит2 Scroll.
    Leds(u8),
    /// `true` — абсолютная мышь.
    MouseMode(bool),
    /// Качество картинки в единицах протокола (40, 50, 70, 80, 90, 100).
    Quality(u8),
    /// BMC прислал ключ переподключения: соединение подтверждено.
    Authenticated,
    Closed(CloseReason),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("BMC требует шифрование KVM, но в ответе логина нет ключа decrykey")]
    MissingUserKeys,
}

pub struct Session {
    info: LoginInfo,
    kdf: KdfParams,
    encode_key: Option<[u8; 24]>,
    session_key: Option<SessionKey>,
    reconnect_key: Option<Vec<u8>>,
    color_bit: u8,
    next_heartbeat: Instant,
    connect_at: Option<Instant>,
    deframer: Deframer,
    assembler: Assembler,
    decoder: Decoder,
    throttle: IFrameThrottle,
    keyboard: Keyboard,
    quality: Option<u8>,
    out: Vec<u8>,
}

impl Session {
    /// Новая сессия сразу после TCP-соединения. `reconnect_key` — ключ из прошлой
    /// сессии ([`Session::reconnect_key`]) для переподключения без повторного логина.
    pub fn new(
        info: LoginInfo,
        reconnect_key: Option<Vec<u8>>,
        now: Instant,
    ) -> Result<Self, SessionError> {
        let kdf = KdfParams::default();
        let encode_key = if info.compress {
            let keys = info
                .user_keys
                .as_ref()
                .ok_or(SessionError::MissingUserKeys)?;
            Some(keys.encode_key(&info.verify_value, kdf))
        } else {
            None
        };
        let negotiate = !info.verify_value_ext.is_empty();
        let mut session = Self {
            info,
            kdf,
            encode_key,
            session_key: None,
            reconnect_key,
            color_bit: 0,
            next_heartbeat: now + HEARTBEAT_INTERVAL,
            connect_at: Some(if negotiate { now + CONNECT_DELAY } else { now }),
            deframer: Deframer::new(),
            assembler: Assembler::new(),
            decoder: Decoder::new(),
            throttle: IFrameThrottle::default(),
            keyboard: Keyboard::new(),
            quality: None,
            out: Vec::new(),
        };
        session.send(&[cmd::HEART_BEAT, BLADE]);
        if negotiate {
            session.send(&[cmd::GET_SUITE, BLADE]);
        }
        session.poll(now);
        Ok(session)
    }

    /// Таймеры: heartbeat и отложенный CONNECT_BLADE.
    pub fn poll(&mut self, now: Instant) {
        if now >= self.next_heartbeat {
            self.send(&[cmd::HEART_BEAT, BLADE]);
            self.next_heartbeat = now + HEARTBEAT_INTERVAL;
        }
        if self.connect_at.is_some_and(|at| now >= at) {
            self.connect_at = None;
            self.connect();
        }
    }

    /// Когда в следующий раз вызвать [`Session::poll`].
    pub fn next_deadline(&self) -> Instant {
        match self.connect_at {
            Some(at) => at.min(self.next_heartbeat),
            None => self.next_heartbeat,
        }
    }

    /// Накопленные байты для отправки в сокет.
    pub fn take_outgoing(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }

    /// Ключ для переподключения, если BMC его прислал (`0x40` со 128 байтами).
    pub fn reconnect_key(&self) -> Option<&[u8]> {
        self.reconnect_key.as_deref()
    }

    pub fn receive(&mut self, data: &[u8], now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        for message in self.deframer.push(data) {
            let bytes = message.bytes();
            // [p0, p1, тип, blade, данные…]
            let value = bytes.get(4).copied().unwrap_or(0);
            match message.kind() {
                IMAGE_DATA => self.on_video(bytes, now, &mut events),
                msg::LED => events.push(Event::Leds(value)),
                msg::MOUSE_MODE => events.push(Event::MouseMode(value == 1)),
                msg::QUALITY => {
                    // Оригинал выставляет ползунок качества, и тот отправляет значение
                    // обратно (видно в дампе: 0x28 70 → DQT_MODE_SET 70).
                    if self.quality != Some(value) {
                        self.set_quality(value);
                        events.push(Event::Quality(value));
                    }
                }
                msg::SUITE_LIST => self.on_suite_list(&bytes[2..]),
                msg::KEY_SET => {
                    if self.on_key_set(&bytes[4..]) {
                        events.push(Event::Authenticated);
                    }
                }
                msg::CONNECT_STATE => events.push(Event::Closed(close_reason(value))),
                msg::BLADE_CLOSED => events.push(Event::Closed(CloseReason::BladeClosed)),
                _ => {}
            }
        }
        events
    }

    /// Событие клавиши (`KeyboardEvent.code`).
    pub fn key(&mut self, code: &str, down: bool) {
        if let Some(report) = self.keyboard.key(code, down) {
            self.send_key_report(report);
        }
    }

    /// Отпустить все клавиши — при потере фокуса окна.
    pub fn release_keys(&mut self) {
        let report = self.keyboard.release_all();
        self.send_key_report(report);
    }

    /// Нажать и отпустить комбинацию (например, [`input::combo::CTRL_ALT_DEL`]).
    pub fn send_combo(&mut self, combo: KeyReport) {
        self.send_key_report(combo);
        let report = self.keyboard.release_all();
        self.send_key_report(report);
    }

    /// Мышь в абсолютном режиме. `x`, `y` — пиксели экрана сервера.
    pub fn mouse(&mut self, x: u32, y: u32, buttons: u8, wheel: i8) {
        let (w, h) = (self.decoder.width() as u32, self.decoder.height() as u32);
        if w == 0 || h == 0 {
            return;
        }
        let (ax, ay) = input::absolute(x, y, w, h);
        let report = input::mouse_report(buttons, ax, ay, wheel);
        if self.info.compress {
            let Some(key) = &self.session_key else { return };
            let mut payload = vec![cmd::MOUSE, BLADE];
            payload.extend(key.encrypt_input(&report));
            self.send(&payload);
        } else {
            let mut payload = vec![cmd::MOUSE, BLADE];
            payload.extend_from_slice(&report);
            self.send(&payload);
        }
    }

    /// Качество картинки в единицах протокола (40, 50, 70, 80, 90, 100).
    pub fn set_quality(&mut self, value: u8) {
        self.quality = Some(value);
        self.send(&[cmd::DQT_MODE_SET, 0, value, 2, 0]);
    }

    pub fn power(&mut self, command: PowerCommand) {
        let code = command as u8;
        if self.info.compress {
            let Some(key) = &self.session_key else { return };
            let mut plain = [0u8; 16];
            plain[15] = code;
            let mut payload = vec![cmd::POWER_ENCRYPTED, 0];
            payload.extend(key.encrypt_command(&plain));
            self.send(&payload);
        } else {
            self.send(&[code, 0]);
        }
    }

    /// Полный кадр по кнопке пользователя — сразу, без ограничения частоты.
    pub fn refresh(&mut self) {
        self.send(&[cmd::I_REQ, BLADE]);
    }

    /// Запрос полного кадра при сбое видео: как в оригинале (`KVMUtil.sentIFrame`),
    /// первый сбой запускает таймер, запрос уходит при следующем через 500 мс и позже.
    fn request_iframe(&mut self, now: Instant) {
        if self.throttle.should_send(now) {
            self.send(&[cmd::I_REQ, BLADE]);
        }
    }

    fn send(&mut self, payload: &[u8]) {
        self.out.extend(pack(self.info.code_key, payload));
    }

    fn connect(&mut self) {
        let mut payload = vec![cmd::CONNECT_BLADE, BLADE, self.color_bit, 1, 1];
        if let Some(key) = &self.reconnect_key {
            payload.extend_from_slice(&key[..key.len().min(128)]);
        }
        match &self.encode_key {
            Some(encode_key) => self.out.extend(pack_with_encode_key(encode_key, &payload)),
            None => self.send(&payload),
        }
        self.send(&[cmd::FRAME_COMM, DEFAULT_FPS]);
        self.send(&[cmd::MOUSE_MODE_SET, 0, 2, 0, 0]);
    }

    fn send_key_report(&mut self, report: KeyReport) {
        let cipher = if self.info.compress {
            let Some(key) = &self.session_key else { return };
            key.encrypt_input(&report)
        } else {
            encrypt_legacy(self.info.code_key, &report)
        };
        let mut payload = vec![cmd::KEY, BLADE];
        payload.extend(cipher);
        self.send(&payload);
    }

    /// `0x43`: `[тип, blade, count, count × (alg, iter BE)]` (`distributeConsultation`).
    fn on_suite_list(&mut self, body: &[u8]) {
        let count = body.get(2).copied().unwrap_or(0) as usize;
        if count * 5 + 3 != body.len() {
            return;
        }
        let chosen = body[3..]
            .as_chunks::<5>()
            .0
            .iter()
            .find(|entry| entry[0] == 3)
            .map(|entry| {
                // Java собирает число из знаковых байтов: `b << 24 | b << 16 | …`.
                let b = |i: usize| entry[i] as i8 as i32;
                let iterations = (b(1) << 24 | b(2) << 16 | b(3) << 8 | b(4)) as u32;
                KdfParams {
                    kdf: Kdf::Sha256,
                    iterations,
                }
            });
        match chosen {
            Some(params) => {
                self.kdf = params;
                let [i0, i1, i2, i3] = params.iterations.to_be_bytes();
                self.send(&[cmd::SET_SUITE, BLADE, 3, i0, i1, i2, i3]);
            }
            None => {
                let [i0, i1, i2, i3] = 5000u32.to_be_bytes();
                self.send(&[cmd::SET_SUITE, BLADE, 1, i0, i1, i2, i3]);
            }
        }
        if let (true, Some(keys)) = (self.info.compress, &self.info.user_keys) {
            self.encode_key = Some(keys.encode_key(&self.info.verify_value_ext, self.kdf));
        }
    }

    /// `0x40`: 48 байт — сессионный ключ, 128 — ключ переподключения (`distributeKVMSetKey`).
    fn on_key_set(&mut self, data: &[u8]) -> bool {
        let Some(keys) = &self.info.user_keys else {
            return false;
        };
        match data.len() {
            48 => match keys.session_key(data, self.kdf) {
                Ok(key) => {
                    self.session_key = Some(key);
                    true
                }
                Err(_) => false,
            },
            128 => match keys.reconnect_key(data) {
                Ok(key) => {
                    self.reconnect_key = Some(key);
                    true
                }
                Err(_) => false,
            },
            _ => false,
        }
    }

    fn on_video(&mut self, bytes: &[u8], now: Instant, events: &mut Vec<Event>) {
        let decrypt = |cipher: &[u8]| {
            self.session_key
                .as_ref()
                .and_then(|key| key.decrypt(cipher).ok())
        };
        let encrypted = self.info.compress;
        let fragment = if encrypted {
            fragment_from_message(bytes, Some(&decrypt))
        } else {
            fragment_from_message(bytes, None)
        };
        let Some(fragment) = fragment else {
            return self.request_iframe(now);
        };
        let outcome = self.assembler.push(&fragment);
        if outcome.request_iframe {
            self.request_iframe(now);
        }
        let Some(frame) = outcome.frame else { return };
        let (width, height) = (frame.width as usize, frame.height as usize);
        match self
            .decoder
            .decode(&frame.data, width, height, frame.quality_index())
        {
            Ok(changed) => events.push(Event::Frame {
                width,
                height,
                changed,
            }),
            Err(_) => self.request_iframe(now),
        }
    }
}

fn close_reason(code: u8) -> CloseReason {
    match code {
        2 => CloseReason::TooManyUsers,
        3 => CloseReason::BadCredentials,
        4 => CloseReason::SignalOutOfRange,
        5 => CloseReason::ServerDisabled,
        7 => CloseReason::UserDeleted,
        17 => CloseReason::SessionTimeout,
        other => CloseReason::Other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::UserKeys;

    const CODE_KEY: i32 = 0x635a_8e2f;

    fn info(ext: &str) -> LoginInfo {
        LoginInfo {
            code_key: CODE_KEY,
            verify_value: CODE_KEY.to_string(),
            user_keys: Some(
                UserKeys::from_hex(&"00112233445566778899aabbccddeeff".repeat(2)).unwrap(),
            ),
            privilege: "4".into(),
            compress: false,
            vmm_compress: false,
            kvm_port: 2198,
            vmm_port: 8208,
            serial_number: String::new(),
            verify_value_ext: ext.into(),
        }
    }

    /// Разбор исходящего потока на полезные нагрузки (заголовок с codeKey).
    fn payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            assert_eq!(&bytes[i..i + 2], &[0xFE, 0xF6]);
            let len = (u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) & 0x7FFF) as usize - 2;
            assert_eq!(&bytes[i + 4..i + 8], &CODE_KEY.to_be_bytes());
            out.push(bytes[i + 10..i + 10 + len].to_vec());
            i += 10 + len;
        }
        out
    }

    fn incoming(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut body = vec![0, 0, kind, 0];
        body.extend_from_slice(data);
        let mut out = vec![0xFE, 0xF6, 0, body.len() as u8];
        out.extend(body);
        out
    }

    /// Последовательность из дампа реальной сессии (protocol.md §7).
    #[test]
    fn startup_matches_capture() {
        let t0 = Instant::now();
        let mut s = Session::new(info("ext"), None, t0).unwrap();
        assert_eq!(
            payloads(&s.take_outgoing()),
            vec![vec![0x09, 1], vec![0x42, 1]]
        );

        let suites = [
            3, 1, 0, 0, 0x13, 0x88, 2, 0, 0, 0x27, 0x10, 3, 0, 0, 0x27, 0x10,
        ];
        s.receive(&incoming(0x43, &suites), t0);
        assert_eq!(
            payloads(&s.take_outgoing()),
            vec![vec![0x44, 1, 3, 0, 0, 0x27, 0x10]]
        );
        assert_eq!(
            s.kdf,
            KdfParams {
                kdf: Kdf::Sha256,
                iterations: 10_000
            }
        );

        assert_eq!(s.next_deadline(), t0 + CONNECT_DELAY);
        s.poll(t0 + CONNECT_DELAY);
        assert_eq!(
            payloads(&s.take_outgoing()),
            vec![
                vec![0x06, 1, 0, 1, 1],
                vec![0x1C, 35],
                vec![0x24, 0, 2, 0, 0]
            ]
        );

        let events = s.receive(&incoming(0x28, &[70, 0, 0, 0]), t0);
        assert_eq!(events, vec![Event::Quality(70)]);
        assert_eq!(payloads(&s.take_outgoing()), vec![vec![0x27, 0, 70, 2, 0]]);
        assert!(s.receive(&incoming(0x28, &[70, 0, 0, 0]), t0).is_empty());

        assert_eq!(
            s.receive(&incoming(0x25, &[1, 0, 0, 0]), t0),
            vec![Event::MouseMode(true)]
        );
        assert_eq!(
            s.receive(&incoming(0x04, &[2, 0, 0, 0]), t0),
            vec![Event::Leds(2)]
        );

        s.poll(t0 + HEARTBEAT_INTERVAL);
        assert_eq!(payloads(&s.take_outgoing()), vec![vec![0x09, 1]]);
        assert_eq!(s.next_deadline(), t0 + HEARTBEAT_INTERVAL * 2);
    }

    #[test]
    fn connects_immediately_without_negotiation() {
        let mut s = Session::new(info(""), None, Instant::now()).unwrap();
        let p = payloads(&s.take_outgoing());
        assert_eq!(p[0], vec![0x09, 1]);
        assert_eq!(p[1], vec![0x06, 1, 0, 1, 1]);
    }

    #[test]
    fn reconnect_key_is_stored_and_sent() {
        let t0 = Instant::now();
        let mut s = Session::new(info(""), None, t0).unwrap();
        let keys = s.info.user_keys.clone().unwrap();
        let reconnect = vec![0x5A; 128];
        let cipher = crate::crypto::aes_cbc_encrypt(&keys.key, &keys.iv, &reconnect);
        assert_eq!(
            s.receive(&incoming(0x40, &cipher), t0),
            vec![Event::Authenticated]
        );
        assert_eq!(s.reconnect_key(), Some(reconnect.as_slice()));

        let mut again = Session::new(info(""), Some(reconnect.clone()), t0).unwrap();
        let connect = &payloads(&again.take_outgoing())[1];
        assert_eq!(connect[..5], [0x06, 1, 0, 1, 1]);
        assert_eq!(connect[5..], reconnect[..]);
    }

    #[test]
    fn keyboard_uses_legacy_key_when_not_compressed() {
        let mut s = Session::new(info(""), None, Instant::now()).unwrap();
        s.take_outgoing();
        s.key("KeyA", true);
        let p = payloads(&s.take_outgoing());
        assert_eq!(p.len(), 1);
        assert_eq!(p[0][..2], [0x03, 1]);
        assert_eq!(
            p[0][2..],
            encrypt_legacy(CODE_KEY, &[0, 0, 0x04, 0, 0, 0, 0, 0])[..]
        );
    }

    #[test]
    fn plain_power_and_close_reason() {
        let t0 = Instant::now();
        let mut s = Session::new(info(""), None, t0).unwrap();
        s.take_outgoing();
        s.power(PowerCommand::GracefulReboot);
        assert_eq!(payloads(&s.take_outgoing()), vec![vec![0x23, 0]]);
        assert_eq!(
            s.receive(&incoming(0x08, &[17, 0, 0, 0]), t0),
            vec![Event::Closed(CloseReason::SessionTimeout)]
        );
    }

    #[test]
    fn mouse_waits_for_first_frame_size() {
        let mut s = Session::new(info(""), None, Instant::now()).unwrap();
        s.take_outgoing();
        s.mouse(10, 10, 1, 0);
        assert!(s.take_outgoing().is_empty());
    }

    #[test]
    fn compress_requires_user_keys() {
        let mut i = info("");
        i.compress = true;
        i.user_keys = None;
        assert_eq!(
            Session::new(i, None, Instant::now()).err(),
            Some(SessionError::MissingUserKeys)
        );
    }

    /// Видео от сокета до события: сообщения 0x02 → сборка → декодер → `Event::Frame`,
    /// затем мышь в координатах этого кадра. Данные приходят кусками, как из TCP.
    #[test]
    fn video_frame_then_absolute_mouse() {
        let t0 = Instant::now();
        let mut s = Session::new(info(""), None, t0).unwrap();
        s.take_outgoing();

        // Два сплошных RLE-блока (YCbCr): белый и серый; качество 7, I-кадр.
        let blocks = [0x00, 255, 128, 128, 0x00, 128, 128, 128];
        let mut stream = Vec::new();
        for fragment in crate::video::assembler::tests::fragments(1, 128, 64, 0x87, &blocks) {
            stream.extend(incoming(IMAGE_DATA, &fragment));
        }
        let mut events = Vec::new();
        for piece in stream.chunks(7) {
            events.extend(s.receive(piece, t0));
        }
        assert_eq!(
            events,
            vec![Event::Frame {
                width: 128,
                height: 64,
                changed: vec![0, 1]
            }]
        );
        assert!(s
            .decoder()
            .block_pixels(0)
            .unwrap()
            .iter()
            .all(|&p| p == 0xFFFFFF));
        assert!(s
            .decoder()
            .block_pixels(1)
            .unwrap()
            .iter()
            .all(|&p| p == 0x808080));
        assert!(s.take_outgoing().is_empty(), "без I_REQ");

        s.mouse(64, 32, input::button::LEFT, 0);
        assert_eq!(
            payloads(&s.take_outgoing()),
            vec![vec![0x05, 1, 1, 0x05, 0xDC, 0x05, 0xDC, 0]]
        );
    }

    #[test]
    fn broken_video_requests_iframe_once_per_interval() {
        let t0 = Instant::now();
        let mut s = Session::new(info(""), None, t0).unwrap();
        s.take_outgoing();
        // Фрагмент 2 без фрагмента 1 — рассинхронизация.
        let lost = incoming(IMAGE_DATA, &[0, 2, 5, 1, 2, 3]);
        s.receive(&lost, t0);
        s.receive(&lost, t0 + Duration::from_millis(600));
        s.receive(&lost, t0 + Duration::from_millis(700));
        s.receive(&lost, t0 + Duration::from_millis(1300));
        assert_eq!(
            payloads(&s.take_outgoing()),
            vec![vec![0x08, 1], vec![0x08, 1]]
        );
    }
}
