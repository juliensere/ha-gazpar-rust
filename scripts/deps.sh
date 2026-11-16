#!/usr/bin/env bash
# Inventaire des dépendances d'exécution (PLAN.md §4.3), appelé par le Makefile.
#
#   scripts/deps.sh count    nombre de crates de l'arbre d'exécution
#   scripts/deps.sh update   régénère le bloc généré de DEPENDENCIES.md
#   scripts/deps.sh verify   échoue si ce bloc n'est pas à jour
#
# On compte les crates distinctes (nom + version) de `cargo tree -e normal`
# pour la cible des images, sans compter ha-gazpar-rust lui-même : les
# dépendances de compilation (cc…) et de test sont exclues.
set -euo pipefail
# Sortie sans couleur, quelle que soit CARGO_TERM_COLOR (la CI la force à always).
export CARGO_TERM_COLOR=never
cd "$(dirname "$0")/.."

target=${CRATES_TARGET:-aarch64-unknown-linux-musl}
max=${MAX_CRATES:-?}
file=DEPENDENCIES.md
begin='<!-- DÉBUT : bloc généré par `make deps`, ne pas modifier à la main -->'
end='<!-- FIN du bloc généré -->'

crate_list() {
    cargo tree --locked --color never -e normal --prefix none --target "$target" \
        | sed 's/ (\*)$//' | grep -v '^ha-gazpar-rust ' | sort -u
}

count() {
    crate_list | wc -l | tr -d ' '
}

block() {
    echo "$begin"
    echo
    echo "Cible : \`$target\`, dépendances d'exécution uniquement (\`cargo tree -e normal\`)."
    echo
    echo "- Crates dans l'arbre (hors ha-gazpar-rust) : **$(count)**"
    echo "- Plafond (\`MAX_CRATES\` du Makefile) : **$max**"
    echo
    echo '```text'
    cargo tree --locked --color never -e normal --target "$target" | sed 's/^ha-gazpar-rust \(v[^ ]*\) (.*)$/ha-gazpar-rust \1/'
    echo '```'
    echo
    echo "$end"
}

render() {
    local b e
    b=$(grep -nF -- "$begin" "$file" | cut -d: -f1)
    e=$(grep -nF -- "$end" "$file" | cut -d: -f1)
    if [[ -z $b || -z $e || $b -ge $e ]]; then
        echo "marqueurs du bloc généré introuvables dans $file" >&2
        exit 1
    fi
    head -n "$((b - 1))" "$file"
    block
    tail -n "+$((e + 1))" "$file"
}

case ${1:-} in
count) count ;;
update)
    tmp=$(mktemp)
    render >"$tmp"
    mv "$tmp" "$file"
    echo "$file régénéré ($(count) crates)"
    ;;
verify)
    if ! diff -u "$file" <(render); then
        echo "$file n'est pas à jour : lancer make deps" >&2
        exit 1
    fi
    ;;
*)
    echo "usage : $0 count|update|verify" >&2
    exit 2
    ;;
esac
