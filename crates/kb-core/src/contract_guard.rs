//! `docs/contract.md` の変更を、コア強制点の見直しなしでは通さないためのテスト。

use sha2::{Digest, Sha256};

const CONTRACT_SHA256: &str = "f25732667b87c9521a4c39e0e62c637378e7b19d139da36d52090c550d07a0ac";

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
