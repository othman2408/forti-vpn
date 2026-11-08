#!/usr/bin/env bash
# Build Forti-VPN packages from the working tree in containers, each on the
# oldest release of its family so it installs on all newer ones:
#
#   deb   ubuntu:24.04       cargo-deb
#   rpm   almalinux:10       rpmbuild, with an upstream Rust toolchain
#   arch  archlinux:latest   makepkg
#
# Packages and SHA256SUMS go to dist/. Needs docker or podman, GNU tar, and
# git (optional). The host's rustup toolchain (RUSTUP_HOME, CARGO_HOME) is
# mounted read-only when there is one; otherwise the deb and Arch builds
# install Rust in the container, cached between runs.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/build-packages.sh [options] [deb] [rpm] [arch]

Builds the named formats (default: all three).

  -o, --output DIR    write packages and SHA256SUMS to DIR (default: dist)
  -e, --engine NAME   docker or podman (default: $CONTAINER_ENGINE, else
                      podman when installed, else docker)
  --cache DIR         downloaded tools and build directories (default:
                      ${XDG_CACHE_HOME:-~/.cache}/forti-vpn-packages)
  --sources DIR       build from DIR/forti-vpn-VERSION.tar.gz and
                      DIR/forti-vpn-VERSION-vendor.tar.gz (from `make dist`)
                      instead of packing the working tree
  --keep-work         keep the build directories, for debugging
  -h, --help          show this help
EOF
}

die() {
    printf 'build-packages: %s\n' "$*" >&2
    exit 1
}

log() {
    printf '\n==> %s\n' "$*" >&2
}

root=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output=$root/dist
engine=${CONTAINER_ENGINE:-}
cache=${XDG_CACHE_HOME:-$HOME/.cache}/forti-vpn-packages
sources=
keep_work=false
formats=()
while (($#)); do
    case $1 in
        -o | --output | -e | --engine | --cache | --sources)
            (($# >= 2)) || die "$1 needs a value"
            case $1 in
                -o | --output) output=$2 ;;
                -e | --engine) engine=$2 ;;
                --cache) cache=$2 ;;
                --sources) sources=$2 ;;
            esac
            shift 2
            ;;
        --keep-work) keep_work=true && shift ;;
        -h | --help) usage && exit 0 ;;
        deb | rpm | arch) formats+=("$1") && shift ;;
        *) usage >&2 && exit 2 ;;
    esac
done
((${#formats[@]})) || formats=(deb rpm arch)

if [[ -z $engine ]]; then
    for candidate in podman docker; do
        command -v "$candidate" >/dev/null 2>&1 && engine=$candidate && break
    done
    [[ -n $engine ]] || die "install podman or docker"
fi
command -v "$engine" >/dev/null 2>&1 || die "$engine is not installed"
# The docker command may be podman's compatibility wrapper.
podman=false
"$engine" --version 2>/dev/null | grep -qi podman && podman=true

version_of() {
    sed -n "s/$2/\\1/p" "$root/$1" | head -n 1
}
version=$(version_of Cargo.toml '^version = "\([^"]*\)".*')
[[ -n $version ]] || die "no version in Cargo.toml"
for pair in "packaging/rpm/forti-vpn.spec:^Version: *\([^ ]*\)$" \
    "packaging/arch/PKGBUILD:^pkgver=\(.*\)$" \
    "data/io.github.fortivpn.Utility.metainfo.xml:.*<release version=\"\([^\"]*\)\".*"; do
    file=${pair%%:*}
    found=$(version_of "$file" "${pair#*:}")
    [[ $found == "$version" ]] || die "$file has version '$found', Cargo.toml has $version"
done

if [[ -z ${SOURCE_DATE_EPOCH:-} ]]; then
    if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        SOURCE_DATE_EPOCH=$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)
    else
        SOURCE_DATE_EPOCH=$(date +%s)
    fi
fi
export SOURCE_DATE_EPOCH

# The host's rustup toolchain, mounted at the same paths in containers.
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
cargo_home=${CARGO_HOME:-$HOME/.cargo}
host_toolchain=false
[[ -x $cargo_home/bin/cargo && -d $rustup_home/toolchains ]] && host_toolchain=true

mkdir -p "$cache" "$output"
cache=$(CDPATH='' cd -- "$cache" && pwd)
output=$(CDPATH='' cd -- "$output" && pwd)
uid=$(id -u)
gid=$(id -g)
work=
container=

cleanup() {
    if [[ -n $container ]]; then
        "$engine" rm -f "$container" >/dev/null 2>&1 || true
    fi
    if [[ -n $work ]]; then
        if $keep_work; then
            printf 'build-packages: kept %s\n' "$work" >&2
        else
            rm -rf "$work"
        fi
    fi
    container='' work=''
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Start a container from image $1 with the work and cache directories at
# the same paths as on the host.
start() {
    container=forti-vpn-build-$1-$$
    local image=$2
    local -a args=(run -d --name "$container" --security-opt label=disable
        -v "$cache:$cache" -e "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" -e LC_ALL=C.UTF-8)
    # Rootless podman maps the host user to the same uid inside.
    $podman && args+=(--userns=keep-id)
    if $host_toolchain; then
        args+=(-v "$rustup_home:$rustup_home:ro" -v "$cargo_home:$cargo_home"
            -e "RUSTUP_HOME=$rustup_home" -e "CARGO_HOME=$cargo_home"
            -e "PATH=$cargo_home/bin:$cache/tools/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
    else
        args+=(-e "RUSTUP_HOME=$cache/rustup" -e "CARGO_HOME=$cache/cargo-$1"
            -e "PATH=$cache/cargo-$1/bin:$cache/tools/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
    fi
    log "Starting $image"
    "$engine" "${args[@]}" "$image" sleep infinity >/dev/null
}

# A named user for $uid, as makepkg and rpmbuild expect, once the image has
# useradd.
add_user() {
    as_root "getent group $gid >/dev/null || groupadd -g $gid builder
        getent passwd $uid >/dev/null || useradd -u $uid -g $gid -M -d '$work/home' builder"
}

as_root() {
    "$engine" exec --user 0:0 -e HOME=/root "$container" sh -euc "$1"
}

as_user() {
    "$engine" exec --user "$uid:$gid" -e "HOME=$work/home" -w "$work" "$container" sh -euc "$1"
}

# The source tarball, and for rpm the vendor tarball, in $work.
prepare_sources() {
    if [[ -n $sources ]]; then
        cp "$sources/forti-vpn-$version.tar.gz" "$work/"
        [[ $1 != rpm ]] || cp "$sources/forti-vpn-$version-vendor.tar.gz" "$work/"
    else
        sh "$root/scripts/dist.sh" --working-tree --source-only --output "$work" >/dev/null
    fi
    tar -C "$work/src" -xzf "$work/forti-vpn-$version.tar.gz"
}

new_work() {
    work=$(mktemp -d "$cache/pkg-$1.XXXXXX")
    mkdir -p "$work/home" "$work/src"
    prepare_sources "$1"
}

install_rustup() {
    $host_toolchain && return
    # Checked by path: a distribution's own cargo may already be installed
    # (rpm's builddep installs one) and must not be used for release builds.
    # shellcheck disable=SC2016 # $CARGO_HOME expands in the container.
    as_user '[ -x "$CARGO_HOME/bin/cargo" ] ||
        curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable'
}

# Instructions that only x86-64-v3 CPUs (Haswell and newer) have. Packages
# must run on any x86-64 CPU the target distributions support, and some
# distributions' compilers default to v3 (RHEL 10 and its rebuilds).
require_baseline_x86_64() {
    local binary=$1
    [[ $(uname -m) == x86_64 ]] || return 0
    local count
    count=$(as_user "objdump -d --no-show-raw-insn '$binary' |
        grep -cEw 'shlx|shrx|sarx|rorx|mulx|pdep|pext|bzhi|andn|movbe' || true")
    [[ $count == 0 ]] || die "${binary##*/} uses $count x86-64-v3 instructions: build it with an upstream Rust toolchain"
}

build_deb() {
    new_work deb
    start deb ubuntu:24.04
    log "Installing the Ubuntu 24.04 build dependencies"
    as_root 'export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends ca-certificates curl dpkg-dev gcc \
            libc6-dev libadwaita-1-dev libgtk-4-dev make pkg-config >/dev/null'
    add_user
    install_rustup
    # cargo-deb is cached, built here so it runs on the oldest glibc.
    as_user "command -v cargo-deb >/dev/null 2>&1 ||
        cargo install cargo-deb --locked --root '$cache/tools'"
    log "Building the .deb"
    as_user "cd src/forti-vpn-$version
        make build CARGO_TARGET_DIR='$work/target'
        CARGO_TARGET_DIR='$work/target' cargo deb -p forti-vpn --no-build --locked \
            --output '$work/out'"
    collect "$work/out"/forti-vpn_*.deb
    cleanup
}

build_rpm() {
    new_work rpm
    start rpm almalinux:10
    local spec=$work/src/forti-vpn-$version/packaging/rpm/forti-vpn.spec
    log "Installing the AlmaLinux 10 build dependencies (EPEL and CRB)"
    as_root "dnf install -y -q epel-release dnf-plugins-core >/dev/null
        dnf config-manager --set-enabled crb
        dnf install -y -q binutils cpio curl gzip rpm-build shadow-utils tar >/dev/null
        dnf builddep -y -q '$spec' >/dev/null"
    add_user
    # AlmaLinux's own Rust builds for x86-64-v3 (the RHEL 10 baseline), but
    # this package also installs on Fedora and openSUSE, which run on older
    # CPUs. An upstream toolchain builds portable x86-64 code; the package
    # still links against EL 10's glibc, the oldest target.
    install_rustup
    if [[ -z $sources ]]; then
        log "Vendoring crates with the distribution's cargo"
        as_user "sh src/forti-vpn-$version/scripts/dist.sh --working-tree --vendor-only \
            --output '$work' >/dev/null"
    fi
    log "Building the .rpm"
    as_user "mkdir -p rpmbuild/BUILD rpmbuild/RPMS rpmbuild/SOURCES rpmbuild/SPECS rpmbuild/SRPMS
        cp forti-vpn-$version.tar.gz forti-vpn-$version-vendor.tar.gz rpmbuild/SOURCES/
        rpmbuild -ba --define '_topdir $work/rpmbuild' '$spec'"
    as_user "mkdir -p check && cd check &&
        rpm2cpio ../rpmbuild/RPMS/*/forti-vpn-$version-*.rpm | cpio -idm --quiet ./usr/bin/forti-vpn"
    require_baseline_x86_64 "$work/check/usr/bin/forti-vpn"
    # Debuginfo packages are left out: they belong in a repository.
    collect "$work"/rpmbuild/RPMS/*/forti-vpn-"$version"-*.rpm "$work"/rpmbuild/SRPMS/*.src.rpm
    cleanup
}

build_arch() {
    new_work arch
    start arch archlinux:latest
    local rust=
    $host_toolchain || rust=rust
    log "Installing the Arch build dependencies"
    as_root "pacman -Syu --noconfirm --needed --noprogressbar base-devel adwaita-icon-theme \
        gtk4 hicolor-icon-theme libadwaita $rust >/dev/null"
    add_user
    log "Building the Arch package"
    # The tarball beside the PKGBUILD stands in for the release download.
    # Without debug packages: they would need full debuginfo for every crate.
    as_user "mkdir -p arch out \"\$HOME/.config/pacman\"
        cp src/forti-vpn-$version/packaging/arch/PKGBUILD arch/
        cp forti-vpn-$version.tar.gz arch/
        echo 'OPTIONS+=(!debug)' >\"\$HOME/.config/pacman/makepkg.conf\"
        cd arch
        PKGDEST='$work/out' BUILDDIR='$work/build' makepkg --nodeps --noconfirm --noprogressbar"
    collect "$work"/out/forti-vpn-"$version"-*.pkg.tar.zst
    cleanup
}

built=()
collect() {
    local file
    for file in "$@"; do
        [[ -f $file ]] || die "the build produced no ${file##*/}"
        cp -f "$file" "$output/"
        built+=("${file##*/}")
    done
}

for format in "${formats[@]}"; do
    "build_$format"
done

cd "$output"
# Every package here, not only this run's, so earlier builds stay listed.
find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name '.*' -printf '%f\n' | LC_ALL=C sort |
    xargs -r sha256sum >.SHA256SUMS.tmp
mv -f .SHA256SUMS.tmp SHA256SUMS
log "Built in $output:"
printf '  %s\n' "${built[@]}" >&2
