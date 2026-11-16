#!/usr/bin/env python3
"""Verify packaged ARM64 apps without launching them, on ARM64 or Intel macOS."""
import argparse
import json
from pathlib import Path
import plistlib
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
APPS = ('macrdp.app', 'macrdpController.app')
PROVIDER = 'macrdp.app/Contents/Helpers/macrdpFiles.app'
EXTENSION = PROVIDER + '/Contents/PlugIns/macrdpFiles.appex'
REQUIRED = (
    'macrdp.app/Contents/MacOS/macrdp',
    'macrdpController.app/Contents/MacOS/macrdptray',
    'macrdp.app/Contents/Resources/macrdphud',
    'macrdp.app/Contents/Resources/macrdpshield',
    'macrdp.app/Contents/Resources/ifd-macrdp.bundle/Contents/MacOS/libifd_macrdp.dylib',
    'macrdp.app/Contents/Resources/macrdp-mic.driver/Contents/MacOS/macrdp-mic',
    PROVIDER + '/Contents/MacOS/macrdpFiles',
    EXTENSION + '/Contents/MacOS/macrdpFilesProvider',
)
SOURCE_BUNDLES = {
    'macrdp.app': 'macrdp',
    'macrdpController.app': 'macrdptray',
    PROVIDER: 'macrdpFiles',
    EXTENSION: 'macrdpFilesProvider',
}
# Thin/fat Mach-O, both byte orders, including fat_arch_64 containers.
MACHO_MAGICS = {
    bytes.fromhex(magic) for magic in (
        'feedface', 'cefaedfe', 'feedfacf', 'cffaedfe',
        'cafebabe', 'bebafeca', 'cafebabf', 'bfbafeca',
    )
}


def output(command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()


def is_macho(path):
    with path.open('rb') as stream:
        return stream.read(4) in MACHO_MAGICS


def verify(app_root, source_sha):
    if not re.fullmatch(r'[0-9a-f]{40}', source_sha):
        raise ValueError('source SHA must be a full lowercase commit SHA')
    if output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD']) != source_sha:
        raise ValueError('checked-out source does not match the expected SHA')
    app_root = Path(app_root).resolve()
    for name in APPS:
        app = app_root / name
        if app.is_symlink() or not app.is_dir():
            raise ValueError(f'missing or symlinked app bundle: {name}')
    # Reject links escaping a bundle before opening executable or metadata files.
    # In-bundle framework symlinks are permitted; their real files are scanned.
    for name in APPS:
        app = app_root / name
        for path in app.rglob('*'):
            if path.is_symlink():
                resolved = path.resolve(strict=True)
                if resolved != app and app not in resolved.parents:
                    raise ValueError(f'bundle symlink escapes its app: {path}')
    for relative in REQUIRED:
        path = app_root / relative
        if not path.is_file() or not is_macho(path):
            raise ValueError(f'missing required Mach-O component: {relative}')
    version = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]
    for relative, executable in SOURCE_BUNDLES.items():
        with (app_root / relative / 'Contents/Info.plist').open('rb') as stream:
            info = plistlib.load(stream)
        if info.get('MacrdpSourceCommit') != source_sha:
            raise ValueError(f'bundle source SHA mismatch: {relative}')
        if info.get('CFBundleExecutable') != executable:
            raise ValueError(f'bundle executable mismatch: {relative}')
        if info.get('CFBundleShortVersionString') != version:
            raise ValueError(f'bundle version mismatch: {relative}')
    verified = {}
    seen = set()
    for name in APPS:
        app = app_root / name
        for path in sorted(app.rglob('*')):
            if not path.is_file() or path.resolve() in seen:
                continue
            seen.add(path.resolve())
            if is_macho(path):
                architectures = output(['/usr/bin/lipo', '-archs', str(path)]).split()
                if 'arm64' not in architectures:
                    raise ValueError(f'ARM64 slice missing: {path}: {architectures}')
                verified[str(path.relative_to(app_root))] = architectures
        # Do not weaken signature verification for Intel runners. codesign and
        # lipo inspect ARM64 files without executing the packaged applications.
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    return dict(source_commit=source_sha, version=version, required_components=len(REQUIRED),
                macho_files=verified, signatures_verified=list(APPS), executed_app=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-root', type=Path, required=True)
    parser.add_argument('--source-sha', required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.app_root, args.source_sha), indent=2))
