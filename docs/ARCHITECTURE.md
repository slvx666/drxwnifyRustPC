# ARCHITECTURE

Как устроено ядро и как к нему подключаются две редакции. Почему так — `docs/DECISIONS.md`.

## Слои

```
apps/mini (нативный UI)      apps/full (Tauri + Svelte)
        \                        /
         \   только через app   /
          +--------------------+
          |  crates/app        |  фасад: команды, запросы, ручки списков, события, профиль редакции
          +--------------------+
   catalog  resolver  audio  library  lyrics  social  together  integrations  transfer  jobs  platform
          \______________________ core ______________________/
                    (модели, база, настройки, HTTP, картинки, секреты, журнал)
```

Правила:
- Интерфейсы зависят **только** от `app` (и `platform` для окна/трея, если нужно). Ни SQL, ни HTTP, ни бизнес-логики в UI,
  ни в TypeScript, ни в обработчиках Mini.
- Доменные crates не знают об интерфейсе. Разница редакций передаётся в `app` как `EditionProfile`
  (например: `animated_lyrics`, `auto_spectrum`, `po_token: Webview | Helper | None`), а не `cfg(feature)` в ядре.
- `platform` зависит только от типов `core`.

## Crates

| Crate | Что внутри | Android-исходник |
|---|---|---|
| `core` | модели (Song, Album, Artist, Playlist, Event…), база SQLite со схемой Room v40 + таблицы `pc_*`, настройки (имена ключей как в `PreferenceKeys.kt`), один HTTP-клиент (лимиты по хостам, Retry-After, cookie по аккаунтам, прокси), служба картинок, хранилище секретов (DPAPI), журнал с вычищением секретов, шина изменений базы | `db/`, `constants/PreferenceKeys.kt` |
| `app` | фасад для UI | ViewModel-слой `viewmodels/` |
| `audio` | источник байтов с дисковым кэшем, демуксеры (MP4, Ogg, WebM, MPEG-TS, fMP4→ADTS), декодеры (symphonia + libopus), HLS AES-128, микшер голосов (текущий, следующий, история), эквалайзер, нормализация, кроссфейд/gapless, анализ спектра, вывод WASAPI | `playback/`, `eq/` |
| `catalog` | Spotify, Яндекс, ВК (как каталог), YouTube Music как каталог; основной аккаунт, перекрёстный поиск, режим лайков, рекомендации, главная | `catalog/`, `spotify/`, `yandex/`, `recommend*` |
| `resolver` | поиск звука для трека каталога: провайдеры (YouTube, ВК, SoundCloud, Bandcamp, Audius, lossless, Soulseek), порядок и пороги, совпадение треков, кэш выбора; `trait JsSolver`, `trait PoTokenProvider` | `resolver/`, `utils/FuzzyTrackMatch.kt`, `utils/potoken`, `utils/cipher`, `utils/sabr` |
| `library` | медиатека, история, статистика, итоги месяца, Wrapped-данные | `catalog/ServiceStats.kt`, `viewmodels/*Library*` |
| `lyrics` | 14 источников текстов, перевод ИИ, романизация | `lyrics/` |
| `social` | Nostr: ключи, NIP-01/04/19/42/78, пул релеев, друзья, чаты, стикеры, «слушать вместе» через Nostr | `friends/` |
| `together` | второй «Слушать вместе»: protobuf через WebSocket | `listentogether/` |
| `integrations` | Last.fm, Discord RPC, концерты, SponsorBlock, распознавание (Shazam) | `utils/`, `recognition/` |
| `transfer` | импорт бэкапа Android, бэкап ПК в формате Android, импорт плейлистов, экспорт скачанного, перенос из аккаунта | `viewmodels/BackupRestoreViewModel.kt`, `utils/DownloadExporter.kt`, `playback/AccountTransfer.kt` |
| `jobs` | очередь фоновых задач в SQLite: шаги, повторы, паузы между запросами, продолжение после перезапуска | `playback/RemotePlaylists.kt` и др. |
| `platform` | свой поток со скрытым окном: SMTC, медиаклавиши, горячие клавиши, трей, единственный экземпляр (именованный канал), `drxwnify://`, автозапуск, уведомления, сон/пробуждение | ПК-функции `PLATFORMS.md` 2.2 |

## Фасад `app`

- **Команды** (`enum Command`): `Play(TrackRef)`, `Pause`, `Seek(ms)`, `Like(TrackRef)`, `ImportBackup(path)`… Возвращают
  `Result<CommandOutcome, AppError>`. Типы для TypeScript генерируются автоматически.
- **Запросы** (`enum Query`): одноразовые данные (страница альбома, профиль друга).
- **Ручки списков**: `open_list(ListSpec) -> ListHandle`, `rows(handle, range)`, событие `ListChanged { handle, version,
  dirty: Vec<Range> }`. Подгрузка страниц, «сначала кэш, потом свежее», «всё загружено» — внутри ядра.
- **Снимки состояния** через `watch`: плеер, очередь, аккаунты, друзья, загрузки. UI получает целый снимок при изменении.
- **Часы воспроизведения**: `PlaybackClock { position_ms, at_instant, rate, playing }` — шлётся при изменении, UI
  досчитывает позицию сам. Никаких тиков каждые N мс через фасад.
- **Акторы**: по одному на область (плеер, писатель базы, resolver, каталоги, социальное, задачи). Внутри — tokio
  (в Mini 2 рабочих потока). Аудиовывод — свой поток реального времени без блокировок.
- **База**: одно соединение-писатель + `update_hook` → шина «изменилась таблица X» → ручки списков пересчитывают диапазоны.

## Процессы-помощники (тот же exe)
- `--login <service>` — окно входа на WebView2, возвращает cookie/токен через канал и завершается; папка данных —
  временная, удаляется.
- `--js-worker` — QuickJS для подписи и n-параметра YouTube; запускается лениво, результат кэшируется по хешу player.js.
- PoToken: Full — скрытое окно Tauri; Mini — помощник с WebView2 по требованию или без него.
