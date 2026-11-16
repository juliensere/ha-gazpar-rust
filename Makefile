# Point d'entrée unique du développement (PLAN.md §7). `make help` liste les cibles.

# Version de Home Assistant de référence (PLAN.md §10.1). Seul endroit où elle
# est écrite ; la CI la lit ici. Elle doit valoir AAAA.M de la version de
# Cargo.toml (vérifié par `make version-check`).
HA_VERSION ?= 2026.9

# Home Assistant jetable (`make ha-up`, `make test-ha`). L'étiquette `2026.9`
# de l'image suit les correctifs de la lignée (2026.9.4 le 2026-11-16) ; la CI
# planifiée passe HA_TAG=stable.
HA_TAG ?= $(HA_VERSION)
HA_IMAGE := ghcr.io/home-assistant/home-assistant:$(HA_TAG)
HA_PORT := 8124
HA_TEST_DIR := target/ha-test

# Plafond du nombre de crates de l'arbre d'exécution, hors ha-gazpar-rust
# (PLAN.md §4.3). Mesuré à 29 le 2026-11-16 pour aarch64-unknown-linux-musl,
# plus 3 de marge. Le relever exige une justification dans DEPENDENCIES.md.
MAX_CRATES := 32
CRATES_TARGET := aarch64-unknown-linux-musl

# Seuil de couverture des lignes (PLAN.md §8), hors main.rs.
COVER_MIN ?= 90

CARGO ?= cargo
SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
export MAX_CRATES CRATES_TARGET HA_IMAGE HA_PORT HA_TEST_DIR

.DEFAULT_GOAL := help
.PHONY: help check fmt fmt-check lint test deny crates deps deps-check version-check \
	cover build release ha-up ha-down test-ha dev image

help: ## Liste les cibles
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

check: fmt-check lint test deny crates deps-check version-check ## Tout ce que fait la CI
	@echo "make check : tout est vert"

fmt: ## Met le code en forme
	$(CARGO) fmt --all

fmt-check: ## Vérifie la mise en forme
	$(CARGO) fmt --all -- --check

lint: ## Clippy, avertissements = erreurs
	$(CARGO) clippy --all-targets --locked -- -D warnings

test: ## Tests unitaires et d'intégration hors réseau
	$(CARGO) test --locked

deny: ## Avis de sécurité, licences, sources, doublons (cargo-deny)
	@if $(CARGO) deny --version >/dev/null 2>&1; then \
		$(CARGO) deny --locked check; \
	elif [ -n "$${CI:-}" ]; then \
		echo "ERREUR : cargo-deny absent en CI" >&2; exit 1; \
	else \
		echo "ATTENTION : cargo-deny absent, vérification sautée (cargo install --locked cargo-deny)" >&2; \
	fi

crates: ## Échoue si l'arbre d'exécution dépasse MAX_CRATES
	@n=$$(scripts/deps.sh count); \
	if [ "$$n" -gt "$(MAX_CRATES)" ]; then \
		echo "ERREUR : $$n crates dans l'arbre d'exécution, plafond $(MAX_CRATES) (voir DEPENDENCIES.md)" >&2; exit 1; \
	fi; \
	echo "crates : $$n / $(MAX_CRATES)"

deps: ## Régénère le bloc généré de DEPENDENCIES.md
	@scripts/deps.sh update

deps-check: ## Vérifie que DEPENDENCIES.md est à jour
	@scripts/deps.sh verify

version-check: ## Vérifie que HA_VERSION concorde avec la version de Cargo.toml
	@v=$$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\.[0-9]*"$$/\1/p' Cargo.toml | head -n 1); \
	if [ "$$v" != "$(HA_VERSION)" ]; then \
		echo "ERREUR : Cargo.toml est en $$v.N mais HA_VERSION vaut $(HA_VERSION) (PLAN.md §10.1)" >&2; exit 1; \
	fi; \
	echo "version : $$v.N, Home Assistant de référence $(HA_VERSION)"

cover: ## Couverture (cargo-llvm-cov), rapport HTML dans coverage/
	@if $(CARGO) llvm-cov --version >/dev/null 2>&1; then \
		$(CARGO) llvm-cov --locked --html --output-dir coverage \
			--ignore-filename-regex 'src/main\.rs$$' --fail-under-lines $(COVER_MIN); \
	else \
		echo "ATTENTION : cargo-llvm-cov absent, couverture sautée (cargo install --locked cargo-llvm-cov)" >&2; \
	fi

build: ## Compilation de développement
	$(CARGO) build --locked

release: ## Compilation optimisée, affiche la taille du binaire
	$(CARGO) build --release --locked
	@echo "target/release/ha-gazpar : $$(stat -c %s target/release/ha-gazpar) octets"

ha-up: ## Home Assistant jetable sur 127.0.0.1:8124 (utilisateur et jeton de test)
	@scripts/ha-test.sh up

ha-down: ## Arrête et supprime le Home Assistant jetable
	@scripts/ha-test.sh down

test-ha: ha-up ## Essais réels contre le Home Assistant jetable
	HA_TEST_URL=ws://127.0.0.1:$(HA_PORT)/api/websocket \
	HA_TEST_TOKEN_FILE=$(HA_TEST_DIR)/token \
	HA_TEST_TAG=$(HA_TAG) \
	$(CARGO) test --locked --test ha_real -- --ignored --test-threads=1

dev: ## Chaîne complète contre faux GRDF et HA jetable (itération ultérieure)
	@echo "make $@ : itération ultérieure (étapes 2 à 5)"

image: ## Image arm64 et amd64 (itération ultérieure)
	@echo "make $@ : itération ultérieure (étape 6)"
