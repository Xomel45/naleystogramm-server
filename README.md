<div align="center">

# Naleystogramm Server

**Опциональный сервер обнаружения, релея и групповых чатов для Naleystogramm**

[![Version](https://img.shields.io/badge/version-0.1.0-7c6aff?style=flat-square)](https://github.com/Xomel45/naleystogramm-server/releases)
[![Platform](https://img.shields.io/badge/platform-Linux-4a4a7a?style=flat-square)](#сборка)
[![Rust](https://img.shields.io/badge/Rust-stable-dea584?style=flat-square)](https://www.rust-lang.org/)

</div>

---

## О проекте

[Naleystogramm](https://github.com/Xomel45/naleystogramm) — P2P-мессенджер, который работает и без единого сервера. `naleys-server` — необязательная надстройка для тех, кому нужно:

- находить контакты по имени (`vasya@myserver.example`) вместо ручного ввода UUID+IP;
- передавать файлы, когда прямое P2P-соединение невозможно (relay-fallback);
- запустить групповой чат с друзьями.

Сервер **не видит содержимое переписки** — он либо просто отдаёт `ip:port + pubkey` для прямого соединения (discovery), либо форвардит уже зашифрованные байты (relay), либо хранит сообщения, зашифрованные общим групповым ключом, который сам расшифровать не может (group).

---

## Режимы запуска

| Режим | Флаг | Что делает |
|---|---|---|
| **Discovery** (по умолчанию) | `--mode discovery` | Хранит `username → pubkey + ip:port`. Трафик сообщений через сервер **не идёт** — только поиск контакта, дальше клиенты соединяются P2P напрямую. |
| **Group** | `--mode group` | Чат-комната: сообщения проходят через сервер и рассылаются всем участникам по WebSocket. Содержимое зашифровано групповым ключом, сервер его не знает. |

Relay (передача файлов через сервер как fallback) работает в обоих режимах, если `relay.enabled: true` в конфиге.

Полная спецификация API, схема БД и формат WebSocket-фреймов — в [DESIGN.md](DESIGN.md).

---

## Быстрый старт

```bash
git clone https://github.com/Xomel45/naleystogramm-server.git
cd naleystogramm-server
cp config.json config.local.json   # отредактируй под себя
cargo run --release -- --config config.local.json --mode discovery
```

Сервер поднимется на порту из `config.json` (по умолчанию `47822`) и создаст SQLite-базу `naleys_server.db` рядом с собой.

Проверка, что всё работает:

```bash
curl http://localhost:47822/info
```

---

## Конфигурация

Ключевые поля `config.json` (полный список — в [DESIGN.md](DESIGN.md#конфиг-configjson)):

| Поле | Значение |
|---|---|
| `server.port` | Порт сервера (по умолчанию `47822`) |
| `registration.open` | `false` — регистрация только по инвайту |
| `registration.max_users` | `0` — без лимита |
| `storage.driver` | `sqlite` (по умолчанию) или `postgres` |
| `tokens.ttl_days` | `0` — токены бессрочные |
| `compatibility.min_client_version` | `"latest"` — сервер сам подтягивает минимальную версию клиента из [GitHub Releases](https://github.com/Xomel45/naleystogramm/releases) каждые 6 часов |
| `relay.enabled` | Включить relay-fallback для передачи файлов |
| `group.*` | Настройки группового режима (имя, лимит участников, история) |

---

## Сборка и деплой

```bash
# glibc-бинарь (Debian, Ubuntu, AlmaLinux, CentOS, Rocky, Oracle) → builds/releases/VERSION-gnu/
./deploy.sh gnu

# musl-бинарь (Alpine) → builds/releases/VERSION-musl/
./deploy.sh musl

# Оба варианта сразу
./deploy.sh all

# Пакеты
./deploy.sh deb     # .deb (Debian / Ubuntu)
./deploy.sh rpm     # .rpm (AlmaLinux / Rocky / CentOS)

# Установка на текущий хост (нужен root) — бинарь в /usr/local/bin,
# конфиг в /etc/naleys-server.json, systemd unit из deploy/
./deploy.sh install

# Очистить целевую директорию перед сборкой
./deploy.sh gnu --clean
```

Каждый таргет кладёт в `builds/releases/` готовый бинарь + `config.json.example` + `naleys-server.service` + `INSTALL.md` с инструкцией для конкретной платформы.

### Запуск как systemd-служба

```bash
cp deploy/naleys-server.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now naleys-server
journalctl -u naleys-server -f
```

---

## Технологии

| | |
|---|---|
| **Язык** | Rust (один статический бинарь) |
| **HTTP / WebSocket** | `axum` + `tokio` |
| **БД** | SQLite (по умолчанию) или PostgreSQL — через `sqlx` |
| **Криптография** | `x25519-dalek`, `hkdf`, `aes-gcm` (групповой ключ), `sha2` (хеш токенов) |
| **Прочее** | `clap` (CLI), `reqwest` (проверка версии клиента через GitHub), `tracing` (логи) |

---

## Структура проекта

```
src/
├── main.rs          — точка входа, CLI-флаги (--config, --mode), сборка роутера
├── config.rs        — парсинг config.json
├── state.rs         — общее состояние приложения (пул БД, конфиг)
├── db.rs            — пул соединений, миграции
├── auth.rs          — Bearer-токены
├── discovery.rs     — режим discovery: /register, /lookup, /search, /heartbeat...
├── group.rs         — режим group: /group/join, /group/ws, /group/history...
├── relay.rs         — relay-сессии для передачи файлов (общий для обоих режимов)
├── rate_limit.rs    — ограничение запросов по IP
├── version.rs       — фоновое обновление min_client_version из GitHub
└── error.rs         — единый тип ошибок API
deploy/              — systemd unit
deploy.sh            — сборка и упаковка релизов
DESIGN.md            — полная спецификация API, схема БД, протокол
```

---

## Безопасность

- Токены хранятся только как SHA-256-хеш, не в открытом виде
- Rate limiting на `/register` и `/lookup`
- Сервер не участвует в E2E-соединении — скомпрометированный сервер может вернуть чужой `pubkey`, поэтому клиент обязан верифицировать ключ собеседника через X3DH handshake

Подробнее — раздел [«Безопасность» в DESIGN.md](DESIGN.md#безопасность).

---

<div align="center">

*Часть проекта [Naleystogramm](https://github.com/Xomel45/naleystogramm) — desktop-клиент: [naleystogramm](https://github.com/Xomel45/naleystogramm) · Android: [naleystogramm-mobile](https://github.com/Xomel45/naleystogramm-mobile)*

</div>
