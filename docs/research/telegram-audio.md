# Какие файлы Telegram показывает как музыку

Проверено по исходникам клиентов (2026-11-14). Решает **отправитель**: клиент читает файл и, если распознал песню,
прикладывает атрибут `documentAttributeAudio` (длительность, название, исполнитель) и миниатюру-обложку. Получатели на
любых устройствах видят то, что приложил отправитель.

## Telegram Desktop — `Telegram/SourceFiles/storage/localimageloader.cpp`, `FileLoadTask::CheckForSong`
- Подходит MIME `audio/mp3`, `audio/m4a`, `audio/aac`, `audio/ogg`, `audio/flac`, `audio/opus`
  **или** расширение `.mp3 .m4a .aac .ogg .flac .opus .oga`.
- Затем FFmpeg должен прочитать длительность (`duration >= 0`), иначе файл уходит документом.
- Название, исполнитель и обложка берутся из тегов (обложка отбрасывается, если размеры недопустимы).

## Telegram для Android — `SendMessagesHelper.java` (~стр. 9500)
- `.mp3`, `.m4a` — свой разборщик `AudioInfo` (ID3 / MP4 `ilst`: ©nam, ©ART, covr).
- `.opus`, `.ogg`, `.flac` — `MediaMetadataRetriever` (длительность, TITLE, ARTIST, встроенная картинка).
- **Ловушка:** `.ogg` с кодеком Opus отправляется как **голосовое** (`isOpusFile`). `.opus` — как музыка.
- Длительность 0 → документ.

## Вывод для экспорта Drxwnify
| Исходник | Как экспортировать | Расширение | Почему |
|---|---|---|---|
| MP3 | копия + ID3v2 (TIT2, TPE1, TALB, APIC JPEG) | `.mp3` | музыка везде |
| AAC/M4A | копия в MP4 + `ilst` (©nam, ©ART, ©alb, covr), moov в начале | `.m4a` | музыка везде |
| FLAC | копия + Vorbis comment + PICTURE | `.flac` | музыка везде |
| Opus (WebM YouTube) | **перепаковка без перекодирования** в Ogg, OpusTags + `METADATA_BLOCK_PICTURE` | `.opus` | музыка в обоих клиентах; не `.ogg`, иначе Android отправит голосовым |
| Vorbis | перепаковка в Ogg + Vorbis comment | `.ogg` | Vorbis в `.ogg` — музыка |
| по желанию | AAC через Media Foundation | `.m4a` | для устройств Apple, где Opus не играет |

Итог: перекодирование для Telegram не нужно — хватает перепаковки и правильных тегов. Кодировщик AAC (Media Foundation)
остаётся только как необязательный режим «совместимый m4a». Проверить вживую: отправить по файлу каждого вида из
Telegram Desktop и Telegram для Android в «Избранное».
