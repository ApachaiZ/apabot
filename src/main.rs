//! Point d'entrée du bot (portage de `apabot.js`).
//!
//! Le binaire porte AUSSI le mission control (voir `ops` et `tui`) : le
//! premier argument est une sous-commande éventuelle
//! (`start`/`stop`/`restart`/`status`/`logs`/`mission-control`/`systemd`),
//! sinon le bot démarre au premier plan (comportement historique).
//!
//! Ordre de démarrage du bot (identique au JS) :
//! 1. `ensure_env` : complète `.config.d/.env` de façon interactive si besoin
//!    (ou refuse de démarrer avec la liste des variables manquantes en
//!    mode `--non-interactive` / sans TTY) ;
//! 2. langue résolue (`--lang=xx` > `BOT_LANGUAGE` > `en`) ;
//! 3. configuration validée → provider construit → état partagé ;
//! 4. enregistrement des slash commands (hash + purge, voir `deploy.rs`) ;
//! 5. connexion gateway + démarrage du watchdog au premier Ready.
//!
//! Codes de sortie : 78 = erreur de CONFIGURATION (le superviseur embarqué
//! et systemd ne redémarrent pas en boucle), 1 = tout le reste.
//! Arrêt propre : SIGINT/SIGTERM, ou message `shutdown` du superviseur.

mod api;
mod assets;
mod commands;
mod config;
mod confirm;
mod crypto;
mod deploy;
mod embeds;
mod errors;
mod fsutil;
mod i18n;
mod logger;
mod logs_cmd;
mod manage;
mod members;
mod ops;
mod paths;
mod power;
mod providers;
mod services;
mod setup;
mod state;
mod stats;
mod tui;
mod watchdog;

use poise::serenity_prelude as serenity;
use serenity::GatewayIntents;
use std::sync::Arc;

use crate::commands::Data;
use crate::errors::ExitError;

/// Parcours la chaîne de causes d'une erreur pour retrouver un [`ExitError`]
/// (le code de sortie 78 est transporté à travers les wrappers du framework).
/// Le `'static` est requis par `downcast_ref` : nos erreurs de framework
/// sont des `Box<dyn Error + Send + Sync>`, donc `'static`.
fn find_exit_code(err: &(dyn std::error::Error + 'static)) -> Option<i32> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(exit) = e.downcast_ref::<ExitError>() {
            return Some(exit.code);
        }
        current = e.source();
    }
    None
}

/// Arrêt propre sur Ctrl-C (SIGINT), SIGTERM, ou demande du superviseur —
/// équivalent des handlers `process.on("SIGINT"/"SIGTERM")` du JS, plus
/// l'arrêt cross-platform du mode daemon (pas de signal sous Windows).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installation du handler SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ── Optimisation du runtime : 2 worker threads suffisent ──
// Le bot est I/O-bound (gateway Discord, HTTP providers, canal de contrôle
// du superviseur, TUI). Chaque worker tokio = une pile de thread + l'état
// de l'exécuteur ; en garder `nproc` (le défaut) gaspille de la mémoire
// pour zéro débit en plus.
#[tokio::main(worker_threads = 2)]
async fn main() {
    // Panique : on log AVANT le hook par défaut (équivalent du
    // `uncaughtException` du JS — en Rust, un panic est contenu dans sa
    // tâche, mais autant le rendre visible dans le fichier de log).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logger::error(format!("panic: {info}"));
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();
    let non_interactive = args.iter().any(|a| a == "--non-interactive");

    // ── 0a. Mode installé ──
    // Si ce binaire EST le binaire installé (`install`), toutes les
    // opérations (bot, superviseur, logs, TUI) basculent sur la racine des
    // données de l'installation — le mode portable (binaire non inscrit,
    // chemins relatifs au répertoire courant) reste le comportement par
    // défaut et n'est pas affecté.
    ops::install::enter_installed_root();

    // ── 0b. Sous-commandes mission control (`start`, `status`, `logs`…) ──
    // Le premier argument détermine le mode. Un argument commençant par `-`
    // est un flag du bot (`-h`/`--help` affiche l'usage) , `run` est la
    // forme explicite du premier plan, un mot inconnu affiche l'usage.
    if let Some(sub) = args.get(1) {
        if !sub.starts_with('-') || sub == "-h" || sub == "--help" {
            let sub = if sub.starts_with('-') { "help" } else { sub.as_str() };
            if !ops::SUBCOMMANDS.contains(&sub) {
                let catalog = i18n::catalog(i18n::resolve_initial_lang(&args));
                eprintln!("{}", catalog.ops.usage);
                std::process::exit(2);
            }
            if let Some(code) = ops::dispatch(sub, &args).await {
                std::process::exit(code);
            }
            // `run` retombe ici : le bot démarre au premier plan.
        }
    }

    // ── 1. Environnement complet (setup interactif si TTY) ──
    let initial_catalog = i18n::catalog(i18n::resolve_initial_lang(&args));
    if let Err(e) = setup::ensure_env(non_interactive, initial_catalog) {
        eprintln!("{}", e.msg);
        std::process::exit(e.code);
    }

    // ── 2. Rechargement du .env (éventuellement écrit par le setup) ──
    let _ = dotenvy::from_path(paths::env_file());
    let language = i18n::resolve_initial_lang(&args);
    let catalog = i18n::catalog(language);

    logger::init();

    // ── 3. Configuration + provider ──
    let (cfg, provider) = match config::load(catalog, language) {
        Ok(v) => v,
        Err(e) => {
            logger::error(e.msg.clone());
            eprintln!("{}", e.msg);
            std::process::exit(e.code);
        }
    };

    // ── 4. État partagé ──
    let state = match state::State::new(&cfg.token, &cfg.owner_id, catalog) {
        Ok(s) => s,
        Err(e) => {
            logger::error(format!("Startup failed: {e}"));
            std::process::exit(1);
        }
    };

    let token = cfg.token.clone();
    let client_id = cfg.client_id.clone();
    // `api` et `state` sont PARTAGÉS avec les tâches détachées (watchdog) :
    // ils vivent dans des `Arc`. `cfg` est immuable, `catalog` est `Copy`.
    let data = Data {
        cfg,
        api: Arc::new(api::Api::new(provider)),
        state: Arc::new(state),
        catalog,
    };

    // ── 5. Enregistrement des slash commands (AVANT la connexion) ──
    // Un client REST Discord autonome suffit : pas besoin de gateway.
    // `set_application_id` est requis : les routes de commandes globales
    // (`create_global_commands`) en dérivent l'URL.
    let http = serenity::Http::new(&token);
    http.set_application_id(serenity::ApplicationId::new(
        client_id.parse().unwrap_or_default(),
    ));
    if let Err(e) = deploy::deploy_commands_if_needed(&http, &data.cfg, catalog, &commands::commands()).await {
        let code = find_exit_code(&*e).unwrap_or(1);
        logger::error(format!("Startup failed: {e}"));
        eprintln!("{e}");
        std::process::exit(code);
    }

    // ── 6. Framework poise + client gateway ──
    let options = poise::FrameworkOptions {
        commands: commands::commands(),
        command_check: Some(commands::command_check),
        on_error: |err| Box::pin(async move { commands::handle_error(err).await }),
        event_handler: |ctx, event, framework, data| {
            Box::pin(async move {
                commands::on_ready(ctx, event, framework, data).await;
                Ok(())
            })
        },
        ..Default::default()
    };
    let framework = poise::Framework::builder()
        .options(options)
        .setup(move |_ctx, _ready, _framework| Box::pin(async move { Ok(data) }))
        .build();

    // GuildMembers (intent privilégié) : requis pour que le sélecteur de
    // membres de /users liste TOUS les membres de la guilde. Doit aussi
    // être activé dans le portail développeur (SERVER MEMBERS INTENT).
    //
    // Note mémoire : le cache des MESSAGES est déjà désactivé par défaut
    // dans serenity 0.12 (`max_messages: 0`) — le bot n'en lit aucun.
    // Seul le cache de guilde/membres est actif (nécessaire au sélecteur).
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_MEMBERS;
    let mut client = match serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
    {
        Ok(client) => client,
        Err(e) => {
            let code = find_exit_code(&e).unwrap_or(1);
            logger::error(format!("Startup failed: {e}"));
            eprintln!("{e}");
            std::process::exit(code);
        }
    };

    // ── 7. Boucle principale (gateway vs signal d'arrêt) ──
    tokio::select! {
        result = client.start() => {
            if let Err(e) = result {
                let code = find_exit_code(&e).unwrap_or(1);
                logger::error(format!("Startup failed: {e}"));
                std::process::exit(code);
            }
        }
        _ = shutdown_signal() => {
            logger::info("Received signal, shutting down…");
            // Équivalent du `client.destroy()` du JS : on coupe proprement
            // la connexion gateway avant de laisser le process se terminer.
            let _ = client.shard_manager.shutdown_all().await;
        }
        _ = ops::daemon_shutdown() => {
            logger::info("Supervisor requested shutdown…");
            let _ = client.shard_manager.shutdown_all().await;
        }
    }
}
