//! `docs/contract.md` の変更を、コア強制点の見直しなしでは通さないためのテスト。

use sha2::{Digest, Sha256};

const CONTRACT_SHA256: &str = "9f616daf2944855e87792e47b362207f6f197351a62f464b9549f252baddb3e7";

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
