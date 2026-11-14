//! Audio: decoding (AAC/M4A, MP3, FLAC, Opus/WebM, Ogg/Vorbis; HLS with AES-128 later), output, queue,
//! crossfade and gapless, loudness normalisation, equaliser, spectrum. Behaviour from Android: docs/specs/audio.md.

pub mod decode;
pub mod mixer;
pub mod oggopus;
pub mod resample;
pub mod source;
