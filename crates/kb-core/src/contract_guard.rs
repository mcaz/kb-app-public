//! `docs/contract.md` の変更を、コア強制点の見直しなしでは通さないためのテスト。

use sha2::{Digest, Sha256};

const CONTRACT_SHA256: &str = "f60b59b4da48f2511a34587d61324ee4307c71d497465b41676dc3805461bebe";

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
