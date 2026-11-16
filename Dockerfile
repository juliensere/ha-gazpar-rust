# Image de ha-gazpar (PLAN.md §4.3, §11 étape 6 ; manuel §3).
#
# Image finale `FROM scratch` : le binaire statique seul. Ni certificats
# (autorités de Mozilla embarquées par webpki-roots), ni base de fuseaux
# (embarquée par jiff, feature `tzdb-bundle-always`), ni shell.
#
# Multi-architecture (linux/amd64, linux/arm64) : chaque architecture est
# compilée nativement dans l'image Rust de sa plateforme, sous émulation QEMU
# (binfmt) pour celle qui n'est pas celle de la machine. Choix délibéré plutôt
# qu'une compilation croisée depuis $BUILDPLATFORM :
# - l'image officielle `rust:*-alpine` existe pour les deux architectures et
#   contient déjà gcc et musl-dev, ce qu'il faut à ring (seul code C) : la
#   compilation n'a besoin de rien d'autre que cette image et crates.io ;
# - une compilation croisée demanderait la cible Rust aarch64 (`rustup target
#   add`, téléchargée hors de l'image), une chaîne C aarch64-musl (absente
#   d'Alpine) ou zig et cargo-zigbuild : autant de pièces à épingler et à
#   maintenir en plus ;
# - le même `cargo build` produit les deux binaires, sans configuration
#   propre à une cible.
# Le prix est la durée : quelques minutes de plus sous émulation, seulement
# à la publication (la CI ne construit que l'image amd64).
#
# Construction : `make image` (architecture locale) ou `make image-all`.

# Rust 1.99, comme rust-toolchain.toml (à monter ensemble). Épinglée par le
# condensé de l'index multi-architecture, pas d'une plateforme :
#   docker buildx imagetools inspect rust:1.99.1-alpine3.24
FROM rust:1.99.1-alpine3.24@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS build

WORKDIR /src

# rust-toolchain.toml n'est pas copié : rustup tenterait d'installer la
# chaîne qu'il nomme alors que l'image fournit déjà Rust 1.99.
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Profil release de Cargo.toml (opt-level z, LTO, strip) ; la cible musl de
# l'image produit un binaire statique. Le cache de target/ est propre à chaque
# architecture : les deux compilations tournent en parallèle.
ARG TARGETARCH
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ha-gazpar-target-${TARGETARCH},target=/src/target \
    cargo build --release --locked \
    && cp target/release/ha-gazpar /ha-gazpar

# Étape intermédiaire pour extraire le binaire seul :
#   docker buildx build --target bin --output type=local,dest=dist .
FROM scratch AS bin
COPY --from=build /ha-gazpar /ha-gazpar

FROM scratch

ARG VERSION=dev
ARG REVISION=unknown
ARG SOURCE=https://github.com/juliensere/ha-gazpar-rust

LABEL org.opencontainers.image.title="ha-gazpar-rust" \
      org.opencontainers.image.description="Importe la consommation de gaz Gazpar (GRDF) dans les statistiques long terme de Home Assistant" \
      org.opencontainers.image.source="${SOURCE}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"

COPY --from=build /ha-gazpar /ha-gazpar

# Aucun ENV de configuration (GAZPAR_*, GRDF_*, HA_*) : l'environnement passe
# avant le fichier de configuration et le masquerait (manuel §4.3).

# Utilisateur non root sans nom (pas de /etc/passwd) ; le compose du manuel
# le remplace par 1000:1000.
USER 65532:65532

ENTRYPOINT ["/ha-gazpar"]
CMD ["run"]
