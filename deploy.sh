#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# deploy.sh — сборка и упаковка naleys-server для деплоя на VPS
# ══════════════════════════════════════════════════════════════════════════════
#
# Использование:
#   ./deploy.sh gnu        — бинарь для glibc-VPS → builds/releases/VERSION-gnu/
#   ./deploy.sh musl       — бинарь для Alpine    → builds/releases/VERSION-musl/
#   ./deploy.sh all        — оба варианта
#   ./deploy.sh deb        — .deb пакет (Debian / Ubuntu)
#   ./deploy.sh rpm        — .rpm пакет (AlmaLinux / Rocky / CentOS)
#   ./deploy.sh install    — установить на текущий хост (нужен root)
#
# Поддерживаемые VPS-ОС:
#   glibc (gnu):  Debian, Ubuntu, AlmaLinux, CentOS, Rocky Linux, Oracle Linux
#   musl:         Alpine Linux
#
# Опции:
#   --clean     Очистить целевую директорию перед сборкой
#
# Требования:
#   gnu:  rustup target add x86_64-unknown-linux-gnu  (обычно уже установлен)
#   musl: rustup target add x86_64-unknown-linux-musl
#         sudo pacman -S musl  /  sudo apt install musl-tools
#   deb:  dpkg-deb  (sudo pacman -S dpkg  /  sudo apt install dpkg-dev)
#   rpm:  rpmbuild  (sudo pacman -S rpm-tools  /  sudo dnf install rpm-build)
# ══════════════════════════════════════════════════════════════════════════════

set -euo pipefail

# ── Цвета и вывод ─────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

log()    { echo -e "${BLUE}[DEPLOY]${NC} $*"; }
ok()     { echo -e "${GREEN}[  OK  ]${NC} $*"; }
warn()   { echo -e "${YELLOW}[ WARN ]${NC} $*"; }
fail()   { echo -e "${RED}[ERROR ]${NC} $*" >&2; exit 1; }
header() { echo -e "\n${BOLD}${CYAN}══ $* ══${NC}"; }
rule()   { printf "${CYAN}%.0s─${NC}" {1..60}; echo; }

# ── Всегда из корня проекта ────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Добавляем cargo в PATH (rustup мог быть установлен в ~/.cargo/bin)
export PATH="$HOME/.cargo/bin:$PATH"

# ── Константы ─────────────────────────────────────────────────────────────────
APP_NAME="naleys-server"
BUILDS_DIR="builds"
TARGET_GNU="x86_64-unknown-linux-gnu"
TARGET_MUSL="x86_64-unknown-linux-musl"
BIN_GNU="target/${TARGET_GNU}/release/${APP_NAME}"
BIN_MUSL="target/${TARGET_MUSL}/release/${APP_NAME}"

# ── Версия из Cargo.toml ──────────────────────────────────────────────────────
get_version() {
    grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'
}
VERSION=$(get_version)

# ── Парсинг аргументов ────────────────────────────────────────────────────────
MODE="${1:-help}"
DO_CLEAN=false
for arg in "$@"; do [[ "$arg" == "--clean" ]] && DO_CLEAN=true; done

# ── Вспомогательные функции ───────────────────────────────────────────────────

file_size() { du -h "$1" 2>/dev/null | cut -f1 || echo "?"; }
git_hash()  { git rev-parse --short HEAD 2>/dev/null || echo "unknown"; }
nj()        { echo "$(( $(nproc) > 2 ? $(nproc) - 2 : 1 ))"; }

write_build_info() {
    printf "version=%s\nbuilt=%s\ncommit=%s\ntarget=%s\n" \
        "$VERSION" "$(date '+%Y-%m-%d %H:%M:%S')" "$(git_hash)" "${2:-gnu}" \
        > "$1/build-info.txt"
}

ensure_builds_tree() { mkdir -p "$BUILDS_DIR/releases"; }

safe_clean() {
    local abs_builds abs_target
    abs_builds="$(realpath -m "$BUILDS_DIR")"
    abs_target="$(realpath -m "$1")"
    [[ "$abs_target" != "$abs_builds/"* && "$abs_target" != "$abs_builds" ]] && \
        fail "safe_clean: '$1' вне $abs_builds — отказ!"
    log "Очистка $1 ..."; rm -rf "${1:?}"
}

# ── Проверка цели rustup ──────────────────────────────────────────────────────
check_target() {
    local target="$1"
    if ! rustup target list --installed 2>/dev/null | grep -q "^${target}$"; then
        warn "Цель ${target} не установлена."
        log  "Установка: rustup target add ${target}"
        rustup target add "$target"
    fi
}

# ── Проверка наличия musl-gcc ─────────────────────────────────────────────────
check_musl_linker() {
    if ! command -v musl-gcc &>/dev/null; then
        warn "musl-gcc не найден."
        log  "Arch:   sudo pacman -S musl"
        log  "Ubuntu: sudo apt install musl-tools"
        fail "Установите musl-gcc и повторите."
    fi
}

# ── Сборка: glibc (gnu) ───────────────────────────────────────────────────────
build_gnu() {
    header "Сборка → ${TARGET_GNU}"
    # Если хост уже x86_64-linux-gnu — собираем без явного --target
    local host_triple
    host_triple=$(rustc -vV 2>/dev/null | grep '^host:' | awk '{print $2}' || echo "")
    if [[ "$host_triple" == "$TARGET_GNU" ]]; then
        cargo build --release -j "$(nj)"
        mkdir -p "$(dirname "$BIN_GNU")"
        [[ ! -f "$BIN_GNU" ]] && cp "target/release/${APP_NAME}" "$BIN_GNU"
    else
        check_target "$TARGET_GNU"
        cargo build --target "$TARGET_GNU" --release -j "$(nj)"
    fi
    ok "  ${BIN_GNU}  ($(file_size "$BIN_GNU"))"
}

# ── Сборка: musl (Alpine) ─────────────────────────────────────────────────────
build_musl() {
    header "Сборка → ${TARGET_MUSL}"
    check_target "$TARGET_MUSL"
    check_musl_linker
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
        cargo build --target "$TARGET_MUSL" --release -j "$(nj)"
    ok "  ${BIN_MUSL}  ($(file_size "$BIN_MUSL"))"
}

# ── Пакет: tarball (.tar.gz) ──────────────────────────────────────────────────
# Содержимое: бинарь + config.json (пример) + systemd-юнит + README
make_tarball() {
    local bin="$1" suffix="$2" dest="$3"
    local staging="${BUILDS_DIR}/_staging_${suffix}"
    local tarball="${dest}/${APP_NAME}-${VERSION}-${suffix}.tar.gz"

    rm -rf "$staging"
    mkdir -p "$staging/${APP_NAME}-${VERSION}"

    cp "$bin"                          "$staging/${APP_NAME}-${VERSION}/${APP_NAME}"
    cp config.json                     "$staging/${APP_NAME}-${VERSION}/config.json.example"
    cp deploy/naleys-server.service    "$staging/${APP_NAME}-${VERSION}/${APP_NAME}.service"

    chmod +x "$staging/${APP_NAME}-${VERSION}/${APP_NAME}"

    cat > "$staging/${APP_NAME}-${VERSION}/INSTALL.md" << MDEOF
# naleys-server v${VERSION} — установка на VPS

## Быстрый старт

\`\`\`bash
# 1. Скопировать бинарь
cp ${APP_NAME} /usr/local/bin/
chmod +x /usr/local/bin/${APP_NAME}

# 2. Скопировать конфиг
cp config.json.example /etc/${APP_NAME}.json
# Отредактируйте /etc/${APP_NAME}.json по необходимости

# 3. Создать пользователя (опционально, но рекомендуется)
useradd -r -s /usr/sbin/nologin naleys
mkdir -p /opt/${APP_NAME}
chown naleys:naleys /opt/${APP_NAME}

# 4. Установить systemd-юнит
cp ${APP_NAME}.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now ${APP_NAME}
\`\`\`

## Режимы запуска

| Команда                           | Назначение                   |
|-----------------------------------|------------------------------|
| \`${APP_NAME} --mode discovery\`  | Сервер обнаружения (default) |
| \`${APP_NAME} --mode group\`      | Чат-комната                  |

## Порт

По умолчанию: **47822**
Открыть: \`ufw allow 47822/tcp\` или \`firewall-cmd --add-port=47822/tcp --permanent\`

## VPS-ОС

- Этот тариф (${suffix}):
$(if [[ "$suffix" == *musl* ]]; then
    echo "  **Alpine Linux** (musl libc) — статически слинкован"
else
    echo "  **Debian / Ubuntu / AlmaLinux / CentOS / Rocky Linux** (glibc)"
fi)
MDEOF

    log "TAR: ${tarball}..."
    tar -czf "$tarball" -C "$staging" "${APP_NAME}-${VERSION}"
    rm -rf "$staging"
    ok "  + $(basename "$tarball")  ($(file_size "$tarball"))"
}

# ── РЕЖИМ: gnu ────────────────────────────────────────────────────────────────
deploy_gnu() {
    header "Deploy GNU → ${BUILDS_DIR}/releases/${VERSION}-gnu/"
    ensure_builds_tree

    [[ ! -f "$BIN_GNU" ]] && build_gnu

    local dest="${BUILDS_DIR}/releases/${VERSION}-gnu"
    $DO_CLEAN && safe_clean "$dest"
    mkdir -p "$dest"

    cp "$BIN_GNU" "$dest/${APP_NAME}"
    chmod +x "$dest/${APP_NAME}"
    cp config.json "$dest/config.json.example"
    cp deploy/naleys-server.service "$dest/${APP_NAME}.service"
    write_build_info "$dest" "gnu"

    make_tarball "$BIN_GNU" "gnu" "$dest"

    rule
    ok "GNU-релиз готов!"
    echo "  Директория:  ${BOLD}${dest}/${NC}"
    echo "  Бинарь:      ${APP_NAME}  ($(file_size "$dest/${APP_NAME}"))"
    echo "  Архив:       ${APP_NAME}-${VERSION}-gnu.tar.gz  ($(file_size "$dest/${APP_NAME}-${VERSION}-gnu.tar.gz"))"
    echo "  Версия:      ${VERSION}  |  commit: $(git_hash)"
    echo ""
    echo -e "  ${CYAN}VPS-ОС:${NC} Debian · Ubuntu · AlmaLinux · CentOS · Rocky Linux"
    echo "  Скопировать на сервер:"
    echo "    scp $dest/${APP_NAME} root@server:/usr/local/bin/"
    echo "    scp $dest/config.json.example root@server:/etc/${APP_NAME}.json"
    echo "    scp $dest/${APP_NAME}.service root@server:/etc/systemd/system/"
    echo ""
}

# ── РЕЖИМ: musl (Alpine) ──────────────────────────────────────────────────────
deploy_musl() {
    header "Deploy MUSL → ${BUILDS_DIR}/releases/${VERSION}-musl/"
    ensure_builds_tree

    [[ ! -f "$BIN_MUSL" ]] && build_musl

    local dest="${BUILDS_DIR}/releases/${VERSION}-musl"
    $DO_CLEAN && safe_clean "$dest"
    mkdir -p "$dest"

    cp "$BIN_MUSL" "$dest/${APP_NAME}"
    chmod +x "$dest/${APP_NAME}"
    cp config.json "$dest/config.json.example"
    cp deploy/naleys-server.service "$dest/${APP_NAME}.service"
    write_build_info "$dest" "musl"

    make_tarball "$BIN_MUSL" "musl" "$dest"

    rule
    ok "MUSL-релиз готов!"
    echo "  Директория:  ${BOLD}${dest}/${NC}"
    echo "  Бинарь:      ${APP_NAME}  ($(file_size "$dest/${APP_NAME}"))"
    echo "  Архив:       ${APP_NAME}-${VERSION}-musl.tar.gz  ($(file_size "$dest/${APP_NAME}-${VERSION}-musl.tar.gz"))"
    echo "  Версия:      ${VERSION}  |  commit: $(git_hash)"
    echo ""
    echo -e "  ${CYAN}VPS-ОС:${NC} Alpine Linux"
    echo "  Статически слинкован — нет зависимостей от системных libc"
    echo "  Скопировать на Alpine-сервер:"
    echo "    scp $dest/${APP_NAME} root@server:/usr/local/bin/"
    echo ""
}

# ── РЕЖИМ: deb (Debian / Ubuntu) ─────────────────────────────────────────────
deploy_deb() {
    header "Deploy .deb → ${BUILDS_DIR}/releases/${VERSION}-gnu/"
    ensure_builds_tree

    command -v dpkg-deb &>/dev/null || \
        fail "dpkg-deb не найден.\n  Arch: sudo pacman -S dpkg\n  Ubuntu: sudo apt install dpkg-dev"

    [[ ! -f "$BIN_GNU" ]] && build_gnu

    local dest="${BUILDS_DIR}/releases/${VERSION}-gnu"
    local deb_name="${APP_NAME}_${VERSION}_amd64.deb"
    local staging="${BUILDS_DIR}/_deb_staging"

    mkdir -p "$dest"
    rm -rf "$staging"
    mkdir -p "$staging/DEBIAN"
    mkdir -p "$staging/usr/local/bin"
    mkdir -p "$staging/etc/systemd/system"
    mkdir -p "$staging/etc"

    local installed_kb
    installed_kb=$(du -sk "$BIN_GNU" | cut -f1)

    cat > "$staging/DEBIAN/control" << CTRL
Package: naleys-server
Version: ${VERSION}
Section: net
Priority: optional
Architecture: amd64
Installed-Size: ${installed_kb}
Depends: libc6
Maintainer: xomel45 <xom.xom.zip@gmail.com>
Homepage: https://github.com/Xomel45/naleystogramm
Description: Naleystogramm discovery, relay and group server
 P2P-сервер для нахождения контактов по имени (Discovery),
 relay-передачи файлов при NAT и групповых чатов (Group-режим).
CTRL

    cp "$BIN_GNU" "$staging/usr/local/bin/${APP_NAME}"
    chmod 755 "$staging/usr/local/bin/${APP_NAME}"

    cp config.json "$staging/etc/${APP_NAME}.json.example"

    # postinst: подсказки после установки
    cat > "$staging/DEBIAN/postinst" << 'POSTINST'
#!/bin/sh
set -e
echo ""
echo "naleys-server установлен."
echo "  Конфиг: /etc/naleys-server.json.example → скопируйте и отредактируйте"
echo "  Запуск: naleys-server --config /etc/naleys-server.json"
echo ""
POSTINST
    chmod 755 "$staging/DEBIAN/postinst"

    find "$staging/usr" -type f -exec chmod 644 {} \;
    chmod 755 "$staging/usr/local/bin/${APP_NAME}"
    find "$staging" -type d -exec chmod 755 {} \;

    dpkg-deb --build --root-owner-group "$staging" "$dest/$deb_name"
    rm -rf "$staging"

    rule
    ok ".deb готов!"
    echo "  Пакет:  ${BOLD}${dest}/${deb_name}${NC}  ($(file_size "$dest/$deb_name"))"
    echo "  Версия: ${VERSION}  |  commit: $(git_hash)"
    echo ""
    echo "  Установка на Debian/Ubuntu:"
    echo "    scp $dest/$deb_name root@server:~/"
    echo "    ssh root@server 'apt install ~/${deb_name}'"
    echo ""
}

# ── РЕЖИМ: rpm (AlmaLinux / Rocky / CentOS) ──────────────────────────────────
deploy_rpm() {
    header "Deploy .rpm → ${BUILDS_DIR}/releases/${VERSION}-gnu/"
    ensure_builds_tree

    command -v rpmbuild &>/dev/null || \
        fail "rpmbuild не найден.\n  Arch: sudo pacman -S rpm-tools\n  Fedora/RHEL: sudo dnf install rpm-build"

    [[ ! -f "$BIN_GNU" ]] && build_gnu

    local dest="${BUILDS_DIR}/releases/${VERSION}-gnu"
    local rpm_version="${VERSION//-/_}"
    local rpm_name="${APP_NAME}-${rpm_version}-1.x86_64.rpm"
    local rpm_topdir="${BUILDS_DIR}/_rpm_staging"
    local buildroot="${rpm_topdir}/BUILDROOT/${APP_NAME}-${rpm_version}-1.x86_64"

    mkdir -p "$dest"
    rm -rf "$rpm_topdir"
    mkdir -p "${rpm_topdir}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
    mkdir -p "$buildroot/usr/local/bin"
    mkdir -p "$buildroot/etc"

    cp "$BIN_GNU" "$buildroot/usr/local/bin/${APP_NAME}"
    chmod 755 "$buildroot/usr/local/bin/${APP_NAME}"
    cp config.json "$buildroot/etc/${APP_NAME}.json.example"

    local files_list
    files_list=$(find "$buildroot" \( -type f -o -type l \) \
        | sed "s|${buildroot}||" | sort)

    local changelog_date
    changelog_date=$(LC_ALL=C date '+%a %b %d %Y')

    cat > "${rpm_topdir}/SPECS/${APP_NAME}.spec" << SPEC
Name:           ${APP_NAME}
Version:        ${rpm_version}
Release:        1
Summary:        Naleystogramm discovery, relay and group server
License:        Proprietary
URL:            https://github.com/Xomel45/naleystogramm
BuildArch:      x86_64
%define __spec_install_pre %{nil}
%define _unpackaged_files_terminate_build 0

%description
P2P-сервер для нахождения контактов по имени (Discovery),
relay-передачи файлов при NAT и групповых чатов (Group-режим).

%build
%install

%files
${files_list}

%post
echo "naleys-server установлен."
echo "  Конфиг: /etc/${APP_NAME}.json.example → скопируйте и отредактируйте"

%changelog
* ${changelog_date} xomel45 <xom.xom.zip@gmail.com> - ${rpm_version}-1
- Release ${VERSION}
SPEC

    rpmbuild -bb \
        --nodeps \
        --define "_topdir $(realpath "$rpm_topdir")" \
        --buildroot "$(realpath "$buildroot")" \
        "${rpm_topdir}/SPECS/${APP_NAME}.spec"

    local created_rpm
    created_rpm=$(find "${rpm_topdir}/RPMS" -name "*.rpm" | head -1)
    [[ -z "$created_rpm" ]] && fail ".rpm не создан. Проверь вывод rpmbuild."

    cp "$created_rpm" "$dest/$rpm_name"
    rm -rf "$rpm_topdir"

    rule
    ok ".rpm готов!"
    echo "  Пакет:  ${BOLD}${dest}/${rpm_name}${NC}  ($(file_size "$dest/$rpm_name"))"
    echo "  Версия: ${VERSION}  |  commit: $(git_hash)"
    echo ""
    echo "  Установка на AlmaLinux / Rocky / CentOS:"
    echo "    scp $dest/$rpm_name root@server:~/"
    echo "    ssh root@server 'dnf install ~/${rpm_name}'"
    echo ""
}

# ── РЕЖИМ: install (деплой на текущий хост) ──────────────────────────────────
deploy_install() {
    header "Установка naleys-server на текущий хост"

    [[ "$EUID" -ne 0 ]] && fail "Требуется root. Запусти: sudo ./deploy.sh install"

    # Определяем какой бинарь использовать
    local bin=""
    if   [[ -f "$BIN_MUSL" ]]; then bin="$BIN_MUSL"
    elif [[ -f "$BIN_GNU"  ]]; then bin="$BIN_GNU"
    else
        warn "Готовый бинарь не найден, собираю gnu..."
        build_gnu
        bin="$BIN_GNU"
    fi

    log "Бинарь: $bin  ($(file_size "$bin"))"

    # Копируем бинарь
    cp "$bin" /usr/local/bin/${APP_NAME}
    chmod +x /usr/local/bin/${APP_NAME}
    ok "  /usr/local/bin/${APP_NAME}"

    # Конфиг (не перезаписываем существующий)
    if [[ ! -f /etc/${APP_NAME}.json ]]; then
        cp config.json /etc/${APP_NAME}.json
        ok "  /etc/${APP_NAME}.json  (новый)"
    else
        warn "  /etc/${APP_NAME}.json уже существует — не перезаписываю"
    fi

    # Директория данных
    mkdir -p /opt/${APP_NAME}
    if ! id naleys &>/dev/null; then
        useradd -r -s /usr/sbin/nologin -d /opt/${APP_NAME} naleys
        ok "  Пользователь naleys создан"
    fi
    chown naleys:naleys /opt/${APP_NAME}

    # Systemd
    cp deploy/naleys-server.service /etc/systemd/system/
    systemctl daemon-reload
    systemctl enable ${APP_NAME}

    rule
    ok "Установка завершена!"
    echo "  Бинарь:    /usr/local/bin/${APP_NAME}"
    echo "  Конфиг:    /etc/${APP_NAME}.json"
    echo "  Данные:    /opt/${APP_NAME}/"
    echo "  Сервис:    ${APP_NAME}.service  (enabled)"
    echo ""
    echo -e "  ${YELLOW}Отредактируйте конфиг перед запуском:${NC}"
    echo "    ${EDITOR:-nano} /etc/${APP_NAME}.json"
    echo ""
    echo "  Запустить:"
    echo "    systemctl start ${APP_NAME}"
    echo "    journalctl -fu ${APP_NAME}"
    echo ""
}

# ── Справка ───────────────────────────────────────────────────────────────────
show_help() {
    echo ""
    echo -e "${BOLD}${CYAN}deploy.sh${NC} — упаковщик naleys-server v${BOLD}${VERSION}${NC}"
    rule
    echo ""
    echo -e "  ${BOLD}Команды:${NC}"
    echo "    ./deploy.sh gnu       Собрать + архив для Debian/Ubuntu/Alma/CentOS/Rocky"
    echo "    ./deploy.sh musl      Собрать + архив для Alpine Linux"
    echo "    ./deploy.sh all       Оба варианта"
    echo "    ./deploy.sh deb       .deb пакет (Debian / Ubuntu)"
    echo "    ./deploy.sh rpm       .rpm пакет (AlmaLinux / Rocky / CentOS)"
    echo "    ./deploy.sh install   Установить на текущий VPS (нужен root)"
    echo ""
    echo -e "  ${BOLD}Опции:${NC}"
    echo "    --clean     Очистить целевую директорию перед сборкой"
    echo ""
    echo -e "  ${BOLD}Требования:${NC}"
    echo "    gnu:  rustup target add x86_64-unknown-linux-gnu  (обычно уже есть)"
    echo "    musl: rustup target add x86_64-unknown-linux-musl"
    echo "          Arch: sudo pacman -S musl"
    echo "          Ubuntu: sudo apt install musl-tools"
    echo "    deb:  dpkg-deb  (sudo pacman -S dpkg)"
    echo "    rpm:  rpmbuild  (sudo pacman -S rpm-tools)"
    echo ""
    echo -e "  ${BOLD}Структура вывода:${NC}"
    echo "    builds/releases/"
    echo "    ├── ${VERSION}-gnu/"
    echo "    │   ├── naleys-server                          ← для Debian/Ubuntu/Alma/Rocky"
    echo "    │   ├── naleys-server-${VERSION}-gnu.tar.gz"
    echo "    │   ├── naleys-server_${VERSION}_amd64.deb     ← после ./deploy.sh deb"
    echo "    │   ├── naleys-server-${VERSION}-1.x86_64.rpm ← после ./deploy.sh rpm"
    echo "    │   ├── config.json.example"
    echo "    │   ├── naleys-server.service"
    echo "    │   └── build-info.txt"
    echo "    └── ${VERSION}-musl/"
    echo "        ├── naleys-server                          ← для Alpine (статик)"
    echo "        ├── naleys-server-${VERSION}-musl.tar.gz"
    echo "        ├── config.json.example"
    echo "        └── build-info.txt"
    echo ""
    echo -e "  ${BOLD}Быстрый деплой на VPS:${NC}"
    echo "    ./deploy.sh gnu"
    echo "    scp builds/releases/${VERSION}-gnu/naleys-server-${VERSION}-gnu.tar.gz root@server:~/"
    echo "    ssh root@server 'tar -xzf naleys-server-${VERSION}-gnu.tar.gz && cd naleys-server-${VERSION} && bash install.sh'"
    echo ""
}

# ── Точка входа ───────────────────────────────────────────────────────────────
case "$MODE" in
    gnu|linux|glibc)
        deploy_gnu
        ;;
    musl|alpine)
        deploy_musl
        ;;
    all)
        if $DO_CLEAN; then
            ensure_builds_tree
            [[ -d "${BUILDS_DIR}/releases/${VERSION}-gnu"  ]] && safe_clean "${BUILDS_DIR}/releases/${VERSION}-gnu"
            [[ -d "${BUILDS_DIR}/releases/${VERSION}-musl" ]] && safe_clean "${BUILDS_DIR}/releases/${VERSION}-musl"
            DO_CLEAN=false
        fi
        deploy_gnu
        deploy_musl
        ;;
    deb|debian|ubuntu)
        deploy_deb
        ;;
    rpm|rh|rhel|alma|rocky|centos)
        deploy_rpm
        ;;
    install)
        deploy_install
        ;;
    help|--help|-h)
        show_help
        ;;
    *)
        show_help
        fail "Неизвестная команда: '$MODE'"
        ;;
esac
