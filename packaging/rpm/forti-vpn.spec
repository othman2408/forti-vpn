# Forti-VPN for Fedora, RHEL/EPEL 10 and its rebuilds, and openSUSE.
#
# Plain cargo with the release's vendor tarball instead of the %%cargo_*
# macros, which only Fedora ships, so one spec builds everywhere. Release
# RPMs are built on EL 10 (the oldest glibc) with an upstream Rust
# toolchain, because EL 10's own Rust targets x86-64-v3 CPUs; see
# scripts/build-packages.sh.
# See docs/releases.md for building it.

# Fedora and EL define %%build_rustflags (optimization, debuginfo,
# hardening); elsewhere the release profile applies, with line tables so
# the debuginfo packages are not empty. CARGO_HOME stays in the build tree.
%global cargo_env export CARGO_HOME="$PWD/.cargo" RUSTFLAGS='%{?build_rustflags}' CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
%global app_id io.github.fortivpn.Utility

Name:           forti-vpn
Version:        0.2.0
Release:        1%{?dist}
Summary:        Desktop app for FortiClient VPN
# forti-vpn is MIT; the Adwaita symbolic icons compiled into it are
# LGPL-3.0-only OR CC-BY-SA-3.0 (see NOTICE), and the crates compiled
# into it are listed with their license texts in THIRD-PARTY-LICENSES.txt.
License:        MIT AND (LGPL-3.0-only OR CC-BY-SA-3.0) AND (Apache-2.0 OR MIT) AND (MIT OR Unlicense) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT)
URL:            https://github.com/othman2408/forti-vpn
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4) >= 4.14
BuildRequires:  pkgconfig(libadwaita-1) >= 1.5
# File paths resolve on every distribution: the packages are named
# glib2-devel and appstream on Fedora, AppStream on openSUSE.
BuildRequires:  /usr/bin/glib-compile-resources
BuildRequires:  /usr/bin/appstreamcli
BuildRequires:  desktop-file-utils

Requires:       adwaita-icon-theme
Requires:       hicolor-icon-theme
# GTK before 4.20 draws full-colour SVG icons (the app icon in the window)
# through gdk-pixbuf's SVG loader, which minimal installs can lack. Fedora and
# RHEL ship it in librsvg2, openSUSE in gdk-pixbuf-loader-rsvg.
Requires:       (librsvg2 or gdk-pixbuf-loader-rsvg)
# GNOME shows tray icons only with this extension; other desktops have a
# tray, so it is never pulled onto them.
Recommends:     (gnome-shell-extension-appindicator if gnome-shell)
# FortiClient comes from Fortinet and is never a dependency.
Suggests:       forticlient

%description
Forti VPN is an unofficial desktop app for the FortiClient VPN command-line
client. It connects and disconnects with password and MFA prompts, manages
VPN profiles, shows a tray icon, and copies sanitized diagnostics.

FortiClient is not included and is not a dependency of this package. Install
it separately from Fortinet; the app guides you through setup when the
FortiClient CLI is missing. Forti VPN is not affiliated with Fortinet.

%prep
%autosetup -a 1
# Build only from the vendored crates.
mkdir -p .cargo
cat >.cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
%{cargo_env}
cargo build --release --locked --offline -p forti-vpn
CARGO='cargo --offline' sh scripts/third-party-licenses -o target/release/THIRD-PARTY-LICENSES.txt

%install
%make_install PREFIX=%{_prefix} BINDIR=%{_bindir} DATADIR=%{_datadir} \
    DOCDIR=%{_docdir}/%{name} LICENSEDIR=%{_licensedir}/%{name} CARGO_TARGET_DIR=target

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstreamcli validate --no-net %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml
%{cargo_env}
# The release profile reuses the dependencies %%build compiled.
cargo test --release --locked --offline -p forti-vpn-core

%files
%dir %{_licensedir}/%{name}
%license %{_licensedir}/%{name}/LICENSE
%license %{_licensedir}/%{name}/THIRD-PARTY-LICENSES.txt
%dir %{_docdir}/%{name}
%doc %{_docdir}/%{name}/README.md
%doc %{_docdir}/%{name}/NOTICE
%{_bindir}/forti-vpn
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/metainfo/%{app_id}.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-disconnected-symbolic.svg

%changelog
* Sun Nov 08 2026 othman2408 <acerxr@gmail.com> - 0.2.0-1
- Rust rewrite for all major Linux distributions
- First RPM release
