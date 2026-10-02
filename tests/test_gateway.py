import unittest
from forti_vpn.backend import VPNError
from forti_vpn.gateway import build_gateway, split_gateway


class GatewayTests(unittest.TestCase):
    def test_existing_gateway_round_trip(self):
        for value in ('vpn.example.org', 'vpn.example.org:443', 'vpn.example.org:0443', 'vpn.example.org:10443',
                      '192.0.2.4:8443', '[2001:db8::1]:8443', '2001:db8::1'):
            with self.subTest(value=value):
                self.assertEqual(build_gateway(*split_gateway(value), original=value), value)

    def test_custom_port_and_ipv6(self):
        self.assertEqual(build_gateway('vpn.example.org', '10443'), 'vpn.example.org:10443')
        self.assertEqual(build_gateway('2001:db8::1', '8443'), '[2001:db8::1]:8443')
        self.assertEqual(build_gateway('vpn.example.org:8443', '443'), 'vpn.example.org:8443')

    def test_invalid_input_and_conflicting_ports(self):
        for host, port in (('', '443'), ('https://vpn.example.org', '443'),
                           ('vpn.example.org/path', '443'), ('bad host', '443'),
                           ('vpn.example.org', '0'), ('vpn.example.org', '65536'),
                           ('vpn.example.org', 'abc'), ('vpn.example.org', '٤٤٣'),
                           ('vpn.example.org:8443', '10443')):
            with self.subTest(host=host, port=port), self.assertRaises(VPNError):
                build_gateway(host, port)
