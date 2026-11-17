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
	cover build release ha-up ha-down test-ha dev image image-all dist release-check

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
	@v=$$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\.[0-9]*\(-[0-9a-z.]*\)\{0,1\}"$$/\1/p' Cargo.toml | head -n 1); \
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

dev: ## check, once (info puis debug), une panne par famille, contre les faux GRDF et HA (127.0.0.1)
	$(CARGO) test --locked --test commands -- --ignored --exact demo --nocapture

# Image Docker (Dockerfile). Rien n'est poussé : la publication est faite par
# .github/workflows/release.yml. PLATFORMS vaut l'architecture du démon Docker ;
# `make image-all` construit amd64 et arm64 (celle qui n'est pas locale sous
# émulation QEMU, plus lente).
IMAGE ?= ha-gazpar-rust:dev
PLATFORMS ?= linux/$(shell docker version -f '{{.Server.Arch}}' 2>/dev/null || echo amd64)
ALL_PLATFORMS := linux/amd64,linux/arm64
VERSION := $(shell sed -n 's/^version = "\(.*\)"$$/\1/p' Cargo.toml | head -n 1)
REVISION := $(shell git rev-parse HEAD 2>/dev/null || echo unknown)

image: ## Image de l'architecture locale (ha-gazpar-rust:dev), affiche sa taille
	docker buildx build --platform $(PLATFORMS) \
		--build-arg VERSION=$(VERSION) --build-arg REVISION=$(REVISION) \
		--tag $(IMAGE) --load .
	@for p in $$(echo "$(PLATFORMS)" | tr ',' ' '); do \
		s=$$(docker image inspect --platform $$p -f '{{.Size}}' $(IMAGE) 2>/dev/null \
			|| docker image inspect -f '{{.Size}}' $(IMAGE)); \
		echo "$(IMAGE) ($$p) : $$s octets"; \
	done

image-all: ## Image amd64 et arm64 (ha-gazpar-rust:dev), arm64 sous émulation
	@$(MAKE) --no-print-directory image PLATFORMS=$(ALL_PLATFORMS)

# Binaires joints aux versions GitHub : l'étape `bin` du Dockerfile, sans
# l'image. Noms ha-gazpar-<version>-linux-<arch>, sommes dans SHA256SUMS.
dist: ## Binaires statiques amd64 et arm64 dans dist/, avec SHA256SUMS
	rm -rf dist
	docker buildx build --platform $(ALL_PLATFORMS) --target bin \
		--provenance=false --sbom=false --output type=local,dest=dist/oci .
	@for a in amd64 arm64; do \
		mv dist/oci/linux_$$a/ha-gazpar dist/ha-gazpar-$(VERSION)-linux-$$a; \
	done
	@rm -r dist/oci
	@cd dist && sha256sum ha-gazpar-* > SHA256SUMS && cat SHA256SUMS
	@for f in dist/ha-gazpar-*; do echo "$$f : $$(stat -c %s $$f) octets"; done

# Hors de `make check` : échoue tant que CHANGELOG.md n'a pas de section
# datée pour la version de Cargo.toml.
release-check: ## Concordance tag, Cargo.toml, HA_VERSION et CHANGELOG.md (TAG=vAAAA.M.N[-alpha.K])
	@scripts/release-check.sh check $(if $(TAG),$(TAG),v$(VERSION))
