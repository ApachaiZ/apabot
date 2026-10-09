//! Petites primitives de fichiers partagées.
//!
//! Le bot persiste plusieurs fichiers d'état ; ils doivent tous respecter
//! deux garanties (identiques à la version JS) :
//!
//! 1. **Écriture atomique** (`tmp` + `rename`) — un crash au milieu d'une
//!    écriture ne peut pas laisser un fichier tronqué : le fichier visible
//!    est toujours soit l'ancienne version complète, soit la nouvelle.
//!    C'est le rôle de [`atomic_write`].
//! 2. **Permissions restrictives** (0600) pour tout ce qui contient des
//!    données privées. C'est le rôle de [`secure_write`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Écrit `bytes` dans `file` de façon atomique (fichier temporaire puis rename).
/// Crée les répertoires parents au besoin.
pub fn atomic_write(file: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".tmp"); // même convention que le JS : `<fichier>.tmp`
    let tmp_path = PathBuf::from(tmp);
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, file)
}

/// Comme [`atomic_write`], puis positionne le mode 0600 (lecture/écriture
/// uniquement par le propriétaire) — utilisé pour les fichiers sensibles.
pub fn secure_write(file: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(file, bytes)?;
    #[cfg(unix)]
    {
        // `PermissionsExt::from_mode` n'existe que sur Unix : sur Windows on
        // garde les permissions par défaut (le bot vise Linux en production).
        let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
