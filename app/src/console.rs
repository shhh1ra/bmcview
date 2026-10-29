//! Сетевой цикл KVM-сессии: TCP-сокет, таймеры и ввод из UI вокруг [`Session`] из ядра.
//!
//! Видео уходит в UI бинарным каналом (формат — [`encode_frame`]), остальные события —
//! JSON-каналом ([`UiEvent`]). При обрыве TCP цикл переподключается по ключу из `0x40`
//! без повторного логина, как оригинал (`BladeReconnect`).

use std::time::{Duration, Instant};

use ibmc_core::input::{self, combo, KeyReport};
use ibmc_core::login::LoginInfo;
use ibmc_core::session::{CloseReason, Event, PowerCommand, Session};
use ibmc_core::video::{Decoder, BLOCK_SIZE};
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_ATTEMPTS: u32 = 5;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Без данных от BMC дольше этого считаем соединение мёртвым (видео и ответы на heartbeat
/// приходят постоянно; на заставке приходят хотя бы кадры «не изменился»).
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Действия пользователя из UI.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    Key {
        code: String,
        down: bool,
    },
    ReleaseKeys,
    /// Готовая комбинация из панели «Комбинации клавиш».
    Combo {
        combo: ComboKey,
    },
    /// Своя комбинация: до шести `KeyboardEvent.code`.
    CustomCombo {
        codes: Vec<String>,
    },
    ResetKeyboard,
    Mouse {
        x: u32,
        y: u32,
        buttons: u8,
        wheel: i8,
    },
    Quality {
        value: u8,
    },
    Power {
        action: PowerAction,
    },
    Refresh,
    Disconnect,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComboKey {
    CtrlShift,
    CtrlEsc,
    CtrlAltDel,
    AltTab,
    CtrlSpace,
}

impl ComboKey {
    fn report(self) -> KeyReport {
        match self {
            ComboKey::CtrlShift => combo::CTRL_SHIFT,
            ComboKey::CtrlEsc => combo::CTRL_ESC,
            ComboKey::CtrlAltDel => combo::CTRL_ALT_DEL,
            ComboKey::AltTab => combo::ALT_TAB,
            ComboKey::CtrlSpace => combo::CTRL_SPACE,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PowerAction {
    ForceOff,
    PowerOn,
    ColdReset,
    GracefulReboot,
    GracefulOff,
    UsbReset,
}

impl From<PowerAction> for PowerCommand {
    fn from(action: PowerAction) -> Self {
        match action {
            PowerAction::ForceOff => PowerCommand::ForceOff,
            PowerAction::PowerOn => PowerCommand::PowerOn,
            PowerAction::ColdReset => PowerCommand::ColdReset,
            PowerAction::GracefulReboot => PowerCommand::GracefulReboot,
            PowerAction::GracefulOff => PowerCommand::GracefulOff,
            PowerAction::UsbReset => PowerCommand::UsbReset,
        }
    }
}

/// События для UI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UiEvent {
    Connected,
    Reconnecting { attempt: u32 },
    Closed { reason: String },
    Leds { value: u8 },
    MouseMode { absolute: bool },
    Quality { value: u8 },
}

pub type InputSender = mpsc::UnboundedSender<Input>;

/// Запускает сессию в фоне. Возвращает канал ввода; сессия завершается по
/// [`Input::Disconnect`], закрытию канала или окончательному обрыву связи.
pub fn spawn(
    info: LoginInfo,
    host: String,
    frames: Channel<InvokeResponseBody>,
    events: Channel<UiEvent>,
) -> InputSender {
    let (tx, rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(run(info, host, frames, events, rx));
    tx
}

enum End {
    User,
    Server(CloseReason),
    Lost(String),
}

async fn run(
    info: LoginInfo,
    host: String,
    frames: Channel<InvokeResponseBody>,
    events: Channel<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<Input>,
) {
    let mut reconnect_key: Option<Vec<u8>> = None;
    let mut attempt = 0;
    loop {
        let end = match connect(&host, info.kvm_port).await {
            Ok(stream) => match Session::new(info.clone(), reconnect_key.clone(), Instant::now()) {
                Ok(session) => {
                    let _ = events.send(UiEvent::Connected);
                    let (end, key, got_video) =
                        drive(stream, session, &frames, &events, &mut rx).await;
                    if key.is_some() {
                        reconnect_key = key;
                    }
                    if got_video {
                        attempt = 0;
                    }
                    end
                }
                Err(e) => End::Lost(e.to_string()),
            },
            Err(e) => End::Lost(e),
        };
        let reason = match end {
            End::User => return,
            End::Server(reason) => describe(reason),
            End::Lost(_) if reconnect_key.is_some() && attempt < RECONNECT_ATTEMPTS => {
                attempt += 1;
                let _ = events.send(UiEvent::Reconnecting { attempt });
                // Ввод во время паузы не нужен, но Disconnect должен сработать сразу.
                let deadline = tokio::time::Instant::now() + RECONNECT_DELAY;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        input = rx.recv() => match input {
                            None | Some(Input::Disconnect) => return,
                            Some(_) => {}
                        },
                    }
                }
                continue;
            }
            End::Lost(error) => format!("связь с BMC потеряна: {error}"),
        };
        let _ = events.send(UiEvent::Closed { reason });
        return;
    }
}

async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| "BMC не принял KVM-соединение".to_string())?
        .map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Одна TCP-сессия. Возвращает причину конца, ключ переподключения и были ли кадры.
async fn drive(
    mut stream: TcpStream,
    mut session: Session,
    frames: &Channel<InvokeResponseBody>,
    events: &Channel<UiEvent>,
    rx: &mut mpsc::UnboundedReceiver<Input>,
) -> (End, Option<Vec<u8>>, bool) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut got_video = false;
    let mut last_data = Instant::now();
    let end = loop {
        let out = session.take_outgoing();
        if !out.is_empty() {
            if let Err(e) = stream.write_all(&out).await {
                break End::Lost(e.to_string());
            }
        }
        let deadline = session.next_deadline().min(last_data + IDLE_TIMEOUT);
        tokio::select! {
            read = stream.read(&mut buf) => match read {
                Ok(0) => break End::Lost("BMC закрыл соединение".into()),
                Ok(n) => {
                    last_data = Instant::now();
                    for event in session.receive(&buf[..n], last_data) {
                        match event {
                            Event::Frame { width, height, changed } => {
                                got_video = true;
                                let bytes = encode_frame(session.decoder(), width, height, &changed);
                                let _ = frames.send(InvokeResponseBody::Raw(bytes));
                            }
                            Event::Leds(value) => { let _ = events.send(UiEvent::Leds { value }); }
                            Event::MouseMode(absolute) => {
                                let _ = events.send(UiEvent::MouseMode { absolute });
                            }
                            Event::Quality(value) => { let _ = events.send(UiEvent::Quality { value }); }
                            Event::Authenticated => {}
                            Event::Closed(reason) => {
                                return (End::Server(reason), session.reconnect_key().map(<[u8]>::to_vec), got_video);
                            }
                        }
                    }
                }
                Err(e) => break End::Lost(e.to_string()),
            },
            input = rx.recv() => match input {
                None | Some(Input::Disconnect) => break End::User,
                Some(input) => apply(&mut session, input),
            },
            _ = tokio::time::sleep_until(deadline.into()) => {
                let now = Instant::now();
                if now >= last_data + IDLE_TIMEOUT {
                    break End::Lost("BMC перестал отвечать".into());
                }
                session.poll(now);
            }
        }
    };
    (end, session.reconnect_key().map(<[u8]>::to_vec), got_video)
}

fn apply(session: &mut Session, input: Input) {
    match input {
        Input::Key { code, down } => session.key(&code, down),
        Input::ReleaseKeys => session.release_keys(),
        Input::Combo { combo } => session.send_combo(combo.report(), Instant::now()),
        Input::CustomCombo { codes } => {
            if let Some(report) = input::custom_report(&codes) {
                session.send_combo(report, Instant::now());
            }
        }
        Input::ResetKeyboard => session.reset_keyboard(Instant::now()),
        Input::Mouse {
            x,
            y,
            buttons,
            wheel,
        } => session.mouse(x, y, buttons, wheel),
        Input::Quality { value } => session.set_quality(value),
        Input::Power { action } => session.power(action.into()),
        Input::Refresh => session.refresh(),
        Input::Disconnect => {}
    }
}

fn describe(reason: CloseReason) -> String {
    match reason {
        CloseReason::TooManyUsers => "превышено число пользователей KVM".into(),
        CloseReason::BadCredentials => "BMC отклонил данные сессии".into(),
        CloseReason::SignalOutOfRange => "видеосигнал вне допустимого диапазона".into(),
        CloseReason::ServerDisabled => "KVM на BMC отключён".into(),
        CloseReason::UserDeleted => "пользователь удалён на BMC".into(),
        CloseReason::SessionTimeout => "сессия закрыта по таймауту".into(),
        CloseReason::BladeClosed => "BMC закрыл KVM-сессию".into(),
        CloseReason::Other(code) => format!("BMC закрыл сессию (код {code})"),
    }
}

/// Бинарный формат кадра для UI (little-endian):
/// `width u16 | height u16 | count u16 | count × (x u16 | y u16 | 64×64×4 байт RGBA)`.
/// Крайние блоки передаются целиком — canvas сам обрезает лишнее.
pub fn encode_frame(decoder: &Decoder, width: usize, height: usize, changed: &[usize]) -> Vec<u8> {
    let blocks: Vec<_> = changed
        .iter()
        .filter_map(|&i| Some((decoder.block_origin(i), decoder.block_pixels(i)?)))
        .collect();
    let mut out = Vec::with_capacity(6 + blocks.len() * (4 + BLOCK_SIZE * BLOCK_SIZE * 4));
    out.extend_from_slice(&(width as u16).to_le_bytes());
    out.extend_from_slice(&(height as u16).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u16).to_le_bytes());
    for ((x, y), pixels) in blocks {
        out.extend_from_slice(&(x as u16).to_le_bytes());
        out.extend_from_slice(&(y as u16).to_le_bytes());
        for &c in pixels.iter() {
            out.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8, 0xFF]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::net::TcpListener;

    use super::*;

    fn message(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut body = vec![0, 0, kind, 0];
        body.extend_from_slice(data);
        let mut out = vec![0xFE, 0xF6, 0, body.len() as u8];
        out.extend(body);
        out
    }

    /// Кадр 128×64 из двух сплошных блоков (белый и серый), как его режет BMC.
    fn video() -> Vec<u8> {
        let blocks = [0x00, 255, 128, 128, 0x00, 128, 128, 128];
        let mut header = vec![0, 0, 1];
        header.extend_from_slice(&(blocks.len() as u32).to_be_bytes());
        header.extend_from_slice(&[0, 128, 0, 64, 0, 0, 10, 0, 20, 0x87]);
        let mut data = vec![0, 1, 1];
        data.extend_from_slice(&blocks);
        [message(0x02, &header), message(0x02, &data)].concat()
    }

    fn collector<S, T: Send + 'static>(
        convert: fn(InvokeResponseBody) -> T,
    ) -> (Channel<S>, Arc<Mutex<Vec<T>>>) {
        let store = Arc::new(Mutex::new(Vec::new()));
        let sink = store.clone();
        let channel = Channel::new(move |body| {
            sink.lock().unwrap().push(convert(body));
            Ok(())
        });
        (channel, store)
    }

    /// Сквозная проверка сетевого цикла на поддельном BMC: подключение, кадр в UI, закрытие.
    #[tokio::test]
    async fn session_against_fake_bmc() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let bmc = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 1024];
            // Ждём CONNECT_BLADE: заголовок 10 байт, затем 06 01 …
            while !received.windows(2).any(|w| w == [0x06, 0x01]) {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "клиент закрыл соединение");
                received.extend_from_slice(&buf[..n]);
            }
            socket.write_all(&video()).await.unwrap();
            socket.write_all(&message(0x04, &[2])).await.unwrap();
            socket.write_all(&message(0x08, &[17])).await.unwrap();
            received
        });

        let info = LoginInfo {
            code_key: 0x635a_8e2f,
            verify_value: String::new(),
            user_keys: None,
            privilege: "4".into(),
            compress: false,
            vmm_compress: false,
            kvm_port: port,
            vmm_port: 0,
            serial_number: String::new(),
            verify_value_ext: String::new(),
        };
        let (frames, frame_log) = collector::<InvokeResponseBody, _>(|body| match body {
            InvokeResponseBody::Raw(bytes) => bytes,
            InvokeResponseBody::Json(_) => panic!("кадр должен быть бинарным"),
        });
        let (events, event_log) = collector::<UiEvent, _>(|body| match body {
            InvokeResponseBody::Json(json) => json,
            InvokeResponseBody::Raw(_) => panic!("событие должно быть JSON"),
        });
        let (_tx, rx) = mpsc::unbounded_channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            run(info, "127.0.0.1".into(), frames, events, rx),
        )
        .await
        .expect("сессия завершилась");

        let received = bmc.await.unwrap();
        assert_eq!(&received[..2], &[0xFE, 0xF6]);
        assert_eq!(&received[4..8], &0x635a_8e2fi32.to_be_bytes());

        let events = event_log.lock().unwrap().clone();
        assert_eq!(events.first().unwrap(), r#"{"type":"connected"}"#);
        assert!(events.contains(&r#"{"type":"leds","value":2}"#.to_string()));
        assert!(events.last().unwrap().contains("таймауту"), "{events:?}");

        let frames = frame_log.lock().unwrap();
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!(&frame[..6], &[128, 0, 64, 0, 2, 0]);
        assert_eq!(&frame[6..10], &[0, 0, 0, 0], "первый блок в (0, 0)");
        assert_eq!(&frame[10..14], &[0xFF, 0xFF, 0xFF, 0xFF]);
        let second = 6 + 4 + 64 * 64 * 4;
        assert_eq!(
            &frame[second..second + 4],
            &[64, 0, 0, 0],
            "второй блок в (64, 0)"
        );
        assert_eq!(&frame[second + 4..second + 8], &[0x80, 0x80, 0x80, 0xFF]);
    }
}
