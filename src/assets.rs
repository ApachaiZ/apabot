//! GIFs de résultat servis en pièce jointe d'embed.
//!
//! Deux sources, dans l'ordre :
//! 1. le DISQUE (`assets/emojis/`) — permet de personnaliser les GIFs
//!    (repo, mode portable) ;
//! 2. les octets EMBARQUÉS (`include_bytes!`) — un binaire installé via
//!    `cargo install --git` n'a PAS de checkout source à côté de lui ; les
//!    GIFs voyagent avec lui et `apabot install` les matérialise dans
//!    la structure de données.
//!
//! Un fichier manquant partout donne une carte en texte seul — le même
//! repli que la version JS.

use poise::serenity_prelude as serenity;
use serenity::CreateAttachment;

use crate::paths;

/// GIF prêt à joindre : pièce jointe + URL `attachment://` pour l'embed.
pub struct Gif {
    pub attachment: Option<CreateAttachment>,
    pub image_url: Option<String>,
}

/// Octets embarqués d'un GIF (`loading` | `waiting` | `success` | `error`),
/// `None` pour tout autre nom. ~340 Ko au total — négligeable face au
/// binaire (~14 Mo).
pub fn embedded(kind: &str) -> Option<&'static [u8]> {
    match kind {
        "loading" => Some(include_bytes!("../assets/emojis/loading.gif")),
        "waiting" => Some(include_bytes!("../assets/emojis/waiting.gif")),
        "success" => Some(include_bytes!("../assets/emojis/success.gif")),
        "error" => Some(include_bytes!("../assets/emojis/error.gif")),
        _ => None,
    }
}

/// Charge le GIF depuis le disque, puis les octets embarqués en repli.
pub fn gif_image(kind: &str) -> Gif {
    let path = paths::gif_path(kind);
    let bytes = std::fs::read(&path)
        .ok()
        .or_else(|| embedded(kind).map(|b| b.to_vec()));
    match bytes {
        Some(bytes) => Gif {
            // `CreateAttachment::bytes` est synchrone (le `::path` de
            // serenity est async) : pas besoin d'async ici.
            attachment: Some(CreateAttachment::bytes(bytes, format!("{kind}.gif"))),
            image_url: Some(format!("attachment://{kind}.gif")),
        },
        None => Gif {
            attachment: None,
            image_url: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_gifs_are_real_gifs() {
        for kind in ["loading", "waiting", "success", "error"] {
            let bytes = embedded(kind).expect("embedded GIF missing");
            assert!(&bytes[..6] == b"GIF89a", "{kind} is not a GIF89a");
        }
    }

    #[test]
    fn unknown_kind_has_no_embedded_bytes() {
        assert!(embedded("nope").is_none());
    }
}
