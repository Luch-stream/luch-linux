<p align="center"><img src="assets/luch.svg" width="128" alt="Luch"></p>

# Луч — передатчик для Linux

[English version](README.md) · Приёмник: [luch-android](https://github.com/Toxblh/luch-android)

Луч транслирует рабочий стол Linux на Android TV или проектор с задержкой как у стриминга игр, а
не как у обычной трансляции экрана. Это маленькая, встроенная в систему версия идеи
Sunshine/Moonlight: служба находит приёмники в локальной сети, доступна по D-Bus и передаёт экран
и звук за несколько миллисекунд.

Замер: ноутбук на Intel Arrow Lake → проектор XGIMI MoGo 4 Laser по Wi‑Fi, весь экран, режим `fast`:

| захват + кодирование (передатчик) | сеть (в одну сторону) | декодирование (приёмник) | **итого, без дисплея** |
|---|---|---|---|
| ≈ 5.5 мс | ≈ 2 мс | ≈ 5.5 мс (HEVC) | **≈ 13 мс** |

## Что в репозитории

| Часть | Что делает |
|-------|------------|
| `luchd` | Пользовательская служба. Находит приёмники (`_luch._tcp` через Avahi), объявляет этот компьютер (`_luchsrc._tcp`), держит каналы управления, захватывает и передаёт, отдаёт D-Bus API `dev.luch.Luch1`. |
| `luch-kms` | Маленький помощник для режима с минимальной задержкой: читает буфер экрана прямо из KMS, переводит в NV12 шейдерами EGL, кодирует через VA-API. Единственная часть, которой нужно право (`CAP_SYS_ADMIN`). |
| `luchctl` | Командная строка и интерактивный экран в терминале. |
| `gnome-extension/` | Переключатель «Луч» в быстрых настройках GNOME Shell. |
| `docs/dbus.ru.md` | Описание D-Bus API. |

![luchctl: приёмники и задержка в реальном времени](docs/img/luchctl.png)

## Режимы трансляции

| Режим | Как | Задержка на передатчике |
|-------|-----|-------------------------|
| `fast` | весь основной монитор из scanout KMS, с курсором, EGL → NV12 → VA-API (HEVC или H.264 — что быстрее декодирует приёмник по его замеру) | ≈ 5–6 мс |
| `monitor` | портал трансляции экрана (PipeWire, DMA-BUF, без копирования) | ≈ 15–25 мс |
| `window` | одно окно через портал | как у `monitor` |
| `virtual` | расширение рабочего стола: GNOME создаёт монитор с разрешением приёмника справа | как у `monitor` |

Звук (Opus, кадры по 5 мс) во всех режимах берётся с основного устройства вывода. Любой браузер с
WebCodecs (подойдёт и браузер телевизора) — тоже приёмник: откройте адрес `WebUrl`, который раздаёт
luchd по HTTPS (см. `docs/dbus.ru.md`), примите локальный сертификат и нажмите «Смотреть» — удобно там,
куда не поставить Android-приёмник. Возможность довериться сертификату зависит от браузера ТВ.
В планах: режим
медиа (передать ссылку или файл и управлять воспроизведением, как у Chromecast), режим фото и
режим презентации с экраном докладчика.

## Как это устроено

```
scanout KMS ─▶ luch-kms ─(EGL: масштаб + RGB→NV12 прямо в VA surface)─▶ hevc/h264_vaapi ─┐
портал/PipeWire ─▶ vapostproc ─▶ DMA-BUF ─▶ h264_vaapi ─────────────────────────────────┤
монитор PulseAudio/PipeWire ─▶ Opus 5 мс ───────────────────────────────────────────────┤
                                                                                        ▼
                    luchd: UDP-шарды (заголовок 24 байта), отправка прямо из потока кодировщика
                    управление: TCP, JSON-строки (hello/welcome, start/ready, keyframe, stats)
```

Приёмы для низкой задержки взяты у Sunshine, Moonlight и ALVR: ни одной копии кадра,
`async_depth=1`, без B-кадров, один опорный кадр, бесконечный GOP и IDR по запросу, VBV на один
кадр, самый быстрый пресет кодировщика, пакеты уходят сразу, как кодировщик их отдал. Сокеты
привязаны к интерфейсу локальной сети, поэтому VPN с полным туннелем поток не перехватит.

## Требования

- Linux с аппаратным кодировщиком VA-API (проверено на Intel iHD; AMD radeonsi должен работать) и Mesa EGL.
- GNOME на Wayland для режимов через портал; `fast` работает с любым композитором, который выводит изображение через KMS.
- `avahi-daemon`, PipeWire (с сервером Pulse), GStreamer ≥ 1.24 с плагинами `va`, `pipewire`,
  `pulse` и `opus`, библиотеки FFmpeg ≥ 6.1.
- Rust ≥ 1.80 для сборки.

Зависимости для сборки:

```sh
# Ubuntu / Debian
sudo apt install build-essential pkg-config libavcodec-dev libavutil-dev libva-dev libdrm-dev \
  libegl-dev libgles-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config ffmpeg-free-devel libva-devel libdrm-devel mesa-libEGL-devel \
  mesa-libGLES-devel gstreamer1-devel gstreamer1-plugins-base-devel
# Arch
sudo pacman -S base-devel ffmpeg libva libdrm mesa gstreamer gst-plugins-base
# ALT Linux
sudo apt-get install libavcodec-devel libavutil-devel libva-devel libdrm-devel libglvnd-devel \
  gstreamer1.0-devel gst-plugins1.0-devel
```

Плагины для работы (названия пакетов отличаются): `gst-plugins-bad` (va), `gst-plugin-pipewire`,
`gst-plugins-good` (pulse), `gst-plugins-base` (opus).

## Установка

```sh
git clone https://github.com/Toxblh/luch-linux && cd luch-linux
./install.sh
```

`install.sh` собирает и ставит `luchd`, `luchctl` и `luch-kms` в `~/.cargo/bin`, выдаёт
`luch-kms` право `CAP_SYS_ADMIN` (спросит sudo), ставит и запускает пользовательскую службу
`luchd` и ставит расширение GNOME. Выйдите из сеанса и войдите снова, чтобы GNOME Shell подхватил
расширение.

Вручную:

```sh
cargo install --path luchd && cargo install --path luchctl && cargo install --path luch-kms
sudo setcap cap_sys_admin+ep ~/.cargo/bin/luch-kms      # повторять после каждой переустановки luch-kms
install -Dm644 contrib/luchd.service ~/.config/systemd/user/luchd.service
systemctl --user daemon-reload && systemctl --user enable --now luchd
cp -r gnome-extension/luch@toxblh.github.io ~/.local/share/gnome-shell/extensions/
gnome-extensions enable luch@toxblh.github.io           # на Wayland — после повторного входа
```

Зачем `CAP_SYS_ADMIN`: ядро отдаёт дескрипторы чужих буферов экрана только привилегированным
процессам (захвату KMS у Sunshine нужно то же самое). Право получает только крошечный бинарник
`luch-kms`, а `luchd` работает без привилегий. Без этого права используйте режимы через портал.

## Использование

Поставьте и откройте **Луч** на приёмнике (см. [luch-android](https://github.com/Toxblh/luch-android)).
Он сам появится в сети.

**GNOME:** быстрые настройки → **Луч**. Нажатие — весь экран в режиме `fast` на последний или
единственный приёмник; стрелка открывает список со всеми режимами. Во время трансляции на
переключателе видна оценка задержки. На телевизоре кнопка OK на пульте показывает оверлей со
статистикой.

**Терминал:**

```sh
luchctl                                   # интерактивно: список, f<N> fast, m<N> экран, w<N> окно, e<N> расширить, s<N> стоп
luchctl list
luchctl refresh                           # немедленная сверка; результаты приходят в TUI/watch
luchctl mirror                            # режим fast; имя приёмника можно не указывать, если он один
luchctl mirror "XGIMI MoGo 4 Laser" window # или monitor | window | virtual
luchctl stats                             # задержка, fps, битрейт в реальном времени
luchctl stop
```

**Свой интерфейс:** всё доступно на сессионной шине, см. [docs/dbus.ru.md](docs/dbus.ru.md).

## Если что-то не так

- `luchctl list` пуст: приложение на приёмнике должно быть запущено, `avahi-daemon` активен, оба
  устройства в одном сегменте сети (mDNS не проходит через маршрутизаторы).
- `fast` падает с «needs CAP_SYS_ADMIN»: повторите `setcap` (переустановка его сбрасывает).
- Логи: `journalctl --user -u luchd -f`. `LUCH_PROBE=1` в окружении службы выводит задержку по этапам.

## Лицензия

GPL-3.0-or-later. Приёмы для низкой задержки — по мотивам [Sunshine](https://github.com/LizardByte/Sunshine),
[Moonlight](https://github.com/moonlight-stream) и [ALVR](https://github.com/alvr-org/ALVR).
