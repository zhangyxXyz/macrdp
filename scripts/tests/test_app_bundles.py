import importlib.util
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).resolve().parents[1] / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bundles = load('check_app_bundles', 'check-app-bundles.py')
provider = load('check_file_provider', 'check-file-provider.py')
SHA = 'a' * 40
MACHO = bytes.fromhex('cffaedfe') + b'fixture'


class AppBundleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'Cargo.toml').write_text('version = "0.10.6"\n')
        for relative in bundles.REQUIRED:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(MACHO)
        for relative, executable in bundles.SOURCE_BUNDLES.items():
            self.write_info(relative, executable)
        self.architectures = {}
        self.head = SHA
        self.bad_signature = False
        self.commands = []
        self.root_patch = patch.object(bundles, 'ROOT', self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        self.run_patch = patch.object(bundles.subprocess, 'run', side_effect=self.run_command)
        self.run_patch.start()
        self.addCleanup(self.run_patch.stop)

    def write_info(self, relative, executable, sha=SHA, version='0.10.6'):
        path = self.root / relative / 'Contents/Info.plist'
        path.write_bytes(plistlib.dumps(dict(CFBundleExecutable=executable,
            CFBundleShortVersionString=version, MacrdpSourceCommit=sha)))

    def run_command(self, command, **kwargs):
        self.commands.append(command)
        if command[0] == 'git':
            result = self.head
        elif command[:2] == ['/usr/bin/lipo', '-archs']:
            result = self.architectures.get(command[-1], 'arm64')
        elif command[:4] == ['/usr/bin/codesign', '--verify', '--deep', '--strict']:
            if self.bad_signature:
                raise subprocess.CalledProcessError(1, command)
            result = ''
        else:
            self.fail(f'unexpected command or attempted application execution: {command}')
        return subprocess.CompletedProcess(command, 0, result, '')

    def test_all_components_and_extra_macho_verified_without_running_apps(self):
        extra = self.root / 'macrdp.app/Contents/Resources/extra.dylib'
        extra.write_bytes(MACHO)
        self.architectures[str(extra.resolve())] = 'x86_64 arm64'
        report = bundles.verify(self.root, SHA)
        self.assertEqual(report['required_components'], 8)
        self.assertEqual(len(report['macho_files']), 9)
        self.assertEqual(report['macho_files']['macrdp.app/Contents/Resources/extra.dylib'],
                         ['x86_64', 'arm64'])
        self.assertFalse(report['executed_app'])
        self.assertEqual(len([c for c in self.commands if c[0] == '/usr/bin/codesign']), 2)

    def test_every_required_component_is_mandatory(self):
        for relative in bundles.REQUIRED:
            with self.subTest(relative=relative):
                path = self.root / relative
                path.unlink()
                with self.assertRaisesRegex(ValueError, 'missing required Mach-O'):
                    bundles.verify(self.root, SHA)
                path.write_bytes(MACHO)

    def test_required_executable_cannot_be_a_script(self):
        (self.root / bundles.REQUIRED[0]).write_bytes(b'#!/bin/sh\nexit 0\n')
        with self.assertRaisesRegex(ValueError, 'missing required Mach-O'):
            bundles.verify(self.root, SHA)

    def test_wrong_architecture_in_unlisted_nested_binary_is_rejected(self):
        extra = self.root / 'macrdpController.app/Contents/Resources/unexpected.dylib'
        extra.parent.mkdir(parents=True)
        extra.write_bytes(MACHO)
        self.architectures[str(extra.resolve())] = 'x86_64'
        with self.assertRaisesRegex(ValueError, 'ARM64 slice missing'):
            bundles.verify(self.root, SHA)

    def test_all_bundle_source_markers_must_match_checkout(self):
        for relative, executable in bundles.SOURCE_BUNDLES.items():
            with self.subTest(relative=relative):
                self.write_info(relative, executable, sha='b' * 40)
                with self.assertRaisesRegex(ValueError, 'source SHA mismatch'):
                    bundles.verify(self.root, SHA)
                self.write_info(relative, executable)
        self.head = 'c' * 40
        with self.assertRaisesRegex(ValueError, 'checked-out source'):
            bundles.verify(self.root, SHA)

    def test_invalid_sha_version_and_executable_metadata_rejected(self):
        with self.assertRaisesRegex(ValueError, 'full lowercase'):
            bundles.verify(self.root, 'short-sha')
        self.write_info('macrdp.app', 'other')
        with self.assertRaisesRegex(ValueError, 'executable mismatch'):
            bundles.verify(self.root, SHA)
        self.write_info('macrdp.app', 'macrdp', version='0.10.5')
        with self.assertRaisesRegex(ValueError, 'version mismatch'):
            bundles.verify(self.root, SHA)

    def test_signature_failure_is_never_ignored(self):
        self.bad_signature = True
        with self.assertRaises(subprocess.CalledProcessError):
            bundles.verify(self.root, SHA)

    def test_symlink_cannot_import_an_external_executable(self):
        outside = self.root / 'external-binary'
        outside.write_bytes(MACHO)
        (self.root / 'macrdp.app/Contents/Resources/escape').symlink_to(outside)
        with self.assertRaisesRegex(ValueError, 'symlink escapes'):
            bundles.verify(self.root, SHA)

    def test_file_provider_checks_select_the_native_test_architecture(self):
        for architecture in ('arm64', 'x86_64'):
            with patch.object(provider.platform, 'machine', return_value=architecture):
                self.assertEqual(provider.host_target(), architecture + '-apple-macos15.0')
        with patch.object(provider.platform, 'machine', return_value='unknown'):
            with self.assertRaisesRegex(ValueError, 'unsupported macOS test host'):
                provider.host_target()


if __name__ == '__main__':
    unittest.main()
