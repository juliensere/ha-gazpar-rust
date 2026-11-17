# Manuel d'utilisation de ha-gazpar

`ha-gazpar` importe la consommation de gaz de votre compteur Gazpar (GRDF) dans les statistiques long terme de Home Assistant, pour l'afficher dans le tableau de bord Énergie.

C'est un petit programme autonome, à faire tourner à côté de Home Assistant, de préférence en conteneur Docker. Home Assistant n'a rien à installer : `ha-gazpar` lui envoie les données par son API WebSocket.

Sommaire :

1. [Fonctionnement](#1-fonctionnement)
2. [Prérequis](#2-prérequis)
3. [Installation et premier lancement](#3-installation-et-premier-lancement)
4. [Configuration](#4-configuration)
5. [Commandes](#5-commandes)
6. [Tableau de bord Énergie](#6-tableau-de-bord-énergie)
7. [Journal et codes de sortie](#7-journal-et-codes-de-sortie)
8. [Dépannage](#8-dépannage)
9. [Sécurité et protection du compte GRDF](#9-sécurité-et-protection-du-compte-grdf)
10. [Limites connues](#10-limites-connues)
11. [Mises à jour et versions](#11-mises-à-jour-et-versions)
12. [Reprendre l'historique de gazpar2haws](#12-reprendre-lhistorique-de-gazpar2haws)
13. [Désinstallation](#13-désinstallation)

## 1. Fonctionnement

À chaque relevé, `ha-gazpar` :

1. demande à Home Assistant les consommations déjà importées ;
2. se connecte à Mon Espace GRDF et récupère les relevés quotidiens manquants, plus les derniers jours déjà connus (la « fenêtre de rattrapage », 14 jours par défaut), pour prendre en compte les valeurs que GRDF publie ou corrige en retard ;
3. importe ces jours dans Home Assistant, sous forme de deux statistiques : l'énergie en kWh et le volume en m³.

En mode démon (`run`), il recommence toutes les 8 heures par défaut.

À savoir :

- **GRDF publie les données avec 2 à 5 jours de retard**, parfois plus. Les derniers jours vides dans le tableau de bord sont normaux.
- Un jour pour lequel GRDF n'a pas encore de valeur n'est **pas** importé (et surtout pas à zéro) : il le sera dès que GRDF le publiera, s'il est encore dans la fenêtre de rattrapage.
- Au premier lancement, `ha-gazpar` demande jusqu'à **3 ans** d'historique (ce que GRDF conserve réellement peut être moins).
- Les jours sont découpés selon le **fuseau horaire de Home Assistant**, lu automatiquement : il n'y a rien à régler.
- `ha-gazpar` ne contacte que des adresses `grdf.fr` (le site Mon Espace GRDF et son service de connexion) et votre Home Assistant. Aucune autre adresse, aucune télémétrie.

## 2. Prérequis

### Compte Mon Espace GRDF

- Un compte sur [monespace.grdf.fr](https://monespace.grdf.fr), avec votre compteur rattaché et la consommation quotidienne visible sur le site. Vérifiez-le dans un navigateur avant de configurer `ha-gazpar`.
- Ce sont les identifiants **de GRDF**, pas ceux de votre fournisseur de gaz (Engie, TotalEnergies…), ni ceux d'un compte professionnel « Gaz Passerelle ».
- Votre **PCE** (point de comptage et d'estimation) : 14 chiffres, indiqué sur votre facture et dans Mon Espace GRDF. Gardez le zéro initial s'il y en a un.

### Home Assistant

- Home Assistant 2026.9 ou plus récent. Une version plus ancienne n'est pas refusée, mais `ha-gazpar` affiche un avertissement au démarrage.
- Home Assistant joignable en `ws://` (sans chiffrement) depuis la machine qui fait tourner `ha-gazpar` : utilisez son adresse sur le réseau local (port 8123), pas une adresse publique en HTTPS derrière un mandataire inverse.
- Un **jeton d'accès longue durée** d'un utilisateur **administrateur** (l'import de statistiques l'exige). Recommandé : créez un utilisateur dédié, nommé par exemple « ha-gazpar », puis, connecté avec lui : Profil → Sécurité → Jetons d'accès longue durée → Créer un jeton. Vous pourrez révoquer ce seul jeton sans toucher à votre compte.

## 3. Installation et premier lancement

### Docker Compose (recommandé)

Créez un dossier `ha-gazpar/` à côté de votre `compose.yml`, avec le fichier de configuration `ha-gazpar/ha-gazpar.conf` :

```sh
GRDF_USERNAME=prenom.nom@example.org
GRDF_PASSWORD=votre-mot-de-passe-grdf
GRDF_PCE=01234567890123
HA_URL=ws://192.168.1.10:8123
HA_TOKEN=eyJhbGciOi...
```

Rendez-le lisible par le seul utilisateur du conteneur :

```sh
chmod 600 ha-gazpar/ha-gazpar.conf
sudo chown 1000:1000 ha-gazpar/ha-gazpar.conf
```

Ajoutez le service :

```yaml
services:
  ha-gazpar:
    image: ghcr.io/juliensere/ha-gazpar-rust:2026.9   # dernière version validée avec HA 2026.9
    container_name: ha-gazpar
    restart: unless-stopped
    init: true               # arrêt immédiat avec docker stop
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    user: "1000:1000"
    environment:
      GAZPAR_CONFIG: /config/ha-gazpar.conf
    volumes:
      - ./ha-gazpar:/config:ro
```

L'image lance `ha-gazpar run` par défaut. Comme le fichier est désigné par `GAZPAR_CONFIG`, les autres commandes s'utilisent sans le répéter.

Premier lancement :

1. Vérifiez tout, sans rien écrire :

   ```sh
   docker compose run --rm ha-gazpar check
   ```

   `check` affiche la configuration retenue (sans les secrets), la version et le fuseau de Home Assistant, confirme la connexion à GRDF, puis montre les derniers jours lus chez GRDF.
2. Démarrez le service : `docker compose up -d ha-gazpar`.
3. Suivez le premier relevé, qui importe l'historique : `docker logs -f ha-gazpar`.
4. Branchez la statistique dans le tableau de bord Énergie ([§6](#6-tableau-de-bord-énergie)).

L'image ne contient que le programme : pas de shell, pas de système d'exploitation. Elle existe pour `linux/amd64` et `linux/arm64` (Raspberry Pi en 64 bits).

**Politique de redémarrage.** `ha-gazpar run` ne s'arrête pas sur une erreur passagère (GRDF ou Home Assistant indisponible) : il la journalise et réessaie au relevé suivant. Il ne s'arrête que sur une erreur de configuration, détectée avant toute connexion à GRDF. `unless-stopped` le relance donc sans risque pour votre compte, y compris après un redémarrage de la machine.

### Binaire seul (systemd)

Les binaires statiques sont joints à chaque version sur la page des versions GitHub.

```ini
# /etc/systemd/system/ha-gazpar.service
[Unit]
Description=Import de la consommation Gazpar dans Home Assistant
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=/usr/local/bin/ha-gazpar run --config ${CREDENTIALS_DIRECTORY}/conf
LoadCredential=conf:/etc/ha-gazpar/ha-gazpar.conf
DynamicUser=yes
Restart=on-failure
RestartPreventExitStatus=2

[Install]
WantedBy=multi-user.target
```

`LoadCredential=` rend le fichier de configuration (propriété de root, mode 600) lisible par le seul service. `RestartPreventExitStatus=2` évite de relancer en boucle sur une erreur de configuration.

## 4. Configuration

### 4.1 Trois façons de configurer

Chaque option peut être donnée de trois façons, combinables :

| Source | Forme | Exemple |
|---|---|---|
| Fichier de configuration | `NOM=valeur` | `GRDF_PCE=01234567890123` |
| Variable d'environnement | `NOM=valeur` | `GRDF_PCE=01234567890123` |
| Paramètre de la commande | `--nom valeur` | `--grdf-pce 01234567890123` |

Le fichier utilise exactement les noms des variables d'environnement. Le nom du paramètre s'en déduit : minuscules, `_` remplacé par `-`.

**Priorité**, de la plus forte à la plus faible : paramètre de la commande, puis variable d'environnement, puis fichier, puis valeur par défaut. Exemple : un fichier qui contient `GAZPAR_LOG_LEVEL=info`, lancé avec `--gazpar-log-level debug`, donne `debug`.

`check` affiche, pour chaque option, la valeur retenue et sa source, par exemple :

```
GRDF_PCE            01234567890123   (paramètre)
HA_TOKEN            ***              (fichier /run/secrets/ha_token, via HA_TOKEN_FILE)
GAZPAR_LOG_LEVEL    info             (défaut)
```

### 4.2 Liste des options

**Obligatoires**

| Variable / clé du fichier | Paramètre | Rôle |
|---|---|---|
| `GRDF_USERNAME` | `--grdf-username` | e-mail du compte Mon Espace GRDF |
| `GRDF_PASSWORD` | *aucun, voir plus bas* | mot de passe Mon Espace GRDF |
| `GRDF_PCE` | `--grdf-pce` | PCE du compteur, 14 chiffres |
| `HA_URL` | `--ha-url` | adresse de Home Assistant, `ws://hôte:port` ; le chemin `/api/websocket` est ajouté s'il manque |
| `HA_TOKEN` | *aucun, voir plus bas* | jeton d'accès longue durée |

**Facultatives**

| Variable / clé du fichier | Paramètre | Défaut | Rôle |
|---|---|---|---|
| `GAZPAR_SCAN_INTERVAL` | `--gazpar-scan-interval` | `480` | minutes entre deux relevés en mode `run`, 60 au minimum |
| `GAZPAR_HISTORY_DAYS` | `--gazpar-history-days` | `1095` | jours d'historique demandés au premier import (1 à 3650) |
| `GAZPAR_OVERLAP_DAYS` | `--gazpar-overlap-days` | `14` | jours déjà importés relus à chaque relevé (1 à 60) |
| `GAZPAR_STATISTIC_PREFIX` | `--gazpar-statistic-prefix` | `gazpar` | préfixe des statistiques ([§6](#6-tableau-de-bord-énergie), [§12](#12-reprendre-lhistorique-de-gazpar2haws)) |
| `GAZPAR_LOG_LEVEL` | `--gazpar-log-level` | `info` | `info` ou `debug` |

**Valeurs lues dans un fichier à part.** `GRDF_USERNAME`, `GRDF_PASSWORD`, `GRDF_PCE` et `HA_TOKEN` existent aussi en variante `…_FILE` (`GRDF_PASSWORD_FILE`, `--grdf-password-file`, etc.) : la valeur est alors le **chemin absolu** d'un fichier qui contient la valeur, sans rien d'autre ; les espaces et sauts de ligne finaux sont ignorés. C'est la forme à utiliser avec les secrets Docker ou systemd, et pour un mot de passe qu'il serait malcommode d'écrire dans le fichier de configuration.

**Pas de mot de passe ni de jeton sur la ligne de commande.** `--grdf-password` et `--ha-token` sont refusés avec un message explicite : un paramètre est visible de tous les utilisateurs de la machine (`ps`, `docker inspect`) et reste dans l'historique du shell. Utilisez le fichier de configuration, une variable d'environnement, ou `--grdf-password-file` / `--ha-token-file`. L'e-mail, lui, est accepté en paramètre ; il n'est jamais écrit dans le journal.

### 4.3 Règles de combinaison

- Une option et sa variante `_FILE` (par exemple `GRDF_PASSWORD` et `GRDF_PASSWORD_FILE`) sont **une seule option** : la source la plus forte qui donne l'une des deux formes l'emporte entièrement. Les deux formes dans la même source sont une erreur. Exemple : `GRDF_PASSWORD_FILE` en variable d'environnement et `GRDF_PASSWORD` dans le fichier → la variable gagne, le mot de passe est lu dans le fichier désigné.
- Une valeur **vide** dans le fichier ou dans l'environnement compte comme absente. Un paramètre vide (`--grdf-pce=`) est une erreur.
- Une même option donnée deux fois en paramètre, ou deux fois dans le fichier, est une erreur.
- Une variable d'environnement inconnue qui commence par `GAZPAR_` ou `GRDF_` provoque un avertissement (faute de frappe probable).

### 4.4 Fichier de configuration

Le fichier est désigné par `--config <chemin>` ou par la variable `GAZPAR_CONFIG` ; si les deux sont présents, `--config` l'emporte. Il n'y a pas de fichier lu par défaut. Un fichier désigné mais introuvable ou illisible est une erreur.

**Ce n'est pas un fichier `.env`** : ne le passez ni en `env_file:` de Docker Compose ni en `--env-file` de Docker, qui interprètent différemment guillemets et `$`. Montez-le et désignez-le avec `GAZPAR_CONFIG`.

Format, une option par ligne :

```sh
# Commentaire : seulement sur une ligne à part.
GRDF_USERNAME=prenom.nom@example.org
GRDF_PASSWORD="mot de passe avec # et espaces"
GRDF_PCE=01234567890123
```

Règles exactes :

- encodage UTF-8 ; une marque d'ordre d'octets (BOM) en tête est ignorée ; fins de ligne Unix ou Windows acceptées ;
- les lignes vides ou ne contenant que des espaces sont ignorées ; une ligne dont le premier caractère non blanc est `#` est un commentaire ;
- **pas de commentaire en fin de ligne** : `CLE=valeur # note` donne la valeur `valeur # note` ;
- chaque autre ligne est `NOM=valeur`, découpée au **premier** `=` ; les espaces autour du nom et de la valeur sont retirés ; une ligne sans `=` est une erreur ;
- les noms sont ceux des variables d'environnement, en majuscules ; un nom inconnu est une erreur, avec une suggestion si une option proche existe ; `export NOM=…` est refusé avec un message ;
- si la valeur commence et finit par le même guillemet (`"…"` ou `'…'`), ces deux guillemets sont retirés et le contenu est pris tel quel ; un guillemet ouvrant non refermé est une erreur ; il n'y a ni séquence d'échappement ni substitution (`$HOME` reste tel quel) ;
- `GAZPAR_CONFIG` n'est pas accepté dans le fichier.

Les messages d'erreur citent le numéro de ligne et le nom de l'option, **jamais la valeur ni la ligne**, pour qu'un mot de passe mal placé ne se retrouve pas dans le journal.

Le fichier contient des secrets : `ha-gazpar` affiche un avertissement s'il est lisible par le groupe ou par les autres utilisateurs. Ce n'est qu'un avertissement, car certains systèmes de fichiers (partages Windows, Dropbox) affichent tous les fichiers comme lisibles par tous.

### 4.5 Exemples

Tout en variables d'environnement, avec les secrets Docker :

```yaml
services:
  ha-gazpar:
    # … mêmes réglages qu'au §3, sans GAZPAR_CONFIG ni volume …
    environment:
      GRDF_USERNAME: prenom.nom@example.org
      GRDF_PCE: "01234567890123"
      HA_URL: ws://192.168.1.10:8123
      GRDF_PASSWORD_FILE: /run/secrets/grdf_password
      HA_TOKEN_FILE: /run/secrets/ha_token
    secrets: [grdf_password, ha_token]

secrets:
  grdf_password:
    file: ./ha-gazpar/grdf_password   # lisible par l'utilisateur 1000
  ha_token:
    file: ./ha-gazpar/ha_token
```

Fichier commun, réglage ponctuel en paramètre :

```sh
ha-gazpar once --config /etc/ha-gazpar/ha-gazpar.conf --gazpar-log-level debug
```

## 5. Commandes

```
ha-gazpar <commande> [options]
```

| Commande | Effet |
|---|---|
| `run` | mode démon : un relevé tout de suite, puis toutes les `GAZPAR_SCAN_INTERVAL` minutes |
| `once` | un seul relevé, puis sortie |
| `check` | vérifie la configuration, la connexion à Home Assistant et la connexion à GRDF, affiche les derniers jours lus chez GRDF et ce qui serait importé ; n'écrit rien |
| `reset` | efface dans Home Assistant les deux statistiques de `ha-gazpar` ; exige `--yes` |

Options de la commande elle-même (ce ne sont pas des options de configuration) :

| Option | Effet |
|---|---|
| `--config <chemin>` | fichier de configuration ([§4.4](#44-fichier-de-configuration)) |
| `--yes` | confirme `reset` ; refusé avec les autres commandes |
| `--help` | aide ; accepté seul ou après une commande |
| `--version` | version du programme et version de Home Assistant de référence |

Les options s'écrivent **après** la commande, sous la forme `--option valeur` ou `--option=valeur`.

`reset` sans `--yes` ne pose pas de question : il affiche les statistiques qui seraient effacées et sort avec le code 2.

**Une seule instance à la fois.** Arrêtez le service avant de lancer `once` ou `reset` à la main : deux instances qui écrivent en même temps se gênent, et chacune se connecte à GRDF. `check` peut être lancé pendant que le service tourne, mais compte comme une connexion à GRDF.

## 6. Tableau de bord Énergie

`ha-gazpar` crée deux statistiques long terme :

| Statistique | Unité | Nom affiché |
|---|---|---|
| `gazpar:<PCE>_energy` | kWh | Gaz (énergie) |
| `gazpar:<PCE>_volume` | m³ | Gaz (volume) |

Ce sont des statistiques « externes » : elles n'ont pas d'entité associée et n'apparaissent donc pas dans la liste des entités. On les voit dans Outils de développement → Statistiques.

Pour les afficher : Paramètres → Tableaux de bord → Énergie → Consommation de gaz → Ajouter une source de gaz → choisissez **Gaz (énergie)**. Préférez l'énergie au volume : le kWh tient déjà compte du coefficient de conversion de GRDF.

Le suivi du coût n'est pas encore pris en charge ([§10](#10-limites-connues)).

## 7. Journal et codes de sortie

Le journal est écrit sur la sortie d'erreur (`docker logs`), une ligne par événement, horodatée en UTC :

```
2026-11-16T07:00:02Z INFO relevé : 3 jours importés, dernier jour connu 2026-11-13
2026-11-16T15:00:01Z ATTENTION GRDF, relevés du 2026-11-02 au 2026-11-15 : tentative 1/4 échouée, pas de réponse en 60 s (monespace.grdf.fr:443) ; nouvel essai dans 3 s
2026-11-16T15:01:02Z ATTENTION GRDF, relevés du 2026-11-02 au 2026-11-15 : pas de réponse en 60 s (monespace.grdf.fr:443) ; abandon après 4 tentatives ; panne passagère, nouvel essai au prochain relevé, dans 480 min
```

Une ligne d'erreur se lit de gauche à droite :

```
<service>, <opération> : <cause> (<hôte:port>) ; <que faire>
```

- le **service** et l'**opération** : `GRDF, connexion 3/5 (identification Okta)` (les cinq étapes de la connexion vont de 1/5, page d'accueil, à 5/5, fin de connexion), `GRDF, liste des PCE`, `GRDF, relevés du … au …`, `Home Assistant (192.168.0.10:8123), connexion TCP`, `… poignée de main WebSocket`, `… authentification`, `… import de 9 points dans gazpar:…_energy`… ;
- la **cause** : statut HTTP, délai dépassé, connexion refusée, motif donné par Okta (texte entre guillemets, code entre crochets) ou par Home Assistant ; avec l'hôte et le port qui ont répondu, jamais l'adresse complète ;
- **que faire**, quand vous pouvez agir : vérifier `HA_URL`, recréer le jeton, vérifier les identifiants GRDF ou `GRDF_PCE`. Une panne passagère (réseau, service indisponible) le dit : `panne passagère, nouvel essai au prochain relevé, dans 480 min`.

Les appels à GRDF en échec passager sont repris aussitôt, 4 tentatives au plus, chacune journalisée en `ATTENTION` avec son numéro, son motif et le délai avant la suivante (3, 6, 12 s, ou le délai `Retry-After` demandé par GRDF, 60 s au plus).

Niveaux : `ERREUR`, `ATTENTION`, `INFO`, et `DEBUG` si `GAZPAR_LOG_LEVEL=debug`. **Aucun niveau** n'écrit le mot de passe, le jeton, les cookies ni l'e-mail du compte.

| Code | Signification |
|---|---|
| 0 | succès |
| 1 | erreur d'exécution : GRDF ou Home Assistant injoignable ou en refus. Avec `run`, le programme ne sort pas pour ces erreurs. |
| 2 | configuration ou ligne de commande incorrecte, ou `reset` sans `--yes` : corriger avant de relancer |

## 8. Dépannage

| Symptôme | Cause probable | Que faire |
|---|---|---|
| `identifiants refusés par GRDF` | mauvais identifiants, ou identifiants du fournisseur au lieu de GRDF | vérifiez-les sur monespace.grdf.fr, corrigez la configuration, puis **redémarrez** le service (voir [§9](#9-sécurité-et-protection-du-compte-grdf)) |
| `GRDF, connexion k/5 (…)` suivi d'un autre motif (`service de connexion indisponible`, `stateToken introuvable`, `réponse inattendue d'Okta`…) | le parcours de connexion de GRDF a changé ou est en panne ; ce n'est pas un refus des identifiants | réessayé automatiquement au relevé suivant ; si cela dure, consultez les tickets du projet en citant la ligne (elle ne contient aucun secret) |
| `connexion refusée`, `pas de réponse en 60 s`, `hôte introuvable par le DNS` après `GRDF, …` | réseau ou site GRDF indisponible | réessayé automatiquement au relevé suivant |
| `le PCE … n'est pas rattaché à ce compte GRDF` | PCE mal recopié, ou compteur non rattaché au compte | comparez avec Mon Espace GRDF et corrigez `GRDF_PCE` |
| `Home Assistant (…), authentification : jeton refusé` | jeton invalide ou révoqué | créez un nouveau jeton d'accès longue durée avec un utilisateur administrateur et mettez-le dans `HA_TOKEN` |
| `… refusée par Home Assistant : … (code unauthorized)` | utilisateur du jeton non administrateur | utilisez le jeton d'un utilisateur administrateur |
| `Home Assistant (…), connexion TCP : connexion refusée` (ou `délai de connexion … dépassé`, `hôte introuvable par le DNS`) | adresse ou port faux, HA arrêté ou en cours de redémarrage | vérifiez `HA_URL` (`ws://`, port 8123 en général) ; réessayé automatiquement au relevé suivant |
| `Home Assistant (…), poignée de main WebSocket : réponse « HTTP/1.1 404 … »` | l'adresse ne mène pas à l'API WebSocket de Home Assistant (chemin, port d'un autre service, mandataire) | vérifiez `HA_URL` : `ws://hôte:8123/api/websocket` |
| `pas de réponse dans le délai imparti` après `Home Assistant (…)` | Home Assistant surchargé ou bloqué | réessayé automatiquement au relevé suivant ; si cela dure, consultez le journal de Home Assistant |
| `HA_URL … adresse invalide` (code 2) | adresse mal écrite | corrigez `HA_URL` : `ws://hôte:port`, sans `?` ni `#` |
| rien dans le tableau de bord | délai de publication de GRDF, ou statistique non choisie | attendez 2 à 5 jours après la mise en service ; vérifiez [§6](#6-tableau-de-bord-énergie) |
| un jour manquant | GRDF n'avait pas la valeur | rattrapé automatiquement s'il est dans la fenêtre de rattrapage ; sinon lancez une fois avec `--gazpar-overlap-days 60` |
| historique faux à refaire | — | arrêtez le service, `docker compose run --rm ha-gazpar reset --yes`, redémarrez le service : le démarrage réimporte l'historique |

En cas de doute, relancez avec `--gazpar-log-level debug` le temps d'un relevé.

GRDF peut modifier son site à tout moment et bloquer la connexion des outils comme celui-ci (ce fut le cas avec un captcha en 2023). Si la connexion échoue alors que vos identifiants fonctionnent dans un navigateur, consultez les tickets du projet.

## 9. Sécurité et protection du compte GRDF

**Secrets.**

- Le mot de passe GRDF, le jeton Home Assistant, les cookies et l'e-mail ne sont jamais écrits dans le journal ni affichés par `check`.
- Préférez le fichier de configuration en mode `600` ou les variantes `…_FILE` aux variables d'environnement, visibles avec `docker inspect`.
- Le jeton Home Assistant donne des droits d'administrateur : utilisez un utilisateur dédié pour pouvoir le révoquer seul.
- La connexion à Home Assistant se fait en `ws://`, donc **en clair** : faites tourner `ha-gazpar` sur la même machine ou le même réseau de confiance que Home Assistant.
- La connexion à GRDF se fait en HTTPS avec vérification des certificats (autorités de certification embarquées dans le programme). Les redirections hors de `grdf.fr` sont refusées.

**Compte GRDF.** Des connexions répétées, surtout en échec, peuvent faire bloquer un compte. `ha-gazpar` les limite ainsi :

- en mode `run`, une connexion par relevé (deux au plus si la session expire en cours de relevé), donc au plus une par heure, même en cas d'erreur : une erreur n'avance jamais le relevé suivant ;
- si GRDF **refuse les identifiants**, `run` cesse toute tentative de connexion jusqu'à son redémarrage : il le signale dans le journal à chaque intervalle, sans sortir (pour ne pas être relancé automatiquement). Corrigez la configuration, puis redémarrez le service ;
- en revanche, chaque **démarrage** de `run`, chaque `once` et chaque `check` est une connexion de plus : `ha-gazpar` ne garde aucun état entre deux lancements. Ne lancez pas `once` par cron plus d'une fois par heure, et n'enchaînez pas les `check`.

## 10. Limites connues

- Pas de suivi du coût dans le tableau de bord Énergie (Home Assistant ne l'applique pas aux statistiques externes ; une statistique de coût calculée par `ha-gazpar` est envisagée).
- Seulement les relevés quotidiens « informatifs » de GRDF ; pas les relevés « publiés » (périodes de facturation).
- Pas de connexion chiffrée `wss://` à Home Assistant.
- Un seul compteur (PCE) par instance : lancez une instance par compteur, avec des préfixes différents.
- Images pour `amd64` et `arm64` seulement (pas de Raspberry Pi en 32 bits).

## 11. Mises à jour et versions

Les versions sont numérotées `AAAA.M.N` : `AAAA.M` est la version de Home Assistant avec laquelle cette version a été validée, `N` le numéro de publication. Exemple : `2026.9.1` est la deuxième publication validée avec Home Assistant 2026.9.

- Une version reste valable avec les versions suivantes de Home Assistant tant que rien ne casse ; chaque semaine, l'intégration continue la teste contre la dernière version stable de Home Assistant.
- Étiquettes de l'image : `2026.9.1` (figée), `2026.9` (dernière publication de cette lignée), `latest`.
- Les préversions (`2026.9.0-alpha.1`, `-beta.K`, `-rc.K`) n'ont que leur étiquette exacte : elles ne remplacent jamais `2026.9` ni `latest`.
- Les nouveautés et corrections de chaque version sont dans [CHANGELOG.md](../CHANGELOG.md) et sur la page des versions GitHub.
- Si une option est renommée ou retirée, l'ancien nom reste accepté avec un avertissement pendant au moins une lignée de versions.

## 12. Reprendre l'historique de gazpar2haws

Si vous utilisiez [gazpar2haws](https://github.com/ssenart/gazpar2haws), ses statistiques s'appellent `sensor.<nom>_energy` et `sensor.<nom>_volume` (par défaut `sensor.gazpar2haws_…`). Pour que `ha-gazpar` les prolonge au lieu d'en créer de nouvelles :

```
GAZPAR_STATISTIC_PREFIX=sensor.gazpar2haws
```

1. Arrêtez gazpar2haws : les deux ne doivent jamais écrire en même temps dans les mêmes statistiques.
2. Pour le premier relevé, élargissez la fenêtre de rattrapage (`GAZPAR_OVERLAP_DAYS=60`) : gazpar2haws importait à zéro les jours que GRDF n'avait pas encore, ce qui sera ainsi corrigé sur les 60 derniers jours. Revenez ensuite à la valeur par défaut.
3. Le nom affiché et l'unité des statistiques sont réécrits par `ha-gazpar`. Les statistiques de coût de gazpar2haws ne sont plus alimentées.

**Attention** : avec ce préfixe, `reset` efface l'historique de gazpar2haws. Ce qui est plus ancien que ce que GRDF fournit encore ne pourra pas être réimporté.

## 13. Désinstallation

1. Si vous voulez effacer les statistiques importées (sinon elles restent dans Home Assistant) : arrêtez le service, puis `docker compose run --rm ha-gazpar reset --yes`.
2. Retirez la source de gaz du tableau de bord Énergie.
3. Supprimez le service et son dossier de configuration, puis révoquez le jeton Home Assistant.
