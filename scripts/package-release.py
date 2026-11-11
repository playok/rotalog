#!/usr/bin/env python3
"""Package a release binary without embedding machine-specific paths."""
import argparse
import hashlib
import pathlib
import re
import tarfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--version', required=True)
parser.add_argument('--target', required=True)
args = parser.parse_args()
for value in [args.version, args.target]:
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.+-]*', value):
        parser.error('version and target must be filename-safe identifiers')
binary = pathlib.Path('target') / args.target / 'release' / 'rotalog'
if not binary.is_file():
    parser.error(f'release binary is missing: {binary}')
out = pathlib.Path('dist')
out.mkdir(exist_ok=True)
archive = out / f'rotalog-v{args.version}-{args.target}.tar.gz'
with tarfile.open(archive, 'w:gz') as tar:
    for source, name in [(binary, 'rotalog'), (pathlib.Path('LICENSE'), 'LICENSE'),
                         (pathlib.Path('README.md'), 'README.md'),
                         (pathlib.Path('README.en.md'), 'README.en.md')]:
        info = tar.gettarinfo(str(source), arcname=name)
        info.uid = info.gid = 0
        info.uname = info.gname = ''
        info.mode = 0o755 if name == 'rotalog' else 0o644
        with source.open('rb') as stream:
            tar.addfile(info, stream)
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_name(archive.name + '.sha256').write_text(f'{checksum}  {archive.name}\n')
print(archive)
