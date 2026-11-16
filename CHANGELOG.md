# Notes de version

Toutes les évolutions notables de ha-gazpar-rust sont consignées ici, la version la plus récente en haut.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/). Les numéros de version sont de la forme `AAAA.M.N` : `AAAA.M` est la version de Home Assistant contre laquelle la version a été validée, `N` le rang de la publication dans cette lignée.

## Non publié

### Nouveautés

- Plan de conception du projet ([PLAN.md](PLAN.md)).
- Licence MIT, notices des projets d'origine ([NOTICE](NOTICE)) et remerciements.
- Commande `ha-gazpar` avec les sous-commandes `run`, `once`, `check`, `reset`, `--version` et `--help` ; codes de sortie 0 (succès), 2 (configuration ou usage incorrects), 1 (autre erreur). Seule `check` est opérationnelle pour l'instant.
- `ha-gazpar check` valide la configuration, liste d'un coup tous les problèmes et affiche un résumé sans aucun secret.
- Configuration par variables d'environnement (`GRDF_*`, `HA_URL`, `HA_TOKEN`, `GAZPAR_*`, `LOG_LEVEL`), variantes `_FILE` pour les secrets Docker, exemple dans [.env.example](.env.example).
- Préfixe des statistiques réglable (`GAZPAR_STATISTIC_PREFIX`, `gazpar` par défaut ; `sensor.<nom>` pour reprendre des statistiques d'entité existantes).
- `HA_URL` en `wss://` refusé avec un message explicite (seul `ws://` est pris en charge).
- Journal horodaté sur la sortie d'erreur, niveaux `info` et `debug`.
- Harnais de développement (`make check`, `test`, `lint`, `deny`, `crates`, `deps`, `cover`, `release`…), politique de dépendances (`deny.toml`) et inventaire des dépendances ([DEPENDENCIES.md](DEPENDENCIES.md)) : 3 dépendances directes, 29 crates, plafond 32.
- Intégration continue GitHub Actions : `make check` à chaque push et pull request, compilation statique x86_64 avec taille du binaire, vérification hebdomadaire des avis de sécurité ; Dependabot limité aux mises à jour de sécurité.
- Client WebSocket minimal pour Home Assistant (RFC 6455, bibliothèque standard uniquement) : poignée de main vérifiée, messages fragmentés, réponse aux `ping`, fermeture propre, messages reçus plafonnés à 16 Mio, délais de connexion, lecture et écriture.
