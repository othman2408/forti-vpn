#!/usr/bin/env bash
set -euo pipefail
if [[ $(id -u) == 0 ]]; then
    echo 'Run this installer as your regular desktop user.' >&2
    exit 1
fi
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${XDG_DATA_HOME:-$HOME/.local/share}/forti-vpn"
launcher_dir="$HOME/.local/bin"
/usr/bin/python3 -c 'import gi; gi.require_version("Gtk", "4.0"); gi.require_version("Adw", "1"); from gi.repository import Gtk, Adw'
mkdir -p "$install_dir" "$launcher_dir" "${XDG_DATA_HOME:-$HOME/.local/share}/applications" "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps" "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/symbolic/apps"
cp -R "$project_dir/forti_vpn" "$install_dir/"
# Quote the installation path for bash without evaluating its contents.
printf '#!/usr/bin/env bash\ncd -- %q\nexec /usr/bin/python3 -m forti_vpn "$@"\n' "$install_dir" > "$launcher_dir/forti-vpn"
chmod 755 "$launcher_dir/forti-vpn"
/usr/bin/python3 - "$project_dir/data/io.github.fortivpn.Utility.desktop" "${XDG_DATA_HOME:-$HOME/.local/share}/applications/io.github.fortivpn.Utility.desktop" "$launcher_dir/forti-vpn" <<'PY'
from pathlib import Path
import sys
source, target, executable = sys.argv[1:]
escaped = executable.replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
Path(target).write_text(Path(source).read_text().replace('Exec=forti-vpn', 'Exec="' + escaped + '"'))
PY
cp "$project_dir/data/io.github.fortivpn.Utility.svg" "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps/"
cp "$project_dir/data/io.github.fortivpn.Utility-symbolic.svg" "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/symbolic/apps/"
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" >/dev/null 2>&1 || true
fi
echo "Installed. Open Forti VPN from your applications menu or run $launcher_dir/forti-vpn"
