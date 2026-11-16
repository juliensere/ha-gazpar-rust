# Jeux d'essai repris de PyGazpar

Origine : [PyGazpar](https://github.com/ssenart/PyGazpar) 1.4.0, © 2025 Stéphane Senart, licence MIT (texte et notice dans [`NOTICE`](../../../NOTICE) à la racine du dépôt).

## `donnees_informatives_extrait.json`

Extrait de `tests/resources/donnees_informatives.json` de PyGazpar 1.4.0 (SHA-256 du fichier complet : `35c186ac7ee7d0cfdc1851b2d7ed567fee89934bb29268bba4d91e8485a234ac`, 1 096 relevés du 2019-11-30 au 2022-11-29).

Extraction faite :

- relevés dont `journeeGaziere` va du **2021-07-26 au 2021-08-22** inclus (28 relevés, 4 semaines), recopiés sans modification de valeur ;
- PCE d'origine (`12345678901234`, déjà factice) remplacé par la valeur factice `98765432109876`, dans la clé et dans `idPce` ;
- remise en forme identique à l'original (indentation de 4 espaces, clés triées, `é` écrit `é`).

Ce que contient la fenêtre :

- un changement de mois (juillet → août) ;
- un jour sans donnée : 2021-08-10 (`qualificationReleve` « Absence de Données », valeurs à `null`) ;
- le 2021-08-11, `indexDebut` à `null` mais volume et énergie présents (0) ;
- le 2021-08-08, une énergie (3 kWh) avec un volume nul, telle que publiée.

Le fichier complet ne contient **aucune** valeur décimale de volume ni d'énergie (seul `coeffConversion` est décimal) : les décimales sont couvertes par le fichier suivant.

## `journee_gaziere_decimales.json`

Reprise du relevé écrit en dur dans le test `test_informative_readings_use_gas_day` de `tests/test_jsonparser.py` (PyGazpar 1.4.0), seul relevé informatif à valeurs décimales des tests de PyGazpar (0,28 m³, 3,16 kWh). Seul le PCE est remplacé par `98765432109876`.

## `okta_identify_response.json`

Copie sans modification de `tests/resources/okta_identify_response.json` de PyGazpar 1.4.0 : vraie réponse de l'étape `identify` d'Okta pour un compte GRDF (capturée par PyGazpar le 2026-10-11), où PyGazpar a déjà remplacé le `stateHandle`, l'e-mail et le nonce OAuth par des valeurs factices. Elle n'a **pas** de `stateHandle` à la racine : il n'apparaît que dans les formulaires des étapes proposées. Utilisée par les tests de `src/grdf/auth.rs` et servie, adaptée, par le faux serveur GRDF (`tests/support/fake_grdf.rs`).

## Origine des valeurs attendues

Dans `src/readings.rs` (tests `pygazpar_*`), chaque valeur attendue indique sa source :

- **assertion PyGazpar** : le jour du relevé est `journeeGaziere` (2026-07-23) et non la date de fin (`test_informative_readings_use_gas_day`) ; un relevé quotidien par `journeeGaziere`, jour sans donnée compris (`test_informative_day_without_data_at_the_start_stays_without_data`, et 1 096 relevés pour 1 096 jours du fichier complet dans `test_datasource.py::test_jsonfile_sample`, non rejoué ici faute d'embarquer le fichier complet) ;
- **fichier** : volumes et énergies quotidiens de l'extrait, lus dans le JSON ci-dessus ; ce sont aussi les valeurs que produit `JsonParser` de PyGazpar sur cet extrait (relevés « Mesuré » recopiés tels quels, jour du 2021-08-10 sans valeur).

## Écart assumé avec PyGazpar

PyGazpar reconstruit un jour sans donnée à partir des index encadrants quand ils se suivent (`test_informative_day_without_data_is_rebuilt_from_the_indexes`). ha-gazpar-rust ne le fait pas : le jour reste sans valeur et n'est pas importé, puis il est rattrapé quand GRDF publie la valeur (fenêtre de recouvrement, PLAN.md §3). Sur l'extrait ci-dessus, le résultat est le même que PyGazpar, car l'index de début du 2021-08-11 manque et PyGazpar ne peut pas reconstruire le 2021-08-10.
