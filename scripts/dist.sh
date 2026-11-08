#!/bin/sh
# Write reproducible release tarballs:
#
#   forti-vpn-VERSION.tar.gz         the source tree, under forti-vpn-VERSION/
#   forti-vpn-VERSION-vendor.tar.gz  vendor/ and .cargo/config.toml; unpacked
#                                    inside the source tree, cargo builds
#                                    offline from them
#
# Entries are sorted, owned by root, and dated SOURCE_DATE_EPOCH (default:
# the last commit's time), so the same tree gives the same bytes.
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/dist.sh [--working-tree] [--source-only | --vendor-only] [-o DIR]

  -o, --output DIR  write the tarballs to DIR (default: dist)
  --working-tree    also pack untracked files that git does not ignore;
                    outside a git checkout, pack everything but build output
  --source-only     write only the source tarball
  --vendor-only     write only the vendor tarball

The source tarball holds the files git tracks, as they are in the working
tree. Set CARGO to pass cargo options, for example CARGO='cargo --offline'.
EOF
}

die() {
    printf 'dist: %s\n' "$*" >&2
    exit 1
}

output=dist
working_tree=false
source=true
vendor=true
while [ $# -gt 0 ]; do
    case $1 in
        -o | --output)
            [ $# -ge 2 ] || die "$1 needs a directory"
            output=$2
            shift 2
            ;;
        --working-tree)
            working_tree=true
            shift
            ;;
        --source-only)
            vendor=false
            shift
            ;;
        --vendor-only)
            source=false
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)
[ -n "$version" ] || die "no version in $root/Cargo.toml"
name=forti-vpn-$version

# Only a checkout of this project counts, not a tree unpacked inside some
# other repository.
in_git() {
    command -v git >/dev/null 2>&1 &&
        [ "$(git -C "$root" rev-parse --show-toplevel 2>/dev/null)" = "$(cd "$root" && pwd -P)" ]
}

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    in_git || die "set SOURCE_DATE_EPOCH outside a git checkout"
    SOURCE_DATE_EPOCH=$(git -C "$root" log -1 --format=%ct)
fi
case $SOURCE_DATE_EPOCH in
    '' | *[!0-9]*) die "SOURCE_DATE_EPOCH must be a number of seconds" ;;
esac

# Untracked paths --working-tree never packs, relative to $root: build
# output, caches, flatpak-builder's directories and generated crate list,
# and `make vendor`'s vendor/ and .cargo/, which would make the archive
# build offline by surprise. .gitignore lists them as well; naming them
# here also covers trees outside git and a .gitignore without them.
untracked_excludes='target dist .cache vendor .cargo .flatpak-builder build-dir flatpak_app repo
packaging/flatpak/cargo-sources.json packaging/arch/src packaging/arch/pkg'

# One path per line, relative to $root.
list_files() {
    if in_git; then
        git -C "$root" -c core.quotePath=false ls-files --cached
        if $working_tree; then
            set --
            for path in $untracked_excludes; do
                set -- "$@" ":(exclude)$path"
            done
            git -C "$root" -c core.quotePath=false ls-files --others --exclude-standard \
                -- . "$@"
        fi
    elif $working_tree; then
        set -- -path ./.git -o -name __pycache__
        for path in $untracked_excludes; do
            set -- "$@" -o -path "./$path"
        done
        (cd "$root" && find . \( "$@" \) -prune -o \( -type f -o -type l \) -print |
            sed 's,^\./,,')
    else
        die "$root is not a git checkout: use --working-tree"
    fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$output"
# Normalized metadata; gzip -n leaves out the name and time of the input.
# tar runs gzip itself, so a failure of either fails the command.
reproducible='--format=gnu --owner=0 --group=0 --numeric-owner --mode=u=rwX,go=rX'

if $source; then
    # Not in a pipeline, so a failure stops here with its own message.
    list_files >"$work/listed.raw" || exit 1
    LC_ALL=C sort -u "$work/listed.raw" >"$work/listed"
    : >"$work/files"
    while IFS= read -r path; do
        case $path in
            \"*) die "file names that git quotes are not supported: $path" ;;
        esac
        # Tracked files deleted in the working tree are left out.
        if [ -e "$root/$path" ] || [ -L "$root/$path" ]; then
            printf '%s\n' "$path" >>"$work/files"
        fi
    done <"$work/listed"
    grep -qx 'Cargo.lock' "$work/files" || die "Cargo.lock is not in the file list"

    # The list is sorted, so the archive is too.
    # shellcheck disable=SC2086
    tar -C "$root" --verbatim-files-from --no-recursion -T "$work/files" \
        --transform="flags=r;s,^,$name/," $reproducible --mtime="@$SOURCE_DATE_EPOCH" \
        --use-compress-program='gzip -9n' -cf "$output/$name.tar.gz.tmp"
    mv -f "$output/$name.tar.gz.tmp" "$output/$name.tar.gz"
    printf '%s\n' "$output/$name.tar.gz"
fi

if $vendor; then
    cargo=${CARGO:-cargo}
    # $cargo holds a command and its options, so it is split on purpose.
    # shellcheck disable=SC2086
    $cargo vendor --quiet --locked --versioned-dirs --manifest-path "$root/Cargo.toml" \
        "$work/vendor/vendor" >/dev/null
    mkdir -p "$work/vendor/.cargo"
    cat >"$work/vendor/.cargo/config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF
    # shellcheck disable=SC2086
    tar -C "$work/vendor" --sort=name $reproducible --mtime="@$SOURCE_DATE_EPOCH" \
        --use-compress-program='gzip -9n' -cf "$output/$name-vendor.tar.gz.tmp" vendor .cargo
    mv -f "$output/$name-vendor.tar.gz.tmp" "$output/$name-vendor.tar.gz"
    printf '%s\n' "$output/$name-vendor.tar.gz"
fi
