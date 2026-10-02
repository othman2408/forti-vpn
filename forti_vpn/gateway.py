"""Translate the native server/port form to FortiClient's gateway value."""
import ipaddress
import re
from .backend import VPNError


def split_gateway(value):
    if value.startswith('['):
        match = re.fullmatch(r'\[([^]]+)\](?::([0-9]+))?', value)
        if match:
            return match[1], match[2] or '443'
    if value.count(':') == 1:
        server, port = value.rsplit(':', 1)
        if port.isascii() and port.isdigit():
            return server, port
    return value, '443'


def build_gateway(server, port, original=''):
    server, port = server.strip(), port.strip()
    if not server:
        raise VPNError('Enter a remote gateway hostname or IP address.')
    if not port.isascii() or not port.isdigit() or not 1 <= int(port) <= 65535:
        raise VPNError('Enter a port between 1 and 65535.')
    port = str(int(port))
    # Accept a pasted host:port, but never silently override a different port.
    host, pasted_port = split_gateway(server)
    if host != server:
        if port != '443' and port != pasted_port:
            raise VPNError('Use the Port field to set the remote gateway port.')
        server, port = host, pasted_port
        if not 1 <= int(port) <= 65535:
            raise VPNError('Enter a port between 1 and 65535.')
    try:
        ipaddress.ip_address(server)
    except ValueError:
        try:
            hostname = server.rstrip('.').encode('idna').decode('ascii')
        except UnicodeError:
            hostname = ''
        if not hostname or len(hostname) > 253 or any(
                not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?', part)
                for part in hostname.split('.')):
            raise VPNError('Enter a hostname or IP address without a URL, spaces, or path.')
    if original:
        original_server, original_port = split_gateway(original)
        if server == original_server and port == str(int(original_port)):
            return original
    host = '[' + server + ']' if ':' in server else server
    return host if port == '443' else host + ':' + port
