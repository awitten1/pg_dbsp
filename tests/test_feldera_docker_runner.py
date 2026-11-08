"""Check Docker runner startup and lifecycle without a Docker daemon."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


RUNNER = Path(__file__).resolve().parents[1] / "experiments/feldera_local/run-docker.sh"


class DockerRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runner = self.root / "experiments/feldera_local/run-docker.sh"
        self.runner.parent.mkdir(parents=True)
        shutil.copy2(RUNNER, self.runner)
        for name in ("program-docker.sql", "postgres.sql", "compare.sql"):
            shutil.copy2(RUNNER.parent / name, self.runner.parent / name)
        self.state = self.root / ".tools/feldera-docker"
        self.state.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "docker.log"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        DOCKER_TEST_LOG=str(self.log),
                        DOCKER_TEST_PAYLOAD=str(self.root / 'payload.json'),
                        DOCKER_TEST_REQUESTS=str(self.root / 'requests.log'),
                        DOCKER_TEST_MARKER=str(self.state / 'started.marker'))
        self.mock("docker", '''#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$DOCKER_TEST_LOG"
case "$*" in
    "volume inspect "*) exit 0 ;;
    *" ps --status running -q pipeline-manager")
        if [[ "${DOCKER_TEST_RUNNING:-0}" == 1 ]]; then echo container-id; fi ;;
    *" exec -T postgres psql "*)
        if [[ "$*" == *" -c "* ]]; then
            if [[ "$*" == *"datname='feldera_manager'"* ]]; then echo 1; fi
        else
            input="$(cat)"
            if [[ "$input" == 'WITH source_totals'* ]]; then echo t; fi
        fi ;;
esac
exit 0
''')
        self.mock("curl", '''#!/usr/bin/env bash
set -euo pipefail
method=GET
url=''
while (( $# )); do
    case "$1" in
        -X) method="$2"; shift ;;
        http://*) url="$1" ;;
    esac
    shift
done
printf '%s %s\\n' "$method" "$url" >> "$DOCKER_TEST_REQUESTS"
case "$method $url" in
    "PUT "*) jq . > "$DOCKER_TEST_PAYLOAD" ;;
    "POST "*/start)
        [[ -f "$DOCKER_TEST_MARKER" ]]
        if [[ "${DOCKER_TEST_FAIL_START:-0}" == 1 ]]; then exit 22; fi ;;
    */query) echo 'east west' ;;
    *) echo '{"program_status":"Success","deployment_status":"Running","deployment_error":null}' ;;
esac
''')

    def mock(self, name, source):
        path = self.bin / name
        path.write_text(source)
        path.chmod(0o755)

    def run_script(self, *args):
        return subprocess.run(["bash", str(self.runner), *args], env=self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def test_help_does_not_contact_docker(self):
        result = self.run_script("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage:", result.stdout)
        self.assertEqual(self.calls(), [])

    def test_unknown_command_does_not_contact_docker(self):
        self.assertNotEqual(self.run_script("unknown").returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_start_streams_pipeline_and_waits_for_snapshot(self):
        self.state.rmdir()
        result = self.run_script("start")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Initial snapshot", result.stdout)
        self.assertTrue((self.state / 'started.marker').exists())
        payload = json.loads((self.root / 'payload.json').read_text())
        self.assertEqual(payload['program_code'],
                         (self.runner.parent / 'program-docker.sql').read_text())
        self.assertNotIn('__PG_USER__', payload['program_code'])
        self.assertIn('@postgres:5432/postgres', payload['program_code'])
        self.assertEqual(payload['runtime_config']['workers'], 2)
        self.assertEqual(payload['program_config']['profile'], 'optimized')

    def test_failed_start_keeps_guard_against_reusing_partial_snapshot(self):
        self.env['DOCKER_TEST_FAIL_START'] = '1'
        self.assertEqual(self.run_script('start').returncode, 22)
        self.assertTrue((self.state / 'started.marker').exists())
        retry = self.run_script('start')
        self.assertEqual(retry.returncode, 1)
        self.assertIn('reset', retry.stderr)

    def test_repeated_start_reports_running_pipeline_without_redeploying(self):
        (self.state / 'started.marker').touch()
        self.env['DOCKER_TEST_RUNNING'] = '1'
        result = self.run_script('start')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('already running', result.stdout)
        self.assertFalse(any(' up ' in call for call in self.calls()))
        requests = (self.root / 'requests.log').read_text()
        self.assertNotIn('PUT ', requests)
        self.assertNotIn('POST ', requests)

    def test_stopped_snapshot_requires_explicit_reset(self):
        marker = self.state / "started.marker"
        marker.touch()
        result = self.run_script("start")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("reset", result.stderr)
        self.assertFalse(any(" up " in call or call.startswith("volume rm") for call in self.calls()))
        self.assertTrue(marker.exists())

    def test_reset_removes_only_database_and_runner_volumes(self):
        marker = self.state / "started.marker"
        marker.touch()
        result = self.run_script("reset")
        self.assertEqual(result.returncode, 0, result.stderr)
        removed = [call for call in self.calls() if call.startswith("volume rm ")]
        self.assertEqual(removed, ["volume rm pg_dbsp_feldera_docker_pgdata",
                                   "volume rm pg_dbsp_feldera_docker_runner_state"])
        self.assertFalse(any("prune" in call or "--volumes" in call for call in self.calls()))
        self.assertFalse(marker.exists())

    def test_stop_preserves_snapshot_marker_and_volumes(self):
        marker = self.state / "started.marker"
        marker.touch()
        result = self.run_script("stop")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        self.assertTrue(any(call.endswith(" stop") for call in self.calls()))
        self.assertFalse(any(call.startswith("volume ") for call in self.calls()))

if __name__ == "__main__":
    unittest.main()
