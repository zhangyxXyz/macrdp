#!/usr/bin/env python3
"""Stage, verify, publish and retain owned fork releases without Actions artifacts."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import zipfile
from urllib.parse import quote

MARKER = '<!-- macrdp-fork-release:v1 '
TAG = re.compile(r'v\d+\.\d+\.\d+(?:-[A-Za-z0-9][A-Za-z0-9.-]*)?\Z')


def gh(*args, data=None):
    command = ['gh', *args]
    if data is not None:
        command += ['--input', '-']
    return subprocess.check_output(command, input=None if data is None else json.dumps(data).encode())


def api(path, method='GET', data=None):
    return json.loads(gh('api', '--method', method, path, data=data))


def marker_sha(release):
    found = re.search(re.escape(MARKER) + r'sha=([a-f0-9]{40}) -->', release.get('body') or '')
    return found.group(1) if found else None


def owner_sha(release):
    return marker_sha(release) if TAG.fullmatch(release['tag_name']) else None


def owned_draft(release, tag, sha):
    # GitHub may rename a draft to untagged-* after an edit. Its stable ID,
    # exact pipeline name, target commit and provenance must all still match.
    return (release.get('draft') is True and marker_sha(release) == sha
            and release.get('name') == 'macrdp ' + tag
            and release.get('target_commitish') == sha
            and (release['tag_name'] == tag or release['tag_name'].startswith('untagged-')))


def upload_assets(repo, release, paths):
    # Never resolve a draft through the tag-based gh release commands.
    for path in paths:
        for old in release['assets']:
            if old['name'] == path.name:
                gh('api', '--method', 'DELETE', f"repos/{repo}/releases/assets/{old['id']}")
        url = f"https://uploads.github.com/repos/{repo}/releases/{release['id']}/assets?name={quote(path.name, safe='')}"
        gh('api', '--method', 'POST', url, '-H', 'Content-Type: application/octet-stream', '--input', str(path))


def verify_files(archive, sums, expected_sha, expected_name):
    # Do not trust arbitrary filenames from a downloaded checksum manifest.
    expected = hashlib.sha256(archive).hexdigest() + '  ' + expected_name + '\n'
    if sums.decode() != expected:
        raise ValueError('release checksum mismatch')
    with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
        if bundle.read('SOURCE_COMMIT').decode().strip() != expected_sha:
            raise ValueError('release source commit mismatch')
        required = ['macrdp.app/Contents/MacOS/macrdp',
                    'macrdpController.app/Contents/MacOS/macrdptray',
                    'macrdp.app/Contents/Helpers/macrdpFiles.app/Contents/MacOS/macrdpFiles',
                    'macrdp.app/Contents/Helpers/macrdpFiles.app/Contents/PlugIns/macrdpFiles.appex/Contents/MacOS/macrdpFilesProvider',
                    'CAPABILITIES.json', 'README.md', 'README_CN.md']
        if any(name not in bundle.namelist() for name in required):
            raise ValueError('release bundle incomplete')


def verify_release(repo, release, expected_tag=None, expected_sha=None):
    tag = expected_tag or release['tag_name']
    sha = expected_sha or owner_sha(release)
    if not TAG.fullmatch(tag) or not sha:
        raise ValueError('release is not owned by this pipeline')
    if release['draft']:
        if not owned_draft(release, tag, sha):
            raise ValueError('draft provenance mismatch')
    elif release['tag_name'] != tag or owner_sha(release) != sha:
        raise ValueError('published provenance mismatch')
    name = f"macrdp-{tag}-aarch64-apple-darwin-app.zip"
    assets = release['assets']
    if sorted(a['name'] for a in assets) != sorted([name, 'SHA256SUMS']):
        raise ValueError('release asset set incomplete or unexpected')
    contents = {}
    for asset in assets:
        content = gh('api', f"repos/{repo}/releases/assets/{asset['id']}",
                     '-H', 'Accept: application/octet-stream')
        if len(content) != asset['size']:
            raise ValueError('release asset size mismatch')
        digest = asset.get('digest')
        if digest and digest != 'sha256:' + hashlib.sha256(content).hexdigest():
            raise ValueError('GitHub asset digest mismatch')
        contents[asset['name']] = content
    verify_files(contents[name], contents['SHA256SUMS'], sha, name)
    return sha


def notes(tag, sha, repo):
    changelog = Path('CHANGELOG.md').read_text()
    heading = '## ' + tag + '\n'
    if heading not in changelog:
        raise ValueError('tag must have a reviewed CHANGELOG section')
    link = f'https://github.com/{repo}/blob/{tag}/CHANGELOG.md#{tag.replace(".", "")}'
    template = Path('packaging/RELEASE_NOTES.md').read_text()
    return f'{MARKER}sha={sha} -->\n\n' + template.format(tag=tag, changelog_url=link, source_sha=sha)


def stage(repo, tag, sha, directory):
    if not TAG.fullmatch(tag):
        raise ValueError('unsupported version tag')
    name = f'macrdp-{tag}-aarch64-apple-darwin-app.zip'
    verify_files(Path(directory, name).read_bytes(), Path(directory, 'SHA256SUMS').read_bytes(), sha, name)
    existing = json.loads(gh('api', '--paginate', '--slurp', f'repos/{repo}/releases?per_page=100'))
    records = [r for page in existing for r in page]
    matches = [r for r in records if r['tag_name'] == tag or owned_draft(r, tag, sha)]
    if len(matches) > 1:
        raise ValueError('ambiguous release drafts; refusing to select one')
    release = matches[0] if matches else None
    body = notes(tag, sha, repo)
    if release:
        if not owned_draft(release, tag, sha):
            raise ValueError('refusing to overwrite a published or unowned release')
        release = api(f"repos/{repo}/releases/{release['id']}", 'PATCH',
            {'tag_name': tag, 'target_commitish': sha, 'name': 'macrdp ' + tag, 'body': body, 'draft': True})
    else:
        release = api(f'repos/{repo}/releases', 'POST', {'tag_name': tag, 'target_commitish': sha,
            'name': 'macrdp ' + tag, 'body': body, 'draft': True, 'prerelease': '-' in tag})
    # Re-read and authenticate before deleting/replacing any asset.
    release = api(f"repos/{repo}/releases/{release['id']}")
    if not owned_draft(release, tag, sha):
        raise ValueError('draft changed before upload')
    upload_assets(repo, release, [Path(directory, name), Path(directory, 'SHA256SUMS')])
    release = api(f"repos/{repo}/releases/{release['id']}")
    verify_release(repo, release, tag, sha)
    # Only publish after round-trip verification. Always restore the real tag.
    published = api(f"repos/{repo}/releases/{release['id']}", 'PATCH',
        {'tag_name': tag, 'target_commitish': sha, 'name': 'macrdp ' + tag,
         'draft': False, 'prerelease': '-' in tag, 'make_latest': 'false' if '-' in tag else 'true'})
    if published['draft'] or published['tag_name'] != tag or owner_sha(published) != sha:
        raise ValueError('publication metadata did not match the verified draft')

    print(f'Published and verified {tag}')


def cleanup(repo, dry_run):
    pages = json.loads(gh('api', '--paginate', '--slurp', f'repos/{repo}/releases?per_page=100'))
    owned = [r for page in pages for r in page if owner_sha(r) and not r['draft']]
    owned.sort(key=lambda r: (r.get('published_at') or '', r['id']), reverse=True)
    if not owned:
        print('No pipeline-owned published releases; nothing to clean')
        return
    current = owned[0]
    verify_release(repo, current)  # Fail closed: never prune before verifying the retained download.
    for old in owned[1:]:
        # Re-read before mutation: drafts, edits or a changed retained release abort deletion.
        fresh_current = api(f"repos/{repo}/releases/{current['id']}")
        fresh_old = api(f"repos/{repo}/releases/{old['id']}")
        for fresh, original in [(fresh_current, current), (fresh_old, old)]:
            if any(fresh.get(k) != original.get(k) for k in ('body', 'draft', 'tag_name', 'assets', 'published_at')):
                raise ValueError('release changed during cleanup; refusing deletion')
        print(('Would delete ' if dry_run else 'Deleting ') + old['tag_name'] + ' (including its tag)')
        if not dry_run:
            gh('release', 'delete', old['tag_name'], '--repo', repo, '--cleanup-tag', '--yes')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['stage', 'cleanup'])
    parser.add_argument('--tag', default=os.environ.get('RELEASE_TAG'))
    parser.add_argument('--sha', default=os.environ.get('GITHUB_SHA'))
    parser.add_argument('--directory', default='dist')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    repo = os.environ['GITHUB_REPOSITORY']
    if args.mode == 'stage':
        stage(repo, args.tag, args.sha, args.directory)
    else:
        cleanup(repo, args.dry_run)


if __name__ == '__main__':
    main()
