import hashlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('fork_release', Path(__file__).resolve().parents[1] / 'fork_release.py')
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)
SHA = 'a' * 40

def release(tag, number, draft=False, owned=True):
    return dict(id=number, tag_name=tag, draft=draft, published_at=f'2026-11-{number:02d}T00:00:00Z',
                body=f'{r.MARKER}sha={SHA} -->' if owned else 'upstream release', assets=[])


class ReleaseTests(unittest.TestCase):
    def test_asset_checksum_commit_and_bundle_are_verified(self):
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as bundle:
            for name in ['macrdp.app/Contents/MacOS/macrdp', 'macrdpController.app/Contents/MacOS/macrdptray',
                         'CAPABILITIES.json', 'README.md', 'README_CN.md',
                         'macrdp.app/Contents/Helpers/macrdpFiles.app/Contents/MacOS/macrdpFiles',
                         'macrdp.app/Contents/Helpers/macrdpFiles.app/Contents/PlugIns/macrdpFiles.appex/Contents/MacOS/macrdpFilesProvider']:
                bundle.writestr(name, 'test')
            bundle.writestr('SOURCE_COMMIT', SHA + '\n')
        data = data.getvalue()
        sums = (hashlib.sha256(data).hexdigest() + '  app.zip\n').encode()
        r.verify_files(data, sums, SHA, 'app.zip')
        for archive, checksum, sha in [(data + b'corrupt', sums, SHA), (data, sums, 'b' * 40), (data, b'bad', SHA)]:
            with self.assertRaises(ValueError):
                r.verify_files(archive, checksum, sha, 'app.zip')

    def test_missing_embedded_provider_executable_rejects_release(self):
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as bundle:
            for name in ['macrdp.app/Contents/MacOS/macrdp', 'macrdpController.app/Contents/MacOS/macrdptray',
                         'CAPABILITIES.json', 'README.md', 'README_CN.md',
                         'macrdp.app/Contents/Helpers/macrdpFiles.app/Contents/MacOS/macrdpFiles']:
                bundle.writestr(name, 'test')
            bundle.writestr('SOURCE_COMMIT', SHA + '\n')
        archive = data.getvalue()
        sums = (hashlib.sha256(archive).hexdigest() + '  app.zip\n').encode()
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            r.verify_files(archive, sums, SHA, 'app.zip')

    def test_cleanup_deletes_only_older_owned_published_releases_and_their_tags(self):
        older, current = release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)
        other, draft = release('v0.10.3', 3, owned=False), release('v0.10.6-dev', 4, draft=True)
        records = {x['id']: x for x in [older, current, other, draft]}
        calls = []
        def gh(*args, **kwargs):
            calls.append(args)
            return json.dumps([[older, current, other, draft]]).encode() if '--paginate' in args else b''
        with patch.object(r, 'gh', gh), patch.object(r, 'verify_release') as verify, patch.object(r, 'api', lambda path: records[int(path.rsplit('/', 1)[1])]):
            r.cleanup('owner/repo', False)
        verify.assert_called_once_with('owner/repo', current)
        self.assertEqual([c for c in calls if c[:2] == ('release', 'delete')],
                         [('release', 'delete', 'v0.10.4-dev', '--repo', 'owner/repo', '--cleanup-tag', '--yes')])
        self.assertFalse(any('git/refs' in str(c) for c in calls))

    def test_dry_run_and_failed_verification_never_delete(self):
        records = [release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)]
        for dry, error in [(True, None), (False, ValueError('broken retained release'))]:
            with patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, patch.object(r, 'verify_release', side_effect=error), patch.object(r, 'api', side_effect=lambda path: records[int(path.rsplit('/', 1)[1]) - 1]):
                if error:
                    with self.assertRaises(ValueError): r.cleanup('owner/repo', dry)
                else:
                    r.cleanup('owner/repo', dry)
                self.assertFalse(any('DELETE' in call.args or call.args[:2] == ('release', 'delete') for call in gh.call_args_list))

    def test_changed_release_aborts_cleanup(self):
        records = [release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)]
        with patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, patch.object(r, 'verify_release'), patch.object(r, 'api', return_value={**records[1], 'draft': True}):
            with self.assertRaises(ValueError): r.cleanup('owner/repo', False)
            self.assertEqual(gh.call_count, 1)

    def test_unowned_and_nonversion_releases_are_not_candidates(self):
        self.assertIsNone(r.owner_sha(release('nightly', 1)))
        self.assertIsNone(r.owner_sha(release('v0.10.5', 1, owned=False)))
        self.assertEqual(r.owner_sha(release('v0.10.5-dev', 1)), SHA)

    def test_notes_require_reviewed_changelog(self):
        template = '**{tag}**\n[Changes]({changelog_url})\nSource: {source_sha}\n'
        def read(path, *args, **kwargs):
            return template if path.name == 'RELEASE_NOTES.md' else '## v0.10.5-dev\n\nVerified facts.\n'
        with patch.object(Path, 'read_text', read):
            text = r.notes('v0.10.5-dev', SHA, 'owner/repo')
            self.assertIn('CHANGELOG.md#v0105-dev', text)
            self.assertNotIn('Verified facts.', text)
            with self.assertRaises(ValueError): r.notes('v0.10.6-dev', SHA, 'owner/repo')

    def test_failed_uploaded_asset_verification_keeps_release_draft(self):
        import tempfile
        tag = 'v0.10.5-dev'
        record = release(tag, 1, draft=True)
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, f'macrdp-{tag}-aarch64-apple-darwin-app.zip').write_bytes(b'archive')
            Path(directory, 'SHA256SUMS').write_bytes(b'checksum')
            with patch.object(r, 'verify_files'), patch.object(r, 'notes', return_value='reviewed'), patch.object(r, 'gh', return_value=json.dumps([[record]]).encode()), patch.object(r, 'api', return_value=record) as api, patch.object(r, 'verify_release', side_effect=ValueError('download corrupt')):
                with self.assertRaises(ValueError): r.stage('owner/repo', tag, SHA, directory)
                self.assertFalse(any(call.kwargs.get('data', {}).get('draft') is False or (len(call.args) > 2 and call.args[2].get('draft') is False) for call in api.call_args_list))

    def test_published_release_is_never_overwritten(self):
        import tempfile
        tag = 'v0.10.5-dev'
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, f'macrdp-{tag}-aarch64-apple-darwin-app.zip').write_bytes(b'archive')
            Path(directory, 'SHA256SUMS').write_bytes(b'checksum')
            with patch.object(r, 'verify_files'), patch.object(r, 'notes', return_value='reviewed'), patch.object(r, 'gh', return_value=json.dumps([[release(tag, 1)]]).encode()) as gh, patch.object(r, 'api') as api:
                with self.assertRaises(ValueError): r.stage('owner/repo', tag, SHA, directory)
                self.assertEqual(gh.call_count, 1)
                api.assert_not_called()
