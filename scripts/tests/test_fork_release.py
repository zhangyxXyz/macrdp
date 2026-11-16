import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
import tempfile
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
        with patch.object(r, 'gh', gh), patch.object(r, 'require_tag_source'), patch.object(r, 'verify_release') as verify, patch.object(r, 'api', lambda path: records[int(path.rsplit('/', 1)[1])]):
            r.cleanup('owner/repo', False)
        verify.assert_called_once_with('owner/repo', current)
        self.assertEqual([c for c in calls if c[:2] == ('release', 'delete')],
                         [('release', 'delete', 'v0.10.4-dev', '--repo', 'owner/repo', '--cleanup-tag', '--yes')])
        self.assertFalse(any('git/refs' in str(c) for c in calls))

    def test_dry_run_and_failed_verification_never_delete(self):
        records = [release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)]
        for dry, error in [(True, None), (False, ValueError('broken retained release'))]:
            with patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, patch.object(r, 'require_tag_source'), patch.object(r, 'verify_release', side_effect=error), patch.object(r, 'api', side_effect=lambda path: records[int(path.rsplit('/', 1)[1]) - 1]):
                if error:
                    with self.assertRaises(ValueError): r.cleanup('owner/repo', dry)
                else:
                    r.cleanup('owner/repo', dry)
                self.assertFalse(any('DELETE' in call.args or call.args[:2] == ('release', 'delete') for call in gh.call_args_list))

    def test_changed_release_aborts_cleanup(self):
        records = [release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)]
        with patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, patch.object(r, 'require_tag_source'), patch.object(r, 'verify_release'), patch.object(r, 'api', return_value={**records[1], 'draft': True}):
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
            with patch.object(r, 'verify_files'), patch.object(r, 'require_tag_source'), patch.object(r, 'notes', return_value='reviewed'), patch.object(r, 'gh', return_value=json.dumps([[record]]).encode()), patch.object(r, 'api', return_value=record) as api, patch.object(r, 'verify_release', side_effect=ValueError('download corrupt')):
                with self.assertRaises(ValueError): r.stage('owner/repo', tag, SHA, directory)
                self.assertFalse(any(call.kwargs.get('data', {}).get('draft') is False or (len(call.args) > 2 and call.args[2].get('draft') is False) for call in api.call_args_list))

    def test_unowned_published_release_is_never_overwritten(self):
        import tempfile
        tag = 'v0.10.5-dev'
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, f'macrdp-{tag}-aarch64-apple-darwin-app.zip').write_bytes(b'archive')
            Path(directory, 'SHA256SUMS').write_bytes(b'checksum')
            with patch.object(r, 'verify_files'), patch.object(r, 'notes', return_value='reviewed'), patch.object(r, 'gh', return_value=json.dumps([[release(tag, 1, owned=False)]]).encode()) as gh, patch.object(r, 'api') as api:
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
            with patch.object(r,'verify_files'), patch.object(r,'require_tag_source'), patch.object(r,'notes',return_value=record['body']), \
                 patch.object(r,'gh',return_value=json.dumps([[record]]).encode()), patch.object(r,'api',side_effect=api), \
                 patch.object(r,'upload_assets') as upload, patch.object(r,'verify_release') as verify:
                r.stage('owner/repo',tag,SHA,directory)
            self.assertFalse(any(method=='POST' for _,method,_ in calls))
            self.assertEqual(upload.call_args.args[1]['id'],7)
            verify.assert_called_once_with('owner/repo',record,tag,SHA)
            self.assertEqual(calls[-1][2]['tag_name'],tag)
            self.assertFalse(calls[-1][2]['draft'])


class StageHarness:
    """In-memory GitHub API: every mutation and verification is observable."""
    def __init__(self, records, verification_error=None, edit_old=None, fail_publish=False, edit_draft=None):
        self.records = {record['id']: copy.deepcopy(record) for record in records}
        self.events = []
        self.verification_error = verification_error
        self.edit_old = edit_old
        self.edit_draft = edit_draft
        self.fail_publish = fail_publish
        self.verified = False
        self.remote_tag_sha = 'b'*40
        self.tag_after_verification = None

    def gh(self, *args, **kwargs):
        if '--paginate' in args:
            return json.dumps([list(self.records.values())]).encode()
        if args[:3] == ('api', '--method', 'DELETE'):
            number = int(args[3].rsplit('/', 1)[1])
            self.events.append(('delete', number))
            del self.records[number]
            return b''
        raise AssertionError(('unexpected gh mutation', args))

    def api(self, path, method='GET', data=None):
        if '/git/ref/tags/' in path:
            self.events.append(('resolve-tag', path.rsplit('/', 1)[1]))
            sha = self.tag_after_verification if self.verified and self.tag_after_verification else self.remote_tag_sha
            return {'object':{'type':'commit', 'sha':sha}}
        if method == 'POST':
            number = max(self.records, default=0) + 1
            self.records[number] = {'id':number, 'assets':[], **copy.deepcopy(data)}
            self.events.append(('create', number, data['tag_name']))
            return copy.deepcopy(self.records[number])
        number = int(path.rsplit('/', 1)[1])
        if method == 'GET':
            self.events.append(('read', number))
            record = copy.deepcopy(self.records[number])
            if self.verified and not record['draft'] and self.edit_old:
                record.update(copy.deepcopy(self.edit_old))
            if self.verified and record['draft'] and self.edit_draft:
                record.update(copy.deepcopy(self.edit_draft))
            return record
        if method == 'PATCH':
            self.events.append(('publish' if data.get('draft') is False else 'update-draft', number))
            if data.get('draft') is False and self.fail_publish:
                raise RuntimeError('GitHub publish failed')
            self.records[number].update(copy.deepcopy(data))
            return copy.deepcopy(self.records[number])
        raise AssertionError(('unexpected API mutation', method, path))

    def upload(self, repo, record, paths):
        self.events.append(('upload', record['id'], [path.name for path in paths]))
        if not record['draft']:
            raise AssertionError('public assets must never be overwritten')

    def verify(self, repo, record, tag, sha):
        self.events.append(('verify', record['id']))
        if self.verification_error:
            raise self.verification_error
        self.verified = True
        return sha

    def stage(self, tag='v0.10.6-dev', sha='b'*40):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, f'macrdp-{tag}-aarch64-apple-darwin-app.zip').write_bytes(b'archive')
            Path(directory, 'SHA256SUMS').write_bytes(b'checksum')
            with patch.object(r, 'verify_files'), \
                 patch.object(r, 'notes', return_value=f'{r.MARKER}sha={sha} -->'), \
                 patch.object(r, 'gh', side_effect=self.gh), patch.object(r, 'api', side_effect=self.api), \
                 patch.object(r, 'upload_assets', side_effect=self.upload), \
                 patch.object(r, 'verify_release', side_effect=self.verify):
                r.stage('owner/repo', tag, sha, directory)


class RepublishTests(unittest.TestCase):
    tag = 'v0.10.6-dev'
    new_sha = 'b'*40

    def test_republish_verifies_dedicated_draft_before_deleting_only_the_old_release(self):
        old = release(self.tag, 1)
        harness = StageHarness([old])
        harness.stage()
        events = harness.events
        self.assertIn(('create', 2, r.staging_tag(self.tag, self.new_sha)), events)
        upload = next(event for event in events if event[0] == 'upload')
        self.assertEqual(upload, ('upload', 2, [f'macrdp-{self.tag}-aarch64-apple-darwin-app.zip', 'SHA256SUMS']))
        self.assertLess(events.index(('verify', 2)), events.index(('read', 1)))
        self.assertLess(events.index(('read', 1)), events.index(('delete', 1)))
        self.assertLess(events.index(('delete', 1)), events.index(('publish', 2)))
        self.assertFalse(any(event[0] == 'update-draft' and event[1] == 1 for event in events))
        self.assertEqual(set(harness.records), {2})
        self.assertEqual(harness.records[2]['tag_name'], self.tag)
        self.assertFalse(harness.records[2]['draft'])
        self.assertEqual(r.owner_sha(harness.records[2]), self.new_sha)

    def test_failed_download_verification_keeps_old_release_and_assets_untouched(self):
        old = {**release(self.tag, 1), 'assets':[{'id':12, 'name':'old.zip', 'size':99}]}
        harness = StageHarness([old], verification_error=ValueError('download checksum mismatch'))
        with self.assertRaisesRegex(ValueError, 'checksum'):
            harness.stage()
        self.assertEqual(harness.records[1], old)
        self.assertTrue(harness.records[2]['draft'])
        self.assertFalse(any(event[0] in ('delete', 'publish') for event in harness.events))

    def test_foreign_release_or_staging_collision_aborts_before_any_mutation(self):
        foreign = release(self.tag, 1, owned=False)
        wrong_name = {**release(self.tag, 1), 'name':'upstream'}
        wrong_target = {**release(self.tag, 1), 'target_commitish':'main'}
        collision = release(r.staging_tag(self.tag, self.new_sha), 2, draft=True, owned=False)
        for records in ([foreign], [wrong_name], [wrong_target], [release(self.tag, 1), collision]):
            with self.subTest(records=records):
                harness = StageHarness(records)
                with self.assertRaisesRegex(ValueError, 'unowned'):
                    harness.stage()
                self.assertEqual(harness.events, [])

    def test_edit_to_old_publication_after_upload_prevents_replacement(self):
        edits = [{'body':'edited'}, {'draft':True}, {'tag_name':'v0.10.7-dev'},
                 {'assets':[{'id':7, 'name':'new-asset', 'size':1}]},
                 {'name':'renamed'}, {'target_commitish':self.new_sha}, {'updated_at':'later'}]
        for edit in edits:
            with self.subTest(edit=edit):
                harness = StageHarness([release(self.tag, 1)], edit_old=edit)
                with self.assertRaisesRegex(ValueError, 'changed'):
                    harness.stage()
                self.assertFalse(any(event[0] in ('delete', 'publish') for event in harness.events))
                self.assertIn(1, harness.records)
                self.assertTrue(harness.records[2]['draft'])

    def test_retry_reuses_owned_staging_draft_instead_of_creating_another(self):
        draft = {**release(self.tag, 2, draft=True), 'tag_name':r.staging_tag(self.tag, self.new_sha),
                 'target_commitish':self.new_sha, 'body':f'{r.MARKER}sha={self.new_sha} -->'}
        harness = StageHarness([release(self.tag, 1), draft])
        harness.stage()
        self.assertFalse(any(event[0] == 'create' for event in harness.events))
        self.assertIn(('verify', 2), harness.events)
        self.assertIn(('publish', 2), harness.events)
        self.assertEqual(set(harness.records), {2})

    def test_final_publish_failure_leaves_verified_draft_for_retry_without_rebuild(self):
        harness = StageHarness([release(self.tag, 1)], fail_publish=True)
        with self.assertRaisesRegex(RuntimeError, 'publish failed'):
            harness.stage()
        self.assertEqual(set(harness.records), {2})
        self.assertTrue(harness.records[2]['draft'])
        self.assertEqual(harness.records[2]['tag_name'], r.staging_tag(self.tag, self.new_sha))
        harness.fail_publish = False
        harness.events = []
        harness.stage()
        self.assertFalse(any(event[0] in ('create', 'delete') for event in harness.events))
        self.assertFalse(harness.records[2]['draft'])

    def test_normal_download_counters_do_not_count_as_publication_edits(self):
        old = {**release(self.tag, 1), 'assets':[{'id':12, 'name':'old.zip', 'size':99, 'download_count':1}]}
        harness = StageHarness([old], edit_old={'assets':[{**old['assets'][0], 'download_count':2}]})
        harness.stage()
        self.assertIn(('delete', 1), harness.events)

    def test_staging_drafts_are_owned_only_for_exact_tag_and_commit(self):
        draft = {**release(self.tag, 2, draft=True), 'tag_name':r.staging_tag(self.tag, SHA)}
        self.assertTrue(r.owned_draft(draft, self.tag, SHA))
        self.assertFalse(r.owned_draft(draft, self.tag, self.new_sha))
        self.assertFalse(r.owned_draft(draft, 'v0.10.7-dev', SHA))
        self.assertIsNone(r.owner_sha(draft), 'staging drafts are never published-cleanup candidates')

    def test_edit_to_verified_staging_assets_keeps_old_public_download(self):
        harness = StageHarness([release(self.tag, 1)], edit_draft={'assets':[{'id':99, 'name':'changed.zip'}]})
        with self.assertRaisesRegex(ValueError, 'verified staging draft changed'):
            harness.stage()
        self.assertIn(1, harness.records)
        self.assertFalse(any(event[0] in ('delete', 'publish') for event in harness.events))

    def test_tag_moved_before_staging_prevents_any_release_mutation(self):
        harness = StageHarness([release(self.tag, 1)])
        harness.remote_tag_sha = SHA
        with self.assertRaisesRegex(ValueError, 'expected source commit'):
            harness.stage()
        self.assertEqual(set(harness.records), {1})
        self.assertFalse(any(event[0] in ('create', 'update-draft', 'upload', 'delete', 'publish') for event in harness.events))

    def test_tag_moved_during_upload_preserves_old_release(self):
        harness = StageHarness([release(self.tag, 1)])
        harness.tag_after_verification = 'c'*40
        with self.assertRaisesRegex(ValueError, 'expected source commit'):
            harness.stage()
        self.assertIn(1, harness.records)
        self.assertTrue(harness.records[2]['draft'])
        self.assertFalse(any(event[0] in ('delete', 'publish') for event in harness.events))

    def test_new_publication_also_rechecks_tag_after_verification(self):
        harness = StageHarness([])
        harness.tag_after_verification = 'c'*40
        with self.assertRaisesRegex(ValueError, 'expected source commit'):
            harness.stage()
        self.assertTrue(harness.records[1]['draft'])
        self.assertFalse(any(event[0] == 'publish' for event in harness.events))


class TagSourceTests(unittest.TestCase):
    def test_lightweight_tag_resolves_directly_to_commit(self):
        with patch.object(r, 'api', return_value={'object':{'type':'commit', 'sha':SHA}}) as api:
            self.assertEqual(r.resolve_tag_commit('owner/repo', 'v0.10.6-dev'), SHA)
        api.assert_called_once_with('repos/owner/repo/git/ref/tags/v0.10.6-dev')

    def test_nested_annotated_tags_are_peeled_to_commit(self):
        objects = [{'object':{'type':'tag', 'sha':'b'*40}},
                   {'object':{'type':'tag', 'sha':'c'*40}},
                   {'object':{'type':'commit', 'sha':SHA}}]
        with patch.object(r, 'api', side_effect=objects) as api:
            self.assertEqual(r.resolve_tag_commit('owner/repo', 'v0.10.6-dev'), SHA)
        self.assertEqual(api.call_args_list[1].args, ('repos/owner/repo/git/tags/' + 'b'*40,))
        self.assertEqual(api.call_args_list[2].args, ('repos/owner/repo/git/tags/' + 'c'*40,))

    def test_noncommit_malformed_and_cyclic_targets_fail_closed(self):
        for obj in [{'type':'tree','sha':SHA}, {'type':'commit','sha':'bad'}, {'type':'tag','sha':SHA}]:
            with self.subTest(obj=obj), patch.object(r, 'api', return_value={'object':obj}), self.assertRaises(ValueError):
                r.resolve_tag_commit('owner/repo', 'v0.10.6-dev')

    def test_cleanup_never_deletes_a_moved_old_tag_or_prunes_against_moved_retained_tag(self):
        records = [release('v0.10.4-dev', 1), release('v0.10.5-dev', 2)]
        for moved_tag in ['v0.10.4-dev', 'v0.10.5-dev']:
            def api(path, method='GET', data=None):
                if '/git/ref/tags/' in path:
                    tag = path.rsplit('/',1)[1]
                    return {'object':{'type':'commit','sha':'b'*40 if tag == moved_tag else SHA}}
                return copy.deepcopy(records[int(path.rsplit('/',1)[1])-1])
            with self.subTest(moved_tag=moved_tag), patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, \
                 patch.object(r, 'api', side_effect=api), patch.object(r, 'verify_release'):
                with self.assertRaisesRegex(ValueError, 'expected source commit'):
                    r.cleanup('owner/repo', False)
                self.assertEqual(gh.call_count, 1, 'only list releases; no delete command')

    def test_cleanup_ignores_download_counter_changes_but_preserves_tag_deletion_policy(self):
        records = [{**release('v0.10.4-dev', 1), 'assets':[{'id':5, 'name':'old.zip', 'size':1, 'download_count':0}]},
                   release('v0.10.5-dev', 2)]
        def api(path, method='GET', data=None):
            if '/git/ref/tags/' in path:
                return {'object':{'type':'commit', 'sha':SHA}}
            result = copy.deepcopy(records[int(path.rsplit('/',1)[1])-1])
            if result['assets']:
                result['assets'][0]['download_count'] += 1
            return result
        with patch.object(r, 'gh', return_value=json.dumps([records]).encode()) as gh, \
             patch.object(r, 'api', side_effect=api), patch.object(r, 'verify_release'):
            r.cleanup('owner/repo', False)
        self.assertEqual(gh.call_args_list[-1].args,
            ('release','delete','v0.10.4-dev','--repo','owner/repo','--cleanup-tag','--yes'))
