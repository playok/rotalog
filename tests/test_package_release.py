"""Check release packaging independently of host architecture."""
import hashlib
import pathlib
import subprocess
import sys
import tarfile
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'scripts/package-release.py'


class ReleasePackageTest(unittest.TestCase):
    def test_all_release_target_names_layout_permissions_and_checksums(self):
        for target in ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
                       'x86_64-apple-darwin', 'aarch64-apple-darwin']:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                binary = root / 'target' / target / 'release' / 'rotalog'
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b'packaging fixture; not an executable')
                for name in ['LICENSE', 'README.md', 'README.en.md']:
                    (root / name).write_text(name)
                subprocess.run([sys.executable, str(SCRIPT), '--version', '0.1.0',
                                '--target', target], cwd=root, check=True, capture_output=True)
                archive = root / 'dist' / f'rotalog-v0.1.0-{target}.tar.gz'
                checksum = archive.with_name(archive.name + '.sha256').read_text()
                self.assertEqual(checksum, f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n')
                with tarfile.open(archive) as tar:
                    self.assertEqual(sorted(tar.getnames()), ['LICENSE', 'README.en.md', 'README.md', 'rotalog'])
                    self.assertEqual(tar.getmember('rotalog').mode, 0o755)
                    self.assertEqual(tar.extractfile('rotalog').read(), binary.read_bytes())
                    self.assertEqual(tar.getmember('LICENSE').mode, 0o644)


if __name__ == '__main__':
    unittest.main()
