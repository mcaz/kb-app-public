//! `docs/contract.md` の変更を、コア強制点の見直しなしでは通さないためのテスト。

use sha2::{Digest, Sha256};

const CONTRACT_SHA256: &str = "2136692d0c2e43334ae4a8cf18a5162127bedcc430dafc808a290484bfdd768a";

#[test]
fn contract_document_matches_reviewed_core_enforcement() {
    let actual = format!(
        "{:x}",
        Sha256::digest(include_bytes!("../../../docs/contract.md"))
    );
    assert_eq!(
        actual, CONTRACT_SHA256,
        "docs/contract.md を変えたら、対応する kb-core の強制実装とこの fingerprint を同じコミットで更新する"
    );
}
