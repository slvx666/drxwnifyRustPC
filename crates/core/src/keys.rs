//! Setting key names, exactly as in `android/.../constants/PreferenceKeys.kt` (look them up there by name).
//! Only the keys the core needs are listed; the rest pass through `Prefs` untouched.

pub mod spotify {
    pub const SP_DC: &str = "spotifySpDc";
    pub const SP_KEY: &str = "spotifySpKey";
    pub const ACCESS_TOKEN: &str = "spotifyAccessToken";
    pub const TOKEN_EXPIRY: &str = "spotifyTokenExpiry";
    pub const USERNAME: &str = "spotifyUsername";
    pub const USER_ID: &str = "spotifyUserId";
}

pub mod yandex {
    pub const ACCESS_TOKEN: &str = "yandexAccessToken";
    pub const UID: &str = "yandexUid";
    pub const USERNAME: &str = "yandexUsername";
}

pub mod vk {
    pub const ACCESS_TOKEN: &str = "vkAccessToken";
    pub const USER_ID: &str = "vkUserId";
}

pub mod youtube {
    pub const COOKIE: &str = "innerTubeCookie";
    pub const VISITOR_DATA: &str = "visitorData";
    pub const DATA_SYNC_ID: &str = "dataSyncId";
    pub const ACCOUNT_NAME: &str = "accountName";
    pub const ACCOUNT_EMAIL: &str = "accountEmail";
    pub const ACCOUNT_HANDLE: &str = "accountChannelHandle";
    /// Keys dropped when a backup is imported on the PC (decision D-012): Google rotates these cookies and one
    /// cookie used on two devices can sign both out.
    pub const SESSION_KEYS: [&str; 6] = [COOKIE, VISITOR_DATA, DATA_SYNC_ID, ACCOUNT_NAME, ACCOUNT_EMAIL, ACCOUNT_HANDLE];
}

/// SharedPreferences file names and keys.
pub mod sp {
    /// Friends profile; `sk` is the Nostr private key (hex).
    pub const FRIENDS: &str = "friends";
    pub const FRIENDS_SECRET_KEY: &str = "sk";
    /// Chat state; `cursor_<my pubkey>` is the time of the newest fetched message.
    pub const CHAT: &str = "chat";
    pub const CHAT_CURSOR_PREFIX: &str = "cursor_";
}
