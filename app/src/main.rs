//! Оболочка Tauri: окно, IPC между UI и ядром, сеть.

// В релизной сборке под Windows не открывать консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod console;
mod known_hosts;
mod tls;
mod web_login;

use std::sync::Mutex;

use ibmc_core::login::{KvmMode, LoginError};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager, State};

use console::{Input, InputSender, UiEvent};
use known_hosts::KnownHosts;
use web_login::{Address, WebLoginError};

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    core_version: &'static str,
    os: &'static str,
    arch: &'static str,
}

/// Сведения о сборке для UI.
#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        core_version: ibmc_core::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

#[derive(Default)]
struct ConsoleState {
    input: Mutex<Option<InputSender>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Connected {
    fingerprint: String,
    /// Сертификат этого BMC встретился впервые и запомнен.
    new_certificate: bool,
    /// KVM-трафик не шифруется (`compress=0`): ввод с клавиатуры читаем в сети.
    unencrypted: bool,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ConnectError {
    BadAddress,
    CertificateChanged { expected: String, actual: String },
    Failed { message: String },
}

impl From<WebLoginError> for ConnectError {
    fn from(e: WebLoginError) -> Self {
        match e {
            WebLoginError::CertificateChanged { expected, actual } => {
                ConnectError::CertificateChanged { expected, actual }
            }
            WebLoginError::Login(LoginError::Failed(_)) => ConnectError::Failed {
                message: "неверное имя пользователя или пароль".into(),
            },
            other => ConnectError::Failed {
                message: other.to_string(),
            },
        }
    }
}

/// Логин и запуск KVM-сессии. `trust` — отпечаток, которому пользователь явно доверился
/// после предупреждения о смене сертификата.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, ConsoleState>,
    address: String,
    user: String,
    password: String,
    exclusive: bool,
    trust: Option<String>,
    frames: Channel<InvokeResponseBody>,
    events: Channel<UiEvent>,
) -> Result<Connected, ConnectError> {
    disconnect(state.clone());
    let address = Address::parse(&address).ok_or(ConnectError::BadAddress)?;
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| ConnectError::Failed {
            message: e.to_string(),
        })?
        .join("known_hosts.json");
    let mut known = KnownHosts::load(path);
    let remembered = known.get(&address.key());
    let mode = if exclusive {
        KvmMode::Exclusive
    } else {
        KvmMode::Shared
    };
    let result = web_login::login(
        &address,
        &user,
        &password,
        mode,
        trust.or(remembered.clone()),
    )
    .await?;
    let new_certificate = remembered.as_deref() != Some(result.fingerprint.as_str());
    known
        .set(address.key(), result.fingerprint.clone())
        .map_err(|e| ConnectError::Failed {
            message: format!("не удалось сохранить отпечаток сертификата: {e}"),
        })?;
    let unencrypted = !result.info.compress;
    let sender = console::spawn(result.info, address.host, frames, events);
    *state.input.lock().unwrap() = Some(sender);
    Ok(Connected {
        fingerprint: result.fingerprint,
        new_certificate,
        unencrypted,
    })
}

#[tauri::command]
fn send_input(state: State<'_, ConsoleState>, input: Input) {
    if let Some(sender) = state.input.lock().unwrap().as_ref() {
        let _ = sender.send(input);
    }
}

#[tauri::command]
fn disconnect(state: State<'_, ConsoleState>) {
    if let Some(sender) = state.input.lock().unwrap().take() {
        let _ = sender.send(Input::Disconnect);
    }
}

fn main() {
    tauri::Builder::default()
        .manage(ConsoleState::default())
        .invoke_handler(tauri::generate_handler![
            app_info, connect, send_input, disconnect
        ])
        .run(tauri::generate_context!())
        .expect("ошибка запуска приложения");
}
