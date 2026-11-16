# Fichiers de configuration d'essai

Valeurs factices, au format du fichier de configuration ([manuel §4.4](../../../docs/MANUEL.md#44-fichier-de-configuration)). Lus par `tests/cli.rs`, qui les recopie dans un dossier temporaire en mode 600 quand le test dépend des droits.

- `complet.conf` : toutes les options obligatoires, avec espaces autour de `=`, guillemets doubles et simples, `#` et `$HOME` dans une valeur (pris tels quels), commentaire indenté, valeur vide (comptée comme absente), et trois options facultatives.

Les fins de ligne Windows et la marque d'ordre d'octets ne peuvent pas figurer ici (`.gitattributes` normalise en LF) : les tests les écrivent eux-mêmes dans un fichier temporaire.
