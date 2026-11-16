//! Listen together over the protobuf WebSocket protocol (android listentogether/): rooms on a public server,
//! a host whose playback the guests follow.

pub mod client;
pub mod proto;

pub use client::{Client, DEFAULT_SERVER, Event, LINK_PREFIX, Role, Room, Suggestion, normalize_code};
pub use proto::{PlaybackAction, RoomState, SyncState, TrackInfo, UserInfo};
