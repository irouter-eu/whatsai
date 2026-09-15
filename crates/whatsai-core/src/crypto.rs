use crate::protocol::*;
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hpke::{Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
type Kem = hpke::kem::X25519HkdfSha256;
type Kdf = hpke::kdf::HkdfSha256;
type AeadHpke = hpke::aead::ChaCha20Poly1305;
const SIGN_DOMAIN: &[u8] = b"whatsai/signed/v1\0";
const HPKE_INFO: &[u8] = b"whatsai/key/v1";
#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub version: u32,
    pub signing: String,
    pub encryption: String,
    pub transport: String,
    pub name: String,
}
impl Identity {
    pub fn generate(name: &str) -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let (sk, _) = Kem::gen_keypair(&mut OsRng);
        let mut transport = [0u8; 32];
        OsRng.fill_bytes(&mut transport);
        Self {
            version: VERSION,
            signing: hex::encode(bytes),
            encryption: hex::encode(sk.to_bytes()),
            transport: hex::encode(transport),
            name: name.into(),
        }
    }
    fn signing_key(&self) -> Result<SigningKey> {
        Ok(SigningKey::from_bytes(
            &hex::decode(&self.signing)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid signing key"))?,
        ))
    }
    pub fn member(&self) -> Result<Member> {
        ensure!(self.version == VERSION, "unsupported identity version");
        let sk = <Kem as KemTrait>::PrivateKey::from_bytes(&hex::decode(&self.encryption)?)?;
        Ok(Member {
            id: hex::encode(self.signing_key()?.verifying_key().to_bytes()),
            encryption_key: hex::encode(Kem::sk_to_pk(&sk).to_bytes()),
            name: self.name.clone(),
        })
    }
    pub fn sign<T: Serialize>(&self, body: T) -> Result<Signed<T>> {
        let key = self.signing_key()?;
        let mut bytes = SIGN_DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(&body)?);
        Ok(Signed {
            body,
            signer: hex::encode(key.verifying_key().to_bytes()),
            signature: hex::encode(key.sign(&bytes).to_bytes()),
        })
    }
    pub fn seal(
        &self,
        header: Header,
        plaintext: &[u8],
        members: &[Member],
    ) -> Result<Signed<Sealed>> {
        ensure!(header.sender == self.member()?.id, "sender mismatch");
        ensure!(
            header.recipients == members.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "recipient mismatch"
        );
        let aad = serde_json::to_vec(&header)?;
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = ChaCha20Poly1305::new((&key).into())
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        let mut keys = BTreeMap::new();
        for m in members {
            let pk = <Kem as KemTrait>::PublicKey::from_bytes(&hex::decode(&m.encryption_key)?)?;
            let (enc, mut ctx) = hpke::setup_sender::<AeadHpke, Kdf, Kem, _>(
                &OpModeS::Base,
                &pk,
                HPKE_INFO,
                &mut OsRng,
            )?;
            keys.insert(
                m.id.clone(),
                WrappedKey {
                    encapsulated: B64.encode(enc.to_bytes()),
                    ciphertext: B64.encode(ctx.seal(&key, &aad)?),
                },
            );
        }
        self.sign(Sealed {
            header,
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
            keys,
        })
    }
    pub fn open(&self, sealed: &Signed<Sealed>) -> Result<Vec<u8>> {
        verify(sealed)?;
        let body = &sealed.body;
        ensure!(
            body.header.version == VERSION && body.header.sender == sealed.signer,
            "invalid envelope"
        );
        let own = self.member()?.id;
        ensure!(body.header.recipients.contains(&own), "not a recipient");
        let wrapped = body
            .keys
            .get(&own)
            .ok_or_else(|| anyhow::anyhow!("no recipient key"))?;
        let aad = serde_json::to_vec(&body.header)?;
        let sk = <Kem as KemTrait>::PrivateKey::from_bytes(&hex::decode(&self.encryption)?)?;
        let enc = <Kem as KemTrait>::EncappedKey::from_bytes(&B64.decode(&wrapped.encapsulated)?)?;
        let mut ctx =
            hpke::setup_receiver::<AeadHpke, Kdf, Kem>(&OpModeR::Base, &sk, &enc, HPKE_INFO)?;
        let key = ctx.open(&B64.decode(&wrapped.ciphertext)?, &aad)?;
        ensure!(key.len() == 32, "invalid content key");
        let nonce = B64.decode(&body.nonce)?;
        ensure!(nonce.len() == 12, "invalid nonce");
        ChaCha20Poly1305::new_from_slice(&key)
            .map_err(|_| anyhow::anyhow!("invalid key"))?
            .decrypt(
                nonce.as_slice().into(),
                Payload {
                    msg: &B64.decode(&body.ciphertext)?,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("content authentication failed"))
    }
}
pub fn verify<T: Serialize>(signed: &Signed<T>) -> Result<()> {
    let key = VerifyingKey::from_bytes(
        &hex::decode(&signed.signer)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid public key"))?,
    )?;
    let sig = Signature::from_slice(&hex::decode(&signed.signature)?)?;
    let mut data = SIGN_DOMAIN.to_vec();
    data.extend(serde_json::to_vec(&signed.body)?);
    key.verify_strict(&data, &sig)?;
    Ok(())
}
