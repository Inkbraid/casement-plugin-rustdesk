//! Key handling for the peer handshake.
//!
//! Adapted from RustDesk's `src/common.rs` (`get_rs_pk`, `decode_id_pk_dtls`,
//! `create_symmetric_key_msg`) at the commit in [`super::RUSTDESK_COMMIT`].
//! RustDesk is © Purslane Ltd. and contributors, AGPL-3.0.

use hbb_common::anyhow::anyhow;
use hbb_common::bytes::Bytes;
use hbb_common::protobuf::Message as _;
use hbb_common::rendezvous_proto::IdPk;
use hbb_common::sodiumoxide::crypto::{box_, secretbox, sign};
use hbb_common::ResultType;

fn pk32(pk: &[u8]) -> Option<[u8; 32]> {
    pk.try_into().ok()
}

/// The ID server's public signing key from its base64 form.
pub fn server_key(base64_key: &str) -> Option<sign::PublicKey> {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(base64_key.trim())
        .ok()?;
    pk32(&raw).map(sign::PublicKey)
}

/// What a signed `IdPk` says, once its signature checked out.
pub struct SignedIdPk {
    pub id: String,
    pub pk: [u8; 32],
    /// The newest key exchange version the signer speaks (0: the original).
    pub kx_version: u32,
}

/// Verifies `signed` against `key` and decodes the `IdPk` inside.
pub fn open_id_pk(signed: &[u8], key: &sign::PublicKey) -> ResultType<SignedIdPk> {
    let data = sign::verify(signed, key).map_err(|_| anyhow!("signature mismatch"))?;
    let res = IdPk::parse_from_bytes(&data)?;
    let pk = pk32(&res.pk).ok_or_else(|| anyhow!("wrong public key length"))?;
    Ok(SignedIdPk {
        id: res.id,
        pk,
        kx_version: res.kx_version,
    })
}

/// A fresh session key sealed to the peer's ephemeral key: our ephemeral
/// public key, the sealed key, and the key itself.
pub fn sealed_session_key(their_pk: [u8; 32]) -> (Bytes, Bytes, secretbox::Key) {
    let their_pk = box_::PublicKey(their_pk);
    let (our_pk, our_sk) = box_::gen_keypair();
    let key = secretbox::gen_key();
    let nonce = box_::Nonce([0u8; box_::NONCEBYTES]);
    let sealed = box_::seal(&key.0, &nonce, &their_pk, &our_sk);
    (Vec::from(our_pk.0).into(), sealed.into(), key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_id_pk_round_trip() {
        let (pk, sk) = sign::gen_keypair();
        let idpk = IdPk {
            id: "123456789".into(),
            pk: vec![7u8; 32].into(),
            kx_version: 1,
            ..Default::default()
        };
        let signed = sign::sign(&idpk.write_to_bytes().unwrap(), &sk);
        let open = open_id_pk(&signed, &pk).unwrap();
        assert_eq!(
            (open.id.as_str(), open.pk, open.kx_version),
            ("123456789", [7u8; 32], 1)
        );

        let (other, _) = sign::gen_keypair();
        assert!(open_id_pk(&signed, &other).is_err());
    }

    #[test]
    fn parses_server_keys() {
        assert!(server_key("3LtMWxYRybNj1qgmqNfugECWYfSskT4V31Vew23Ta8U=").is_some());
        assert!(server_key("abc").is_none());
    }
}
