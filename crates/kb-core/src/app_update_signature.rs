//! Tauri updater 2.11.0と同じ公開鍵・署名形式を、展開前の同一bytesに対して検証する。
//! 署名の一致だけで配布元の身元・archiveの安全性・旧新版の共存を受け入れない。

use base64::Engine;
use minisign_verify::{PublicKey, Signature};

pub const MAX_SIGNATURE_TEXT_BYTES: usize = 4096;
pub const MAX_PUBLIC_KEY_TEXT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterSignatureError {
    InputLimitExceeded,
    InvalidPublicKey,
    InvalidSignature,
    VerificationFailed,
}

impl std::fmt::Display for UpdaterSignatureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "更新署名検査に失敗しました: {self:?}")
    }
}

impl std::error::Error for UpdaterSignatureError {}

/// 公式pluginと同じSTANDARD base64→UTF-8→minisign decode→verify(..., true)。
/// archive構造は別途app_update_packageへ、同じbytesを所有権ごと渡して検査する。
pub fn verify_updater_signature(
    bytes: &[u8],
    signature_base64: &str,
    public_key_base64: &str,
) -> Result<(), UpdaterSignatureError> {
    if bytes.len() > crate::app_update_package::MAX_COMPRESSED_BYTES
        || signature_base64.len() > MAX_SIGNATURE_TEXT_BYTES
        || public_key_base64.len() > MAX_PUBLIC_KEY_TEXT_BYTES
    {
        return Err(UpdaterSignatureError::InputLimitExceeded);
    }
    let decode = |text: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    let public_key = decode(public_key_base64)
        .and_then(|text| PublicKey::decode(&text).ok())
        .ok_or(UpdaterSignatureError::InvalidPublicKey)?;
    let signature = decode(signature_base64)
        .and_then(|text| Signature::decode(&text).ok())
        .ok_or(UpdaterSignatureError::InvalidSignature)?;
    public_key
        .verify(bytes, &signature, true)
        .map_err(|_| UpdaterSignatureError::VerificationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 公開fixtureの出典: minisign-verify 0.2.5/src/lib.rs (verify_prehashed)。
    // https://github.com/jedisct1/rust-minisign-verify
    // Copyright (c) 2019-2025 Frank Denis. MIT license:
    // Permission is hereby granted, free of charge, to any person obtaining a copy
    // of this software and associated documentation files (the "Software"), to deal
    // in the Software without restriction, including without limitation the rights
    // to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    // copies of the Software, and to permit persons to whom the Software is
    // furnished to do so, subject to the following conditions:
    // The above copyright notice and this permission notice shall be included in
    // all copies or substantial portions of the Software.
    // THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    // IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    // FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    // AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    // LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    // OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
    const PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    fn encode(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    #[test]
    fn official_format_public_fixture_verifies_only_its_original_bytes() {
        assert_eq!(
            verify_updater_signature(b"test", &encode(SIGNATURE), &encode(PUBLIC_KEY)),
            Ok(())
        );
        assert_eq!(
            verify_updater_signature(b"Test", &encode(SIGNATURE), &encode(PUBLIC_KEY)),
            Err(UpdaterSignatureError::VerificationFailed)
        );
        let other_key = PUBLIC_KEY.replace("73Y7GFO3", "73Y7GFO4");
        assert!(
            verify_updater_signature(b"test", &encode(SIGNATURE), &encode(&other_key)).is_err()
        );
    }

    #[test]
    fn malformed_and_oversize_signature_material_is_rejected() {
        assert_eq!(
            verify_updater_signature(b"test", "?", &encode(PUBLIC_KEY)),
            Err(UpdaterSignatureError::InvalidSignature)
        );
        assert_eq!(
            verify_updater_signature(b"test", &encode(SIGNATURE), "?"),
            Err(UpdaterSignatureError::InvalidPublicKey)
        );
        assert_eq!(
            verify_updater_signature(
                b"test",
                &"A".repeat(MAX_SIGNATURE_TEXT_BYTES + 1),
                &encode(PUBLIC_KEY)
            ),
            Err(UpdaterSignatureError::InputLimitExceeded)
        );
        assert_eq!(
            verify_updater_signature(
                b"test",
                &encode(SIGNATURE),
                &"A".repeat(MAX_PUBLIC_KEY_TEXT_BYTES + 1)
            ),
            Err(UpdaterSignatureError::InputLimitExceeded)
        );
    }
}
