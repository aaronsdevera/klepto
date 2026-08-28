.PHONY: help build install-host release release-local release-host release-checksums \
	release-vsix \
	release-linux release-linux-amd64 release-linux-arm64 \
	release-darwin release-darwin-amd64 release-darwin-arm64 \
	release-windows-amd64 \
	run serve doctor doctor-install \
	service-install service-uninstall service-status service-logs \
	image container-which container-run container-stop container-restart \
	container-status container-logs container-shell container-up \
	ext-install ext-compile ext-check ext-vsix \
	bump major minor fix \
	daemon-health clean all

# `make bump` / `make bump fix|minor|major` — default fix (patch +1)
BUMP := $(or $(firstword $(filter major minor fix,$(MAKECMDGOALS))),fix)

KLEPTO_DIR := klepto
EXT_DIR := klepto-vscode
DIST_DIR := dist
LISTEN ?= 127.0.0.1:7420
KLEPTO_IMAGE ?= klepto:local
KLEPTO_CONTAINER_NAME ?= klepto
KLEPTO_INSTALL_DIR ?= $(HOME)/.klepto/bin
# Optional same-path workspace mount: make container-run KLEPTO_MOUNT=$(PWD)
KLEPTO_MOUNT ?=

# Debian/glibc Linux release targets (built via scripts/build-linux.sh + zig)
LINUX_AMD64_TARGET := x86_64-unknown-linux-gnu
LINUX_ARM64_TARGET := aarch64-unknown-linux-gnu

OCI_ENV := KLEPTO_IMAGE=$(KLEPTO_IMAGE) KLEPTO_CONTAINER_NAME=$(KLEPTO_CONTAINER_NAME) \
	KLEPTO_HOST_LISTEN=$(LISTEN) KLEPTO_MOUNT=$(KLEPTO_MOUNT)

# fish + jorgebucaran/nvm.fish — always activate latest Node before npm
define FISH_NPM
	cd $(EXT_DIR) && fish -c 'nvm use latest; npm $(1)'
endef

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  %-22s %s\n", $$1, $$2}'

## --- Rust daemon / CLI ---

build: ## Build klepto (debug, host)
	cd $(KLEPTO_DIR) && cargo build

install-host: ## Build and install host binary to ~/.klepto/bin
	cd $(KLEPTO_DIR) && cargo build --release
	install -d "$(KLEPTO_INSTALL_DIR)"
	install -m 755 "$(KLEPTO_DIR)/target/release/klepto" "$(KLEPTO_INSTALL_DIR)/klepto"
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		codesign --force --sign - "$(KLEPTO_INSTALL_DIR)/klepto"; \
	fi
	sh scripts/install.sh --configure-path "$(KLEPTO_INSTALL_DIR)"
	@echo "Installed Klepto to $(KLEPTO_INSTALL_DIR)/klepto"

release-host: ## Host release binary → dist/klepto
	mkdir -p $(DIST_DIR)
	cd $(KLEPTO_DIR) && cargo build --release
	cp "$(KLEPTO_DIR)/target/release/klepto" "$(DIST_DIR)/klepto"
	chmod +x "$(DIST_DIR)/klepto"
	@if [ "$$(uname -s)" = "Darwin" ] && [ "$$(uname -m)" = "arm64" ]; then \
		cp "$(DIST_DIR)/klepto" "$(DIST_DIR)/klepto-darwin-arm64"; \
	fi
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		codesign --force --sign - "$(DIST_DIR)/klepto"; \
		if [ -f "$(DIST_DIR)/klepto-darwin-arm64" ]; then \
			chmod +x "$(DIST_DIR)/klepto-darwin-arm64"; \
			codesign --force --sign - "$(DIST_DIR)/klepto-darwin-arm64"; \
		fi; \
	fi
	@ls -lh "$(DIST_DIR)/klepto"

release-checksums: ## Write SHA-256 sidecars for release binaries
	@for file in $(DIST_DIR)/klepto-*; do \
		[ -f "$$file" ] || continue; \
		shasum -a 256 "$$file" > "$$file.sha256"; \
	done

release-vsix: ## Extension VSIX → dist/
	chmod +x scripts/build-vsix.sh
	fish -c 'nvm use latest; ./scripts/build-vsix.sh'

release-linux-amd64: ## Cross-build Debian/glibc x86_64 → dist/klepto-linux-amd64
	chmod +x scripts/build-linux.sh
	./scripts/build-linux.sh amd64

release-linux-arm64: ## Cross-build Debian/glibc aarch64 → dist/klepto-linux-arm64
	chmod +x scripts/build-linux.sh
	./scripts/build-linux.sh arm64

release-linux: ## Both Linux binaries (zig, no Docker)
	chmod +x scripts/build-linux.sh
	./scripts/build-linux.sh all

release-darwin-amd64: ## macOS Intel (x86_64) → dist/klepto-darwin-amd64
	chmod +x scripts/build-darwin.sh
	./scripts/build-darwin.sh amd64

release-darwin-arm64: ## macOS Apple Silicon (aarch64) → dist/klepto-darwin-arm64
	chmod +x scripts/build-darwin.sh
	./scripts/build-darwin.sh arm64

release-darwin: ## Both macOS binaries
	chmod +x scripts/build-darwin.sh
	./scripts/build-darwin.sh all

release-windows-amd64: ## Windows MSVC x86_64 → dist/klepto-windows-amd64.exe
	chmod +x scripts/build-windows.sh
	./scripts/build-windows.sh amd64

release: ## Tag v<version> and push; CI builds and attaches GitHub Release assets
	chmod +x scripts/release.sh
	./scripts/release.sh

# On Darwin, also ship an Intel macOS binary alongside the native host build.
ifeq ($(shell uname -s 2>/dev/null),Darwin)
release-local: release-host release-darwin-amd64 release-vsix release-linux release-checksums
else
release-local: release-host release-vsix release-linux release-checksums
endif
release-local: ## All local release artifacts → ./dist
	@echo "Release artifacts:"
	@ls -lh "$(DIST_DIR)"

## --- OCI container (macOS: container, Linux: docker) ---

image: ## Build OCI image klepto:local (needs dist/klepto-linux-* or KLEPTO_BINARY)
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh build

container-which: ## Print OCI runtime (container on macOS, docker on Linux)
	chmod +x scripts/oci.sh
	@./scripts/oci.sh which

container-run: ## Run OCI klepto (LISTEN, optional KLEPTO_MOUNT=path)
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh run

container-stop: ## Stop/remove OCI klepto container
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh stop

container-restart: ## Recreate OCI klepto container
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh restart

container-status: ## Show OCI runtime, container, and /v1/health
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh status

container-logs: ## Tail OCI logs (FOLLOW=1 for -f)
	chmod +x scripts/oci.sh
	@if [ "$(FOLLOW)" = "1" ]; then $(OCI_ENV) ./scripts/oci.sh logs -f; \
	else $(OCI_ENV) ./scripts/oci.sh logs --tail 100; fi

container-shell: ## Shell into the running klepto container
	chmod +x scripts/oci.sh
	$(OCI_ENV) ./scripts/oci.sh shell

container-up: image container-run ## Build image and run container
	@$(MAKE) container-status

run: ## Run klepto with args, e.g. make run ARGS='--help'
	cd $(KLEPTO_DIR) && cargo run -- $(ARGS)

serve: ## Start the daemon in foreground (auto-installs deps)
	cd $(KLEPTO_DIR) && cargo run -- serve --listen $(LISTEN)

doctor: ## Check runtime deps (tmux/pi/rg)
	cd $(KLEPTO_DIR) && cargo run -- doctor

doctor-install: ## Check + auto-install runtime deps
	cd $(KLEPTO_DIR) && cargo run -- doctor --install

service-install: ## Install background service (launchd/systemd)
	cd $(KLEPTO_DIR) && cargo run --quiet -- service install --listen $(LISTEN)

service-uninstall: ## Uninstall background service
	cd $(KLEPTO_DIR) && cargo run --quiet -- service uninstall

service-status: ## Show background service status
	cd $(KLEPTO_DIR) && cargo run --quiet -- service status

service-logs: ## Tail background service logs
	cd $(KLEPTO_DIR) && cargo run --quiet -- service logs -f

daemon-health: ## GET /v1/health on LISTEN
	curl -fsS "http://$(LISTEN)/v1/health"
	@echo

## --- VSCodium / VS Code extension ---

ext-install: ## Install extension npm deps (nvm use latest)
	$(call FISH_NPM,install)

ext-compile: ## Compile extension (tsc via nvm latest)
	$(call FISH_NPM,run compile)

ext-check: ## Typecheck extension without emit
	$(call FISH_NPM,run check-types)

ext-vsix: release-vsix ## Alias for release-vsix

## --- Version ---

bump: ## Bump rust + extension version (default: fix). Usage: make bump [major|minor|fix]
	chmod +x scripts/bump-version.sh
	./scripts/bump-version.sh $(BUMP)

# Allow `make bump major` / `make bump minor` / `make bump fix` without unknown-target errors
major minor fix:
	@:

## --- Combined ---

all: release-local ## Alias for make release-local (artifacts in ./dist)

clean: ## Remove Rust, extension, and ./dist artifacts
	cd $(KLEPTO_DIR) && cargo clean
	rm -rf $(EXT_DIR)/dist
	rm -f $(EXT_DIR)/*.vsix
	rm -rf $(DIST_DIR)
