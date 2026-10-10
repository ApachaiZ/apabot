<div align="center">

# 🤖 apabot — Game Control Bot

### Pilotez et surveillez vos serveurs de jeu depuis Discord — en binaire natif unique 🦀

[![Rust](https://img.shields.io/badge/Rust-%E2%89%A5%201.85-orange?logo=rust)](https://rust-lang.org)
[![serenity](https://img.shields.io/badge/serenity-0.12-5865F2?logo=discord)](https://crates.io/crates/serenity)
[![poise](https://img.shields.io/badge/poise-0.6-5865F2)](https://crates.io/crates/poise)
[![Licence](https://img.shields.io/badge/licence-MIT-blue)](#-licence)

</div>

---

## 📖 Sommaire

1. [Qu'est-ce que c'est](#-quest-ce-que-cest)
2. [Construire et lancer](#-construire-et-lancer)
3. [Configuration](#️-configuration)
4. [Structure du crate](#️-structure-du-crate)
5. [Concepts Rust illustrés (guide de lecture)](#-concepts-rust-illustrés-guide-de-lecture)
6. [Tests](#-tests)
7. [Déploiement](#-déploiement)
8. [Limitations connues](#-limitations-connues)

---

## 🎯 Qu'est-ce que c'est

Un bot Discord **100 % privé** pour démarrer, arrêter, redémarrer et surveiller
vos serveurs de jeu (ou VPS) hébergés chez **9 providers** : YorkHost, Nitrado,
Hetzner, OVHcloud, Scaleway, DigitalOcean, Vultr, UpCloud — plus un provider
**`generic`** qui pilote n'importe quel panneau à API REST.

- 📊 dashboard modulaire (`/status`) — CPU/RAM/disque/joueurs/uptime/adresse/nœud,
  chaque ligne n'apparaît que si le provider la renseigne ; bouton
  **Rafraîchir** (relecture sans retaper la commande, 10 min) ;
- ▶️⏹️🔁 `/start` `/stop` `/restart` avec **double confirmation**, **verrou
  anti-collision par service**, POST power **unique sans retry**, suivi
  silencieux à backoff 5 s → 30 s (2 états stables exigés, 10 min max,
  bouton d'annulation du suivi), GIFs animés et estimation de durée ;
- 👻 **100 % éphémère** + notification DM en fin d'action ;
- 🔔 **watchdog** (alertes crash/CPU/RAM/disque, cooldown 15 min, délai de
  grâce après arrêt volontaire) ;
- 👑 `/users add|remove|list|clear` (owner), opérateurs persistés **chiffrés
  AES-256-GCM** ;
- 📜 `/logs` (owner, avec filtre), 🏓 `/ping`, 🎯 `/server` (session par
  utilisateur) ;
- 🌐 **i18n en/fr** complète ;
- ⚡ enregistrement conditionnel des slash commands (hash + purge des
  doublons, une seule requête groupée) ;
- 🎛 **mission control intégré** : daemon embarqué cross-platform
  (`start`/`stop`/`restart`/`status`/`logs`/`help`), TUI piloté au clavier
  avec lancement de commandes depuis l'outil — aucun PM2, aucun script ;
- 📦 **installation utilisateur** (`install`/`reinstall`/`uninstall`) :
  binaire + données aux conventions de l'OS (Linux/macOS/Windows) ou dans
  un répertoire au choix, **instances multiples** (`--name`), configuration
  au moment de l'install (`--env-file` ou prompts), **profils** de config
  (`config save/load/list/delete`) — le mode portable reste intact.

Le tout tient dans **un seul binaire statique d'environ 14 Mo** : pas de
runtime à installer, pas de dépendance système (TLS en pur Rust), ~10 Mo de
RSS en régime, démarrage en quelques millisecondes.

---

## 🔨 Construire et lancer

```bash
# Prérequis : Rust ≥ 1.85 (rustup) — rien d'autre.
cargo build --release

# Premier lancement : le setup interactif pose les questions nécessaires
# et écrit .config.d/.env.
./target/release/apabot

# En production (superviseur embarqué / systemd / Docker) : jamais de prompt.
./target/release/apabot --non-interactive

# Tests unitaires : 84 (aucune dépendance réseau ni fichier hors d'un dossier temporaire).
cargo test
```

Le bot démarre, enregistre les slash commands (sautées si rien n'a changé),
affiche la bannière et lance le watchdog :

```text
Slash commands registered.
╭─────────────────────────────────╮
│ 🎮  APABOT • GAME CONTROL        │
│ 👤  MonBot#1234                 │
│ 🎯  services: main              │
│ 🌐  language: fr                │
│ 💻  rust 2.0.0  •  pid 12345    │
│ 📅  2026-10-09 10:30:00         │
╰─────────────────────────────────╯
```

---

## ⚙️ Configuration

`.config.d/.env` est lu en priorité, puis l'environnement système. Voir
`.config.d/.env.example` pour le modèle complet.

La commande `config` gère la configuration à la volée (installée ou
portable) : le questionnaire interactif, et des PROFILS nommés :

```bash
apabot config                    # questionnaire interactif (change à la volée)
apabot config save prod          # sauvegarde le .env courant en profil
apabot config load prod          # remplace le .env actif (avec confirmation)
apabot config list               # profils disponibles
apabot config delete prod        # supprime un profil
```

Après un changement, si le daemon tourne, un redémarrage du bot est
proposé pour appliquer la configuration. Les profils vivent dans
`.config.d/profiles/` (mode 0600), isolés par installation/instance.

| Variable | Rôle |
|---|---|
| `DISCORD_TOKEN` / `DISCORD_CLIENT_ID` / `DISCORD_OWNER_ID` / `DISCORD_GUILD_ID` | connexion Discord |
| `PROVIDER` | `yorkhost` (défaut), `hetzner`, `nitrado`, `ovh`, `scaleway`, `digitalocean`, `vultr`, `upcloud`, ou **`generic`** : n'importe quel panneau à API REST, entièrement configuré dans le `.env` |
| `PROVIDER_API_KEY` / `PROVIDER_API_URL` | identifiants du provider |
| `PROVIDER_GET_TIMEOUT_MS` | timeout des lectures d'état en ms (défaut : celui du provider — 60 s pour YorkHost) |
| `PROVIDER_SERVICE_ID` **ou** `PROVIDER_SERVICES=alias=id,…` | cible(s) pilotée(s) |
| `DISCORD_ALERT_CHANNEL_ID` / `ALERT_INTERVAL_MS` | watchdog (optionnel) |
| `BOT_LANGUAGE` | `en` / `fr` (défaut `en`) — surchargeable par `--lang=fr` |

Identifiants supplémentaires : `OVH_APPLICATION_SECRET` + `OVH_CONSUMER_KEY`
(pour `PROVIDER=ovh`), `SCW_ZONE` (Scaleway), `PROVIDER_API_KEY` au format
`user:password` (UpCloud).

> ⚠️ **Rotation du token Discord** : la clé de chiffrement est dérivée du
> token — supprimez `users.json`, `sessions.json` et `members.roster.json`
> après une rotation (le bot les recréera).

---

## 🗂️ Structure du crate

```
src/
├── main.rs            ← bootstrap : mode installé/portable, dispatch mission control, gateway, signaux
├── paths.rs           ← chemins canoniques (.config.d/, logs/, assets/)
├── assets.rs          ← GIFs de résultat (pool disque par état, puis octets embarqués)
├── fsutil.rs          ← écriture atomique (tmp+rename) + permissions 0600
├── logger.rs          ← double sortie console/fichier, rotation 5 Mo × 5, bannière, passthrough
├── config.rs          ← validation de la config (exit 78)
├── setup.rs           ← assistant interactif (dialoguer) + réécriture du .env
├── i18n/              ← catalogues en/fr TYPÉS (structs compilées, zéro JSON)
├── services.rs        ← parsing `alias=id`, résolution, noms d'affichage
├── errors.rs          ← classification des erreurs réseau + messages localisés
├── crypto.rs          ← AES-256-GCM (données privées au repos)
├── state.rs           ← opérateurs, verrous anti-collision, sessions, délai de grâce
├── stats.rs           ← durées moyennes persistées (« En moyenne : 51 s »)
├── ops/               ← mission control (daemon embarqué, installation, config)
│   ├── mod.rs         ← sous-commandes start/stop/restart/status/logs + dispatch
│   ├── supervise.rs   ← superviseur : politique de relance + canal de contrôle
│   ├── protocol.rs    ← protocole JSON-lignes du canal (jeton, ping/status/stop/restart)
│   ├── spawn.rs       ← lancement détaché cross-platform (setsid / creation flags)
│   ├── state.rs       ← .config.d/daemon.json (pid, port, jeton) + détection d'obsolescence
│   ├── tail.rs        ← tail + suivi du log par sondage (cross-platform, rotation-safe)
│   ├── install.rs     ← install/reinstall/uninstall (défaut OS, destination, --name, --refresh-assets)
│   ├── completions.rs ← scripts d'autocomplétion shell (bash|zsh|fish)
│   ├── config.rs      ← config interactive + profils
│   └── systemd.rs     ← `systemd install|remove` optionnel (Linux, confirmations)
├── tui.rs             ← mission control TUI (ratatui : état, logs, commandes au clavier)
├── providers/
│   ├── mod.rs         ← trait Provider + plomberie HTTP commune (Rest) + enum AnyProvider
│   └── yorkhost.rs, hetzner.rs, nitrado.rs, ovh.rs, scaleway.rs,
│       digitalocean.rs, vultr.rs, upcloud.rs, generic.rs (REST configuré .env)
├── api.rs             ← cache 5 s + déduplication des requêtes en vol (OnceCell)
├── embeds.rs          ← cartes + dashboard modulaire + barres visuelles
├── confirm.rs         ← boutons Confirmer/Annuler (collecteur 60 s)
├── power.rs           ← cœur : machine à états power + polling (tokio::select!)
├── commands.rs        ← slash commands poise + hooks globaux (guilde, erreurs, panics)
├── deploy.rs          ← enregistrement conditionnel (hash) + purge anti-doublons
├── watchdog.rs        ← surveillance périodique + alertes (tâche tokio détachée)
├── members.rs         ← roster persisté + autocomplétion (politique opcode 8)
├── manage.rs          ← /users add|remove|list|clear
└── logs_cmd.rs        ← /logs (owner)
```

---

## 🧠 Concepts Rust illustrés (guide de lecture)

Le crate est écrit pour servir de support de cours : chaque fichier documente
ses choix. Parcours conseillé, du plus simple au plus avancé :

1. **Propriété & borrowing** — `paths.rs`, `fsutil.rs` : qui possède quoi,
   `&str` vs `String`, `Path`/`PathBuf`.
2. **Enum & pattern matching** — `providers/mod.rs` : l'enum `AnyProvider`
   fait du dispatch des providers un `match` exhaustif vérifié par le
   compilateur (ajoutez un provider → le compilateur liste les `match` à
   compléter).
3. **Trait** — `trait Provider` : le contrat d'intégration devient une
   garantie de compilation.
4. **Structs + données immuables** — `i18n/` : les catalogues de traductions
   sont des `static` de structs `&'static str` — une clé manquante est une
   ERREUR DE COMPILATION, pas un bug en production.
5. **Gestion d'erreurs** — `errors.rs` : `thiserror` (erreurs typées avec
   code de sortie) vs `anyhow` (bootstrap) ; `?`, `downcast_ref`, chaîne de
   `source()` pour retrouver un code d'erreur à travers les wrappers.
6. **Concurrence (tokio)** — `api.rs` (`tokio::sync::OnceCell` : une seule
   requête HTTP partagée entre appels concurrents), `power.rs`
   (`tokio::select!` : la « course » entre le polling et le clic
   d'annulation), `watchdog.rs` (tâche détachée + `join_all` parallèle +
   `Mutex` à sections courtes).
7. **Partage d'état** — `commands.rs` (`Data`) : `Arc` pour ce qui traverse
   les tâches, `&'static` pour ce qui vit pour toujours, valeur pour ce qui
   est immuable.
8. **Crypto** — `crypto.rs` : AES-256-GCM, format base64 `iv‖tag‖ct`,
   migration silencieuse des fichiers clairs et de l'ancien sel de clé.

---

## 🧪 Tests

```bash
cargo test          # 84 tests, aucune connexion réseau nécessaire
cargo clippy        # propre
```

Couvert : parsing des services, crypto (aller-retour, altération, clé
erronée, blob hérité), normalisation des providers (tables d'états,
adresses, stats Nitrado, aliases du provider generic), signature OVH
(vecteur de contrôle `sha1sum`), embeds (barres, durées, formatage),
interpolation i18n, classification des erreurs réseau, cooldowns du
watchdog, validation de `users.json`, déploiement (hash), politique de
relance du superviseur, protocole du canal de contrôle, tail/suivi de log,
génération d'unité systemd, registres et nommage des instances…

---

## 🚀 Déploiement

### Installation via `cargo install --git` (recommandé pour les utilisateurs Rust)

```bash
cargo install --git https://github.com/ApachaiZ/apachBotR
apabot install        # crée la structure aux conventions de l'OS
```

Le binaire arrive dans `~/.cargo/bin` (déjà dans le PATH avec rustup) et
**embarque les GIFs des cartes** (`include_bytes!`) : `apabot install`
matérialise la structure de données sans aucun checkout source à côté, et
propose la configuration immédiatement (prompts ou `--env-file`).
Le mode portable fonctionne aussi : le binaire tourne n'importe où.

### Installation utilisateur (`install` / `reinstall` / `uninstall`)

Le binaire s'installe lui-même, sans root et sans gestionnaire de paquets.
Trois modes de vie coexistent :

- **Portable** : un binaire NON installé garde tous ses chemins relatifs à
  son répertoire courant — repo de dev, clé USB, n'importe où ;
- **Installation par défaut** : conventions de l'OS —
  `~/.local/bin/apabot` + `~/.config/apabot/` sur Linux/macOS (XDG),
  `%LOCALAPPDATA%\Programs\apabot\apabot.exe` + `%APPDATA%\apabot\` sur
  Windows ;
- **Destination personnalisée** : un répertoire choisi au clavier —
  `D/bin/apabot` + `D/` (données) : tout tient dans D.

```bash
apabot install                          # prompts : destination + confirmation
apabot install --name apabot01          # instance NOMMÉE (config isolée)
apabot install --name auto              # nom généré (apabot01, apabot02…)
apabot install --env-file ~/mon.env     # configuration depuis un fichier
apabot install --refresh-assets         # réécrit aussi les GIFs de cartes
apabot reinstall --name apabot01        # met à jour un binaire (données intactes)
apabot reinstall --refresh-assets       # binaire + GIFs de cartes (le reste intact)
apabot uninstall --name apabot01        # retire une instance (données : NON par défaut)
apabot install --dry-run                # plan sans rien écrire
```

**Les GIFs des cartes** vivent dans `assets/emojis/` en TROIS dossiers :
`loading/` (chargements et attentes), `success/` et `error/`. À chaque
affichage, un GIF est tiré au hasard dans le dossier de l'état. Pour
personnaliser : ajoute ou retire des `.gif` dans le dossier (disque) sans
recompiler ; le pool EMBARQUÉ est généré au build depuis le contenu réel
— retirer un asset entre deux builds ne casse jamais la compilation.

Un GIF déjà sur disque n'est JAMAIS écrasé (personnalisation préservée).
Après un `git pull` + rebuild, force le rafraîchissement avec
`apabot reinstall --refresh-assets` (ou `--refresh-assets` dès
l'installation) : chaque dossier est vidé puis réécrit avec les GIFs par
défaut — les GIFs retirés du repo disparaissent aussi du pool installé.

**Instances multiples** : `--name` isole COMPLÈTEMENT chaque bot —
`~/.config/apabot/instances/<nom>/` contient son propre `.config.d/.env`,
ses logs, ses profils et l'état de son daemon ; le binaire s'appelle
`apabot-<nom>` et se reconnaît par son nom. Plusieurs instances tournent
en même temps, chacune avec son daemon et son canal de contrôle.

### Autocomplétion shell

`apabot completions` imprime le script d'autocomplétion pour bash, zsh ou
fish — généré depuis la liste réelle des sous-commandes.

```bash
apabot completions bash > ~/.local/share/bash-completion/completions/apabot
apabot completions zsh  > ~/.zsh/completions/_apabot      # dossier présent dans $fpath
apabot completions fish > ~/.config/fish/completions/apabot.fish
```

### Mission control (daemon embarqué, tous les OS)

Le binaire est son propre superviseur : daemon embarqué (`src/ops/`) +
TUI de pilotage (`src/tui.rs`).

```bash
apabot start            # bot en daemon supervisé (relances auto)
apabot status           # carte : pid, uptime, relances
apabot logs             # dernières lignes + suivi en direct (Ctrl-C pour sortir)
apabot logs --lines 5 --no-follow
apabot restart          # redémarre le processus du bot
apabot stop             # arrêt propre (message shutdown sur le canal de contrôle)
apabot mission-control  # TUI : état + logs + commandes au clavier
apabot                  # premier plan (développement)
```

Garanties du superviseur : relance après 5 s, 10 relances max
consécutives, compteur remis à zéro après 30 s stables, **exit 78 = jamais
de relance** (un `.env` invalide ne boucle pas), arrêt propre jusqu'à 10 s
avant le kill forcé, sortie de l'enfant capturée dans `logs/apabot.log`.
L'état vivant est dans `.config.d/daemon.json` (mode 0600, jeton aléatoire
du canal de contrôle).

Dans le TUI : `q` quitter, `s` démarrer, `x` arrêter, `r` redémarrer,
`f` suivre le log (↑/↓/PgUp/PgDn pour défiler), `:` pour taper une commande
(`start`, `stop`, `restart`, `status`, `help`, `quit`) — `?` affiche l'aide.

### systemd (optionnel, Linux)

Le superviseur embarqué reste le défaut ; systemd s'installe sur demande,
avec une **confirmation à chaque étape** (prévisualisation de l'unité,
écriture, `daemon-reload`, activation) :

```bash
apabot systemd install           # prompts interactifs (user ou system)
apabot systemd install --dry-run # affiche l'unité sans rien écrire
apabot systemd remove            # désactive + supprime (confirmations)
```

L'unité générée (équivalente à `deploy/apabot.service`, mais avec les
chemins RÉELS de l'installation) durcit le service
(`ProtectSystem=strict`, `NoNewPrivileges`…) et fixe
`Restart=on-failure`. Ne pas combiner avec `start` : deux superviseurs
piloteraient deux bots sur la même gateway Discord.

---

## ⚠️ Limitations connues

- **Changement de guilde** : les commandes de l'ancienne guilde sont
  purgées au redémarrage (best-effort).
- **Baseline watchdog** : un service qui tombe dans les 15 premières
  secondes après le démarrage ne déclenche pas d'alerte « down » (aucune
  baseline persistée).
- **Cross-platform** : l'arrêt du daemon passe par le canal de contrôle
  (pas de signaux), donc Windows n'a plus de limitation de fond — mais
  l'ensemble reste testé en production sous Linux. Les chemins Windows
  (`creation flags`, `tasklist`, `%APPDATA%`) sont écrits mais non
  vérifiés ici.
- **`mission-control` exige un vrai terminal** ; hors TTY la commande
  échoue avec un message explicite.
- **`systemd install|remove` est Linux seulement**, avec confirmation à
  chaque étape (`--dry-run` affiche sans rien écrire).
- **Garde-fou mémoire** : pas de limite RSS dans le superviseur embarqué
  (lecture non portable) — le binaire tient ~10 Mo RSS ; pour une limite
  dure, l'unité systemd permet `MemoryMax=`.
- **Rotation concurrente** : en mode daemon, l'enfant ET le superviseur
  écrivent le même fichier de log ; deux rotations simultanées peuvent
  réordonner quelques lignes (rien n'est perdu). Rarissime (seuil 5 Mo,
  vérification toutes les 30 s).

## 📜 Licence

MIT — voir `LICENSE`.

---

<div align="center">

**Fait avec ❤️ et beaucoup de match exhaustifs — par Apach**

`🎮 Pilotez. Surveillez. Profitez. — en binaire natif.`

*Ce projet est le successeur en Rust du bot Node.js original d'Apach.*

</div>
