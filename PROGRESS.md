# PROGRESS

Для следующей сессии: читать этот файл, затем `docs/DECISIONS.md` и `docs/PARITY.md`.

## Где что
- Задание: `C:\Users\burger\Desktop\clavdiy\Drownify\PC_RUST_PROMPT.md`.
- Решения владельца и план: `C:\Users\burger\Desktop\clavdiy\Drownify\android\concepts_brief_pc_rust.md`.
- Источники: `Drownify/spec/` (чек-лист, протоколы), `Drownify/android/docs/PORTING-2.3.0.md` (решения 2.3.0),
  код `Drownify/android/` (источник правды). `Drownify/pc/` — только компоновка и темы.
- **Не открывать** `Drownify/rust-rewrite/` и `Drownify/pc/rust-migration-plan/` — неудачная прошлая попытка. Проект с нуля.

## Сборка
    powershell -NoProfile -File scripts\build.ps1 cargo build --workspace
    powershell -NoProfile -File scripts\build.ps1 cargo build -p drxwnify-mini --profile release-mini
Скрипт сам ставит ограничения, пока запущен Apex или VM для iOS, и держит кэши cargo/npm на D:.
Окна приложений поверх игры не открывать: интерфейс проверять без окна, где можно.

## Вехи
| # | Веха | Статус |
|---|---|---|
| 0 | Подготовка: workspace, LICENSE, .gitignore, CI, PROGRESS/PARITY/DECISIONS/PERFORMANCE | сделано (CI проверить после пуша) |
| 1 | Ядро: модели, SQLite (схема `song.db` 40), настройки, HTTP, шина событий | сделано (без интерфейса) |
| 2 | Звук | дальше |
| 3 | Оболочка Mini (Slint, решено D-014) + пустая оболочка Full | — |
| 4 | Spotify, Яндекс, ВК как каталог | — |
| 5 | Поиск звука и источники | — |
| 6 | Экраны | — |
| 7 | Интеграция с ОС | — |
| 8 | Соцфункции (Nostr) | — |
| 9 | Full UI (Tauri) | — |
| 10 | Установщики, замеры, первый релиз (спросить владельца) | — |
| 11 | Linux (Arch) | — |
| 12 | Синхронизация телефона и ПК (нужны доработки Android/iOS) | — |

## Сделано в вехе 1
- `drx-core::db`: схема ровно как Room v40 (из `crates/core/schema/room-40.json`); тест сверяет со схемой настоящего
  бэкапа — совпадает. Проверка версии: Created / Current / Older / Newer / Foreign.
- `drx-core::prefs`: кодек `settings.preferences_pb` (DataStore) без зависимостей; настоящий файл читается и
  пересобирается без потерь.
- `drx-core::shared_prefs`: кодек Android XML; все 41 файла настоящего бэкапа читаются и пересобираются без потерь.
- Тесты с настоящим бэкапом: переменные `DRX_TEST_SONG_DB`, `DRX_TEST_PREFS_PB`, `DRX_TEST_SHARED_PREFS_DIR`
  (бэкап лежит в `D:\dev\private\`, распаковывать во временную папку; в CI эти тесты пропускаются).
- Эксперимент по Mini (`spikes/`): размеры есть в `docs/PERFORMANCE.md`; память и CPU ещё не замерены
  (нужно окно — замерять, когда Apex не запущен: `scripts\measure.ps1 <exe>`).

- `drx-core::secrets`: сессии и ключ друзей при импорте уходят из открытых файлов в `secrets.dat` (DPAPI) и
  возвращаются в бэкап ПК, чтобы телефон их восстановил. Снимок перед импортом тоже зашифрован.
- `drx-core::http`: один клиент (rustls + ring, корни ОС), темп по хостам, одно короткое ожидание по Retry-After.
- `drx-core::store`: поток-писатель базы с событиями «изменились таблицы» (вместо опроса).
- `drx-transfer::backup`: просмотр бэкапа, импорт (снимок, без cookie YouTube, сброс курсоров чатов), создание бэкапа.
- `drx-app`: фасад — запуск ядра (Mini 2 потока), команды ImportBackup/CreateBackup, снимок состояния, живые списки
  (лайки, медиатека, плейлист, история) по диапазонам. Тест на настоящем бэкапе: 2403 лайка.

## Дальше (веха 2 — звук)
1. Выписать из `MusicService.kt` кривые кроссфейда, нормализации, пропуска тишины (COVERAGE.md 4.1 — прочитан не весь).
2. `drx-audio`: источник байтов с дисковым кэшем; демуксеры MP4/Ogg/WebM (symphonia), MPEG-TS и fMP4→ADTS (порт с Kotlin);
   libopus; HLS AES-128.
3. Микшер с голосами (текущий, следующий, история), gapless, кроссфейд; WASAPI shared по событиям, отпускать устройство
   после паузы.
4. Нормализация, эквалайзер (biquad), темп/тон, баланс.

## Известные проблемы
- Нет.
