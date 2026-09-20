import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("connection_diagnostics", Path(__file__).parents[1]/"connection_diagnostics.py")
diagnostics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diagnostics)


class ClockBoundsTests(unittest.TestCase):
    def test_identifies_long_delivery_without_assuming_symmetric_network(self):
        # Host clock is 20 seconds ahead. The two paths are deliberately asymmetric.
        pairs = [
            {"direction":"client_to_host", "sent_us":1000, "received_us":20051000},
            {"direction":"host_to_client", "sent_us":20100000, "received_us":250000},
            {"direction":"host_to_client", "sent_us":20200000, "received_us":6200000},
        ]
        bounds = diagnostics.clock_bounds(pairs)
        self.assertEqual(bounds["status"], "bounded")
        self.assertLessEqual(bounds["lower_us"], 20000000)
        self.assertGreaterEqual(bounds["upper_us"], 20000000)
        self.assertEqual(diagnostics.delivery_bounds(pairs[-1], bounds), [5850000, 6050000])

    def test_missing_or_inconsistent_evidence_never_becomes_zero_latency(self):
        self.assertEqual(diagnostics.clock_bounds([])["status"], "insufficient_bidirectional_packets")
        bad = diagnostics.clock_bounds([
            {"direction":"client_to_host", "sent_us":20, "received_us":10},
            {"direction":"host_to_client", "sent_us":20, "received_us":10},
        ])
        self.assertEqual(bad["status"], "inconsistent_clock_bounds")
        self.assertIsNone(diagnostics.delivery_bounds({}, bad))

    def test_packet_delivery_uses_transport_time_not_recorder_delay(self):
        sent = {"phase":"QuicPacketSent", "group":42, "space":4, "path":0, "packet":0, "packet_valid":1, "at_us":500, "source_at_us":100}
        received = dict(sent, phase="QuicPacketReceived", at_us=1200, source_at_us=200)
        pair = diagnostics.match_packets([sent], [received])[0]
        self.assertEqual((pair["sent_us"], pair["received_us"]), (100, 200))

    def test_packet_numbers_are_scoped_to_connection_space_and_path(self):
        event = {"phase":"QuicPacketSent", "group":42, "space":4, "path":0, "packet":3, "packet_valid":1, "at_us":100}
        duplicate = dict(event, at_us=110)
        other = dict(event, path=1)
        absent = dict(event, packet_valid=0)
        result = diagnostics.unique_packets([event, duplicate, other, absent], "QuicPacketSent")
        self.assertEqual(len(result), 1)
        self.assertEqual(next(iter(result)), (42,4,1,3))


if __name__ == "__main__":
    unittest.main()
