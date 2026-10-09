//! Installation utilisateur (`apabot install|reinstall|uninstall`).
//!
//! Sans root, sans gestionnaire de paquets : le binaire s'installe lui-même.
//! Trois modes de vie coexistent :
//!
//! 1. **Portable** (historique, conservé) : un binaire NON installé garde
//!    tous ses chemins relatifs à son répertoire courant — le repo de dev,
//!    une clé USB, n'importe où. `install` ne le touche jamais.
//! 2. **Installation par défaut** : les conventions de l'OS —
//!    - Unix (Linux/macOS) : `~/.local/bin/apabot` (binaire) et
//!      `~/.config/apabot/` (données : `.config.d/`, `logs/`, `assets/`,
//!      `install.json`) ;
//!    - Windows : `%LOCALAPPDATA%\Programs\apabot\apabot.exe` (binaire)
//!      et `%APPDATA%\apabot\` (données).
//! 3. **Destination personnalisée** : un répertoire D choisi au clavier —
//!    `D/bin/apabot(.exe)` + `D/` comme racine des données. Tout tient
//!    dans D : facile à déplacer ou supprimer.
//!
//! Comment un binaire installé retrouve-t-il ses données, alors que les
//! chemins du bot sont relatifs au RÉPERTOIRE COURANT ? Au démarrage, le
//! binaire cherche son registry (`install.json`) : au pointeur par défaut
//! de l'OS, puis à côté de lui (install personnalisée). S'il EST le
//! binaire inscrit, il `chdir` dans la racine des données avant tout
//! dispatch — bot, superviseur, enfant, TUI et commandes vivent alors dans
//! l'installation, quel que soit le CWD de l'appelant. Un binaire non
//! inscrit reste en mode portable.
//!
//! Chaque étape irréversible demande une CONFIRMATION ; `--dry-run` affiche
//! le plan sans rien écrire. `uninstall` demande séparément pour le binaire
//! (oui) et les DONNÉES (non par défaut : config, stats et logs y vivent).

use dialoguer::{Confirm, Input, Select};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::fsutil;
use crate::i18n::{fill, Catalog};

const BIN_NAME: &str = env!("CARGO_BIN_NAME"); // "apabot" — jamais désynchronisé du Cargo.toml
const REGISTRY_FILE: &str = "install.json";

// ── Emplacements conventionnels (par OS) ───────────────────────────────────

#[cfg(unix)]
fn data_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join(BIN_NAME))
}

#[cfg(windows)]
fn data_root() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(BIN_NAME))
}

#[cfg(not(any(unix, windows)))]
fn data_root() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn bin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"))
}

#[cfg(windows)]
fn bin_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join("Programs").join(BIN_NAME))
}

#[cfg(not(any(unix, windows)))]
fn bin_dir() -> Option<PathBuf> {
    None
}

fn bin_name() -> String {
    if cfg!(windows) {
        format!("{BIN_NAME}.exe")
    } else {
        BIN_NAME.to_string()
    }
}

/// Nom de binaire d'une instance NOMMÉE : `apabot-<name>` (+`.exe`).
fn bin_name_named(name: &str) -> String {
    if cfg!(windows) {
        format!("{BIN_NAME}-{name}.exe")
    } else {
        format!("{BIN_NAME}-{name}")
    }
}

/// Racine des INSTANCES NOMMÉES : `<racine par défaut>/instances/`.
fn instances_dir() -> Option<PathBuf> {
    data_root().map(|r| r.join("instances"))
}

/// Nom d'instance déduit du NOM DU BINAIRE : `apabot-apabot01` →
/// `apabot01`. C'est ce qui permet à chaque copie de se reconnaître et de
/// trouver SES données sans configuration externe.
pub fn instance_name_from_exe(exe: &Path) -> Option<String> {
    let stem = exe.file_stem()?.to_string_lossy().to_string();
    let name = stem.strip_prefix("apabot-")?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Nom d'instance GÉNÉRÉ : `apabot01`, `apabot02`… (premier libre).
pub fn generate_name() -> String {
    let mut used: Vec<u64> = Vec::new();
    if let Some(dir) = instances_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.len() == 8 {
                    if let Some(n) = name.strip_prefix("apabot") {
                        if let Ok(num) = n.parse::<u64>() {
                            used.push(num);
                        }
                    }
                }
            }
        }
    }
    let next = (1u64..=99).find(|n| !used.contains(n)).unwrap_or(99 + used.len() as u64);
    format!("apabot{next:02}")
}

/// Le binaire pour une racine de données donnée (défaut : conventions OS,
/// personnalisé : `<root>/bin/<nom>`). Une instance NOMMÉE suffixe le nom
/// du binaire (`apabot-<name>`) — c'est le nom qui porte l'identité.
fn bin_for_root(root: &Path, default: bool, name: Option<&str>) -> PathBuf {
    match name {
        Some(n) => {
            if default {
                bin_dir().map(|d| d.join(bin_name_named(n))).unwrap_or_default()
            } else {
                root.join("bin").join(bin_name_named(n))
            }
        }
        None => {
            if default {
                bin_dir().map(|d| d.join(bin_name())).unwrap_or_default()
            } else {
                root.join("bin").join(bin_name())
            }
        }
    }
}

// ── Registry ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRegistry {
    pub bin: PathBuf,
    pub root: PathBuf,
    pub version: String,
    pub installed_at: String,
}

/// Registry au pointeur par défaut de l'OS (installations par défaut ET
/// personnalisées : ces dernières y laissent un pointeur).
pub fn read_registry() -> Option<InstallRegistry> {
    let raw = std::fs::read_to_string(data_root()?.join(REGISTRY_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Compare deux chemins (casse insensible sur Windows, sensible sur Unix).
fn paths_equal(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canon(a), canon(b));
    #[cfg(windows)]
    {
        a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Registry de CE binaire : trois endroits possibles —
/// 1. le pointeur par défaut de l'OS (instance sans nom) ;
/// 2. `<racine>/instances/<nom>/install.json`, le nom étant DÉDUIT du nom
///    du binaire (`apabot-apabot01` → instance `apabot01`) ;
/// 3. le voisin du binaire (installation personnalisée `<root>/bin/<nom>`).
pub fn registry_for(exe: &Path) -> Option<InstallRegistry> {
    if let Some(reg) = read_registry() {
        if paths_equal(&reg.bin, exe) {
            return Some(reg);
        }
    }
    if let Some(name) = instance_name_from_exe(exe) {
        let path = instances_dir()?.join(&name).join(REGISTRY_FILE);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(reg) = serde_json::from_str::<InstallRegistry>(&raw) {
                if paths_equal(&reg.bin, exe) {
                    return Some(reg);
                }
            }
        }
    }
    let sibling = exe.parent()?.parent()?.join(REGISTRY_FILE);
    let raw = std::fs::read_to_string(sibling).ok()?;
    let reg: InstallRegistry = serde_json::from_str(&raw).ok()?;
    paths_equal(&reg.bin, exe).then_some(reg)
}

/// Registry pour les commandes `reinstall`/`uninstall` : l'instance par
/// défaut, ou l'instance nommée choisie par `--name`.
fn registry_for_action(name: Option<&String>) -> Option<InstallRegistry> {
    match name {
        Some(n) => {
            let path = instances_dir()?.join(n).join(REGISTRY_FILE);
            let raw = std::fs::read_to_string(path).ok()?;
            serde_json::from_str(&raw).ok()
        }
        None => read_registry(),
    }
}

/// Appelé au TOUT début de `main` : si ce binaire EST le binaire installé,
/// bascule le répertoire courant sur la racine des données — toute la
/// chaîne (bot, superviseur, enfant, TUI) partage alors l'installation,
/// quel que soit le CWD de l'appelant. Un binaire non inscrit (dev, copie
/// portable) garde le mode portable : chemins relatifs au CWD.
pub fn enter_installed_root() {
    let Some(reg) = registry_for(&std::env::current_exe().unwrap_or_default()) else {
        return;
    };
    if let Ok(cwd) = std::env::current_dir() {
        if paths_equal(&cwd, &reg.root) {
            return; // déjà sur place
        }
    }
    if let Err(e) = std::env::set_current_dir(&reg.root) {
        eprintln!("[install] Could not enter the install data directory: {e}");
    }
}

fn write_registry(root: &Path, bin: &Path, pointer: bool) -> Result<(), std::io::Error> {
    let registry = InstallRegistry {
        bin: bin.to_path_buf(),
        root: root.to_path_buf(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        installed_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string(&registry).map_err(std::io::Error::other)?;
    // Registry DANS la racine des données (ou de l'instance).
    fsutil::atomic_write(&root.join(REGISTRY_FILE), json.as_bytes())?;
    // Pointeur à l'emplacement par défaut de l'OS — uniquement pour
    // l'instance SANS NOM (les instances nommées se retrouvent par leur
    // nom de binaire, un pointeur unique ne pourrait pas toutes les
    // représenter).
    if pointer {
        if let Some(default_root) = data_root() {
            if !paths_equal(&default_root, root) {
                let _ = std::fs::create_dir_all(&default_root);
                let _ = fsutil::atomic_write(&default_root.join(REGISTRY_FILE), json.as_bytes());
            }
        }
    }
    Ok(())
}

// ── Commandes ──────────────────────────────────────────────────────────────

/// Point d'entrée de `apabot install|reinstall|uninstall` (async : les
/// arrêts de daemon passent par le canal de contrôle). `args[1]` porte la
/// sous-commande elle-même (`install`, `reinstall` ou `uninstall`).
pub async fn cmd_install(args: &[String], catalog: &'static Catalog) -> i32 {
    match args.get(1).map(String::as_str) {
        Some("install") => install(args, catalog).await,
        Some("reinstall") => reinstall(args, catalog).await,
        Some("uninstall") => uninstall(args, catalog).await,
        _ => {
            eprintln!("{}", catalog.ops.install_usage);
            2
        }
    }
}

/// Choix de destination : défaut OS ou répertoire personnalisé. Une
/// instance nommée vit dans `<racine>/instances/<name>` (défaut) ou
/// `<destination>/<name>` (personnalisé) : configurations, logs et état de
/// CHAQUE bot sont physiquement isolés.
/// Retourne `(root, bin)`.
fn choose_destination(name: Option<&str>, catalog: &Catalog) -> Option<(PathBuf, PathBuf)> {
    let items = vec![catalog.ops.install_scope_default, catalog.ops.install_scope_custom];
    let selection = Select::new()
        .with_prompt(catalog.ops.install_scope_prompt)
        .items(&items)
        .default(0)
        .interact()
        .ok()?;
    if selection == 0 {
        let base = data_root()?;
        let root = match name {
            Some(n) => base.join("instances").join(n),
            None => base,
        };
        let bin = bin_for_root(&root, true, name);
        Some((root, bin))
    } else {
        let input: String = Input::new()
            .with_prompt(catalog.ops.install_dest_prompt)
            .interact_text()
            .ok()?;
        let input = input.trim().to_string();
        if input.is_empty() || input == "/" {
            eprintln!("{}", catalog.ops.install_dest_invalid);
            return None;
        }
        // Expansion du `~` (le shell ne s'en charge pas ici).
        let input = if let Some(rest) = input.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(h) => PathBuf::from(h).join(rest),
                None => PathBuf::from(&input),
            }
        } else {
            PathBuf::from(&input)
        };
        let root = match name {
            Some(n) => input.join(n),
            None => input,
        };
        let bin = bin_for_root(&root, false, name);
        Some((root, bin))
    }
}

async fn install(args: &[String], catalog: &'static Catalog) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");

    // ── Instance : --name <nom> (saisi) | --name auto (généré) | défaut ──
    // Chaque instance vit dans SA racine de données : plusieurs bots aux
    // configs isolées (apabot01, apabot02…).
    let name_arg = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .or_else(|| args.iter().find_map(|a| a.strip_prefix("--name=").map(String::from)));
    let instance_name: Option<String> = match name_arg.as_deref() {
        None => None,
        Some("auto") => {
            let name = generate_name();
            println!("{}", fill(catalog.ops.install_name_generated, &[("name", &name)]));
            Some(name)
        }
        Some(n) => {
            if !crate::ops::config::valid_name(n) {
                eprintln!("{}", catalog.ops.install_name_invalid);
                return 1;
            }
            Some(n.to_string())
        }
    };

    // --dry-run : plan SANS aucune question.
    if dry_run {
        if let (Some(base), Some(dir)) = (data_root(), bin_dir()) {
            let (root, bin) = match instance_name.as_deref() {
                Some(n) => (
                    base.join("instances").join(n),
                    dir.join(bin_name_named(n)),
                ),
                None => (base, dir.join(bin_name())),
            };
            println!(
                "{}",
                fill(
                    catalog.ops.install_plan,
                    &[
                        ("bin", &bin.display().to_string()),
                        ("root", &root.display().to_string()),
                    ]
                )
            );
            return 0;
        }
        eprintln!("{}", fill(catalog.ops.install_unsupported, &[]));
        return 1;
    }

    // Déjà installé ? On vérifie l'instance CONCERNÉE (une instance nommée
    // n'est pas bloquée par l'existence de l'instance par défaut).
    if registry_for_action(instance_name.as_ref()).is_some() {
        println!("{}", catalog.ops.install_already);
        return 0;
    }

    let Some((root, bin)) = choose_destination(instance_name.as_deref(), catalog) else {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    };

    println!(
        "{}",
        fill(
            catalog.ops.install_plan,
            &[
                ("bin", &bin.display().to_string()),
                ("root", &root.display().to_string()),
            ]
        )
    );

    if !Confirm::new()
        .with_prompt(catalog.ops.install_confirm)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }

    if let Err(e) = install_files(&root, &bin, instance_name.is_none(), catalog) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }

    // ── Configuration AU MOMENT de l'installation ──
    // Soit un fichier passé en paramètre (`--env-file`), soit le
    // questionnaire interactif ; sinon un rappel explicite (« .env à
    // compléter pour le bon fonctionnement »).
    let env_target = root.join(".config.d").join(".env");
    let env_file_arg = args
        .iter()
        .position(|a| a == "--env-file")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix("--env-file=").map(String::from))
        });
    let mut configured = false;
    if let Some(src) = env_file_arg {
        let src_path = PathBuf::from(&src);
        if !src_path.is_file() {
            eprintln!(
                "{}",
                fill(catalog.ops.install_env_missing, &[("file", &src_path.display().to_string())])
            );
            return 1;
        }
        if !env_target.exists() {
            if let Err(e) = std::fs::copy(&src_path, &env_target) {
                eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
                return 1;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&env_target, std::fs::Permissions::from_mode(0o600));
            }
            configured = true;
            println!(
                "{}",
                fill(catalog.ops.install_env_file, &[("file", &src_path.display().to_string())])
            );
        } else {
            configured = true; // un .env existant n'est jamais écrasé
        }
    } else if Confirm::new()
        .with_prompt(catalog.ops.install_configure_prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        match crate::setup::ensure_env_at(&env_target, false, catalog) {
            Ok(()) => configured = true,
            Err(e) => {
                eprintln!("{e}");
                configured = false;
            }
        }
    }
    if !configured && !env_target.is_file() {
        println!("{}", catalog.ops.install_env_todo);
    }

    // Avertissement PATH (information seulement, jamais bloquant).
    if let Some(dir) = bin.parent() {
        let in_path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| paths_equal(&d, dir)))
            .unwrap_or(false);
        if !in_path {
            println!(
                "{}",
                fill(catalog.ops.install_path_warn, &[("dir", &dir.display().to_string())])
            );
        }
    }
    0
}

/// Le gros du travail : structure de données + binaire + registry.
/// `pointer` = écrire aussi le pointeur par défaut (instance SANS nom
/// uniquement).
fn install_files(root: &Path, bin: &Path, pointer: bool, catalog: &Catalog) -> Result<(), std::io::Error> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(root.join("logs"))?;
    std::fs::create_dir_all(root.join("assets").join("emojis"))?;
    let configd = root.join(".config.d");
    std::fs::create_dir_all(&configd)?;

    // GIFs (requis par /start /stop /status) : copiés depuis le checkout
    // source s'il existe, sinon matérialisés depuis les octets EMBARQUÉS
    // (installation via `cargo install --git`, sans checkout à côté).
    for kind in ["loading", "waiting", "success", "error"] {
        let target = root.join("assets").join("emojis").join(format!("{kind}.gif"));
        if target.exists() {
            continue;
        }
        let source = cwd.join("assets").join("emojis").join(format!("{kind}.gif"));
        if source.is_file() {
            let _ = std::fs::copy(&source, &target);
        } else if let Some(bytes) = crate::assets::embedded(kind) {
            let _ = std::fs::write(&target, bytes);
        }
    }

    // Config : .env.example toujours, .env existant copié (secrets inclus —
    // c'est SA configuration). Jamais d'écrasement.
    for name in [".env.example", ".env"] {
        let source = cwd.join(".config.d").join(name);
        let target = configd.join(name);
        if source.is_file() && !target.exists() {
            let _ = std::fs::copy(source, target);
        }
    }

    // Binaire : copie + bit exécutable (fs::copy reprend le mode source).
    if let Some(parent) = bin.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let exe = std::env::current_exe()?;
    std::fs::copy(&exe, bin)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755));
    }

    write_registry(root, bin, pointer)?;

    println!(
        "{}",
        fill(
            catalog.ops.install_ok,
            &[
                ("bin", &bin.display().to_string()),
                ("root", &root.display().to_string()),
            ]
        )
    );
    Ok(())
}

/// Arrêt du daemon installé s'il tourne (avec confirmation). `true` = on
/// peut continuer, `false` = annulé par l'utilisateur.
async fn stop_daemon_if_running(catalog: &Catalog) -> bool {
    let Some(st) = crate::ops::state::read() else {
        return true;
    };
    if !crate::ops::state::pid_alive(st.pid) {
        crate::ops::state::remove();
        return true;
    }
    if !Confirm::new()
        .with_prompt(catalog.ops.reinstall_stop_prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        return false;
    }
    let _ = crate::ops::protocol::request(&st, "stop").await;
    true
}

async fn reinstall(args: &[String], catalog: &'static Catalog) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let name = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .or_else(|| args.iter().find_map(|a| a.strip_prefix("--name=").map(String::from)));
    let Some(reg) = registry_for_action(name.as_ref()) else {
        eprintln!("{}", catalog.ops.uninstall_not_installed);
        return 1;
    };
    println!(
        "{}",
        fill(
            catalog.ops.install_plan,
            &[
                ("bin", &reg.bin.display().to_string()),
                ("root", &reg.root.display().to_string()),
            ]
        )
    );
    if dry_run {
        return 0;
    }

    if !stop_daemon_if_running(catalog).await {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }

    if !Confirm::new()
        .with_prompt(fill(catalog.ops.reinstall_confirm, &[("bin", &reg.bin.display().to_string())]))
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
            return 1;
        }
    };
    if let Err(e) = std::fs::copy(&exe, &reg.bin) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&reg.bin, std::fs::Permissions::from_mode(0o755));
    }
    // Registry rafraîchi (version, date) — les données ne sont JAMAIS
    // touchées. Pointeur par défaut réécrit uniquement pour l'instance
    // sans nom (même règle que l'installation).
    let pointer = reg.bin.file_stem().map(|s| s == bin_name().trim_end_matches(".exe")).unwrap_or(false);
    if let Err(e) = write_registry(&reg.root, &reg.bin, pointer) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }
    println!(
        "{}",
        fill(catalog.ops.reinstall_ok, &[("bin", &reg.bin.display().to_string())])
    );
    0
}

async fn uninstall(args: &[String], catalog: &'static Catalog) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let name = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .or_else(|| args.iter().find_map(|a| a.strip_prefix("--name=").map(String::from)));
    let Some(reg) = registry_for_action(name.as_ref()) else {
        eprintln!("{}", catalog.ops.uninstall_not_installed);
        return 1;
    };
    println!(
        "{}",
        fill(
            catalog.ops.install_plan,
            &[
                ("bin", &reg.bin.display().to_string()),
                ("root", &reg.root.display().to_string()),
            ]
        )
    );
    if dry_run {
        return 0;
    }

    // ── 1. Daemon en cours ? Arrêt demandé ──
    if !stop_daemon_if_running(catalog).await {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }

    // ── 2. Binaire + registres (confirmation) ──
    if !Confirm::new()
        .with_prompt(fill(catalog.ops.uninstall_confirm_bin, &[("bin", &reg.bin.display().to_string())]))
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    let mut failed = false;
    // Sous Windows, un binaire EN COURS d'exécution est verrouillé : l'échec
    // est signalé (l'utilisateur le supprime manuellement après coup).
    if let Err(e) = std::fs::remove_file(&reg.bin) {
        eprintln!("{}", fill(catalog.ops.uninstall_failed, &[("error", &e.to_string())]));
        failed = true;
    }
    if let Err(e) = std::fs::remove_file(reg.root.join(REGISTRY_FILE)) {
        eprintln!("{}", fill(catalog.ops.uninstall_failed, &[("error", &e.to_string())]));
        failed = true;
    }
    // Pointeur par défaut (instance sans nom uniquement).
    if name.is_none() {
        if let Some(default_root) = data_root() {
            if !paths_equal(&default_root, &reg.root) {
                let _ = std::fs::remove_file(default_root.join(REGISTRY_FILE));
            }
        }
    }

    // ── 3. Données ? NON par défaut (config, stats, logs y vivent) ──
    if Confirm::new()
        .with_prompt(fill(catalog.ops.uninstall_confirm_data, &[("root", &reg.root.display().to_string())]))
        .default(false)
        .interact()
        .unwrap_or(false)
    {
        if let Err(e) = std::fs::remove_dir_all(&reg.root) {
            eprintln!("{}", fill(catalog.ops.uninstall_failed, &[("error", &e.to_string())]));
            failed = true;
        } else {
            println!("{}", catalog.ops.uninstall_ok_full);
            return if failed { 1 } else { 0 };
        }
    }
    println!(
        "{}",
        fill(catalog.ops.uninstall_ok, &[("root", &reg.root.display().to_string())])
    );
    if failed {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_roundtrips_through_json() {
        let registry = InstallRegistry {
            bin: PathBuf::from("/home/x/.local/bin/apabot"),
            root: PathBuf::from("/home/x/.config/apabot"),
            version: "2.0.0".into(),
            installed_at: "2026-10-08T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&registry).unwrap();
        let back: InstallRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bin, registry.bin);
        assert_eq!(back.root, registry.root);
        assert_eq!(back.version, "2.0.0");
    }

    #[test]
    fn custom_bin_lives_inside_the_root() {
        let root = PathBuf::from("/opt/apabot-test");
        let bin = bin_for_root(&root, false, None);
        assert_eq!(bin, PathBuf::from("/opt/apabot-test/bin/apabot"));
    }

    #[test]
    fn named_instance_binary_carries_the_name() {
        let root = PathBuf::from("/home/x/.config/apabot");
        let bin = bin_for_root(&root, false, Some("apabot01"));
        assert_eq!(bin, PathBuf::from("/home/x/.config/apabot/bin/apabot-apabot01"));
    }

    #[test]
    fn instance_name_comes_from_the_binary_name() {
        assert_eq!(
            instance_name_from_exe(Path::new("/home/x/.local/bin/apabot-apabot01")),
            Some("apabot01".to_string())
        );
        #[cfg(windows)]
        assert_eq!(
            instance_name_from_exe(Path::new("C:\\bin\\apabot-apabot02.exe")),
            Some("apabot02".to_string())
        );
        assert_eq!(instance_name_from_exe(Path::new("/bin/apabot")), None);
    }

    #[test]
    fn generated_names_follow_the_pattern() {
        let name = generate_name();
        assert!(name.starts_with("apabot"));
        assert_eq!(name.len(), 8);
        assert!(name[6..].chars().all(|c| c.is_ascii_digit()));
    }

    #[cfg(unix)]
    #[test]
    fn default_root_ends_with_bot_name() {
        let root = data_root().expect("HOME should be set in tests");
        assert!(root.ends_with("apabot"));
    }
}
