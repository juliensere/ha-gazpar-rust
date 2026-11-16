# Dépendances

Budget (PLAN.md §4.3) : **3 dépendances directes**, aucune dépendance de développement. Toute nouvelle dépendance directe doit être justifiée ici (rôle, alternative maison envisagée, nombre de crates tirées) et le plafond `MAX_CRATES` du `Makefile` ne se relève qu'avec cette justification. `make check` échoue si l'arbre d'exécution dépasse le plafond ou si le bloc généré ci-dessous n'est pas à jour (`make deps` le régénère).

## Dépendances directes

Nombres de crates mesurés avec `cargo tree -e normal` pour `aarch64-unknown-linux-musl`, la crate elle-même comprise.

| Crate | Rôle | Features | Crates tirées | Alternative maison envisagée |
|---|---|---|---|---|
| `ureq` 3 | HTTPS vers GRDF (client bloquant), confiné à `grdf/http.rs` | `default-features = false`, `rustls` | 22 | Aucune raisonnable : TLS ne s'écrit pas à la main. `minreq` 3 (`https-rustls`) tire 12 crates, à réévaluer à l'étape 4. L'option `cookies` d'`ureq` (67 crates) est écartée au profit d'un bocal à cookies maison. |
| `serde_json` 1 | Lecture des réponses GRDF et des messages Home Assistant, écriture des commandes | par défaut (`std`), sans `serde` derive | 5 | Un analyseur JSON maison est faisable mais c'est du code sensible aux entrées hostiles ; `serde_json` est la référence. Lecture par `Value` : pas de macro procédurale (`syn`, `quote`) à compiler. |
| `jiff` 1 | Dates civiles, fuseau `Europe/Paris` (minuit local aux changements d'heure), horodatage du journal | `default-features = false`, `std`, `tzdb-bundle-always` | 3 | Calcul maison des règles d'heure d'été : possible pour la France seule, mais fragile. La base de fuseaux est embarquée car l'image `scratch` n'a pas de `/usr/share/zoneinfo`. |

Ce que chaque dépendance tire (hors elle-même) :

- `ureq` : `rustls` (TLS) avec `ring` (cryptographie, seul code C/assembleur compilé), `rustls-webpki`, `rustls-pki-types`, `webpki-roots` (autorités de certification de Mozilla, embarquées), `untrusted`, `subtle`, `zeroize`, `getrandom`, `libc`, `cfg-if`, `once_cell` ; `ureq-proto` avec `http`, `httparse`, `bytes`, `itoa`, `base64`, `percent-encoding`, `utf8-zero` ; `log`.
- `serde_json` : `serde_core`, `itoa` (partagée avec `ureq`), `memchr`, `zmij`.
- `jiff` : `jiff-core`, `jiff-tzdb` (base de fuseaux IANA embarquée).

Total : 22 + 5 + 3 − 1 crate partagée (`itoa`) = **29 crates**, plafond **32** (3 de marge).

## Licences

Vérifiées par `cargo deny check licenses` (voir `deny.toml`) : MIT, Apache-2.0, ISC (`ring`, `rustls-webpki`, `untrusted`), BSD-3-Clause (`subtle`) et CDLA-Permissive-2.0 (`webpki-roots`, licence des données de certificats de Mozilla, permissive). `ring` est sous `Apache-2.0 AND ISC`.

## Doublons

Aucun : chaque crate n'apparaît qu'en une version (`multiple-versions = "deny"`). `cargo tree -d --target all` signale `jiff-core` deux fois : c'est la même version, atteinte aussi par `jiff-static`, une macro procédurale que `jiff` déclare sous `cfg(any())` (condition jamais vraie, pour figer sa version) et qui n'est donc jamais compilée.

## Dépendances de compilation

`ring` compile du C et de l'assembleur avec `cc` (qui tire `shlex` et `find-msvc-tools`). Ce ne sont pas des dépendances d'exécution et elles ne sont pas comptées dans le plafond ; `cargo deny` les vérifie tout de même.

## Arbre d'exécution

<!-- DÉBUT : bloc généré par `make deps`, ne pas modifier à la main -->

Cible : `aarch64-unknown-linux-musl`, dépendances d'exécution uniquement (`cargo tree -e normal`).

- Crates dans l'arbre (hors ha-gazpar-rust) : **29**
- Plafond (`MAX_CRATES` du Makefile) : **32**

```text
ha-gazpar-rust v2026.9.0
├── jiff v1.0.0
│   ├── jiff-core v1.0.0
│   └── jiff-tzdb v1.0.0
├── serde_json v1.0.154
│   ├── itoa v1.0.19
│   ├── memchr v2.8.3
│   ├── serde_core v1.0.229
│   └── zmij v1.0.23
└── ureq v3.5.2
    ├── base64 v0.23.1
    ├── log v0.4.35
    ├── percent-encoding v2.3.2
    ├── rustls v0.23.46
    │   ├── log v0.4.35
    │   ├── once_cell v1.21.4
    │   ├── ring v0.17.14
    │   │   ├── cfg-if v1.0.5
    │   │   ├── getrandom v0.2.17
    │   │   │   ├── cfg-if v1.0.5
    │   │   │   └── libc v0.2.191
    │   │   ├── libc v0.2.191
    │   │   └── untrusted v0.9.0
    │   ├── rustls-pki-types v1.16.0
    │   │   └── zeroize v1.9.1
    │   ├── rustls-webpki v0.103.17
    │   │   ├── ring v0.17.14 (*)
    │   │   ├── rustls-pki-types v1.16.0 (*)
    │   │   └── untrusted v0.9.0
    │   ├── subtle v2.6.1
    │   └── zeroize v1.9.1
    ├── rustls-pki-types v1.16.0 (*)
    ├── ureq-proto v0.7.1
    │   ├── base64 v0.23.1
    │   ├── http v1.5.0
    │   │   ├── bytes v1.12.1
    │   │   └── itoa v1.0.19
    │   ├── httparse v1.10.1
    │   └── log v0.4.35
    ├── utf8-zero v0.8.1
    └── webpki-roots v1.0.12
        └── rustls-pki-types v1.16.0 (*)
```

<!-- FIN du bloc généré -->
