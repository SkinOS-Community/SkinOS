//! Minimal wire messages from SteamDatabase/Protobufs, checked 2026-10-09.
//! Sources: steam/content_manifest.proto, steammessages_clientserver_2.proto,
//! steammessages_contentsystem.steamclient.proto. Unknown fields are ignored.
use prost::Message;

#[derive(Clone, PartialEq, Message)]
pub struct DepotKeyRequest {
    #[prost(uint32, optional, tag = "1")]
    pub depot_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub app_id: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
pub struct DepotKeyResponse {
    #[prost(int32, optional, tag = "1")]
    pub eresult: Option<i32>,
    #[prost(uint32, optional, tag = "2")]
    pub depot_id: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub depot_encryption_key: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ManifestCodeRequest {
    #[prost(uint32, optional, tag = "1")]
    pub app_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub depot_id: Option<u32>,
    #[prost(uint64, optional, tag = "3")]
    pub manifest_id: Option<u64>,
    #[prost(string, optional, tag = "4")]
    pub app_branch: Option<String>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ManifestCodeResponse {
    #[prost(uint64, optional, tag = "1")]
    pub manifest_request_code: Option<u64>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ServersRequest {
    #[prost(uint32, optional, tag = "1")]
    pub cell_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub max_servers: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ServersResponse {
    #[prost(message, repeated, tag = "1")]
    pub servers: Vec<Server>,
}
#[derive(Clone, PartialEq, Message)]
pub struct Server {
    #[prost(string, optional, tag = "1")]
    pub kind: Option<String>,
    #[prost(int32, optional, tag = "3")]
    pub cell_id: Option<i32>,
    #[prost(float, optional, tag = "5")]
    pub weighted_load: Option<f32>,
    #[prost(string, optional, tag = "8")]
    pub host: Option<String>,
    #[prost(string, optional, tag = "9")]
    pub vhost: Option<String>,
    #[prost(bool, optional, tag = "10")]
    pub use_as_proxy: Option<bool>,
    #[prost(string, optional, tag = "12")]
    pub https_support: Option<String>,
    #[prost(uint32, repeated, tag = "13")]
    pub allowed_app_ids: Vec<u32>,
}
#[derive(Clone, PartialEq, Message)]
pub struct CdnTokenRequest {
    #[prost(uint32, optional, tag = "1")]
    pub depot_id: Option<u32>,
    #[prost(string, optional, tag = "2")]
    pub host_name: Option<String>,
    #[prost(uint32, optional, tag = "3")]
    pub app_id: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
pub struct CdnTokenResponse {
    #[prost(string, optional, tag = "1")]
    pub token: Option<String>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ManifestPayload {
    #[prost(message, repeated, tag = "1")]
    pub mappings: Vec<FileMapping>,
}
#[derive(Clone, PartialEq, Message)]
pub struct FileMapping {
    #[prost(string, optional, tag = "1")]
    pub filename: Option<String>,
    #[prost(uint64, optional, tag = "2")]
    pub size: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub flags: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub sha_content: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "6")]
    pub chunks: Vec<Chunk>,
    #[prost(string, optional, tag = "7")]
    pub linktarget: Option<String>,
}
#[derive(Clone, PartialEq, Message)]
pub struct Chunk {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub sha: Option<Vec<u8>>,
    #[prost(fixed32, optional, tag = "2")]
    pub crc: Option<u32>,
    #[prost(uint64, optional, tag = "3")]
    pub offset: Option<u64>,
    #[prost(uint32, optional, tag = "4")]
    pub original_size: Option<u32>,
    #[prost(uint32, optional, tag = "5")]
    pub compressed_size: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
pub struct ManifestMetadata {
    #[prost(uint32, optional, tag = "1")]
    pub depot_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub manifest_id: Option<u64>,
    #[prost(bool, optional, tag = "4")]
    pub filenames_encrypted: Option<bool>,
    #[prost(uint64, optional, tag = "5")]
    pub original_size: Option<u64>,
}
