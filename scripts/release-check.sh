#!/usr/bin/env bash
# Vérifications avant publication (PLAN.md §9, §10), en local (`make
# release-check`) comme dans .github/workflows/release.yml.
#
#   scripts/release-check.sh check vAAAA.M.N   échoue si le tag ne concorde pas
#                                              (préversion : vAAAA.M.N-alpha.K,
#                                              -beta.K ou -rc.K)
#                                              avec Cargo.toml, HA_VERSION du
#                                              Makefile et CHANGELOG.md
#   scripts/release-check.sh notes AAAA.M.N    affiche la section de cette
#                                              version de CHANGELOG.md (texte de
#                                              la version GitHub)
#
# En-tête de section attendu dans CHANGELOG.md (Keep a Changelog) :
#   ## [AAAA.M.N] - AAAA-MM-JJ
# éventuellement suivi d'un texte (versions de Home Assistant validées).
set -euo pipefail
cd "$(dirname "$0")/.."

changelog=CHANGELOG.md

usage() {
    echo "usage : $0 check vAAAA.M.N | notes AAAA.M.N" >&2
    exit 2
}

# Section de la version $1, sans l'en-tête ni les lignes vides du début et de
# la fin : jusqu'au titre de niveau 2 suivant ou aux références de liens.
section() {
    awk -v ver="$1" '
        index($0, "## [" ver "]") == 1 { on = 1; next }
        on && (/^## / || /^\[[^]]+\]: /) { exit }
        on { lines[n++] = $0 }
        END {
            first = 0; last = n - 1
            while (first <= last && lines[first] ~ /^[[:space:]]*$/) first++
            while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
            for (i = first; i <= last; i++) print lines[i]
        }
    ' "$changelog"
}

check() {
    local tag=$1 errors=()
    local version=${tag#v}
    local re='^[0-9]{4}\.(1[0-2]|[1-9])\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.[1-9][0-9]*)?$'

    if [[ $tag != v* || ! $version =~ $re ]]; then
        echo "ERREUR : tag « $tag » : attendu vAAAA.M.N ou vAAAA.M.N-alpha.K, -beta.K, -rc.K (ex. v2026.9.0, v2026.9.0-alpha.1, mois sans zéro initial)" >&2
        exit 1
    fi
    local base=${version%%-*}
    local lineage=${base%.*}

    local cargo
    cargo=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
    [[ $cargo == "$version" ]] ||
        errors+=("Cargo.toml est en version « $cargo », le tag $tag demande $version")

    local ha
    ha=$(sed -n 's/^HA_VERSION ?= \(.*\)$/\1/p' Makefile | head -n 1)
    [[ $ha == "$lineage" ]] ||
        errors+=("HA_VERSION du Makefile vaut « $ha », le tag $tag demande $lineage (PLAN.md §10.1)")

    # En-tête exact (les points de la version comparés littéralement), puis
    # contenu non vide.
    local escaped=${version//./\\.}
    if ! grep -Eq "^## \[$escaped\] - [0-9]{4}-[0-9]{2}-[0-9]{2}( .*)?$" "$changelog"; then
        if grep -Fq "## [$version]" "$changelog"; then
            errors+=("$changelog : l'en-tête de $version doit être « ## [$version] - AAAA-MM-JJ »")
        else
            errors+=("$changelog : aucune section « ## [$version] - AAAA-MM-JJ » (renommer « Non publié » et la dater)")
        fi
    elif [[ -z $(section "$version") ]]; then
        errors+=("$changelog : la section $version est vide")
    fi

    # Rust de l'image = rust-toolchain.toml.
    local channel
    channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
    grep -Eq "^FROM rust:${channel//./\\.}[.-]" Dockerfile ||
        errors+=("Dockerfile : l'image de compilation n'est pas rust:$channel (rust-toolchain.toml)")

    if ((${#errors[@]})); then
        printf 'ERREUR : %s\n' "${errors[@]}" >&2
        echo "Publication de $tag refusée." >&2
        exit 1
    fi
    echo "release-check : $tag concorde avec Cargo.toml, HA_VERSION ($ha) et $changelog"
}

notes() {
    local text
    text=$(section "$1")
    if [[ -z $text ]]; then
        echo "ERREUR : $changelog : section $1 absente ou vide" >&2
        exit 1
    fi
    printf '%s\n' "$text"
}

[[ $# -eq 2 ]] || usage
case $1 in
    check) check "$2" ;;
    notes) notes "${2#v}" ;;
    *) usage ;;
esac
