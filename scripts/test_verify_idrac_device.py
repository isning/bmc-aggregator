"""Probe contract: status GETs only and selected output fields only."""

import unittest
from unittest.mock import patch

from verify_idrac_device import probe


class ProbeTests(unittest.TestCase):
    def test_only_status_resources_are_requested_and_sensitive_fields_are_omitted(self):
        document = {"available": True, "password": "do-not-print", "power_error": "private"}
        features = {"features": [{"id": "video", "backend": "idrac", "token": "private"}]}
        with patch("verify_idrac_device.request", side_effect=[features] + [document] * 4) as request:
            report = probe("http://localhost:8080/", "lab/device")
        self.assertEqual(
            [call.args[0] for call in request.call_args_list],
            ["http://localhost:8080/bmc/lab%2Fdevice/api/features"]
            + [f"http://localhost:8080/bmc/lab%2Fdevice/api/feature/{feature}/status"
               for feature in ("video", "hid", "power", "msd")],
        )
        self.assertEqual(report["registered_features"], [{"id": "video", "backend": "idrac"}])
        self.assertTrue(report["power"]["has_error"])
        self.assertNotIn("private", str(report))
        self.assertNotIn("do-not-print", str(report))

    def test_power_fault_presence_uses_current_contract_without_exposing_details(self):
        for power, expected in [
            ({"fault": {"code": "status_failed", "message": "private-detail"}}, True),
            ({"fault": None, "power_error": "private-detail"}, True),
            ({"fault": None, "power_error": None}, False),
        ]:
            with self.subTest(power=power), patch("verify_idrac_device.request", side_effect=[
                {"features": []}, {}, {}, power, {},
            ]):
                report = probe("http://localhost:8080", "lab")
            self.assertEqual(report["power"]["has_error"], expected)
            self.assertNotIn("private-detail", str(report))
            self.assertNotIn("status_failed", str(report))

    def test_credentials_and_non_http_urls_are_rejected_before_any_request(self):
        for url in ("file:///etc/passwd", "http://user:password@localhost", "http://localhost?token=private"):
            with self.subTest(url=url), patch("verify_idrac_device.request") as request:
                with self.assertRaises(ValueError):
                    probe(url, "lab")
                request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
