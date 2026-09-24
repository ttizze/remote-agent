import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("diagnostics", Path(__file__).parents[1]/"connection_diagnostics.py")
diagnostics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diagnostics)


def row(operation, message, pid=1):
    return dict(operation=operation, message=message, pid=pid, timestampMs=1, revision="fixture")


class ConnectionDiagnosticsTests(unittest.TestCase):
    def test_recovered_old_capture_does_not_replace_latest_or_invent_summary(self):
        rows = [row("client.connection.timeline", "trace=2 attempt=20 connection=3 dropped=0 platform=Ios started_at_ms=200 recovered=false"),
                row("client.connection.timeline", "trace=1 attempt=10 connection=0 dropped=0 platform=Ios started_at_ms=100 recovered=true"),
                row("client.connection.event", "trace=1 seq=1 phase=ResumeStart group=10 stream=0 at_us=5 value=0"),
                row("client.connection.event", "trace=1 seq=2 phase=ResumeFailed group=10 stream=0 at_us=2005 value=2000")]
        self.assertEqual(diagnostics.analyze(rows)["metadata"]["trace"], 2)
        recovered = diagnostics.analyze(rows, trace_id=1)
        self.assertEqual(recovered["attempts"][0]["result"], "ResumeFailed")
        self.assertEqual(recovered["attempts"][0]["core_elapsed_us"], 2000)
        self.assertIsNone(recovered["summary"])
        self.assertEqual(recovered["host_events"], [])

    def test_restarted_network_survey_is_not_combined_into_one_latency(self):
        events = [dict(phase="start", at_us=1), dict(phase="start", at_us=100), dict(phase="end", at_us=110)]
        self.assertIsNone(diagnostics.interval(events, "start", "end"))

    def test_correlates_only_matching_process_connection_and_stream(self):
        rows = [row("client.connection.timeline", "trace=1 attempt=2 connection=3 dropped=0 platform=Ios client_revision=fixture started_at_ms=1 recovered=false"),
                row("host.connection.link", "trace=1 connection=3 session=4"),
                row("host.rpc.performance", "session=4 stream=8 method=scope decode_us=2 queue_us=3 handle_encode_us=4 write_us=1"),
                row("host.rpc.performance", "session=4 stream=8 method=wrong decode_us=200 queue_us=300 handle_encode_us=400 write_us=1", pid=2)]
        for seq, (phase, group, stream, at) in enumerate([
            ("ResumeStart", 2, 0, 10), ("RequestOpened", 3, 8, 18), ("RequestSent", 3, 8, 20),
            ("ResponseFirstRead", 3, 8, 110), ("ResponseReceived", 3, 8, 120),
            ("RequestSent", 9, 8, 130),
        ]):
            rows.append(row("client.connection.event", f"trace=1 seq={seq} phase={phase} group={group} stream={stream} at_us={at} value=10"))
        result = diagnostics.analyze(rows)
        self.assertEqual(len(result["requests"]), 1)
        request = result["requests"][0]
        self.assertEqual(request["method"], "scope")
        self.assertEqual(request["first_read_after_send_us"], 90)
        self.assertEqual(request["outside_host_interval_us"], 92)
        self.assertIsNone(request["max_observed_wake_to_poll_us"])
        self.assertIsNone(result["ui_to_list_state_us"])

    def test_missing_start_or_reply_is_never_reported_as_zero_latency(self):
        rows = [row("client.connection.timeline", "trace=1 attempt=2 connection=3 dropped=100 platform=Ios client_revision=fixture started_at_ms=1 recovered=false"),
                row("client.connection.event", "trace=1 seq=101 phase=RequestSent group=3 stream=8 at_us=10 value=5")]
        result = diagnostics.analyze(rows)
        self.assertFalse(result["resume_start_observed"])
        self.assertEqual(result["client_dropped_total"], 100)
        self.assertEqual(result["requests"], [])
        self.assertIsNone(result["max_client_runtime_gap_us"])
        rows.append(row("client.connection.event", "trace=1 seq=100 phase=ResumeStart group=2 stream=0 at_us=1 value=0"))
        request = diagnostics.analyze(rows)["requests"][0]
        self.assertIsNone(request["host_local_us"])
        self.assertIsNone(request["response_after_send_us"])
        self.assertIsNone(request["outside_host_interval_us"])


if __name__ == "__main__":
    unittest.main()
