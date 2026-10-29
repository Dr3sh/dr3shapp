//! Theseus utility functions
pub(crate) mod content_hash;
pub mod fetch;
pub mod io;
pub mod jre;
pub mod network;
pub mod platform;
pub mod protocol_version;
pub mod rpc;
pub mod server_ping;

/// Generates standard Minecraft offline player UUID:
/// UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(StandardCharsets.UTF_8))
pub fn offline_player_uuid(username: &str) -> uuid::Uuid {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{}", username).as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // Version 3
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // Variant RFC4122
    uuid::Uuid::from_bytes(bytes)
}

