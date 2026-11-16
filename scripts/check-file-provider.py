#!/usr/bin/env python3
"""Production adapter regression checks; no domain registration or installation."""
import argparse,base64,hashlib,hmac,json,mmap,os,platform,secrets,subprocess,threading,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def host_target():
    # This executable is run by the checks, so match the native runner. The
    # production host/extension remain ARM64 in packaging/make-file-provider.py.
    architecture=platform.machine()
    if architecture not in ('arm64','x86_64'):
        raise ValueError(f'unsupported macOS test host architecture: {architecture}')
    return architecture+'-apple-macos15.0'
def main(root,sdk):
    root.mkdir(parents=True,exist_ok=True)
    probe=root/'checks'
    subprocess.run(['swiftc','-sdk',sdk,'-target',host_target(),'-swift-version','5','-parse-as-library',str(ROOT/'native/file-provider/Core.swift'),str(ROOT/'native/file-provider/LocalCopies.swift'),str(ROOT/'native/file-provider/Checks.swift'),'-o',str(probe)],check=True)
    token=secrets.token_hex(32);data=bytes(range(256))*65537+b'not-zero-tail';ranges=0;mode='normal'
    snapshot=dict(session='session-A',copy='copy-A',lock=7,index=0,name='same.bin',size=len(data))
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            nonlocal ranges
            request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            code=200;body={}
            if not hmac.compare_digest(self.headers.get('Authorization',''),'Bearer '+token):code=401
            elif request['op']=='metadata':
                files=[snapshot]
                if mode=='duplicate':files=files+[dict(snapshot,index=1,name='SAME.bin')]
                body=dict(snapshots=files)
            elif request['op']=='range':
                ranges+=1
                if mode=='stall':time.sleep(0.4)
                if mode=='disconnect' and request['offset']>16384:code=503
                elif any(request.get(key)!=snapshot[key] for key in ['session','copy','lock','index']):code=409
                else:
                    offset=request['offset'];part=data[offset:offset+min(request['length'],16384)]
                    body=dict(bytes=base64.b64encode(part).decode(),offset=offset,**{key:snapshot[key] for key in ['session','copy','lock','index']})
                    if mode=='tamper':body['copy']='other-copy'
            else:code=400
            encoded=json.dumps(body).encode();self.send_response(code);self.send_header('Content-Length',str(len(encoded)));self.end_headers()
            try:self.wfile.write(encoded)
            except (BrokenPipeError,ConnectionResetError):pass
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
    results=[];counter=0
    def call(operation='fetch',auth=token):
        nonlocal counter
        counter+=1;directory=root/f'case-{counter}';directory.mkdir()
        config=dict(port=server.server_port,token=auth,operation=operation,directory=str(directory))
        result=subprocess.run([str(probe)],input=json.dumps(config),capture_output=True,text=True,timeout=60)
        assert result.returncode==0,result.stderr
        return json.loads(result.stdout.strip().splitlines()[-1]),directory
    try:
        assert call('model')[0]['ok'];results.append('unsafe names rejected; empty file metadata supported')
        assert call('metadata')[0]['ok'] and ranges==0;results.append('metadata only causes no content RANGE')
        assert not call('metadata','0'*64)[0]['ok'] and ranges==0;results.append('invalid IPC credential rejected')
        reply,directory=call();assert reply['ok'] and reply['size']==len(data) and reply['sha256']==hashlib.sha256(data).hexdigest(),reply
        with open(reply['path'],'rb') as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as view:assert hashlib.sha256(view).hexdigest()==reply['sha256']
        assert subprocess.check_output(['/bin/cat',reply['path']])==data
        results.append('>16 MiB streamed through short reads; cat/mmap/hash match source, no 8 MiB prototype cap')
        assert call('catalog')[0]['ok'];results.append('atomic metadata journal restored without identity change')
        before=ranges;assert call('local-copy')[0]['ok'] and ranges==before;results.append('local cache edits persist across restart, never fetch/upload remote bytes, reject stale writes/reads and advance change anchors')
        mode='duplicate';assert not call('metadata')[0]['ok'];results.append('case-equivalent duplicate names rejected')
        for broken in ['tamper','disconnect','stall']:
            mode=broken;reply,directory=call('cancel' if broken=='stall' else 'fetch');assert not reply['ok'],reply
            assert not list(directory.iterdir()),list(directory.iterdir())
            results.append(broken+': no partial or ready file escapes failed/cancelled materialization')
        report=dict(passed=len(results),checks=results,os_domain_registered=False,real_mstsc=False)
        (root/'RESULTS.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
    finally:server.shutdown();server.server_close()
if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--build-root',type=Path,required=True);parser.add_argument('--sdk',default=os.environ.get('SWIFT_SDK') or subprocess.check_output(['xcrun','--show-sdk-path'],text=True).strip());args=parser.parse_args();main(args.build_root.resolve(),args.sdk)
