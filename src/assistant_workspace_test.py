"""Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s src -p '*workspace_test.py'."""
import fcntl
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid

MANAGED = '.assistant-parallel'
BINARY = Path(os.environ.get('FIREFLY_TEST_BINARY', Path(__file__).resolve().parents[1] / 'target/debug/firefly')).resolve()


class ParallelWorkspaceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'workspace'
        self.root.mkdir()
        self.source = self.root / "project ' $中文"
        self.source.mkdir()
        self.git(self.source, 'init', '-q')
        self.git(self.source, 'config', 'user.email', 'test@example.com')
        self.git(self.source, 'config', 'user.name', 'Test')
        (self.source / 'tracked').write_text('committed')
        (self.source / '.gitignore').write_text('ignored\n')
        self.git(self.source, 'add', '.')
        self.git(self.source, 'commit', '-qm', 'base')

    def git(self, path, *args):
        env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
        return subprocess.check_output(['git', '-C', str(path), *args], env=env, stderr=subprocess.PIPE).decode().strip()

    def request(self, **options):
        return dict(root=str(self.root), source=self.source.name, workspace_id=str(uuid.uuid4()),
                    attempt=str(uuid.uuid4()), conversation_id=42, user_id='owner', **options)

    def call(self, request, success=True, remote=False):
        if remote:
            command = ' '.join(shlex.quote(value) for value in [str(BINARY), '--workspace-worker', json.dumps(request)])
            result = subprocess.run(['sh', '-c', command], capture_output=True, timeout=30)
        else:
            result = subprocess.run([str(BINARY), '--workspace-worker', json.dumps(request)], capture_output=True, timeout=30)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(result.returncode == 0, success, (events, result.stderr))
        return events

    def work(self, request):
        return self.root / MANAGED / request['workspace_id'] / 'work'

    def test_dirty_source_unchanged_and_untracked_ignored_copied(self):
        (self.source / 'tracked').write_text('staged')
        self.git(self.source, 'add', 'tracked')
        (self.source / 'tracked').write_text('unstaged')
        (self.source / 'new').write_text('untracked')
        (self.source / 'ignored').write_text('ignored')
        (self.source / 'executable').write_text('#!/bin/sh\n')
        (self.source / 'executable').chmod(0o755)
        before = self.git(self.source, 'diff', '--binary')
        staged = self.git(self.source, 'diff', '--cached', '--binary')
        request = self.request()
        self.call(request, remote=True)
        work = self.work(request)
        self.assertEqual((work / 'tracked').read_text(), 'committed')
        self.assertEqual((work / 'new').read_text(), 'untracked')
        self.assertEqual((work / 'ignored').read_text(), 'ignored')
        self.assertTrue(os.access(work / 'executable', os.X_OK))
        self.assertEqual(self.git(self.source, 'diff', '--binary'), before)
        self.assertEqual(self.git(self.source, 'diff', '--cached', '--binary'), staged)
        self.assertNotEqual((work / 'tracked').stat().st_ino, (self.source / 'tracked').stat().st_ino)

    def test_concurrent_copies_and_commits_are_independent(self):
        requests = [self.request(), self.request()]
        processes = [subprocess.Popen([str(BINARY), '--workspace-worker', json.dumps(req)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                     for req in requests]
        for process in processes:
            stdout, stderr = process.communicate(timeout=30)
            self.assertEqual(process.returncode, 0, (stdout, stderr))
        a, b = map(self.work, requests)
        old = self.git(self.source, 'rev-parse', 'HEAD')
        (a / 'tracked').write_text('A only')
        self.git(a, 'commit', '-qam', 'A')
        self.assertEqual((b / 'tracked').read_text(), 'committed')
        self.assertEqual(self.git(b, 'rev-parse', 'HEAD'), old)
        self.assertEqual(self.git(self.source, 'rev-parse', 'HEAD'), old)

    def test_recovery_and_ready_never_reset_or_require_source(self):
        request = self.request()
        self.call(request)
        (self.work(request) / 'tracked').write_text('finished work')
        shutil.rmtree(self.source)
        # Simulates publication succeeding immediately before the DB update failed.
        self.call(request)
        self.call(dict(request, op='check', ready=True))
        self.assertEqual((self.work(request) / 'tracked').read_text(), 'finished work')
        shutil.rmtree(self.work(request).parent)
        self.call(dict(request, ready=True), success=False)
        self.assertFalse(self.work(request).exists())

    def test_multiple_repos_and_non_git_files(self):
        request = self.request()
        request['source'] = ''
        second = self.root / 'second'
        shutil.copytree(self.source, second)
        (second / 'tracked').write_text('dirty second')
        (self.root / 'notes').write_text('notes')
        events = self.call(request)
        self.assertEqual(len(events[-1]['metadata']['repositories']), 2)
        self.assertEqual((self.work(request) / 'second/tracked').read_text(), 'committed')
        self.assertEqual((self.work(request) / 'notes').read_text(), 'notes')
        self.assertFalse((self.work(request) / MANAGED).exists())

    def test_internal_symlink_is_remapped_and_external_link_rejected(self):
        (self.source / 'link').symlink_to(self.source / 'tracked')
        request = self.request()
        self.call(request)
        self.assertEqual((self.work(request) / 'link').resolve(), self.work(request) / 'tracked')
        (self.source / 'outside').symlink_to(self.root)
        failed = self.request()
        self.call(failed, success=False)
        self.assertFalse(self.work(failed).exists())

    def test_worktree_and_git_redirection_rejected(self):
        linked = self.root / 'linked'
        self.git(self.source, 'worktree', 'add', '--detach', str(linked))
        req = self.request()
        req['source'] = 'linked'
        self.call(req, success=False)
        self.git(self.source, 'worktree', 'remove', str(linked))
        self.git(self.source, 'config', 'core.worktree', str(self.source))
        self.call(self.request(), success=False)

    def test_submodule_empty_repo_and_partial_repo_rejected(self):
        sub = self.source / 'sub'
        sub.mkdir()
        req = self.request()
        req['source'] += '/sub'
        self.call(req, success=False)
        empty = self.root / 'empty'
        empty.mkdir()
        self.git(empty, 'init', '-q')
        req['source'] = 'empty'
        self.call(req, success=False)
        head = self.git(self.source, 'rev-parse', 'HEAD')
        self.git(self.source, 'update-index', '--add', '--cacheinfo', f'160000,{head},sub')
        self.git(self.source, 'commit', '-qm', 'submodule')
        self.call(self.request(), success=False)

    def test_invalid_paths_special_files_and_managed_link_rejected(self):
        for source in ('../', str(self.source), '.assistant-parallel'):
            req = self.request()
            req['source'] = source
            self.call(req, success=False)
        os.mkfifo(self.source / 'fifo')
        self.call(self.request(), success=False)
        (self.source / 'fifo').unlink()
        shutil.rmtree(self.root / MANAGED)
        (self.root / MANAGED).symlink_to(self.root.parent)
        self.call(self.request(), success=False)

    def test_cancellation_and_stale_staging(self):
        req = self.request()
        self.call(dict(req, op='cancel'))
        self.call(req, success=False)
        stage = self.root / MANAGED / '.staging' / req['workspace_id']
        stage.mkdir()
        (stage / 'incomplete').write_text('incomplete')
        self.call(dict(req, attempt=str(uuid.uuid4())))
        self.assertFalse(stage.exists())

    def test_deleted_tracked_file_is_restored_from_commit(self):
        (self.source / 'tracked').unlink()
        req = self.request()
        self.call(req)
        self.assertEqual((self.work(req) / 'tracked').read_text(), 'committed')
        self.assertFalse((self.source / 'tracked').exists())

    def test_manifest_conversation_check_and_explicit_cleanup(self):
        request = self.request()
        self.call(request)
        self.call(dict(request, op='check', conversation_id=999), success=False)
        self.call(dict(request, op='remove', conversation_id=999), success=False)
        self.assertTrue(self.work(request).exists())
        self.call(dict(request, op='remove'))
        self.assertFalse(self.work(request).exists())
        self.assertTrue(self.source.exists())

    def test_cancel_while_waiting_for_host_slot_stops_worker(self):
        req = self.request()
        self.call(dict(req, op='cancel'))
        cancel = self.root / MANAGED / '.cancel' / (req['workspace_id'] + '-' + req['attempt'])
        cancel.unlink()
        handles = []
        try:
            for i in range(2):
                handle = (self.root / MANAGED / '.locks' / f'slot-{i}').open('w')
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                handles.append(handle)
            process = subprocess.Popen([str(BINARY), '--workspace-worker', json.dumps(req)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.addCleanup(lambda: process.kill() if process.poll() is None else None)
            self.call(dict(req, op='cancel'))
            stdout, stderr = process.communicate(timeout=10)
            self.assertNotEqual(process.returncode, 0, stderr)
            self.assertIn('已取消', stdout.decode())
            self.assertFalse(self.work(req).exists())
        finally:
            for handle in handles:
                handle.close()

    def test_copying_ready_workspace_without_an_account(self):
        first = self.request()
        self.call(first)
        second = self.request()
        second['source'] = str(self.work(first).relative_to(self.root))
        second.pop('user_id', None)
        self.call(second)
        self.assertEqual((self.work(second) / 'tracked').read_text(), 'committed')

    def test_native_workers_do_not_require_python_on_path(self):
        tools = Path(self.temp.name) / 'bin'
        tools.mkdir()
        (tools / 'git').symlink_to(shutil.which('git'))
        environment = dict(os.environ, PATH=str(tools))
        request = self.request()
        result = subprocess.run([str(BINARY), '--workspace-worker', json.dumps(request)],
                                env=environment, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, (result.stdout, result.stderr))
        result = subprocess.run([str(BINARY), '--file-worker', str(self.work(request)), 'tracked', 'download'],
                                env=environment, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'OK\ncommitted')

    def test_cancelling_during_git_terminates_its_process_group(self):
        tools = Path(self.temp.name) / 'git-wrapper'
        tools.mkdir()
        marker = Path(self.temp.name) / 'git-pid'
        wrapper = tools / 'git'
        wrapper.write_text('''#!/bin/sh
case "$*" in
  *" reset --hard "*)
    echo $$ > "$FIREFLY_GIT_TEST_PID"
    /bin/sleep 3600 &
    wait
    ;;
  *) exec "$FIREFLY_REAL_GIT" "$@" ;;
esac
''')
        wrapper.chmod(0o755)
        environment = dict(os.environ, PATH=str(tools), FIREFLY_REAL_GIT=shutil.which('git'),
                           FIREFLY_GIT_TEST_PID=str(marker))
        request = self.request()
        process = subprocess.Popen([str(BINARY), '--workspace-worker', json.dumps(request)],
                                   env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        def cleanup():
            if process.poll() is None:
                self.call(dict(request, op='cancel'))
                process.communicate(timeout=10)
        self.addCleanup(cleanup)
        deadline = time.monotonic() + 15
        while not marker.exists() or not marker.read_text().strip():
            self.assertIsNone(process.poll(), 'worker exited before Git reset')
            self.assertLess(time.monotonic(), deadline, 'Git reset did not start')
            time.sleep(0.02)
        pid = int(marker.read_text())
        self.call(dict(request, op='cancel'))
        stdout, stderr = process.communicate(timeout=10)
        self.assertNotEqual(process.returncode, 0, stderr)
        self.assertIn('已取消', stdout.decode())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertFalse(self.work(request).exists())
        self.assertFalse((self.root / MANAGED / '.staging' / request['workspace_id']).exists())


if __name__ == '__main__':
    unittest.main()
