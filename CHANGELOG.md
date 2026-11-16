# Notes de version

Toutes les évolutions notables de ha-gazpar-rust sont consignées ici, la version la plus récente en haut.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/). Les numéros de version sont de la forme `AAAA.M.N` : `AAAA.M` est la version de Home Assistant contre laquelle la version a été validée, `N` le rang de la publication dans cette lignée.

## Non publié

### Nouveautés

- Plan de conception du projet ([PLAN.md](PLAN.md)).
- Licence MIT, notices des projets d'origine ([NOTICE](NOTICE)) et remerciements.
- Commande `ha-gazpar <commande> [options]` avec les commandes `run`, `once`, `check`, `reset`, plus `--help` (seul ou après une commande) et `--version` ; codes de sortie 0 (succès), 2 (configuration ou ligne de commande incorrecte, ou `reset` sans `--yes`), 1 (autre erreur). Seule `check` est opérationnelle pour l'instant.
- Options après la commande, `--option valeur` ou `--option=valeur` : `--config <chemin>`, `--yes` (avec `reset` seulement) et un paramètre par option de configuration, au nom déduit de la variable (`GRDF_PCE` → `--grdf-pce`, `GRDF_PASSWORD_FILE` → `--grdf-password-file`). Option répétée, paramètre vide, option inconnue (avec suggestion) refusés ; `--grdf-password` et `--ha-token` refusés avec un message qui renvoie vers `--grdf-password-file` et `--ha-token-file`. Les erreurs ne recopient jamais une valeur.
- `ha-gazpar reset` sans `--yes` affiche les statistiques qui seraient effacées et sort avec le code 2, sans rien contacter.
- `ha-gazpar check` valide la configuration, liste d'un coup tous les problèmes et affiche pour chaque option la valeur retenue et sa provenance (défaut, fichier, environnement, paramètre, fichier lu via une variante `_FILE`), secrets masqués.
- Configuration à trois sources, par ordre de priorité : paramètres de la commande, variables d'environnement (`GRDF_*`, `HA_URL`, `HA_TOKEN`, `GAZPAR_*`), fichier `NOM=valeur` désigné par `--config` ou `GAZPAR_CONFIG`, puis valeurs par défaut. Format exact du fichier ([manuel §4.4](docs/MANUEL.md#44-fichier-de-configuration)) : BOM et fins de ligne Windows acceptés, commentaires sur une ligne à part seulement, guillemets appariés retirés, ni échappement ni substitution, `export`, clé inconnue (avec suggestion) ou répétée refusés ; les erreurs citent la ligne et le nom, jamais la valeur. Variantes `_FILE` (chemin absolu) pour les secrets Docker ou systemd : une option et sa variante forment une seule option, la source la plus forte l'emporte entièrement. Exemple dans [.env.example](.env.example).
- Avertissement pour une variable d'environnement inconnue commençant par `GAZPAR_` ou `GRDF_` (faute de frappe probable) et pour un fichier de configuration lisible par le groupe ou les autres utilisateurs.
- Pas d'option de fuseau horaire (celui de Home Assistant sera lu) ni de type de relevé (seuls les relevés informatifs existent) : `GAZPAR_TIMEZONE` et `GAZPAR_CONSUMPTION_TYPE` n'existent pas.
- Préfixe des statistiques réglable (`GAZPAR_STATISTIC_PREFIX`, `gazpar` par défaut ; `sensor.<nom>` pour reprendre des statistiques d'entité existantes).
- `HA_URL` de la forme `ws://hôte:port` : le chemin `/api/websocket` est ajouté s'il manque ; `wss://` refusé avec un message explicite (seul `ws://` est pris en charge).
- Journal horodaté sur la sortie d'erreur, niveaux `info` et `debug` (`GAZPAR_LOG_LEVEL`).
- Harnais de développement (`make check`, `test`, `lint`, `deny`, `crates`, `deps`, `cover`, `release`…), politique de dépendances (`deny.toml`) et inventaire des dépendances ([DEPENDENCIES.md](DEPENDENCIES.md)) : 3 dépendances directes, 29 crates, plafond 32.
- Intégration continue GitHub Actions : `make check` à chaque push et pull request, compilation statique x86_64 avec taille du binaire, vérification hebdomadaire des avis de sécurité ; Dependabot limité aux mises à jour de sécurité.
- Client WebSocket minimal pour Home Assistant (RFC 6455, bibliothèque standard uniquement) : poignée de main vérifiée, messages fragmentés, réponse aux `ping`, fermeture propre, messages reçus plafonnés à 16 Mio, délais de connexion, lecture et écriture.
- Client des statistiques long terme de Home Assistant : authentification par jeton (jamais journalisé), version de HA annoncée, lecture des points quotidiens, import par lots de 500 points avec minuit dans le fuseau configuré (heure d'été comprise), effacement.
- Lecture des relevés informatifs de GRDF : un relevé par jour gazier, valeurs exactes au millième, jour sans donnée conservé sans valeur (jamais 0), relevé invalide ignoré avec un avertissement. Jeux d'essai repris de PyGazpar.
- Client HTTP vers GRDF : bocal à cookies maison (domaine, chemin, expiration, `Secure`), redirections suivies à la main avec cookies captés à chaque saut, délais de 10 s (connexion) et 60 s (lecture), taille des réponses bornée ; aucune adresse complète, aucun cookie dans les journaux et les erreurs.
- Client Mon Espace GRDF : connexion Okta comme PyGazpar (une seule tentative, motif du refus sans aucun secret), vérification que le PCE appartient au compte, relevés demandés par fenêtres d'au plus 365 jours pour obtenir tout l'historique, reprises sur 429 (`Retry-After` plafonné à 60 s), page d'erreur ou coupure réseau (3, 6, 12 s), une seule reconnexion si la session expire.
- Faux serveur GRDF pour les tests (flux de connexion, liste des PCE, relevés, pannes pilotables), sans aucun accès au vrai GRDF.
- `make ha-up` / `make ha-down` : Home Assistant jetable en conteneur (image `ghcr.io/home-assistant/home-assistant`, version de référence 2026.9 ou `HA_TAG=stable`) sur `127.0.0.1:8124`, fuseau `Europe/Paris`, utilisateur et jeton de test créés au démarrage par l'API d'onboarding (aucun secret commité), jeton renouvelé à chaque `make ha-up`.
- `make test-ha` : essais contre ce Home Assistant jetable (import et relecture autour des changements d'heure, statistique externe `gazpar:` et d'entité `sensor.` en source `recorder`, remplacement d'un point, effacement, `kWh` et `m³`, version de HA, regroupement par jour dans le fuseau de HA).
- Intégration continue : essai réel contre Home Assistant 2026.9 à chaque push, et chaque semaine contre `home-assistant:stable` avec ouverture d'un ticket (sans doublon) en cas d'échec.
