//! Chiffrement au repos des données privées persistées.
//!
//! - algorithme : AES-256-GCM ;
//! - clé : SHA-256 de `"apabot-v1:" + DISCORD_TOKEN` (aucun secret
//!   supplémentaire à gérer) ;
//! - format sur disque : base64 de `iv(12) ‖ tag(16) ‖ ciphertext`.
//!
//! ⚠️ Corollaire : une ROTATION du token Discord rend les fichiers
//! illisibles — supprimez alors `users.json`, `sessions.json` et
//! `members.roster.json`, le bot les recréera.
//!
//! Migration silencieuse : un fichier EN CLAIR hérité d'une ancienne version
//! reste lisible et sera réécrit chiffré à la première sauvegarde. De même,
//! l'ancien sel de dérivation (`apach-v1:`) reste accepté EN LECTURE : les
//! fichiers existants restent lisibles, réécrits au nouveau sel à la
//! première sauvegarde.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::fsutil;
use crate::logger;

/// Sel de dérivation ACTUEL.
const KEY_SALT: &str = "apabot-v1:";
/// Sel HÉRITÉ (projet d'origine) : accepté en LECTURE SEULE pour ne pas
/// casser les fichiers chiffrés existants.
const LEGACY_KEY_SALT: &str = "apach-v1:";

/// Clé de 32 octets dérivée du token Discord (SHA-256 du préfixe + token).
fn derive_key(salt: &str, token: &str) -> [u8; 32] {
    Sha256::digest(format!("{salt}{token}").as_bytes()).into()
}

fn key(token: &str) -> [u8; 32] {
    derive_key(KEY_SALT, token)
}

fn legacy_key(token: &str) -> [u8; 32] {
    derive_key(LEGACY_KEY_SALT, token)
}

/// Chiffre un objet JSON vers le blob base64 au format JS.
pub fn encrypt_json(token: &str, obj: &Value) -> String {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key(token)));
    // IV de 12 octets, issu de l'aléa système (OsRng = /dev/urandom).
    let mut iv = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let plaintext = serde_json::to_string(obj).unwrap_or_else(|_| "{}".to_string());
    // `encrypt` renvoie `ciphertext ‖ tag` (le tag GCM est accolé à la fin) :
    // on le découpe pour respecter le format JS `iv ‖ tag ‖ ciphertext`.
    let ct_and_tag = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .expect("chiffrement AES-GCM");
    let (ct, tag) = ct_and_tag.split_at(ct_and_tag.len() - 16);
    let mut out = Vec::with_capacity(12 + 16 + ct.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(tag);
    out.extend_from_slice(ct);
    BASE64.encode(out)
}

/// Déchiffre un blob base64 vers l'objet JSON — avec le sel actuel, puis
/// l'ancien sel en repli (migration silencieuse).
/// Erreur si le contenu est altéré (tag GCM) ou chiffré avec une autre clé.
pub fn decrypt_json(token: &str, blob: &str) -> Result<Value, String> {
    let buf = BASE64.decode(blob.trim()).map_err(|e| e.to_string())?;
    if buf.len() < 28 {
        return Err("ciphertext trop court".to_string());
    }
    // Reconstitution `ciphertext ‖ tag` attendu par la crate aes-gcm.
    let mut ct_and_tag = buf[28..].to_vec();
    ct_and_tag.extend_from_slice(&buf[12..28]);

    // Sel ACTUEL.
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key(token)));
    if let Ok(plaintext) = cipher.decrypt(Nonce::from_slice(&buf[..12]), ct_and_tag.as_ref()) {
        return serde_json::from_slice(&plaintext).map_err(|e| e.to_string());
    }
    // Sel HÉRITÉ (fichiers de l'ancienne mouture) : lecture seule.
    let legacy = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&legacy_key(token)));
    let plaintext = legacy
        .decrypt(Nonce::from_slice(&buf[..12]), ct_and_tag.as_ref())
        .map_err(|_| "clé incorrecte ou contenu altéré".to_string())?;
    serde_json::from_slice(&plaintext).map_err(|e| e.to_string())
}

/// Lecture tolérante d'un fichier d'état : chiffré → clair (hérité) →
/// `None`. Aucune erreur n'est propagée : un fichier illisible ne doit pas
/// empêcher le bot de démarrer (il sera recréé).
pub fn read_json(token: &str, file: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(file).ok()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    if let Ok(v) = decrypt_json(token, &raw) {
        return Some(v);
    }
    match serde_json::from_str(&raw) {
        Ok(v) => Some(v),
        Err(e) => {
            logger::warn(format!(
                "crypto: {} illisible (chiffré avec une autre clé ou corrompu) : {e}",
                file.display()
            ));
            None
        }
    }
}

/// Écriture chiffrée, atomique (tmp + rename), mode 0600.
pub fn write_json(token: &str, file: &Path, obj: &Value) -> std::io::Result<()> {
    fsutil::secure_write(file, encrypt_json(token, obj).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let obj = json!({"a": [1, 2, 3], "b": "héllo"});
        let blob = encrypt_json("token-test", &obj);
        assert_eq!(decrypt_json("token-test", &blob).unwrap(), obj);
    }

    #[test]
    fn wrong_key_fails() {
        let blob = encrypt_json("token-a", &json!({"x": 1}));
        assert!(decrypt_json("token-b", &blob).is_err());
    }

    #[test]
    fn tampered_blob_fails() {
        let blob = encrypt_json("token-a", &json!({"x": 1}));
        // Corrompt le dernier octet (le tag GCM est en tête du blob ici…).
        let mut bytes = BASE64.decode(blob).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xFF;
        let tampered = BASE64.encode(bytes);
        assert!(decrypt_json("token-a", &tampered).is_err());
    }

    /// INTEROPÉRABILITÉ HÉRITÉE : ce blob a été produit par l'ancienne
    /// mouture (AES-256-GCM, clé = SHA-256 de "apach-v1:"+token, format
    /// base64(iv ‖ tag ‖ ciphertext)). Il doit rester déchiffrable — le sel
    /// hérité est accepté en lecture.
    #[test]
    fn decrypts_legacy_blob() {
        // Produit par l'ancien code (token "test-interop-token",
        // données {"users":["111","222"]}).
        let blob = "Doz4/7AZdgn5y1PHVGSt6BBiOwrIOy0GHDugqx+PO0c3Km7pGsPgz1+nqcZNrJZDOglb";
        let decoded = decrypt_json("test-interop-token", blob).expect("déchiffrement");
        assert_eq!(decoded, json!({"users": ["111", "222"]}));
    }

    #[test]
    fn format_is_base64_iv_tag_ct() {
        // Le blob doit être base64(iv 12 ‖ tag 16 ‖ ct), donc contenir au
        // moins 12 + 16 + 1 octets décodés, et se déchiffrer avec le même
        // token.
        let blob = encrypt_json("k", &json!({"s": "secret"}));
        let bytes = BASE64.decode(&blob).unwrap();
        assert!(bytes.len() > 28);
        assert_eq!(decrypt_json("k", &blob).unwrap(), json!({"s": "secret"}));
    }
}
