# naleystogramm-server — Discovery & Presence Server / Group Server

P2P-мессенджер без центрального сервера, но с опциональным сервером обнаружения.

Два режима запуска:
- **`--mode discovery`** (по умолчанию) — сервер обнаружения: хранит `username → pubkey + ip:port`, трафик сообщений через него **не идёт**
- **`--mode group`** — сервер-группа: чат-комната, сообщения проходят через сервер и рассылаются всем участникам

---

## Концепция

```
Клиент А регистрируется:  POST /register  {username, pubkey, ip, port}
                          ← {token}

Клиент Б ищет:            GET /lookup/vasya
                          ← {pubkey, ip, port, last_seen}

Клиент Б подключается:    P2P напрямую к ip:port — сервер больше не участвует

Верификация:              при первом соединении Б проверяет pubkey из lookup
                          совпадает с ключом из X3DH handshake (защита от MITM)
```

Адрес пользователя: `vasya@myserver.example` или `vasya@1.2.3.4:47822`

---

## Конфиг (config.json)

```json
{
  "server": {
    "port": 47822,
    "name": "My Naleystogramm Server",
    "description": "Private server for friends",
    "public": true
  },
  "registration": {
    "open": true,
    "require_email": false,
    "require_invite": false,
    "max_users": 0,
    "username_min_length": 3,
    "username_max_length": 32,
    "username_regex": "^[a-zA-Z0-9_.-]+$"
  },
  "tokens": {
    "ttl_days": 0,
    "max_per_user": 3
  },
  "storage": {
    "driver": "sqlite",      // или "postgres"
    "path": "./naleys_server.db",
    "url": ""                // для postgres: "postgres://user:pass@localhost/naleys"
  },
  "presence": {
    "offline_after_minutes": 30,
    "ip_update_interval_seconds": 60
  },
  "compatibility": {
    "min_client_version": "latest"
  }
}
```

**Поля:**
- `port` — порт сервера (по умолчанию 47822)
- `public: true` — сервер виден в `/info` и может быть добавлен в публичные списки
- `open: false` — только по инвайту или отключена регистрация (сервер только для известных)
- `require_email: true` — при регистрации обязателен email (для восстановления токена)
- `max_users: 0` — без лимита; `100` — максимум 100 аккаунтов
- `ttl_days: 0` — токены бессрочные; `30` — истекают через 30 дней
- `offline_after_minutes` — через сколько минут без heartbeat считать пользователя offline

---

## API

### Публичные (без токена)

| Метод | Путь | Описание |
|---|---|---|
| `GET` | `/info` | Информация о сервере (name, version, open, require_email, min_client_version, stats) |
| `POST` | `/register` | Регистрация нового пользователя |
| `GET` | `/lookup/{username}` | Получить ip:port + pubkey пользователя |
| `GET` | `/search?q={prefix}` | Поиск по началу имени (опционально) |

### Приватные (Bearer токен)

| Метод | Путь | Описание |
|---|---|---|
| `PUT` | `/update` | Обновить ip:port (при смене адреса) |
| `POST` | `/heartbeat` | Подтвердить online-статус |
| `DELETE` | `/unregister` | Удалить аккаунт |
| `POST` | `/token/refresh` | Обновить токен (если TTL включён) |

---

## Регистрация

**Request:**
```json
POST /register
{
  "username": "vasya",
  "pubkey": "<base64 X25519 public key>",
  "ip": "1.2.3.4",
  "port": 47821,
  "email": "vasya@example.com",   // только если require_email: true
  "invite_code": "XXXX-YYYY",      // только если require_invite: true
  "client_version": "0.8.0"
}
```

**Проверки (в порядке):**
1. `client_version` ≥ `min_client_version` (semver сравнение)
2. Регистрация открыта (`open: true`) или валидный `invite_code`
2. `username` соответствует `username_regex`
3. `username` длина в диапазоне `[min, max]`
4. `username` не занят (case-insensitive)
5. `pubkey` — валидный base64, длина 32 байта (X25519)
6. `ip` — валидный IPv4 или IPv6; `port` 1–65535
7. `email` валиден (если `require_email: true`)
8. Лимит пользователей не превышен (`max_users > 0`)

**Response:**
```json
201 Created
{
  "token": "naleys_<64 hex chars>",
  "username": "vasya",
  "server": "myserver.example:47822"
}
```

**Ошибки:**
```json
409 { "error": "username_taken" }
400 { "error": "invalid_username", "detail": "..." }
403 { "error": "registration_closed" }
403 { "error": "invalid_invite" }
426 { "error": "client_too_old", "min_version": "0.8.0" }
429 { "error": "server_full" }
```

---

## Lookup

```json
GET /lookup/vasya

200 OK
{
  "username": "vasya",
  "pubkey": "<base64>",
  "ip": "1.2.3.4",
  "port": 47821,
  "online": true,
  "last_seen": "2026-05-26T12:34:56Z"
}

404 { "error": "not_found" }
```

---

## Relay — передача файлов

Используется как fallback когда прямое P2P соединение невозможно (симметричный NAT и т.п.).
Сервер — тупая труба: форвардит зашифрованные чанки между двумя клиентами. Содержимое файла недоступно серверу — E2E-шифрование остаётся на клиентах.

**Схема:**
```
Клиент А (sender):   POST /relay/create        → {session_id, token_a}
Клиент Б (receiver): POST /relay/join/{id}     → {token_b}

Оба подключаются по WebSocket:
  ws://server/relay/{session_id}?token=<token>

Сервер соединяет два WebSocket и форвардит байты A→B и B→A.
Сессия закрывается когда один из клиентов отключается.
```

**API:**

| Метод | Путь | Описание |
|---|---|---|
| `POST` | `/relay/create` | Создать relay-сессию (sender, Bearer токен) |
| `POST` | `/relay/join/{id}` | Присоединиться к сессии (receiver, Bearer токен) |
| `GET` | `/relay/{id}` | WebSocket — двусторонний форвард байтов |

**Конфиг:**
```json
"relay": {
  "enabled": true,
  "max_sessions": 100,
  "session_timeout_seconds": 30,
  "max_session_bytes": 0
}
```
- `max_sessions: 0` — без лимита
- `session_timeout_seconds` — сессия закрывается если второй клиент не подключился за N секунд
- `max_session_bytes: 0` — без лимита; например `1073741824` = 1 ГБ максимум на сессию

**Таблица в БД:**
```sql
CREATE TABLE relay_sessions (
  id          TEXT PRIMARY KEY,         -- UUID сессии
  creator_id  INTEGER REFERENCES users(id),
  joiner_id   INTEGER REFERENCES users(id),
  created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
  bytes_total INTEGER DEFAULT 0
);
```

**Клиент — логика выбора:**
```
1. Попытка прямого P2P (существующий FileTransfer)
2. Попытка через UPnP
3. Fallback: relay через сервер (если сервер настроен и relay enabled)
```

---

## База данных (SQLite)

```sql
CREATE TABLE users (
  id          INTEGER PRIMARY KEY,
  username    TEXT UNIQUE NOT NULL,
  username_lc TEXT UNIQUE NOT NULL,  -- lower-case для case-insensitive lookup
  pubkey      TEXT NOT NULL,
  email       TEXT,
  created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE presence (
  user_id     INTEGER PRIMARY KEY REFERENCES users(id),
  ip          TEXT NOT NULL,
  port        INTEGER NOT NULL,
  last_seen   DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE tokens (
  id          INTEGER PRIMARY KEY,
  user_id     INTEGER REFERENCES users(id),
  token_hash  TEXT UNIQUE NOT NULL,  -- SHA-256 токена, не сам токен
  created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
  expires_at  DATETIME               -- NULL = бессрочный
);

CREATE TABLE invites (
  code        TEXT PRIMARY KEY,
  created_by  INTEGER REFERENCES users(id),
  used_by     INTEGER REFERENCES users(id),
  created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
  used_at     DATETIME
);
```

---

## Клиент — интеграция

**Настройки клиента (Desktop / Android):**
```
Discovery Server:  [ vasya@myserver.example        ]  ← полный адрес
                   [  myserver.example:47822        ]  ← только сервер
```

**При добавлении контакта:**
- Поле ввода принимает `vasya`, `vasya@myserver.example`, или `1.2.3.4:47821`
- Если указан сервер — делает GET `/lookup/vasya`, получает ip:port+pubkey
- Если нет сервера — старый режим (ручной UUID+IP)

**При запуске клиента:**
- Если настроен сервер → `PUT /update` с актуальным IP:port
- Каждые N минут → `POST /heartbeat`

---

## Режим группы (--mode group)

Сервер становится чат-комнатой. Пользователи подключаются по WebSocket и получают все сообщения группы в реальном времени.

### Концепция

```
Клиент подключается:  POST /group/join  {username, pubkey}  → {token, group_key_enc}
WebSocket:            ws://server/group/ws?token=<token>
Отправка:             {type:"msg", text:"...", ts:...}   (зашифровано group_key)
Сервер:               форвардит всем подключённым участникам
```

### Шифрование

Групповой ключ (AES-256) генерируется при первом запуске сервера и хранится в БД.
При входе нового участника сервер возвращает `group_key_enc` — групповой ключ, зашифрованный pubkey участника.
Сообщения в WebSocket-фреймах зашифрованы этим ключом — сервер видит только зашифрованные байты.

### Конфиг (только для --mode group)

```json
"group": {
  "name": "My Group",
  "description": "",
  "max_members": 0,
  "invite_only": false,
  "history": true,
  "history_limit": 1000,
  "allow_file_relay": true
}
```

- `min_client_version: "latest"` — сервер при старте и раз в 6 часов запрашивает `https://github.com/Xomel45/naleystogramm/releases/latest`, парсит тег (`v0.8.0` → `0.8.0`) и использует его как минимальную версию; если GitHub недоступен — использует последнее успешно полученное значение; конкретная версия (`"0.8.0"`) отключает автообновление
- `max_members: 0` — без лимита
- `invite_only: true` — вход только по инвайт-коду (те же `/invite` эндпоинты)
- `history: true` — новый участник получает последние N сообщений при подключении
- `history_limit` — сколько сообщений хранить в БД
- `allow_file_relay` — разрешить relay-передачу файлов внутри группы

### API (group-режим)

| Метод | Путь | Описание |
|---|---|---|
| `GET` | `/info` | Информация о группе (name, members_online, invite_only) |
| `POST` | `/group/join` | Войти в группу, получить токен + зашифрованный group_key |
| `DELETE` | `/group/leave` | Покинуть группу |
| `GET` | `/group/members` | Список участников (username, online, last_seen) |
| `GET` | `/group/ws` | WebSocket — реалтайм сообщения |
| `GET` | `/group/history` | Последние N сообщений (Bearer токен) |

### WebSocket фреймы

```json
// Отправка сообщения
{"type": "msg", "data": "<base64 зашифрованный текст>", "ts": 1234567890}

// Системное (от сервера): участник вошёл/вышел
{"type": "join", "username": "vasya"}
{"type": "leave", "username": "vasya"}

// История при подключении
{"type": "history", "messages": [...]}
```

### БД (дополнительные таблицы для group-режима)

```sql
CREATE TABLE group_members (
  id          INTEGER PRIMARY KEY,
  username    TEXT UNIQUE NOT NULL,
  pubkey      TEXT NOT NULL,
  token_hash  TEXT UNIQUE NOT NULL,
  joined_at   DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE group_messages (
  id          INTEGER PRIMARY KEY,
  sender      TEXT NOT NULL,
  data        TEXT NOT NULL,   -- зашифрованный контент (base64)
  ts          INTEGER NOT NULL
);

CREATE TABLE group_config (
  key         TEXT PRIMARY KEY,
  value       TEXT NOT NULL    -- group_key зашифрован pubkey сервера
);
```

### Клиент — интеграция (group-режим)

При подключении к серверу клиент получает `/info` → видит `"mode": "group"` → открывает GroupChatActivity/GroupChatWidget вместо обычного чата.
Адрес группы: `mygroup.example:47822` — выглядит как обычный сервер, режим определяется автоматически.

---

## Стек сервера

- **Язык**: Rust (один статический бинарь)
- **HTTP**: `axum` + `tokio`
- **Сериализация**: `serde` + `serde_json`
- **БД**: SQLite (по умолчанию) или PostgreSQL — через `sqlx` (async, compile-time query check)
- **Хеширование токенов**: `sha2`
- **Конфиг**: JSON, путь передаётся флагом `--config ./config.json`
- **Запуск**: `./naleys-server --config config.json`
- **Systemd unit**: прилагается в `deploy/`

---

## Безопасность

- Токены хранятся только как SHA-256 хеш
- Rate limiting: `/register` — 5 req/IP/час; `/lookup` — 60 req/IP/мин
- IP логируется только в `presence`, не в `users` — можно отключить хранение IP в конфиге
- Сервер не может читать сообщения — он не участвует в соединении
- Скомпрометированный сервер может вернуть чужой pubkey → клиент обязан верифицировать ключ через X3DH handshake (TOFU или fingerprint)
