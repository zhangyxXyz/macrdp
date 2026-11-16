"""Exercise package selection with fake compilers; never install or sign real apps."""
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE_SHA = 'a' * 40
ARM_TARGET = 'aarch64-apple-darwin'
SWIFT_TARGET = 'arm64-apple-macosx'

TOOL = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['PACKAGING_TEST_ROOT'])
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with (root/'calls.jsonl').open('a') as log:
    log.write(json.dumps([name, args])+'\n')
def value(flag, default=None):
    return args[args.index(flag)+1] if flag in args else default
def binary(path, kind):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(kind)
    path.chmod(0o755)
if name == 'cargo':
    target = value('--target', 'native')
    base = root/'ifd-handler' if '--manifest-path' in args else root
    directory = base/'target'
    if target != 'native': directory /= target
    binary(directory/'release'/('libifd_macrdp.dylib' if '--manifest-path' in args else 'macrdp'), target)
elif name == 'swift':
    target = value('--triple', 'native')
    directory = pathlib.Path(value('--scratch-path'))/target/'release'
    if '--show-bin-path' in args:
        print(directory)
    else:
        binary(directory/value('--product'), target)
elif name == 'git':
    print('a'*40)
elif name != 'codesign':
    raise RuntimeError('unexpected tool '+name)
'''


class PackagingTargetTests(unittest.TestCase):
    def setUp(self):
        # Even the fixture's APP_DIR stays under target, never /Applications.
        (ROOT/'target').mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='packaging-target-tests-', dir=ROOT/'target')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for directory in ['packaging', 'gui', 'ifd-handler', 'tools', 'target/install']:
            (self.root/directory).mkdir(parents=True, exist_ok=True)
        for script in ['packaging/make-app.sh', 'gui/make-tray-app.sh']:
            shutil.copy2(ROOT/script, self.root/script)
        (self.root/'Cargo.toml').write_text('version = "0.10.6"\n')
        for name in ['Info.plist', 'ifd-Info.plist']:
            (self.root/'packaging'/name).write_bytes(plistlib.dumps({'CFBundleIdentifier': '__BUNDLE_ID__'}))
        for name in ['make-audio-plugin.sh', 'install-ifd-handler.sh', 'select-usb-trigger.sh', 'install-audio-plugin.sh']:
            path = self.root/'packaging'/name
            path.write_text('#!/bin/bash\nexit 0\n')
            path.chmod(0o755)
        (self.root/'packaging/make-file-provider.py').write_text('# intentionally no real extension build\n')
        for name in ['cargo', 'swift', 'codesign', 'git']:
            path = self.root/'tools'/name
            path.write_text(TOOL)
            path.chmod(0o755)
        (self.root/'tools/python3').symlink_to(sys.executable)
        self.env = {
            'PATH': str(self.root/'tools') + os.pathsep + '/usr/bin:/bin',
            'PACKAGING_TEST_ROOT': str(self.root),
            'APP_DIR': str(self.root/'target/install'),
            'CODESIGN_IDENTITY': '-',
        }

    def run_script(self, script, success=True):
        result = subprocess.run(['bash', str(self.root/script)], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def calls(self):
        path = self.root/'calls.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def assert_bundle_target(self, rust_target, swift_target):
        app = self.root/'target/macrdp.app'
        self.assertEqual((app/'Contents/MacOS/macrdp').read_text(), rust_target)
        self.assertEqual((app/'Contents/Resources/ifd-macrdp.bundle/Contents/MacOS/libifd_macrdp.dylib').read_text(), rust_target)
        for helper in ['macrdphud', 'macrdpshield']:
            self.assertEqual((app/'Contents/Resources'/helper).read_text(), swift_target)
        controller = self.root/'target/macrdpController.app'
        self.assertEqual((controller/'Contents/MacOS/macrdptray').read_text(), swift_target)
        for bundle in [app, controller]:
            info = plistlib.loads((bundle/'Contents/Info.plist').read_bytes())
            self.assertEqual(info['MacrdpSourceCommit'], SOURCE_SHA)

    def test_native_default_preserves_native_builds_and_seals_source_commit(self):
        self.run_script('packaging/make-app.sh')
        self.run_script('gui/make-tray-app.sh')
        self.assert_bundle_target('native', 'native')
        for tool, args in self.calls():
            if tool in ['cargo', 'swift']:
                self.assertNotIn('--target', args)
                self.assertNotIn('--triple', args)
            if tool == 'cargo':
                self.assertIn('--locked', args)

    def test_arm_build_uses_consistent_target_sdk_and_exact_swift_output(self):
        self.env.update(MACRDP_TARGET=ARM_TARGET, SWIFT_SDK='/SDK with spaces/MacOSX.sdk')
        self.run_script('packaging/make-app.sh')
        self.run_script('gui/make-tray-app.sh')
        self.assert_bundle_target(ARM_TARGET, SWIFT_TARGET)
        builds = {}
        for tool, args in self.calls():
            if tool == 'cargo':
                self.assertEqual(args[args.index('--target')+1], ARM_TARGET)
                self.assertIn('--locked', args)
            elif tool == 'swift':
                self.assertEqual(args[args.index('--triple')+1], SWIFT_TARGET)
                self.assertEqual(args[args.index('--sdk')+1], self.env['SWIFT_SDK'])
                product = args[args.index('--product')+1]
                if '--show-bin-path' not in args:
                    builds[product] = args
                else:
                    self.assertEqual(args[:-1], builds[product])
        self.assertEqual(set(builds), {'macrdphud', 'macrdpshield', 'macrdptray'})

    def test_skip_build_reuses_target_outputs_even_when_native_outputs_exist(self):
        self.run_script('packaging/make-app.sh')
        self.env['MACRDP_TARGET'] = ARM_TARGET
        self.run_script('packaging/make-app.sh')
        (self.root/'calls.jsonl').unlink()
        self.env['SKIP_BUILD'] = '1'
        self.run_script('packaging/make-app.sh')
        self.assertEqual((self.root/'target/macrdp.app/Contents/MacOS/macrdp').read_text(), ARM_TARGET)
        self.assertFalse(any(tool == 'cargo' for tool, _ in self.calls()))
        self.assertTrue(all('--show-bin-path' in args for tool, args in self.calls() if tool == 'swift'))
        for helper in ['macrdphud', 'macrdpshield']:
            self.assertEqual((self.root/'target/macrdp.app/Contents/Resources'/helper).read_text(), SWIFT_TARGET)

    def test_missing_cross_binary_never_falls_back_to_native(self):
        self.run_script('packaging/make-app.sh')
        self.env.update(MACRDP_TARGET=ARM_TARGET, SKIP_BUILD='1')
        result = self.run_script('packaging/make-app.sh', success=False)
        self.assertIn('target/'+ARM_TARGET+'/release/macrdp', result.stderr)

    def test_unsupported_target_is_rejected_before_build_or_sign(self):
        self.env['MACRDP_TARGET'] = 'unknown-target'
        for script in ['packaging/make-app.sh', 'gui/make-tray-app.sh']:
            result = self.run_script(script, success=False)
            self.assertIn('unsupported MACRDP_TARGET', result.stderr)
        self.assertFalse(any(tool in ['cargo', 'swift', 'codesign'] for tool, _ in self.calls()))
