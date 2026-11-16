#!/usr/bin/env bash
# Home Assistant jetable pour les essais réels (PLAN.md §7, §8), appelé par
# le Makefile.
#
#   scripts/ha-test.sh up      démarre le conteneur s'il ne tourne pas, crée
#                              l'utilisateur de test, écrit un jeton frais
#   scripts/ha-test.sh token   renouvelle le jeton d'accès (30 min de validité)
#   scripts/ha-test.sh down    sauve le journal, supprime le conteneur et
#                              son dossier
#
# Aucun secret n'est commité : l'utilisateur, son mot de passe (tiré au
# hasard) et les jetons sont créés au démarrage par l'API d'onboarding, puis
# rangés dans $HA_TEST_DIR (sous target/, ignoré par Git), en mode 600.
#
# La configuration de HA n'est pas montée depuis l'hôte : HA tourne en root
# dans le conteneur et y laisserait des fichiers impossibles à effacer sans
# sudo. Elle est copiée dans le conteneur (`docker cp`) avant son démarrage
# et disparaît avec lui.
#
# Variables : HA_IMAGE (image complète, fournie par le Makefile), HA_PORT
# (8124), HA_CONTAINER (ha-gazpar-test), HA_TEST_DIR (target/ha-test),
# HA_WAIT (délai maximal de démarrage en secondes, 300).
set -euo pipefail
cd "$(dirname "$0")/.."

image=${HA_IMAGE:?HA_IMAGE manquant (lancer par make ha-up)}
port=${HA_PORT:-8124}
name=${HA_CONTAINER:-ha-gazpar-test}
dir=${HA_TEST_DIR:-target/ha-test}
wait_max=${HA_WAIT:-300}

base="http://127.0.0.1:$port"
# client_id d'onboarding : une URL, la même pour le code et son échange.
client_id="http://localhost:$port/"
username=gazpar-test

say() { echo "ha-test : $*" >&2; }
die() { say "ERREUR : $*"; exit 1; }

# Champ d'un objet JSON lu sur l'entrée standard (python3 est présent sur
# les exécuteurs ubuntu de GitHub).
json_field() {
    python3 -c 'import json, sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

# Conteneur en marche, et sur l'image demandée (HA_TAG a pu changer).
running() {
    [ "$(docker inspect -f '{{.State.Running}} {{.Config.Image}}' "$name" 2>/dev/null)" = "true $image" ]
}

# Écrit un fichier en mode 600 sans fenêtre où il serait lisible par d'autres.
write_secret() {
    (umask 077 && cat > "$1")
}

# Échange un code d'autorisation ou un jeton de renouvellement contre un
# jeton d'accès ; écrit token (et refresh_token s'il est fourni).
exchange() {
    local reply refresh
    reply=$(curl -fsS -X POST "$base/auth/token" --data-urlencode "client_id=$client_id" "$@") ||
        return 1
    json_field access_token <<< "$reply" | write_secret "$dir/token"
    if refresh=$(json_field refresh_token <<< "$reply" 2>/dev/null); then
        printf %s "$refresh" | write_secret "$dir/refresh_token"
    fi
}

token() {
    [ -s "$dir/refresh_token" ] || return 1
    # Lu par curl dans le fichier : le jeton ne passe pas en argument.
    exchange --data-urlencode grant_type=refresh_token \
        --data-urlencode "refresh_token@$dir/refresh_token"
}

config_yaml() {
    # Le fuseau est celui des jours gaziers : `period: "day"` regroupe selon
    # le fuseau de HA. `default_config:` est inutile : le recorder et l'API
    # WebSocket suffisent (et sont de toute façon chargés).
    cat <<'EOF'
homeassistant:
  name: Essai ha-gazpar
  time_zone: Europe/Paris
  country: FR
  unit_system: metric
  latitude: 48.8566
  longitude: 2.3522
  elevation: 35
http:
onboarding:
recorder:
EOF
}

# Attend que `test` réussisse, au plus jusqu'à `deadline` (secondes epoch).
wait_until() {
    local deadline=$1 what=$2
    shift 2
    until "$@"; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            docker logs --tail 40 "$name" >&2 || true
            die "$what : délai de ${wait_max} s dépassé"
        fi
        sleep 2
    done
}

onboarding_open() {
    curl -fs -o /dev/null "$base/api/onboarding"
}

# HA démarré (state RUNNING) avec le recorder chargé ; écrit la version.
core_running() {
    local reply
    reply=$(curl -fsS -H "Authorization: Bearer $(< "$dir/token")" "$base/api/config") || return 1
    python3 -c '
import json, sys
c = json.load(sys.stdin)
if c.get("state") != "RUNNING" or "recorder" not in c.get("components", []):
    sys.exit(1)
print(c["version"], c["time_zone"])' <<< "$reply" > "$dir/version.tmp" || return 1
    mv "$dir/version.tmp" "$dir/version"
}

up() {
    if running && token 2>/dev/null; then
        say "déjà démarré, jeton renouvelé ($(cut -d' ' -f1 "$dir/version" 2>/dev/null || echo '?'))"
        return
    fi
    down_quiet
    mkdir -p "$dir"
    config_yaml > "$dir/configuration.yaml"

    say "démarrage de $image sur 127.0.0.1:$port"
    docker create --name "$name" -p "127.0.0.1:$port:8123" "$image" > /dev/null
    docker cp "$dir/configuration.yaml" "$name:/config/configuration.yaml"
    docker start "$name" > /dev/null

    local deadline=$(($(date +%s) + wait_max))
    wait_until "$deadline" "API d'onboarding" onboarding_open

    # Utilisateur propriétaire : renvoie un code d'autorisation.
    local password reply code
    password=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')
    write_secret "$dir/password" <<< "$password"
    # Mot de passe passé par l'environnement, pas en argument.
    reply=$(HA_TEST_PASSWORD=$password python3 -c '
import json, os, sys
print(json.dumps({"client_id": sys.argv[1], "name": "Essai ha-gazpar",
                  "username": sys.argv[2], "password": os.environ["HA_TEST_PASSWORD"],
                  "language": "fr"}))' \
        "$client_id" "$username" |
        curl -fsS -X POST -H 'Content-Type: application/json' --data-binary @- \
            "$base/api/onboarding/users") || die "création de l'utilisateur refusée"
    code=$(json_field auth_code <<< "$reply")
    exchange --data-urlencode grant_type=authorization_code --data-urlencode "code=$code" ||
        die "échange du code d'autorisation refusé"
    # Étape suivante de l'onboarding, pour l'interface ; sans effet sur les essais.
    curl -fsS -o /dev/null -X POST -H "Authorization: Bearer $(< "$dir/token")" \
        "$base/api/onboarding/core_config" || say "onboarding core_config refusé (ignoré)"

    wait_until "$deadline" "démarrage complet de HA (recorder)" core_running
    say "Home Assistant $(cut -d' ' -f1 "$dir/version") prêt (fuseau $(cut -d' ' -f2 "$dir/version")), jeton dans $dir/token"
}

down_quiet() {
    if docker inspect "$name" > /dev/null 2>&1; then
        docker rm -f "$name" > /dev/null
    fi
    rm -rf "$dir"
}

down() {
    if docker inspect "$name" > /dev/null 2>&1; then
        # Journal gardé hors du dossier effacé, pour diagnostiquer un échec.
        docker logs "$name" > "target/ha-test.log" 2>&1 || true
        say "journal de HA dans target/ha-test.log"
    fi
    down_quiet
    say "conteneur $name supprimé"
}

case ${1:-} in
up) up ;;
token) token || die "renouvellement du jeton impossible (make ha-down puis make ha-up)" ;;
down) down ;;
*)
    echo "usage : $0 up|token|down" >&2
    exit 2
    ;;
esac
