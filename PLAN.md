# ha-gazpar-rust : plan de réimplémentation

Réécriture en Rust de [gazpar2haws](https://github.com/ssenart/gazpar2haws) 0.6.1 et de la partie utile de [PyGazpar](https://github.com/ssenart/PyGazpar) 1.4.0 (tous deux MIT, Stéphane Senart), pour obtenir un binaire unique et très léger qui importe la consommation de gaz GRDF dans les statistiques long terme de Home Assistant.

## 1. Objectif et forme du livrable

Home Assistant ne charge que des intégrations Python : un « module » Rust ne peut pas vivre dans HA. Le livrable est donc, comme gazpar2haws et ha-linky, **un petit démon externe** qui parle à HA par son API WebSocket.

| | gazpar2haws 0.6.1 (mesuré) | ha-gazpar-rust (cible, à mesurer) |
|---|---|---|
| Image Docker | 241 Mo | < 5 Mo (`FROM scratch`) |
| Code | ≈ 6 100 lignes Python | ≈ 1 800 lignes Rust |
| Dépendances | 21 paquets Python + image Debian | 3 directes, ≈ 30 crates au total, aucun système de base |
| Mémoire au repos | interpréteur Python + numpy + pydantic | quelques Mo |
| Exécution | boucle asyncio + appels HTTP bloquants | synchrone, un seul thread |

Hors périmètre : source Excel, fréquences hebdo/mensuelle/annuelle, CLI PyGazpar, températures (météo), migration des anciens capteurs, moteur de tarification générique.

## 2. Ce que fait le programme (protocole relevé dans le code d'origine)

### 2.1 Connexion GRDF (`pygazpar/api_client.py`)

Session HTTP avec cookies, `User-Agent` de navigateur, délais 10 s (connexion) / 60 s (lecture).

1. `GET https://monespace.grdf.fr/` (redirections suivies) → extraire `"stateToken":"…"` du HTML par expression régulière, remplacer `\x2D` par `-`.
2. Poser le cookie `ln=<e-mail>`.
3. `POST https://connexion.grdf.fr/idp/idx/identify`, JSON `{"identifier": e-mail, "stateHandle": stateToken}`, en-têtes `Accept: application/json; okta-version=1.0.0` et `Content-Type: application/json` → lire `stateHandle`.
4. `POST https://connexion.grdf.fr/idp/idx/challenge/answer`, JSON `{"credentials": {"passcode": mot de passe}, "stateHandle": …}` → lire `success.href`.
5. `GET success.href` (redirections suivies) → 200 = session ouverte.

En cas d'échec, le motif est dans `messages.value[].message` ou `errorSummary`. Le `stateHandle` est un jeton : ne jamais le journaliser.

### 2.2 Lecture des données

- `GET https://monespace.grdf.fr/api/e-conso/pce` → liste des PCE (`idObject`), pour vérifier que le PCE configuré appartient au compte.
- `GET /api/e-conso/pce/consommation/informatives?dateDebut=AAAA-MM-JJ&dateFin=AAAA-MM-JJ&pceList[]=<PCE>` (ou `publiees`).
- Réponse : `{ "<PCE>": { "releves": [ … ] } }`, ou `[]` s'il n'y a rien.
- Champs utiles d'un relevé : `journeeGaziere` (relevé informatif d'un jour), `dateDebutReleve`/`dateFinReleve` (période publiée), `indexDebut`, `indexFin`, `volumeBrutConsomme` (m³), `energieConsomme` (kWh), `coeffConversion`, `qualificationReleve`.
- Erreurs : 401 ou réponse HTML hors `/api` = session expirée (une seule reconnexion) ; 429 = attendre `Retry-After` (plafonné à 60 s) ; HTML = erreur serveur, nouvelle tentative avec délais 3/6/12/15 s.

### 2.3 Home Assistant (`gazpar2haws/haws.py`)

WebSocket `ws://hôte:8123/api/websocket` : recevoir `auth_required`, envoyer `{"type":"auth","access_token":…}`, recevoir `auth_ok`. Puis des commandes numérotées (`id` croissant) :

- `recorder/statistics_during_period` (`statistic_ids`, `start_time`, `end_time`, `period: "day"`) → dernière somme connue (`start` en millisecondes Unix, `sum`).
- `recorder/import_statistics` : `metadata` (`statistic_id`, `source`, `name`, `has_sum: true`, `has_mean: false`, `mean_type: 0`, `unit_class`, `unit_of_measurement`) et `stats: [{start, state, sum}]`, où `start` est minuit heure de Paris et `sum` le cumul.
- `recorder/clear_statistics` (remise à zéro uniquement).
- `get_config` → `time_zone` de HA : c'est le fuseau des jours (pas d'option de fuseau, voir §5).

Vérifié sur HA 2026.9.4 et 2026.11.1 (`make test-ha`) : `start` est un entier en millisecondes Unix ; `period: "day"` regroupe selon le fuseau de HA ; les statistiques externes `gazpar:` et les `sensor.*` en source `recorder` sans entité sont acceptées, `m³` compris. **Les écritures sont asynchrones** : HA répond `success` avant que le recorder ait écrit ; une relecture immédiate peut renvoyer l'ancien état. La synchronisation ne relit donc jamais juste après avoir écrit.

## 3. Corrections par rapport à l'original

L'audit du 2026-11-16 a relevé quatre défauts de fiabilité. La réécriture les corrige dès la conception.

| Défaut de l'original | Comportement de ha-gazpar-rust |
|---|---|
| Une erreur HA fait sortir le processus après le login GRDF ; avec un redémarrage automatique, GRDF est sollicité en boucle. | La boucle de `run` attrape toutes les erreurs, les journalise et attend l'intervalle suivant (60 min au moins) : une erreur n'avance jamais le relevé suivant. Le processus ne sort que sur erreur de configuration, détectée avant toute connexion à GRDF. Garde interne au processus seulement (aucun état persistant, choix de l'utilisateur du 2026-11-16) : chaque démarrage, `once` ou `check` est une connexion de plus, ce que le manuel dit. |
| Un mot de passe refusé est réessayé à chaque relevé, ce qui enchaîne les échecs d'authentification. | Si GRDF refuse les identifiants, `run` cesse toute connexion à GRDF jusqu'à son redémarrage, sans sortir, en le journalisant à chaque intervalle. |
| `reset: true` efface les statistiques à chaque relevé. | La remise à zéro est une sous-commande ponctuelle (`ha-gazpar reset`), jamais une option persistante. |
| Un jour sans donnée est importé à 0 et jamais rattrapé. | Les jours sans valeur ne sont pas importés. Chaque relevé redemande une fenêtre de recouvrement (14 jours par défaut) et recalcule les cumuls à partir de la somme du jour précédant la fenêtre : valeurs tardives et corrections de GRDF sont rattrapées (l'import HA remplace les points de même date). |
| Lecture puis vérification immédiate dans HA. | Jamais de relecture juste après une écriture (écritures asynchrones, §2.3). |

## 4. Choix techniques

**Synchrone, sans runtime asynchrone.** Le programme fait une dizaine de requêtes toutes les 8 heures : `tokio` n'apporte rien et pèse lourd.

### 4.1 Dépendances retenues

Trois dépendances directes, aucune macro procédurale, aucun code C à compiler hors `ring`. Nombres de crates mesurés le 2026-11-16 avec `cargo tree` (cible `aarch64-unknown-linux-musl`, dépendances d'exécution).

| Besoin | Crate (version mesurée) | Crates tirées | Pourquoi celle-ci |
|---|---|---|---|
| HTTPS vers GRDF | `ureq` 3.5 (`default-features = false`, `rustls`) | 22 | client bloquant de référence ; TLS `rustls` + `ring`, racines `webpki-roots` embarquées, pas d'OpenSSL |
| JSON | `serde_json` 1.0, sans `derive` | 5 | lecture par `Value` : pas de `syn`/`quote` à la compilation |
| Dates et fuseau | `jiff` 1.0 (`std`, `tzdb-bundle-always`) | 3 | l'image `scratch` n'a pas de `/usr/share/zoneinfo` ; aucune dépendance tierce |

Total attendu : environ 30 crates (quelques-unes sont partagées), à figer à l'étape 1.

### 4.2 Ce qui est écrit à la main plutôt qu'importé

| Besoin | Alternative écartée (mesure) | Choix |
|---|---|---|
| Cookies de session GRDF | option `cookies` de `ureq` : 67 crates au lieu de 22 | bocal à cookies maison (≈ 80 lignes), redirections suivies à la main pour lire `Set-Cookie` à chaque saut |
| WebSocket vers HA | `tungstenite` 0.30 : 27 crates | client RFC 6455 minimal maison (≈ 200 lignes) : poignée de main, trames texte masquées, fragmentation, ping/pong, fermeture. Norme figée depuis 2011, pair de confiance (son propre HA). `ws://` uniquement ; si `wss://` devient nécessaire, repasser à `tungstenite` |
| Configuration | `clap`, YAML/TOML | variables d'environnement lues avec `std::env` |
| Journal | `log` + backend | `eprintln!` horodaté, niveaux `info`/`debug` ; pas de fichier, Docker s'en charge |
| Erreurs | `anyhow`, `thiserror` | un `enum Error` |
| Extraction de `stateToken` | `regex` | recherche de sous-chaîne |

Le WebSocket ne peut pas être évité : sur HA 2026.9.1, l'API REST expose bien l'action `recorder.get_statistics`, mais l'import de statistiques n'existe que par WebSocket (vérifié sur `/api/services`).

Chaque morceau maison est isolé dans un module de moins de 200 lignes, couvert par des tests, derrière une interface étroite : on peut le remplacer par une crate sans toucher au reste.

Alternative mesurée pour HTTP : `minreq` 3.0 (`https-rustls`), 12 crates au lieu de 22. Écartée par défaut au profit de `ureq`, plus répandu ; à réévaluer à l'étape 4, le client HTTP étant confiné à `grdf/http.rs`.

### 4.3 Règles de maintenance

L'objectif est qu'en régime établi, les seules évolutions soient des correctifs de sécurité ou une adaptation à un changement de GRDF ou de HA.

- **Budget de dépendances** : 3 directes. Toute nouvelle dépendance directe doit être justifiée dans `DEPENDENCIES.md` (rôle, alternative maison envisagée, nombre de crates tirées). La CI échoue si le nombre total de crates dépasse le plafond figé à l'étape 1.
- **`DEPENDENCIES.md`** : liste des dépendances directes et de ce que chacune tire, régénérée par `cargo tree`.
- **`Cargo.lock` commité**, compilation toujours avec `--locked`.
- **`cargo deny`** en CI : avis de sécurité (RustSec), licences autorisées (MIT, Apache-2.0, ISC, BSD, Unicode), sources limitées à crates.io, doublons de versions interdits.
- **Veille de sécurité sans bruit** : alertes et correctifs de sécurité Dependabot activés, mises à jour de version désactivées. Un job planifié hebdomadaire relance `cargo deny check advisories` pour signaler une faille même sans commit.
- **Pas de système de base** : l'image `FROM scratch` ne contient que le binaire, donc aucune mise à jour de paquets Debian ou Alpine à suivre.
- **Chaîne de compilation figée** : `rust-toolchain.toml`, édition 2024, `#![forbid(unsafe_code)]`, `clippy -D warnings`.
- **Publication traçable** : image multi-architecture construite par GitHub Actions à partir d'un tag, avec attestation de provenance (voir §9 et §10).

Points de maintenance connus qui ne sont pas de la sécurité :

- `webpki-roots` embarque les autorités de certification : à mettre à jour si GRDF change d'autorité racine.
- La base de fuseaux embarquée par `jiff` : à mettre à jour seulement si la France change ses règles d'heure d'été.
- Le flux de connexion GRDF et le schéma d'import de HA (voir §12).

Quantités en entiers : les valeurs GRDF sont lues comme texte décimal et converties en millièmes (`i64`), ce qui évite les erreurs de flottants dans les cumuls.

**Profil de compilation** : `opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`. Cible `aarch64-unknown-linux-musl` (statique) pour rasp, `x86_64-unknown-linux-musl` pour le développement.

**Identifiants des statistiques.** Statistiques externes `gazpar:<pce>_energy` et `gazpar:<pce>_volume` (source `gazpar`), sur le modèle de ha-linky (`linky:<PRM>`) déjà présent dans le tableau de bord Énergie. Elles apparaissent dans la liste déroulante, sans faux capteur `sensor.…` ni avertissement « entité non définie ». Préfixe réglable (`GAZPAR_STATISTIC_PREFIX`) pour reprendre des statistiques `sensor.gazpar2haws_*` existantes.

## 5. Interface

**La référence de l'interface est le manuel d'utilisation, [docs/MANUEL.md](docs/MANUEL.md)**, écrit avant l'implémentation et relu : commandes, options, règles de configuration, comportements promis. Le code s'y conforme ; toute évolution de l'interface commence par le manuel.

Points structurants :

- sous-commandes `run`, `once`, `check`, `reset --yes` ;
- **trois sources de configuration** pour une seule liste d'options : paramètre de la commande, variable d'environnement, fichier `NOM=valeur` désigné par `--config` ou `GAZPAR_CONFIG`, dans cet ordre de priorité ; noms dérivés mécaniquement (`GRDF_PCE` ↔ `--grdf-pce`) ; `check` affiche la source de chaque valeur ;
- variantes `…_FILE` (chemin absolu) pour les valeurs sensibles ; mot de passe et jeton refusés en paramètre ; l'e-mail est accepté en paramètre (donnée sensible, jamais journalisée) ;
- options : `GRDF_USERNAME`, `GRDF_PASSWORD`, `GRDF_PCE`, `HA_URL`, `HA_TOKEN`, `GAZPAR_SCAN_INTERVAL`, `GAZPAR_HISTORY_DAYS`, `GAZPAR_OVERLAP_DAYS`, `GAZPAR_STATISTIC_PREFIX`, `GAZPAR_LOG_LEVEL` ; pas d'option de fuseau (celui de HA est lu) ni de type de relevé tant que `published` n'existe pas ;
- `GRDF_BASE_URL` et `GRDF_LOGIN_URL` : réservées au développement, lues dans l'environnement seulement, hors manuel ;
- une option renommée ou retirée reste acceptée avec un avertissement pendant au moins une lignée de versions.

## 6. Organisation du code

```
src/
  main.rs        lit les arguments et appelle la bibliothèque, rien d'autre
  lib.rs         déclare les modules ; toute la logique est dans la bibliothèque
  cli.rs         analyse de la ligne de commande, sous-commandes, boucle du démon ; seul module qui assemble grdf, ha et sync
  config.rs      fusion des trois sources (paramètres, environnement, fichier), provenance de chaque valeur, validation
  config_file.rs analyse du fichier NOM=valeur (fonction pure)
  secret.rs      type Secret : Debug et Display masqués
  error.rs       enum Error
  log.rs         journal horodaté sur stderr
  domain.rs      types du métier, sans entrée-sortie : Day, Quantity (millièmes), DailyReading, StatPoint, StatisticId
  ports.rs       traits aux frontières : ConsumptionSource, StatisticsStore, Clock
  sync.rs        algorithme d'un relevé (§3), ne dépend que de domain et ports
  readings.rs    relevés GRDF (JSON) → DailyReading (fonction pure)
  grdf/          implémente ConsumptionSource
    http.rs      seul module qui touche à ureq : requêtes, bocal à cookies, redirections
    auth.rs      login Okta IDX (§2.1)
    api.rs       appels, reprises, expiration de session (§2.2)
  ha/            implémente StatisticsStore
    websocket.rs client RFC 6455 minimal (std uniquement)
    session.rs   authentification HA, envoi/réponse des commandes
    stats.rs     lecture de la dernière somme, import, effacement
tests/
  fixtures/      réponses GRDF et HA anonymisées
  support/       faux serveurs GRDF et HA (std uniquement)
  ha_reel.rs     essai contre un vrai conteneur Home Assistant (ignoré par défaut)
```

Règles de dépendance entre modules :

- `domain` ne dépend de rien ; `ports` ne dépend que de `domain`.
- `sync` et `readings` ne font aucune entrée-sortie : ni réseau, ni horloge, ni environnement. `sync` se teste avec des implémentations en mémoire des traits de `ports`.
- `grdf` et `ha` ne se connaissent pas ; seul `cli` les assemble.
- Seul `grdf/http.rs` importe `ureq`.

**Arrêt du conteneur.** Pas de gestion des signaux : impossible en bibliothèque standard sans `unsafe`, et inutile, car chaque import est une seule commande WebSocket, donc un arrêt brutal ne laisse rien à moitié écrit. Comme le binaire est PID 1 dans une image `scratch`, le compose met `init: true` pour que `docker stop` soit immédiat.

## 7. Harnais de développement

Un `Makefile` comme point d'entrée unique, sans outil à installer en plus de Rust et Docker.

| Commande | Effet |
|---|---|
| `make check` | tout ce que fait la CI : `fmt --check`, `clippy -D warnings`, tests, `cargo deny`, plafond de crates |
| `make test` | tests unitaires seuls (hors réseau, quelques secondes) |
| `make cover` | couverture avec `cargo llvm-cov`, rapport HTML |
| `make ha-up` / `make ha-down` | démarre ou arrête un Home Assistant jetable en conteneur (port 8124), initialisé par script avec un utilisateur et un jeton de test |
| `make test-ha` | essais d'import réels contre ce Home Assistant jetable |
| `make dev` | lance `ha-gazpar once` contre le faux serveur GRDF et le Home Assistant jetable : toute la chaîne, sans toucher au vrai compte GRDF |
| `make image` | construit l'image arm64 et amd64 en local et affiche sa taille |
| `make deps` | régénère `DEPENDENCIES.md` |

Pour rendre `make dev` possible, les adresses de GRDF sont surchargeables par deux variables réservées au développement (`GRDF_BASE_URL`, `GRDF_LOGIN_URL`), acceptées seulement vers `localhost`.

Fichier `.env.example` pour la configuration locale ; `.env` est ignoré par Git.

## 8. Tests

**Principe : logique pure au centre, entrées-sorties en périphérie.** Tout ce qui décide (analyse, calcul, choix) est une fonction sans réseau, sans horloge et sans environnement ; ces trois éléments sont passés en paramètre. Les modules qui touchent au réseau sont réduits à de la tuyauterie.

Aucune dépendance de test : `#[test]` standard, faux serveurs écrits avec `std::net`. Le budget de dépendances vaut aussi pour `dev-dependencies`.

| Module | Testé par des tests unitaires |
|---|---|
| `config`, `config_file` | priorité des trois sources, option et variante `_FILE` dans des sources différentes, valeurs vides, grammaire du fichier (BOM, CRLF, guillemets, `#`, `export`, clé inconnue ou répétée), messages d'erreur sans valeur, provenance affichée par `check`, bornes, absence de secret dans `Debug` |
| `grdf/http` | bocal à cookies : domaine, chemin, remplacement, expiration ; suivi des redirections ; limite de sauts |
| `grdf/auth` | extraction de `stateToken` (dont `\x2D`), construction des charges utiles, lecture de `stateHandle` et `success.href`, messages d'erreur Okta, absence du jeton dans les erreurs |
| `grdf/api` | construction des URL, détection de session expirée, calcul des délais de reprise, `Retry-After`, réponse `[]` |
| `readings` | relevé informatif, jour sans donnée, enregistrement invalide ignoré, décimales en millièmes, tri ; mêmes résultats que PyGazpar sur ses jeux d'essai |
| `ha/websocket` | exemples de la RFC 6455, longueurs 7/16/64 bits, masquage, fragmentation, ping/pong, fermeture, trame invalide |
| `ha/session` | séquence d'authentification, numérotation des messages, réponse d'erreur |
| `ha/stats` | forme exacte des messages `import_statistics`, lecture de la dernière somme, minuit de Paris aux changements d'heure |
| `sync` | premier import, relevé sans nouveauté (rien envoyé), fenêtre de recouvrement, jour vide rempli plus tard, correction d'une valeur, cumul repris au bon point, délai minimal entre logins, erreur non fatale |

Au-dessus des tests unitaires, deux niveaux plus légers :

- **Tests de bout en bout hors ligne** : `once` contre les faux serveurs GRDF et HA, dans `cargo test`.
- **Essai contre un vrai Home Assistant** (`make test-ha`, et en CI) : seul moyen de vérifier que HA accepte réellement le schéma d'import.

Seuil de couverture en CI : 90 % des lignes hors `main.rs`. Aucun test automatique ne contacte le vrai GRDF.

## 9. CI/CD

Deux workflows GitHub Actions.

**`ci.yml`** — à chaque push et pull request, plus une exécution planifiée hebdomadaire :

1. `make check` (format, lint, tests, couverture, `cargo deny`, plafond de crates) ;
2. compilation statique arm64 et amd64, taille du binaire affichée et comparée à un plafond ;
3. essai contre le conteneur Home Assistant de la version de référence (voir §10) ;
4. exécution planifiée seulement : avis de sécurité, et essai contre `home-assistant:stable`. Un échec ouvre un ticket : c'est l'alerte « une nouvelle version de HA casse l'import » ou « une faille touche une dépendance ».

**`release.yml`** — à la pose d'un tag `vAAAA.M.N` :

1. vérifie que le tag, la version de `Cargo.toml` et une section de `CHANGELOG.md` concordent, sinon échec ;
2. rejoue `make check` ;
3. construit et pousse l'image multi-architecture sur `ghcr.io/juliensere/ha-gazpar-rust` avec attestation de provenance ;
4. crée la version GitHub, avec pour texte la section correspondante de `CHANGELOG.md` et les binaires en pièces jointes.

Publier une version se résume à : compléter `CHANGELOG.md`, changer la version dans `Cargo.toml`, poser le tag.

## 10. Versions et notes de version

### 10.1 Numérotation calée sur Home Assistant

Format **`AAAA.M.N`** :

- `AAAA.M` est la version de Home Assistant contre laquelle la version a été validée en CI (la « version de référence », ex. `2026.9`) ;
- `N` est le rang de la publication dans cette lignée, à partir de 0.

Exemple : `2026.9.0` puis `2026.9.1` (correctif), puis `2026.12.0` quand la référence passe à HA 2026.12. C'est un numéro valide pour Cargo.

Étiquettes de l'image : `2026.9.1` (figée), `2026.9` (dernière de la lignée), `latest`.

**On ne publie pas à chaque version mensuelle de HA.** Une version reste valable tant que l'essai hebdomadaire contre `home-assistant:stable` passe. La référence n'avance que lorsqu'une publication est de toute façon nécessaire (correctif de sécurité, changement de GRDF, changement du schéma d'import de HA). La version de référence est écrite à un seul endroit, `HA_VERSION` dans le `Makefile`, lu par la CI.

Le programme affiche au démarrage sa version et la version de HA à laquelle il est connecté, et avertit si HA est plus ancien que sa référence.

### 10.2 Notes de version

`CHANGELOG.md` à la racine, au format [Keep a Changelog](https://keepachangelog.com/fr/), en français, une section par version, la plus récente en haut :

- en tête de section : la date et les versions de Home Assistant validées ;
- rubriques **Nouveautés**, **Modifications**, **Corrections**, **Sécurité** (dont les mises à jour de dépendances motivées par une faille, avec l'identifiant de l'avis) ;
- une section **Non publié** en haut, complétée dans le même commit que chaque changement visible ; la CI refuse une pull request qui modifie `src/` sans toucher à `CHANGELOG.md`, sauf mention explicite.

La page des versions GitHub est générée à partir de ce fichier : une seule source pour les notes de version.

## 11. Étapes

Chaque étape se termine par un critère vérifiable.

1. **Squelette.** `cargo init`, profil de compilation, `config.rs`, `error.rs`, sous-commandes, `Makefile`, `ci.yml` (§9), `CHANGELOG.md`, `DEPENDENCIES.md`, `deny.toml`, `rust-toolchain.toml`, `.env.example`. *Critère : `ha-gazpar check` refuse une config incomplète avec un message clair ; la CI est verte ; le nombre de crates est figé.*

2. **Client Home Assistant.** `ha/websocket.rs` (testé sur les exemples de la RFC 6455 et sur une réponse fragmentée de plus de 64 Ko), `ha/session.rs`, `ha/stats.rs`. Tests contre un faux serveur WebSocket local, puis `make ha-up` et `make test-ha` contre un Home Assistant jetable. *Critère : import puis relecture de 10 points sur un conteneur HA 2026.9, avec validation du schéma `metadata` (`unit_class`, `mean_type`) accepté par cette version ; l'essai tourne aussi en CI.*

3. **Transformation des relevés.** `readings.rs` pour le type `informative` (un relevé = un jour). Jeux d'essai repris des tests de PyGazpar. *Critère : mêmes valeurs quotidiennes que PyGazpar sur les mêmes fichiers.*

4. **Client GRDF.** `grdf/http.rs`, `grdf/auth.rs`, `grdf/api.rs`, testés contre un faux serveur HTTP rejouant le flux Okta, puis un essai réel unique avec `check`. *Critère : `ha-gazpar check` affiche les 7 derniers jours du vrai compte.* C'est l'étape la plus risquée : à faire tôt si l'on veut lever le doute, en limitant les essais réels pour ne pas faire bloquer le compte.

5. **Synchronisation.** `sync.rs` : premier import, relevés suivants, fenêtre de recouvrement, erreurs non fatales, délai minimal entre logins. *Critère : `once` lancé deux fois de suite n'ajoute aucun point et ne change aucune somme ; un jour vide rempli plus tard dans le jeu d'essai est bien rattrapé.*

6. **Image et déploiement.** `Dockerfile` multi-étapes vers `FROM scratch`, compilation croisée arm64 (`cargo-zigbuild` ou `cross`), `release.yml` et première publication `2026.9.0` (§9, §10). Pas de contrôle de santé : sans état partagé (système de fichiers en lecture seule), une commande lancée à part ne peut rien savoir du démon. Remplacement du service `gazpar2haws` dans `~/homeassistant/compose.yml` (mêmes durcissements : `read_only`, `cap_drop: ALL`, utilisateur non root). *Critère : taille de l'image et mémoire résidente mesurées et notées dans le README ; historique de 3 ans visible dans le tableau de bord Énergie.*

7. **Options, selon besoin.**
   - Type `published` : répartition d'une période sur ses jours (volume réparti également, énergie au prorata, arrondis qui retombent exactement sur les totaux) et reconstruction des trous à partir des index.
   - Coût : statistique `gazpar:<pce>_cost` calculée à partir d'un prix du kWh et d'un abonnement datés. HA ne sait pas appliquer un prix à une statistique externe, il faut donc la calculer ici.

## 12. Risques

- **Authentification GRDF.** Le flux Okta est rétro-conçu et peut changer (captcha en 2023-2024, ticket ouvert #132 sur gazpar2haws). Le projet devra suivre PyGazpar. Le bocal à cookies maison doit reproduire fidèlement cookies et redirections entre `monespace.grdf.fr` et `connexion.grdf.fr` : à valider à l'étape 4 ; en cas d'échec, activer l'option `cookies` de `ureq` (+45 crates).
- **Code maison à la place de crates.** Le client WebSocket et le bocal à cookies sont du code à maintenir soi-même. Le risque est borné : normes figées, modules courts et testés, pairs connus.
- **Schéma de `recorder/import_statistics`.** Il a évolué récemment (`mean_type`, `unit_class`). À figer à l'étape 2 sur la version de HA en place, et à revérifier aux mises à jour de HA.
- **Jeton HA administrateur.** Inchangé par rapport à l'original : utiliser un utilisateur HA dédié.
- **Maintenance.** Un seul mainteneur ici aussi, sans la communauté de gazpar2haws pour signaler les changements de GRDF.

## 13. Licence

Vérifié le 2026-11-16 sur les sources (gazpar2haws tag 0.6.1, PyGazpar tag 1.4.0) :

| Projet | Licence | Titulaire | Source |
|---|---|---|---|
| gazpar2haws | MIT | © 2024 Stéphane Senart | `LICENSE`, `pyproject.toml` (`license = "MIT"`) |
| PyGazpar | MIT | © 2025 Stéphane Senart | `LICENSE`, `pyproject.toml` (`license = "MIT"`) |

Aucun autre fichier de licence ni en-tête de copyright dans les deux dépôts. La licence MIT autorise la réutilisation, la modification et la redistribution, à condition de conserver la notice de copyright et le texte de la licence dans les copies ou portions substantielles.

Conséquences pour ce projet :

- `LICENSE` : MIT, © 2026 Julien Séré.
- `NOTICE` : notices de copyright et texte MIT des deux projets d'origine. Une réécriture en Rust n'est pas une copie au sens strict, mais elle est faite en lisant leur code et reprendra leurs jeux d'essai : conserver les notices est la lecture prudente.
- `README.md` : remerciements à Stéphane Senart et aux contributeurs.
- Les fichiers repris tels quels (jeux d'essai) vont dans `tests/fixtures/pygazpar/` avec un `README` indiquant leur origine.
