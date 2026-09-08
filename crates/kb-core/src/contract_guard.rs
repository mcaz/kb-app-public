//! `docs/contract.md` の変更を、コア強制点の見直しなしでは通さないためのテスト。

use sha2::{Digest, Sha256};

const CONTRACT_SHA256: &str = "e4d3187ea081e2d5c1012d7fedfa9d2083c354e985ba725d8f0b41dd0fb6b06a";

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
