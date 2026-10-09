//! Lancement détaché du superviseur, cross-platform.
//!
//! Le but : un processus qui SURVIT au terminal qui l'a lancé.
//! - Unix : `setsid()` avant l'exécution détache le processus du terminal
//!   de contrôle (il ne recevra plus le SIGHUP du logout) ;
//! - Windows : les flags de création `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
//!   + `CREATE_NO_WINDOW` donnent le même résultat, sans fenêtre console.
//!
//! Le superviseur écrit lui-même son état (`.config.d/daemon.json`) : l'appelant
//! n'a pas besoin de récupérer le handle du processus, il le jette et attend
//! l'apparition du fichier d'état (voir `ops::cmd_start`).

use std::io;
use std::process::{Command, Stdio};

/// Spawn le binaire courant en mode `supervise`, détaché du terminal.
/// stdout/stderr sont déviés vers null : le superviseur écrit TOUT dans le
/// fichier de log via le logger partagé. `lang` (déjà validée) est transmise
/// pour que les messages du superviseur suivent la langue du CLI.
pub fn spawn_supervisor(lang: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("supervise")
        .env("APABOT_LANG", lang)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `setsid` détache du terminal de contrôle : SIGHUP ne tuera plus le
        // superviseur quand le shell appelant se ferme. (Échec toléré : le
        // processus resterait simplement attaché, pas de panique.)
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    cmd.spawn()?;
    Ok(())
}
