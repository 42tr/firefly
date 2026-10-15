"""Run with: python3 -m unittest discover -s src/api -p '*_test.py'."""
import io
import json
import os
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile


BINARY = Path(os.environ.get('FIREFLY_TEST_BINARY', Path(__file__).resolve().parents[2] / 'target/debug/firefly')).resolve()


class WorkspaceFilesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'workspace'
        self.root.mkdir()
        (self.root / 'dist').mkdir()
        (self.root / 'dist' / 'empty').mkdir()
        self.name = "报告 ' $test\n.txt"
        (self.root / 'dist' / self.name).write_bytes(b'build output\x00\xff')
        (self.root / '.hidden').write_text('hidden')
        self.outside = Path(self.temp.name) / 'secret'
        self.outside.write_text('never include')
        (self.root / 'dist' / 'escape').symlink_to(self.outside)
        (self.root / 'escape-dir').symlink_to(self.root.parent, target_is_directory=True)

    def call(self, path, operation):
        return subprocess.run([str(BINARY), '--file-worker',
                               str(self.root), path, operation], capture_output=True, timeout=10)

    def test_listing_metadata_hidden_and_sort(self):
        result = self.call('', 'list')
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = json.loads(result.stdout)['entries']
        self.assertEqual([item['name'] for item in entries], ['dist', '.hidden'])
        self.assertTrue(entries[0]['is_dir'])
        self.assertEqual(entries[1]['size'], 6)
        self.assertGreater(entries[1]['modified_at'], 0)
        nested = json.loads(self.call('dist', 'list').stdout)['entries']
        self.assertEqual(nested[1]['path'], 'dist/' + self.name)

    def test_file_download_exact_bytes(self):
        result = self.call('dist/' + self.name, 'download')
        self.assertEqual(result.stdout, b'OK\nbuild output\x00\xff')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_shell_quoted_arguments(self):
        command = ' '.join(shlex.quote(value) for value in
                           [str(BINARY), '--file-worker', str(self.root), 'dist/' + self.name, 'download'])
        result = subprocess.run(['sh', '-c', command], capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'OK\nbuild output\x00\xff')

    def test_zip_nested_empty_directory_and_no_symlinks(self):
        result = self.call('dist', 'archive')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout[:3], b'OK\n')
        with zipfile.ZipFile(io.BytesIO(result.stdout[3:])) as archive:
            self.assertEqual(set(archive.namelist()), {'dist/', 'dist/empty/', 'dist/' + self.name})
            self.assertEqual(archive.read('dist/' + self.name), b'build output\x00\xff')

    def test_escape_missing_and_type_changes_fail_before_header(self):
        for path, operation in [('../secret', 'download'), (str(self.outside), 'download'),
                                ('dist/escape', 'download'), ('escape-dir', 'list'),
                                ('missing', 'download'), ('dist', 'download'),
                                ('.hidden', 'archive')]:
            with self.subTest(path=path, operation=operation):
                result = self.call(path, operation)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, b'')

    def test_special_files_are_not_downloadable(self):
        os.mkfifo(self.root / 'pipe')
        self.assertNotEqual(self.call('pipe', 'download').returncode, 0)
        self.assertNotIn('pipe', [item['name'] for item in json.loads(self.call('', 'list').stdout)['entries']])

    def test_temporary_images_validate_format_and_preserve_bytes(self):
        samples = {
            '图片 1.PNG': b'\x89PNG\r\n\x1a\nimage data',
            'image.jpg': b'\xff\xd8\xffimage data',
            'image.gif': b'GIF89aimage data',
            'image.webp': b'RIFF\x00\x00\x00\x00WEBPimage data',
            'image.avif': b'\x00\x00\x00\x20ftypavifimage data',
        }
        for name, data in samples.items():
            with self.subTest(name=name):
                (self.root / name).write_bytes(data)
                result = self.call(name, 'image')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, b'OK\n' + data)

    def test_temporary_images_reject_other_files_and_symlinks_before_header(self):
        (self.root / 'fake.png').write_text('secret data')
        (self.root / 'wrong.jpg').write_bytes(b'\x89PNG\r\n\x1a\n')
        (self.root / 'vector.svg').write_text('<svg onload="alert(1)"/>')
        (self.root / 'link.png').symlink_to(self.outside)
        os.mkfifo(self.root / 'pipe.png')
        for path in ['fake.png', 'wrong.jpg', 'vector.svg', 'link.png', 'pipe.png',
                     '.hidden', 'dist', '../secret', str(self.outside), 'escape-dir/secret', 'missing.png']:
            with self.subTest(path=path):
                result = self.call(path, 'image')
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, b'')


if __name__ == '__main__':
    unittest.main()
