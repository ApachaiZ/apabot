//! GIFs de résultat servis en pièce jointe d'embed.
//!
//! Chaque ÉTAT a son dossier dans `assets/emojis/` :
//! - `loading/` : pool des chargements/attentes (l'état `waiting` pioche
//!   dans le même dossier) ;
//! - `success/` : carte de succès ;
//! - `error/` : carte d'erreur.
//!
//! À chaque affichage, UN GIF est tiré au hasard dans le bon dossier.
//! Deux sources, dans l'ordre :
//! 1. le DISQUE (`assets/emojis/<état>/`) — permet de personnaliser le
//!    pool (ajouter/retirer des GIFs sans recompiler) ;
//! 2. les octets EMBARQUÉS — le pool généré au BUILD par `build.rs`
//!    (voir [`EMBEDDED_GIFS`]) : un binaire installé via
//!    `cargo install --git` n'a PAS de checkout source à côté de lui ; un
//!    asset supprimé entre deux builds est simplement absent du pool —
//!    jamais une erreur de compilation.
//!
//! Un dossier vide partout donne une carte en texte seul — le même repli
//! que la version JS.

use poise::serenity_prelude as serenity;
use rand::seq::SliceRandom;
use serenity::CreateAttachment;
use std::path::PathBuf;

use crate::paths;

/// GIF prêt à joindre : pièce jointe + URL `attachment://` pour l'embed.
pub struct Gif {
    pub attachment: Option<CreateAttachment>,
    pub image_url: Option<String>,
}

/// Un GIF embarqué : `kind` = état (`loading` | `success` | `error`),
/// `name` = nom de fichier SANS extension, `bytes` = octets compilés.
pub struct EmbeddedGif {
    pub kind: &'static str,
    pub name: &'static str,
    pub bytes: &'static [u8],
}

/// Pool généré par `build.rs` depuis le contenu RÉEL de
/// `assets/emojis/{loading,success,error}/` au moment du build.
pub static EMBEDDED_GIFS: &[EmbeddedGif] = include!(concat!(env!("OUT_DIR"), "/emojis_pool.rs"));

/// Dossier du pool d'un état : `waiting` partage le pool `loading`.
fn pool_of(kind: &str) -> &str {
    match kind {
        "loading" | "waiting" => "loading",
        other => other,
    }
}

/// Tire au hasard un GIF EMBARQUÉ du pool `kind` (`name` sans extension +
/// octets). `None` si le pool est vide.
fn random_embedded_gif(kind: &str) -> Option<(&'static str, &'static [u8])> {
    EMBEDDED_GIFS
        .iter()
        .filter(|g| g.kind == kind)
        .collect::<Vec<_>>()
        .choose(&mut rand::thread_rng())
        .map(|g| (g.name, g.bytes))
}

/// Tire au hasard un GIF du pool SUR DISQUE (`nom de fichier` + octets).
/// `None` si le dossier n'existe pas ou ne contient aucun `.gif`.
fn random_disk_gif(kind: &str) -> Option<(String, Vec<u8>)> {
    let dir = paths::gif_dir(kind);
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("gif"))
        .collect();
    let file = files.choose(&mut rand::thread_rng())?;
    let name = file.file_stem()?.to_string_lossy().into_owned();
    let bytes = std::fs::read(&file).ok()?;
    Some((name, bytes))
}

/// Charge un GIF de l'état `kind` : disque (pool du dossier) puis pool
/// embarqué en repli. `loading` et `waiting` tirent au hasard dans le même
/// pool `loading`.
pub fn gif_image(kind: &str) -> Gif {
    let pool = pool_of(kind);
    let (name, bytes) = match random_disk_gif(pool) {
        Some((name, bytes)) => (name, bytes),
        None => match random_embedded_gif(pool) {
            Some((name, bytes)) => (name.to_string(), bytes.to_vec()),
            None => {
                return Gif {
                    attachment: None,
                    image_url: None,
                };
            }
        },
    };
    Gif {
        // `CreateAttachment::bytes` est synchrone (le `::path` de
        // serenity est async) : pas besoin d'async ici.
        attachment: Some(CreateAttachment::bytes(bytes, format!("{name}.gif"))),
        image_url: Some(format!("attachment://{name}.gif")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_gifs_are_real_gifs() {
        // Pool généré par build.rs : chaque GIF embarqué doit être valide.
        // (Vide si aucun asset n'est présent — le build reste vert.)
        for gif in EMBEDDED_GIFS {
            assert!(
                &gif.bytes[..6] == b"GIF89a",
                "{} n'est pas un GIF89a",
                gif.name
            );
        }
    }

    #[test]
    fn embedded_pool_members_are_well_formed() {
        for gif in EMBEDDED_GIFS {
            assert!(
                matches!(gif.kind, "loading" | "success" | "error"),
                "état inconnu : {}",
                gif.kind
            );
            assert!(!gif.name.is_empty(), "nom vide pour {}", gif.kind);
            assert!(
                !gif.name.ends_with(".gif"),
                "nom avec extension : {}",
                gif.name
            );
        }
    }

    #[test]
    fn random_embedded_gif_stays_in_its_pool() {
        for kind in ["loading", "success", "error"] {
            for _ in 0..16 {
                if let Some((name, bytes)) = random_embedded_gif(kind) {
                    assert!(
                        EMBEDDED_GIFS.iter().any(|g| g.kind == kind
                            && g.name == name
                            && g.bytes.len() == bytes.len())
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_kind_has_no_pool() {
        assert_eq!(pool_of("nope"), "nope");
        assert!(random_embedded_gif("nope").is_none());
        // `waiting` partage le pool `loading` : plus de GIF dédié.
        assert_eq!(pool_of("waiting"), "loading");
    }

    #[test]
    fn waiting_resolves_to_the_loading_pool() {
        // Dans le repo, `assets/emojis/loading/` existe : l'URL d'attachement
        // doit pointer un GIF du pool loading (disque ou embarqué).
        let gif = gif_image("waiting");
        if let Some(url) = gif.image_url {
            assert!(
                url.contains("loading"),
                "attente hors du pool loading : {url}"
            );
        }
    }
}
