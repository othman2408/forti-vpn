# Releases

Each release ships a `.deb`, an `.rpm` with its source RPM, an Arch package,
an experimental Flatpak bundle, the source and vendor tarballs, and
`SHA256SUMS`. Packages are built on the oldest release of each family, so
they install on all newer ones:

| Package | Built on | Installs on |
| --- | --- | --- |
| `forti-vpn_VERSION-1_amd64.deb` | Ubuntu 24.04 | Ubuntu 24.04 and newer, Debian 13 |
| `forti-vpn-VERSION-1.el10.x86_64.rpm` | AlmaLinux 10, with an upstream Rust toolchain | Fedora 43 or newer (tested on Fedora 45), RHEL 10 and rebuilds, openSUSE Tumbleweed and Leap 16 |
| `forti-vpn-VERSION-1-x86_64.pkg.tar.zst` | Arch Linux | Arch Linux and derivatives |
| `io.github.fortivpn.Utility.flatpak` | GNOME 51 runtime | Any system with Flatpak |

FortiClient is never bundled or depended on, as Fortinet's license terms
require: packages only suggest it. The GNOME tray extension is a Suggests on
Debian (a Recommends would pull GNOME Shell onto KDE and Xfce systems), a
Recommends only when GNOME Shell is installed in the RPM, and an optional
dependency on Arch.

Every package ships the MIT license text, `NOTICE`, and
`THIRD-PARTY-LICENSES.txt`; the `.deb` has the license in its `copyright`
file, `packaging/debian/copyright`. The RPM's `License` and the PKGBUILD's
`license` name MIT for the app, LGPL-3.0-only OR CC-BY-SA-3.0 for the
Adwaita icons compiled into it, and the licenses of its crates; the
AppStream metainfo names the first two.

## Prepare

1. Set the new version in `Cargo.toml` (`[workspace.package]`) and run
   `cargo update --workspace` to update `Cargo.lock`.
2. Use the same version in `packaging/rpm/forti-vpn.spec` (`Version`,
   `Release: 1`, and a `%changelog` entry), `packaging/arch/PKGBUILD`
   (`pkgver`, `pkgrel=1`), and a new first `<release>` with its date in
   `data/io.github.fortivpn.Utility.metainfo.xml`.
   `scripts/build-packages.sh` refuses to build when they disagree.
3. Write `docs/release-notes/VERSION.md`; the release workflow uses it as
   the release description, so link to other files with absolute URLs.
4. Check that every `*-symbolic.svg` in `crates/forti-vpn/resources/icons`
   is a copy of an Adwaita icon: `NOTICE` and `packaging/debian/copyright`
   attribute them all to the GNOME Project.
5. Check that CI passes on `main`, and run `make validate`.
6. Run the release workflow by hand on `main` (**Actions → Release → Run
   workflow**). Without a tag it builds every asset as workflow artifacts
   and creates no release; install and launch them as described under
   [Tag and publish](#tag-and-publish).

## Build locally

```bash
scripts/build-packages.sh              # deb, rpm, and arch into dist/
scripts/build-packages.sh rpm arch     # some of them
make dist                              # source and vendor tarballs into dist/
```

`build-packages.sh` packs the working tree, including uncommitted and
untracked files that git does not ignore, and builds each package in a
container with docker or podman. It mounts your rustup toolchain read-only
when there is one; otherwise it installs Rust in the container. Build tools
and downloaded crates are cached in `~/.cache/forti-vpn-packages`. The RPM
is built with that upstream toolchain, not AlmaLinux's own Rust: AlmaLinux
compiles for x86-64-v3 CPUs (the RHEL 10 baseline), while the same RPM must
run on Fedora and openSUSE systems with older CPUs. The script fails the
build if the binary contains x86-64-v3 instructions. The minimum Rust
version is checked by the CI job that builds with AlmaLinux's Rust.

`make dist` packs the files git tracks into `forti-vpn-VERSION.tar.gz` and
vendors every crate into `forti-vpn-VERSION-vendor.tar.gz`. The tarballs are
reproducible: entries are sorted, owned by root, and dated
`SOURCE_DATE_EPOCH`, which defaults to the last commit's time. Unpacked
inside the source tree, the vendor tarball makes cargo build offline.

Pull requests and pushes to `main` that change `packaging/`, `scripts/`,
`data/`, the `Makefile`, `Cargo.toml`, or `crates/forti-vpn/Cargo.toml` also
build the three packages this way (`.github/workflows/packages.yml`), so a
broken package shows up before a release.

Without containers, on a system of the right family:

```bash
make && cargo deb -p forti-vpn --no-build      # cargo install cargo-deb --locked
cp dist/forti-vpn-*.tar.gz ~/rpmbuild/SOURCES/ && rpmbuild -ba packaging/rpm/forti-vpn.spec
cd packaging/arch && makepkg -si               # downloads the release tarball
```

## Tag and publish

```bash
git tag -s vVERSION -m "Forti-VPN VERSION"
git push origin vVERSION
```

The release workflow (`.github/workflows/release.yml`) checks that the tag
matches `Cargo.toml`, runs `make dist`, builds the three packages from those
tarballs with `scripts/build-packages.sh`, builds the Flatpak bundle, writes
`SHA256SUMS`, and saves everything as a draft release. The Flatpak is
experimental and does not hold up the release: when it fails, the draft is
created without it and the run shows a warning, so remove it from the
release notes or attach a bundle by hand.

Before publishing the draft, download the assets, verify them with
`sha256sum -c SHA256SUMS`, and install, launch, and remove each package on
its oldest target and on a current release:

```bash
sudo apt install ./forti-vpn_VERSION-1_amd64.deb                    # Ubuntu, Debian
sudo dnf install ./forti-vpn-VERSION-1.el10.x86_64.rpm              # Fedora, EL 10
sudo zypper install --allow-unsigned-rpm ./forti-vpn-VERSION-1.el10.x86_64.rpm
sudo pacman -U ./forti-vpn-VERSION-1-x86_64.pkg.tar.zst
```

The packages are not signed. On Fedora 45, whose rpm 6 enforces signatures
(`%_pkgverify_level all`), dnf 5 installs a local package anyway, because
`localpkg_gpgcheck` is off, and prints "skipped OpenPGP checks for 1 package
from repository: @commandline"; `rpm -i` fails with "does not verify: no
signature" unless given `--nosignature`. zypper aborts on "Package header is
not signed!" unless given `--allow-unsigned-rpm`. AlmaLinux 10 (rpm 4.19)
installs it without a warning.

Removing the app does not remove FortiClient or its profiles, or disconnect
an established VPN.

## AUR

The AUR package is `packaging/arch/PKGBUILD` with the release tarball's
checksum, which cannot be in the repository's copy because the tarball
contains it. In the AUR repository, copy the PKGBUILD, run `updpkgsums` and
check the checksum against `SHA256SUMS`, run
`makepkg --printsrcinfo > .SRCINFO`, test with `makepkg -si`, then commit
and push. The README tells people who build from the repository's copy to
do the same check before building.
