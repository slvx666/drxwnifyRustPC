//! Audio sources and the resolver: finds a playable stream for a catalog track (YouTube, VK, SoundCloud, Bandcamp,
//! Audius, lossless mirrors, Soulseek) with the order and thresholds of the Android app.

pub mod matcher;
pub mod parallel;
pub mod provider;
pub mod providers;
