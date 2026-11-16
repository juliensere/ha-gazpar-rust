# ha-gazpar-rust

Importe la consommation de gaz d'un compteur Gazpar (GRDF) dans les statistiques long terme de Home Assistant, pour le tableau de bord Énergie.

C'est un petit programme autonome écrit en Rust, pensé pour tourner en conteneur à côté de Home Assistant : un seul binaire statique, une image de quelques mégaoctets, trois dépendances directes.

> **État du projet : en conception.** Rien n'est encore implémenté. Le plan détaillé est dans [PLAN.md](PLAN.md).

## Principe

1. Le programme lit dans Home Assistant la dernière consommation connue.
2. Il se connecte à Mon Espace GRDF et récupère les relevés quotidiens manquants.
3. Il les importe dans Home Assistant par l'API WebSocket, sous forme de statistiques `gazpar:<pce>_energy` (kWh) et `gazpar:<pce>_volume` (m³).
4. Il recommence toutes les 8 heures.

Home Assistant ne chargeant que des intégrations Python, ce n'est pas une intégration au sens strict mais un service externe, comme ha-linky pour l'électricité.

## Objectifs

- **Léger** : image `FROM scratch` de moins de 5 Mo, quelques mégaoctets de mémoire.
- **Peu de dépendances** : trois dépendances directes, listées et justifiées, pour que la maintenance se limite autant que possible à des correctifs de sécurité.
- **Prudent avec GRDF** : jamais de boucle de connexion, même en cas d'erreur.
- **Discret avec les secrets** : identifiants et jeton jamais écrits dans les journaux.

## Remerciements

Ce projet n'existerait pas sans le travail de **Stéphane Senart** ([@ssenart](https://github.com/ssenart)), auteur de :

- [gazpar2haws](https://github.com/ssenart/gazpar2haws), la passerelle Gazpar vers Home Assistant dont ce projet reprend le principe ;
- [PyGazpar](https://github.com/ssenart/PyGazpar), la bibliothèque qui a défriché la connexion à Mon Espace GRDF et la lecture de son API.

ha-gazpar-rust est une réécriture en Rust fondée sur l'étude de ces deux projets : le flux de connexion à GRDF, le format des relevés et la manière d'importer des statistiques dans Home Assistant viennent de là. Merci à lui pour ces années de maintenance, souvent à contre-courant des changements de GRDF, et merci aux contributeurs des deux dépôts.

Si vous cherchez une solution éprouvée, complète (tarification, module complémentaire Home Assistant, source Excel) et suivie par une communauté, utilisez gazpar2haws.

## Licence

[MIT](LICENSE). gazpar2haws et PyGazpar sont eux aussi sous licence MIT ; leurs notices de copyright sont reproduites dans [NOTICE](NOTICE).

Ce projet n'est affilié ni à GRDF ni à Home Assistant.
