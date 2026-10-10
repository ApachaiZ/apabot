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
use rand::seq::SliceRandom;
use serenity::CreateAttachment;

use crate::paths;

/// GIF prêt à joindre : pièce jointe + URL `attachment://` pour l'embed.
pub struct Gif {
    pub attachment: Option<CreateAttachment>,
    pub image_url: Option<String>,
}

/// Les GIFs de chargement disponibles : `loading` + variantes `loading1..6`.
/// Chaque état de chargement ou d'attente tire l'un d'eux AU HASARD, pour
/// que l'utilisateur ne voie pas toujours le même loader.
pub const LOADING_KINDS: [&str; 7] = [
    "loading", "loading1", "loading2", "loading3", "loading4", "loading5", "loading6",
];

/// Tire un GIF de chargement au hasard dans [`LOADING_KINDS`].
pub fn random_loading_kind() -> &'static str {
    LOADING_KINDS
        .choose(&mut rand::thread_rng())
        .copied()
        .unwrap_or("loading")
}

/// Octets embarqués d'un GIF (`loading` | `loading1..6` | `success` |
/// `error`), `None` pour tout autre nom. ~9 Mo au total — acceptable face
/// au binaire (~23 Mo).
pub fn embedded(kind: &str) -> Option<&'static [u8]> {
    match kind {
        "loading" => Some(include_bytes!("../assets/emojis/loading.gif")),
        "loading1" => Some(include_bytes!("../assets/emojis/loading1.gif")),
        "loading2" => Some(include_bytes!("../assets/emojis/loading2.gif")),
        "loading3" => Some(include_bytes!("../assets/emojis/loading3.gif")),
        "loading4" => Some(include_bytes!("../assets/emojis/loading4.gif")),
        "loading5" => Some(include_bytes!("../assets/emojis/loading5.gif")),
        "loading6" => Some(include_bytes!("../assets/emojis/loading6.gif")),
        "success" => Some(include_bytes!("../assets/emojis/success.gif")),
        "error" => Some(include_bytes!("../assets/emojis/error.gif")),
        _ => None,
    }
}

/// Charge le GIF depuis le disque, puis les octets embarqués en repli.
///
/// `loading` et `waiting` (les deux états de chargement/attente) tirent au
/// hasard l'un des [`LOADING_KINDS`] à chaque appel.
pub fn gif_image(kind: &str) -> Gif {
    let chosen = match kind {
        "loading" | "waiting" => random_loading_kind(),
        other => other,
    };
    let path = paths::gif_path(chosen);
    let bytes = std::fs::read(&path)
        .ok()
        .or_else(|| embedded(chosen).map(|b| b.to_vec()));
    match bytes {
        Some(bytes) => Gif {
            // `CreateAttachment::bytes` est synchrone (le `::path` de
            // serenity est async) : pas besoin d'async ici.
            attachment: Some(CreateAttachment::bytes(bytes, format!("{chosen}.gif"))),
            image_url: Some(format!("attachment://{chosen}.gif")),
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
        let kinds = LOADING_KINDS.iter().copied().chain(["success", "error"]);
        for kind in kinds {
            let bytes = embedded(kind).expect("embedded GIF missing");
            assert!(&bytes[..6] == b"GIF89a", "{kind} is not a GIF89a");
        }
    }

    #[test]
    fn unknown_kind_has_no_embedded_bytes() {
        assert!(embedded("nope").is_none());
        // `waiting` reste un état valide (`gif_image`), mais n'a plus
        // d'octets propres : il tire dans le pool des loaders.
        assert!(embedded("waiting").is_none());
    }

    #[test]
    fn random_loading_kind_stays_in_pool() {
        for _ in 0..64 {
            let kind = random_loading_kind();
            assert!(LOADING_KINDS.contains(&kind), "{kind} hors du pool");
        }
    }
}
