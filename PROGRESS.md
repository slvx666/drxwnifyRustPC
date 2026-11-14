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
| 1 | Ядро: модели, SQLite (схема `song.db` 40), настройки, HTTP, шина событий | идёт: база v40, preferences_pb, shared_prefs XML готовы и проверены на настоящем бэкапе |
| 2 | Звук | — |
| 3 | Оболочка Mini (выбор Slint/egui по замерам) | — |
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

## Дальше (веха 1)
1. Определить границу ядра и интерфейса: общий стор состояния + команды, чтобы Full и Mini не дублировали логику.
2. Модели из `android/.../db/entities/`, схема SQLite совместимая с `song.db` v40 (импорт бэкапа).
3. Настройки с теми же именами ключей, что `constants/PreferenceKeys.kt`.
4. HTTP-клиент (reqwest + rustls), логирование.

## Известные проблемы
- Нет.
