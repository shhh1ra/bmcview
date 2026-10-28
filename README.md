# bmcview

Собственный клиент KVM-консоли для Huawei iBMC (серверы RH1288 V3) вместо штатного Java-клиента.
Работает на Windows 10/11 и macOS.

Статус: **ранняя разработка**. Готово ядро: фрейминг пакетов, CRC, криптография сессии,
сборка и декодирование видео. Видео и криптография побитово сверены с оригинальным Java-кодом.
Есть оболочка Tauri с заглушкой UI, CI на Windows и macOS и пре-релизы по тегу.
Сети и логина ещё нет. Журнал работ — [docs/devlog.md](docs/devlog.md).

## Документация

| Документ | О чём |
|---|---|
| [docs/protocol.md](docs/protocol.md) | Протокол, восстановленный по декомпиляции оригинального клиента |
| [docs/architecture.md](docs/architecture.md) | Устройство приложения и принятые решения |
| [docs/verification.md](docs/verification.md) | Как проверяем, что порт совпадает с оригиналом |
| [docs/releasing.md](docs/releasing.md) | CI, выпуск пре-релизов, установка без подписи, стоимость Actions |
| [docs/devlog.md](docs/devlog.md) | Журнал разработки: что сделано, что выяснилось, что дальше |

## Структура

```
core/     протокол, крипто, видео — Rust, без UI и без зависимостей от ОС
app/      оболочка Tauri v2: окно, IPC, сборка установщиков
ui/       интерфейс: TypeScript + Vite (пока заглушка)
tools/    генераторы и проверочные скрипты (oracle — прогон оригинального Java-кода)
assets/   исходник иконки
docs/     документация
.github/  CI и релизы
```

Будет добавлен платформенный захват клавиатуры.

## Сборка и тесты

Нужны Rust 1.88+ и Node.js 22+. На Windows дополнительно — MSVC Build Tools (C++).

```bash
npm ci
npm run build          # UI — нужен до сборки Rust-части
cargo test --workspace
npm run tauri dev      # запустить приложение
```

Подробнее про сборку установщиков и релизы — [docs/releasing.md](docs/releasing.md).

Без MSVC Build Tools ядро собирается и GNU-тулчейном Rust (свой линкер, права администратора не нужны):

```bash
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
cargo +stable-x86_64-pc-windows-gnu test
```

Пересборка эталонов из оригинального клиента описана в [docs/verification.md](docs/verification.md).
