//! GUI から `invoke` で呼べる関数。機能ごとに分けている。
//!
//! すべて kb-core の API を呼ぶだけで、統治のロジックはここに置かない
//! (NFR-M2 相当。コアはヘッドレスで完結し、GUI・CLI・MCP はその薄い口)。

pub mod connect;
pub mod favorites;
pub mod files;
pub mod home;
pub mod notes;
pub mod setup;
