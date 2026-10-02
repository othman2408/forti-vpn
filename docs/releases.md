# Releases

Build from the commit you want to release:

```bash
/usr/bin/python3 scripts/build-deb.py
```

The version comes from `forti_vpn/__init__.py`. The build writes a `.deb` and
`SHA256SUMS` to `dist/`. No root access or FortiClient files are needed to build.

Check installation, launch, and removal on Ubuntu 26.04. Verify the checksum:

```bash
cd dist
sha256sum -c SHA256SUMS
sudo apt install ./forti-vpn_0.1.0_all.deb
```

The package installs the app and declares its GTK dependencies. FortiClient
is installed separately. Removing the app does not remove FortiClient or its
profiles, or disconnect an established VPN.

Create a GitHub release for the matching `vVERSION` tag. Attach the `.deb`
and `SHA256SUMS`, describe the supported Ubuntu and FortiClient versions,
and save as a draft before publishing.
