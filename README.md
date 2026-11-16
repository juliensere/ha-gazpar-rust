# ha-gazpar-rust

Importe la consommation de gaz d'un compteur Gazpar (GRDF) dans les statistiques long terme de Home Assistant, pour le tableau de bord Énergie.

C'est un petit programme autonome écrit en Rust, à faire tourner en conteneur à côté de Home Assistant : un seul binaire statique, trois dépendances directes, aucune télémétrie.

> **État : première version en préparation (2026.9.0), pas encore publiée.** Le programme est complet et testé contre de faux serveurs GRDF et Home Assistant, et contre un vrai Home Assistant en conteneur ; il n'a pas encore été essayé sur un vrai compte GRDF.

## Démarrage rapide

```sh
# ha-gazpar/ha-gazpar.conf (chmod 600)
GRDF_USERNAME=prenom.nom@example.org
GRDF_PASSWORD=votre-mot-de-passe-grdf
GRDF_PCE=01234567890123
HA_URL=ws://192.168.1.10:8123
HA_TOKEN=eyJhbGciOi...
```

```sh
docker compose run --rm ha-gazpar check   # vérifie tout sans rien écrire
docker compose up -d ha-gazpar            # un relevé toutes les 8 heures
```

Le service Docker Compose complet, la configuration par paramètres, variables d'environnement ou fichier, et le dépannage sont dans le **[manuel d'utilisation](docs/MANUEL.md)**.

## Principe

1. Le programme lit dans Home Assistant les consommations déjà importées.
2. Il se connecte à Mon Espace GRDF et récupère les relevés quotidiens manquants, plus une fenêtre de rattrapage de 14 jours pour les valeurs que GRDF publie en retard.
3. Il importe les jours qui ont changé dans Home Assistant par l'API WebSocket, sous forme de statistiques `gazpar:<pce>_energy` (kWh) et `gazpar:<pce>_volume` (m³).
4. Il recommence toutes les 8 heures.

Home Assistant ne chargeant que des intégrations Python, ce n'est pas une intégration au sens strict mais un service externe, comme ha-linky pour l'électricité.

## En chiffres

Mesures du 2026-11-16, comparées à gazpar2haws 0.6.1 :

| | gazpar2haws 0.6.1 | ha-gazpar-rust |
|---|---|---|
| Image Docker | 241 Mo | 1,2 Mo compressée (amd64, `FROM scratch`) |
| Binaire | interpréteur Python et 21 paquets | 2,4 Mo, statique |
| Dépendances | 21 paquets Python + image Debian | 3 crates directes, 29 au total, plafond vérifié en CI |
| Mémoire | interpréteur, numpy, pydantic | ≈ 4 Mo résidents au maximum (mesuré sans TLS, contre les faux serveurs) |
| Code | ≈ 6 100 lignes Python | ≈ 5 200 lignes Rust hors tests |
| Tests | — | 457 tests, 98 % des lignes couvertes |

Le code n'est pas plus court que l'original : ce qui aurait été des dépendances (client WebSocket, cookies, analyse d'adresses) est écrit à la main, en modules courts et testés, pour que la maintenance se limite autant que possible aux correctifs de sécurité.

## Choix de conception

- **Prudent avec le compte GRDF** : une connexion par relevé, jamais de boucle en cas d'erreur, plus aucune tentative si GRDF refuse les identifiants.
- **Discret avec les secrets** : mot de passe, jeton, e-mail et cookies ne sont jamais écrits dans le journal ni dans un message d'erreur.
- **Rattrapage** : un jour que GRDF publie en retard ou corrige est réimporté, au lieu de rester à zéro.
- **Peu de dépendances** : `ureq` (HTTPS vers GRDF), `serde_json`, `jiff` ; voir [DEPENDENCIES.md](DEPENDENCIES.md).

Le plan de conception et l'architecture sont décrits dans [PLAN.md](PLAN.md).

## Développement

```sh
make check     # tout ce que fait la CI : format, clippy, tests, cargo deny, plafond de crates
make cover     # couverture des tests (cargo-llvm-cov)
make dev       # check puis deux relevés contre de faux serveurs GRDF et Home Assistant
make test-ha   # essais contre un vrai Home Assistant jetable en conteneur (port 8124)
make image     # image Docker pour l'architecture locale
make help      # toutes les cibles
```

Aucun test automatique ne contacte le vrai GRDF.

Versions numérotées `AAAA.M.N`, calées sur la version de Home Assistant contre laquelle elles sont validées ; nouveautés dans [CHANGELOG.md](CHANGELOG.md).

## Remerciements

Ce projet n'existerait pas sans le travail de **Stéphane Senart** ([@ssenart](https://github.com/ssenart)), auteur de :

- [gazpar2haws](https://github.com/ssenart/gazpar2haws), la passerelle Gazpar vers Home Assistant dont ce projet reprend le principe ;
- [PyGazpar](https://github.com/ssenart/PyGazpar), la bibliothèque qui a défriché la connexion à Mon Espace GRDF et la lecture de son API.

ha-gazpar-rust est une réécriture en Rust fondée sur l'étude de ces deux projets : le flux de connexion à GRDF, le format des relevés et la manière d'importer des statistiques dans Home Assistant viennent de là, ainsi que des jeux d'essai. Merci à lui pour ces années de maintenance, souvent à contre-courant des changements de GRDF, et merci aux contributeurs des deux dépôts.

Si vous cherchez une solution éprouvée, complète (tarification, module complémentaire Home Assistant, source Excel) et suivie par une communauté, utilisez gazpar2haws.

## Licence

[MIT](LICENSE). gazpar2haws et PyGazpar sont eux aussi sous licence MIT ; leurs notices de copyright sont reproduites dans [NOTICE](NOTICE).

Ce projet n'est affilié ni à GRDF ni à Home Assistant.
