//! Génère le pool des GIFs embarqués (`OUT_DIR/emojis_pool.rs`) en scannant
//! `assets/emojis/{loading,success,error}/` AU MOMENT DU BUILD.
//!
//! Pourquoi ? Un `include_bytes!(".../loading3.gif")` écrit en dur CASSE le
//! build dès que le fichier disparaît. Ici, un asset retiré entre deux
//! builds est simplement absent du pool généré — jamais une erreur de
//! compilation. Le binaire embarque exactement ce qui est présent dans les
//! dossiers au moment du build.

use std::env;
use std::fs;
use std::path::Path;

const KINDS: [&str; 3] = ["loading", "success", "error"];

fn main() {
    // Tout changement dans les assets déclenche une régénération (et donc
    // une recompilation du crate).
    println!("cargo:rerun-if-changed=assets/emojis");

    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let emojis = Path::new(&manifest).join("assets").join("emojis");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let out_path = Path::new(&out_dir).join("emojis_pool.rs");

    let mut out = String::from("// Généré par build.rs — ne pas éditer.\n&[\n");
    for kind in KINDS {
        let dir = emojis.join(kind);
        let mut files: Vec<String> = match fs::read_dir(&dir) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gif"))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect(),
            Err(_) => Vec::new(),
        };
        // Tri : pool déterministe à contenu égal.
        files.sort();
        for file in files {
            // `name` = nom SANS l'extension `.gif` (sert de clé partout).
            // `strip_suffix` : exactement UNE occurrence (`.gif` dans un nom
            // de base, ex. `foo.gif.gif`, reste intact au lieu de tronquer).
            let name = file.strip_suffix(".gif").unwrap_or(&file);
            let abs = dir.join(&file);
            // `\` → `/` (Windows) ; `"` échappé : le chemin vit dans un
            // littéral de chaîne normal, un nom exotique ne casse plus la
            // génération (les noms contrôlés par le repo restent simples).
            let abs = abs
                .to_string_lossy()
                .replace('\\', "/")
                .replace('"', "\\\"");
            out.push_str(&format!(
                "EmbeddedGif {{ kind: \"{kind}\", name: \"{name}\", bytes: include_bytes!(\"{abs}\") }},\n"
            ));
        }
    }
    out.push_str("]\n");
    fs::write(out_path, out).expect("écriture du pool emojis généré");
}
