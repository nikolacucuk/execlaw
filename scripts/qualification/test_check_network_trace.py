import unittest

from check_network_trace import unexpected_addresses


class NetworkTraceTests(unittest.TestCase):
    def test_loopback_fixtures_are_permitted(self):
        trace = '\n'.join(
            [
                'connect(4, {sa_family=AF_INET, sin_addr=inet_addr("127.0.0.1")}, 16) = 0',
                'sendto(4, "fixture", 7, 0, {sa_family=AF_INET6, sin6_addr=inet_pton(AF_INET6, "::1")}, 28) = 7',
                'connect(5, {sa_family=AF_UNIX, sun_path="/tmp/socket"}, 20) = 0',
            ]
        )
        self.assertEqual(unexpected_addresses(trace), [])

    def test_external_and_metadata_destinations_fail(self):
        trace = '\n'.join(
            [
                'connect(4, {sa_family=AF_INET, sin_addr=inet_addr("203.0.113.7")}, 16) = -1 ENETUNREACH',
                'sendto(5, "query", 5, 0, {sa_family=AF_INET, sin_addr=inet_addr("169.254.169.254")}, 16) = -1 ENETUNREACH',
            ]
        )
        self.assertEqual(
            unexpected_addresses(trace),
            [(1, "203.0.113.7"), (2, "169.254.169.254")],
        )


if __name__ == "__main__":
    unittest.main()
