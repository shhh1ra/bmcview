# bmcview

Собственный клиент KVM-консоли для Huawei iBMC (серверы RH1288 V3) вместо штатного Java-клиента.
Работает на Windows 10/11 и macOS.

Статус: **ранняя разработка**. Готово ядро: фрейминг пакетов, CRC, криптография сессии,
сборка и декодирование видео. Видео и криптография побитово сверены с оригинальным Java-кодом.
Сети, логина и UI ещё нет. Журнал работ — [docs/devlog.md](docs/devlog.md).

## Документация

| Документ | О чём |
|---|---|
| [docs/protocol.md](docs/protocol.md) | Протокол, восстановленный по декомпиляции оригинального клиента |
| [docs/architecture.md](docs/architecture.md) | Устройство приложения и принятые решения |
| [docs/verification.md](docs/verification.md) | Как проверяем, что порт совпадает с оригиналом |
| [docs/devlog.md](docs/devlog.md) | Журнал разработки: что сделано, что выяснилось, что дальше |

## Структура

```
core/     протокол, крипто, видео — Rust, без UI и без зависимостей от ОС
tools/    генераторы и проверочные скрипты (oracle — прогон оригинального Java-кода)
docs/     документация
```

Будут добавлены: `app/` (оболочка Tauri), `ui/` (TypeScript + canvas), платформенный захват клавиатуры.

## Сборка и тесты

Нужен Rust 1.88+. На Windows дополнительно — MSVC Build Tools (C++).

```bash
cargo test
```

Без MSVC Build Tools ядро собирается и GNU-тулчейном Rust (свой линкер, права администратора не нужны):

```bash
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
cargo +stable-x86_64-pc-windows-gnu test
```

Пересборка эталонов из оригинального клиента описана в [docs/verification.md](docs/verification.md).
