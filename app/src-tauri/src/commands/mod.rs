//! GUI から `invoke` で呼べる関数。機能ごとに分けている。
//!
//! すべて kb-core の API を呼ぶだけで、統治のロジックはここに置かない
//! (NFR-M2 相当。コアはヘッドレスで完結し、GUI・CLI・MCP はその薄い口)。

pub mod background;
pub mod connect;
pub mod distillation;
pub mod favorites;
pub mod files;
pub mod home;
pub mod notes;
pub mod proposals;
pub mod settings;
pub mod setup;
pub mod workspace_tabs;
