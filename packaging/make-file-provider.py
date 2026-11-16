#!/usr/bin/env python3
"""Build/sign a sandboxed File Provider helper; no registration or installation."""
import argparse
import os
from pathlib import Path
import plistlib
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    subprocess.run(list(map(str, args)), check=True)


def plist(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(plistlib.dumps(value))


def git_value(*args):
    return subprocess.run(['git', '-C', str(ROOT), *args], capture_output=True, text=True).stdout.strip()


def build(destination, sdk, identity, prefix):
    app = destination/'macrdpFiles.app'
    extension = app/'Contents/PlugIns/macrdpFiles.appex'
    host = app/'Contents/MacOS/macrdpFiles'
    provider = extension/'Contents/MacOS/macrdpFilesProvider'
    host.parent.mkdir(parents=True, exist_ok=True)
    provider.parent.mkdir(parents=True, exist_ok=True)
    common = ['swiftc', '-O', '-sdk', sdk, '-target', 'arm64-apple-macos15.0', '-swift-version', '5']
    core = ROOT/'native/file-provider/Core.swift'
    local = ROOT/'native/file-provider/LocalCopies.swift'
    run(*common, core, local, ROOT/'native/file-provider/Host.swift', '-o', host)
    run(*common, '-parse-as-library', '-application-extension', '-module-name', 'MacrdpFiles',
        '-emit-executable', '-Xlinker', '-e', '-Xlinker', '_NSExtensionMain',
        core, local, ROOT/'native/file-provider/Provider.swift', '-o', provider)
    count = git_value('rev-list', '--count', 'HEAD')
    base = dict(CFBundleVersion=count if count.isdigit() else '1',
                CFBundleShortVersionString=re.search(r'^version = "([^"]+)"', (ROOT/'Cargo.toml').read_text(), re.M)[1], LSMinimumSystemVersion='15.0',
                MacrdpSourceCommit=git_value('rev-parse', 'HEAD'),
                NSAppTransportSecurity=dict(NSAllowsLocalNetworking=True))
    plist(app/'Contents/Info.plist', dict(base, CFBundleIdentifier=prefix+'.macrdp.files',
        CFBundleExecutable='macrdpFiles', CFBundleName='macrdp Files', CFBundleDisplayName='macrdp Files',
        CFBundlePackageType='APPL', LSUIElement=True, NSPrincipalClass='NSApplication'))
    plist(extension/'Contents/Info.plist', dict(base, CFBundleIdentifier=prefix+'.macrdp.files.provider',
        CFBundleExecutable='macrdpFilesProvider', CFBundleName='macrdp Clipboard Files',
        CFBundleDisplayName='macrdp Clipboard Files', CFBundlePackageType='XPC!',
        NSExtension=dict(NSExtensionPointIdentifier='com.apple.fileprovider-nonui',
            NSExtensionPrincipalClass='MacrdpFilesExtension', NSExtensionFileProviderSupportsEnumeration=True,
            NSExtensionFileProviderDownloadPipelineDepth=1)))
    entitlements = destination/'file-provider.entitlements'
    plist(entitlements, {'com.apple.security.app-sandbox': True, 'com.apple.security.network.client': True})
    timestamp = '--timestamp=none' if identity in ['-', 'macrdp-dev'] else '--timestamp'
    for bundle in [extension, app]:
        run('codesign', '--force', '--options', 'runtime', timestamp,
            '--entitlements', entitlements, '--sign', identity, bundle)
    run('codesign', '--verify', '--deep', '--strict', app)
    entitlements.unlink()
    print(app)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--sdk', default=os.environ.get('SWIFT_SDK') or
                        subprocess.check_output(['xcrun', '--show-sdk-path'], text=True).strip())
    parser.add_argument('--identity', default=os.environ.get('CODESIGN_IDENTITY', '-'))
    parser.add_argument('--prefix', default=os.environ.get('BUNDLE_PREFIX', 'com.clintcan'))
    args = parser.parse_args()
    build(args.destination.resolve(), args.sdk, args.identity, args.prefix)
