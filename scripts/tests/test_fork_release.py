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
                name='macrdp ' + tag, target_commitish=SHA, body=f'{r.MARKER}sha={SHA} -->' if owned else 'upstream release', assets=[])


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

    def test_untagged_draft_requires_exact_identity_and_provenance(self):
        tag = 'v0.10.6-dev'
        draft = release(tag, 1, draft=True)
        draft['tag_name'] = 'untagged-github-generated'
        self.assertTrue(r.owned_draft(draft, tag, SHA))
        self.assertIsNone(r.owner_sha(draft), 'untagged drafts never enter published cleanup')
        for key, value in [('target_commitish', 'b'*40), ('name', 'unrelated'), ('draft', False), ('body', 'unowned')]:
            self.assertFalse(r.owned_draft({**draft, key:value}, tag, SHA))

    def test_uploads_replace_only_named_assets_by_stable_release_id(self):
        tag = 'v0.10.6-dev'
        record = {**release(tag, 7, draft=True), 'tag_name':'untagged-any',
                  'assets':[{'id':10,'name':'app.zip'}, {'id':11,'name':'unrelated.txt'}]}
        with patch.object(r, 'gh', return_value=b'') as gh:
            r.upload_assets('owner/repo', record, [Path('/private/app.zip')])
        self.assertEqual(gh.call_args_list[0].args,
                         ('api','--method','DELETE','repos/owner/repo/releases/assets/10'))
        upload = gh.call_args_list[1].args
        self.assertIn('https://uploads.github.com/repos/owner/repo/releases/7/assets?name=app.zip',upload)
        self.assertEqual(upload[-2:],('--input','/private/app.zip'))
        self.assertFalse(any('release' in c.args or '11' in c.args for c in gh.call_args_list))

    def test_draft_download_verification_uses_asset_ids_not_tag_lookup(self):
        tag='v0.10.6-dev'; name=f'macrdp-{tag}-aarch64-apple-darwin-app.zip'
        contents={21:b'archive',22:b'checksum'}
        record={**release(tag,7,draft=True),'tag_name':'untagged-github-generated',
                'assets':[{'id':i,'name':n,'size':len(contents[i]),'digest':'sha256:'+hashlib.sha256(contents[i]).hexdigest()}
                          for i,n in [(21,name),(22,'SHA256SUMS')]]}
        def gh(*args,**kwargs):
            self.assertEqual(args[0],'api');self.assertIn('Accept: application/octet-stream',args)
            return contents[int(args[1].rsplit('/',1)[1])]
        with patch.object(r,'gh',side_effect=gh) as get, patch.object(r,'verify_files') as verify:
            self.assertEqual(r.verify_release('owner/repo',record,tag,SHA),SHA)
            verify.assert_called_once_with(b'archive',b'checksum',SHA,name)
            self.assertEqual(get.call_count,2)
        broken={**record,'assets':[{**record['assets'][0],'digest':'sha256:wrong'},record['assets'][1]]}
        with patch.object(r,'gh',side_effect=gh), self.assertRaisesRegex(ValueError,'digest'):
            r.verify_release('owner/repo',broken,tag,SHA)

    def test_stage_adopts_owned_untagged_draft_and_restores_tag_on_publication(self):
        import tempfile
        tag='v0.10.6-dev';record={**release(tag,7,draft=True),'tag_name':'untagged-generated'}
        calls=[]
        def api(path,method='GET',data=None):
            calls.append((path,method,data))
            if method=='PATCH' and data.get('draft') is False:
                return {**record,**data}
            return record
        with tempfile.TemporaryDirectory() as directory:
            Path(directory,f'macrdp-{tag}-aarch64-apple-darwin-app.zip').write_bytes(b'archive')
            Path(directory,'SHA256SUMS').write_bytes(b'sums')
            with patch.object(r,'verify_files'), patch.object(r,'notes',return_value=record['body']), \
                 patch.object(r,'gh',return_value=json.dumps([[record]]).encode()), patch.object(r,'api',side_effect=api), \
                 patch.object(r,'upload_assets') as upload, patch.object(r,'verify_release') as verify:
                r.stage('owner/repo',tag,SHA,directory)
            self.assertFalse(any(method=='POST' for _,method,_ in calls))
            self.assertEqual(upload.call_args.args[1]['id'],7)
            verify.assert_called_once_with('owner/repo',record,tag,SHA)
            self.assertEqual(calls[-1][2]['tag_name'],tag)
            self.assertFalse(calls[-1][2]['draft'])
