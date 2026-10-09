//! État persisté du superviseur (`mission control`).
//!
//! Le fichier `.config.d/daemon.json` est le point de rencontre entre le
//! superviseur (qui l'ÉCRIT au démarrage, avec pid + port + jeton) et les
//! commandes clientes (`start`/`stop`/`restart`/`status`) qui le LISENT pour
//! joindre le canal de contrôle. Format JSON, mode 0600 (il contient le
//! jeton d'authentification du canal).

use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    /// PID du processus superviseur (pas du bot).
    pub pid: u32,
    /// Port du canal de contrôle (TCP sur 127.0.0.1 uniquement).
    pub port: u16,
    /// Jeton d'authentification du canal (aléatoire, régénéré à chaque start).
    pub token: String,
    /// ISO 8601 du démarrage du superviseur.
    pub started_at: String,
}

pub fn read() -> Option<DaemonState> {
    let raw = std::fs::read_to_string(paths::daemon_state_file()).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write(state: &DaemonState) -> Result<(), std::io::Error> {
    let json = serde_json::to_string(state).map_err(std::io::Error::other)?;
    fsutil::secure_write(&paths::daemon_state_file(), json.as_bytes())
}

/// Supprime le fichier d'état (fin normale du superviseur, ou nettoyage
/// d'un état obsolète). Best-effort : un échec ne bloque jamais.
pub fn remove() {
    let _ = std::fs::remove_file(paths::daemon_state_file());
}

/// Le PID vit-il encore ? Cross-platform : `kill(pid, 0)` sur Unix,
/// `tasklist` sur Windows (où il n'existe pas de « signal 0 »).
pub fn pid_alive(pid: u32) -> bool {
    // Garde-fou : pid 0 et les valeurs > i32::MAX ne sont pas des pids
    // valides — `kill(-1, 0)` signale TOUS les processus, retournerait
    // « vivant » à tort.
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    {
        // `kill(pid, 0)` n'envoie RIEN : il vérifie seulement l'existence
        // du processus et notre droit de le sonder. ESRCH = mort.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // `tasklist /FI "PID eq N" /NH` retourne une ligne « image.exe ... »
        // si le processus existe, rien sinon. Suffisant pour un détecteur
        // d'état obsolète (pas un mécanisme de sécurité).
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(&pid.to_string())),
            Err(_) => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn absurd_pid_is_dead() {
        assert!(!pid_alive(u32::MAX));
    }

    #[test]
    fn state_roundtrips_through_json() {
        let state = DaemonState {
            pid: 4242,
            port: 31337,
            token: "abc123".into(),
            started_at: "2026-10-08T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: DaemonState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 4242);
        assert_eq!(back.port, 31337);
        assert_eq!(back.token, "abc123");
    }
}
