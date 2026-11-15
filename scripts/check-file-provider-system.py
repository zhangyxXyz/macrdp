#!/usr/bin/env python3
"""Opt-in OS dataless/same-domain reconnect checks with synthetic bytes, not mstsc."""
import argparse,base64,hashlib,hmac,json,os,secrets,subprocess,threading,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
REGISTER='/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister'
def main(app,root):
    root.mkdir(parents=True,exist_ok=True)
    binary=app/'Contents/MacOS/macrdpFiles';token=secrets.token_hex(32);instance=str(uuid.uuid4());ranges=0;metadata=0;connected=True
    data=b'A'*307469;fresh=b'B'*len(data)
    def snap(session,copy,index,name):return dict(session=session,copy=copy,lock=7 if session=='A' else 8,index=index,name=name,size=len(data))
    old=[snap('A','first',0,'same.bin'),snap('A','first',1,'unread.bin')];new=[snap('B','second',0,'same.bin')];current=list(old)
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            nonlocal ranges,metadata
            request=json.loads(self.rfile.read(int(self.headers['Content-Length'])));code=200;body={}
            if not hmac.compare_digest(self.headers.get('Authorization',''),'Bearer '+token):code=401
            elif request['op']=='metadata':metadata+=1;body=dict(snapshots=current)
            elif request['op']=='range':
                ranges+=1;target=next((s for s in current if all(request.get(k)==s[k] for k in ['session','copy','lock','index'])),None)
                if not connected or target is None:code=503
                else:
                    source=data if target['session']=='A' else fresh;offset=request['offset'];length=request['length']
                    body={**{k:target[k] for k in ['session','copy','lock','index']},'offset':offset,'bytes':base64.b64encode(source[offset:offset+min(length,16384)]).decode()}
            else:code=400
            encoded=json.dumps(body).encode();self.send_response(code);self.send_header('Content-Length',str(len(encoded)));self.end_headers()
            try:self.wfile.write(encoded)
            except (BrokenPipeError,ConnectionResetError):pass
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
    def host(operation,**kwargs):
        result=subprocess.run([str(binary),operation],input=json.dumps(dict(instance=instance,**kwargs)),capture_output=True,text=True,timeout=30)
        value=json.loads(result.stdout.strip().splitlines()[-1]);assert result.returncode==0,value;return value
    def resolve(files):return [Path(p) for p in host('resolve',items=[f"{s['session']}/{s['copy']}/{s['index']}" for s in files])['paths']]
    def cat(path):return subprocess.run(['/bin/cat',str(path)],capture_output=True,timeout=30)
    checks=[];registered=False
    try:
        subprocess.run([REGISTER,'-f',str(app)],check=True,capture_output=True)
        subprocess.run(['pluginkit','-a',str(app/'Contents/PlugIns/macrdpFiles.appex')],check=True,capture_output=True)
        domain=host('start',port=server.server_port,token=token,pid=os.getpid());registered=True
        a,b=resolve(old);assert a.stat().st_flags&0x40000000 and b.stat().st_flags&0x40000000
        assert ranges==0;checks.append('same-domain flat multi-file metadata is genuinely dataless, zero content requests')
        result=cat(a);assert result.returncode==0 and result.stdout==data and not a.stat().st_flags&0x40000000
        checks.append('direct POSIX read hydrates complete bytes with known SHA-256')
        before=ranges;connected=False;cached=cat(a);assert cached.returncode==0 and cached.stdout==data and ranges==before
        checks.append('OS-materialized file is readable offline without network')
        failed=cat(b);assert failed.returncode!=0 and not failed.stdout;checks.append('unmaterialized read after disconnect fails without zero-filled success')
        connected=True;current[:]=new
        c=resolve(new)[0];assert c!=a and c.name==a.name and c.stat().st_flags&0x40000000
        result=cat(c);assert result.returncode==0 and result.stdout==fresh;checks.append('same domain reconnect resolves same filename under a fresh immutable copy and reads new bytes')
        failed=cat(b);assert failed.returncode!=0 and not failed.stdout;checks.append('old unmaterialized URL cannot return new-session bytes')
        report=dict(passed=len(checks),checks=checks,domain=domain['domain'],sha256=hashlib.sha256(data).hexdigest(),ranges=ranges,metadata_requests=metadata,actual_mstsc=False)
    finally:
        if registered:host('stop')
        server.shutdown();server.server_close()
    report['domain_removed']=True
    (root/'RESULTS.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--host-app',type=Path,required=True);p.add_argument('--build-root',type=Path,required=True);p.add_argument('--allow-system-registration',action='store_true');args=p.parse_args()
    if not args.allow_system_registration:p.error('explicit --allow-system-registration is required')
    main(args.host_app.resolve(),args.build_root.resolve())
