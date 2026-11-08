# Forti-VPN: build, test, install, and release tarballs. Needs GNU make.
#
#   make             build the release binary and its third-party licenses
#   make check       run the test suite (also: make test)
#   make install     install into $(DESTDIR)$(PREFIX) (PREFIX=/usr/local)
#   make uninstall   remove the installed files
#   make validate    check the desktop entry and the AppStream metainfo
#   make vendor      copy all crates into vendor/ for offline builds
#   make dist        write source and vendor tarballs to dist/
#   make clean       remove the build directory
#
# `make install` never builds, so `make && sudo make install` leaves no
# root-owned files in the build directory.

APP_ID := io.github.fortivpn.Utility
NAME := forti-vpn

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
DOCDIR ?= $(DATADIR)/doc/$(NAME)
LICENSEDIR ?= $(DATADIR)/licenses/$(NAME)
# Desktop entries find /usr/bin programs by name; anywhere else needs the
# absolute path. Flatpak sets this to the name, as /app/bin is on its PATH.
DESKTOP_EXEC ?= $(if $(filter /usr/bin,$(BINDIR)),$(NAME),$(BINDIR)/$(NAME))

CARGO ?= cargo
CARGO_TARGET_DIR ?= target
INSTALL ?= install
INSTALL_PROGRAM ?= $(INSTALL) -m 755
INSTALL_DATA ?= $(INSTALL) -m 644

SHELL := /bin/sh
BINARY := $(CARGO_TARGET_DIR)/release/$(NAME)
LICENSES := $(CARGO_TARGET_DIR)/release/THIRD-PARTY-LICENSES.txt
ICONS := $(DATADIR)/icons/hicolor
INSTALLED := \
	$(BINDIR)/$(NAME) \
	$(DATADIR)/applications/$(APP_ID).desktop \
	$(DATADIR)/metainfo/$(APP_ID).metainfo.xml \
	$(ICONS)/scalable/apps/$(APP_ID).svg \
	$(ICONS)/symbolic/apps/$(APP_ID)-symbolic.svg \
	$(ICONS)/symbolic/apps/$(APP_ID)-disconnected-symbolic.svg \
	$(DOCDIR)/README.md \
	$(DOCDIR)/NOTICE \
	$(LICENSEDIR)/LICENSE \
	$(LICENSEDIR)/THIRD-PARTY-LICENSES.txt

# Like meson's gnome.post_install: refresh the icon and desktop caches for
# a live install only. Packages (DESTDIR set) leave this to their triggers.
REFRESH_CACHES = if [ -z '$(DESTDIR)' ]; then \
	for tool in gtk4-update-icon-cache gtk-update-icon-cache; do \
		if command -v $$tool >/dev/null 2>&1; then \
			$$tool -q -t -f '$(ICONS)' || true; break; \
		fi; \
	done; \
	if command -v update-desktop-database >/dev/null 2>&1; then \
		update-desktop-database -q '$(DATADIR)/applications' || true; \
	fi; \
fi

.DEFAULT_GOAL := build
.PHONY: build all check test install uninstall validate vendor dist clean help

build:
	$(CARGO) build --release --locked -p $(NAME) --target-dir '$(CARGO_TARGET_DIR)'
	CARGO='$(CARGO)' $(SHELL) scripts/third-party-licenses -o '$(LICENSES)'

all: build

check test:
	$(CARGO) test --workspace --locked --target-dir '$(CARGO_TARGET_DIR)'

install:
	@if [ ! -x '$(BINARY)' ] || [ ! -f '$(LICENSES)' ]; then \
		echo 'Nothing to install from $(CARGO_TARGET_DIR): run make first.' >&2; exit 1; \
	fi
	@case '$(DESKTOP_EXEC)' in *[!A-Za-z0-9_./+-]*) \
		echo 'Install paths with spaces or special characters are not supported: $(DESKTOP_EXEC)' >&2; \
		exit 1;; \
	esac
	$(INSTALL) -d '$(DESTDIR)$(BINDIR)' '$(DESTDIR)$(DATADIR)/applications' \
		'$(DESTDIR)$(DATADIR)/metainfo' '$(DESTDIR)$(ICONS)/scalable/apps' \
		'$(DESTDIR)$(ICONS)/symbolic/apps' '$(DESTDIR)$(DOCDIR)' '$(DESTDIR)$(LICENSEDIR)'
	$(INSTALL_PROGRAM) '$(BINARY)' '$(DESTDIR)$(BINDIR)/$(NAME)'
	sed 's|^Exec=$(NAME)$$|Exec=$(DESKTOP_EXEC)|' data/$(APP_ID).desktop \
		>'$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop'
	chmod 644 '$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop'
	$(INSTALL_DATA) data/$(APP_ID).metainfo.xml '$(DESTDIR)$(DATADIR)/metainfo/'
	$(INSTALL_DATA) data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		'$(DESTDIR)$(ICONS)/scalable/apps/'
	$(INSTALL_DATA) data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		data/icons/hicolor/symbolic/apps/$(APP_ID)-disconnected-symbolic.svg \
		'$(DESTDIR)$(ICONS)/symbolic/apps/'
	$(INSTALL_DATA) README.md NOTICE '$(DESTDIR)$(DOCDIR)/'
	$(INSTALL_DATA) LICENSE '$(LICENSES)' '$(DESTDIR)$(LICENSEDIR)/'
	@$(REFRESH_CACHES)

uninstall:
	rm -f $(foreach file,$(INSTALLED),'$(DESTDIR)$(file)')
	rmdir '$(DESTDIR)$(DOCDIR)' '$(DESTDIR)$(LICENSEDIR)' 2>/dev/null || true
	@$(REFRESH_CACHES)

validate:
	desktop-file-validate data/$(APP_ID).desktop
	appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

# cargo prints the source replacement on stdout; it is written as
# .cargo/config.toml unless that file already says something else.
vendor:
	@mkdir -p .cargo
	$(CARGO) vendor --locked --versioned-dirs vendor >.cargo/vendor.toml
	@if [ -e .cargo/config.toml ] && ! cmp -s .cargo/vendor.toml .cargo/config.toml; then \
		echo 'Kept .cargo/config.toml: add .cargo/vendor.toml to it to build offline.' >&2; \
	else \
		mv -f .cargo/vendor.toml .cargo/config.toml; \
		echo 'Wrote .cargo/config.toml: cargo now builds from vendor/.'; \
	fi

dist:
	CARGO='$(CARGO)' $(SHELL) scripts/dist.sh --output dist

clean:
	$(CARGO) clean --target-dir '$(CARGO_TARGET_DIR)'

help:
	@sed -n 's/^#   //p' Makefile
