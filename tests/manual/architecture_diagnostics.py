#!/usr/bin/env python3
"""Read-only checks of IO quality, register confirmation, and production telemetry."""
import argparse
import http.cookiejar
import json
import os
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone


def emit(event, **fields):
    print(json.dumps(dict(at=datetime.now(timezone.utc).isoformat(),event=event,**fields)),flush=True)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host')
    parser.add_argument('--duration',type=float,default=180)
    args=parser.parse_args()
    password=os.environ.get('GW_STABILITY_WEB_PASSWORD')
    if not password:parser.error('set GW_STABILITY_WEB_PASSWORD; not logged')
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base='http://'+args.host
    def login():
        request=urllib.request.Request(base+'/login',data=urllib.parse.urlencode(dict(username='admin',pwd=password)).encode())
        with opener.open(request,timeout=5) as response:
            if json.load(response).get('code')!='0':raise ValueError('login rejected')
    def get(endpoint):
        with opener.open(base+'/'+endpoint,timeout=5) as response:
            if 'application/json' not in response.headers.get('Content-Type',''):
                raise PermissionError('authenticated API redirected: '+response.url)
            obj=json.load(response)
        if obj.get('code')!='0':raise ValueError('API error: '+endpoint)
        return obj['data']
    login();start=time.monotonic();errors=0;samples=0;auth_changes=0;initial_reset=None;performance_seen=False
    while time.monotonic()-start<args.duration:
        try:
            status=get('getsystemstatus');io=get('getiodata');d=io['diagnostics'];q=d['io']
            if initial_reset is None:initial_reset=status['reset_count']
            if status['reset_count']!=initial_reset:raise ValueError('unplanned reset')
            if not q['di_valid'] or q['di_failures']!=0:raise ValueError('input freshness/read failure')
            if not q['do_readback_valid'] or q['do_failures']!=0:raise ValueError('output register readback failure')
            if q['output_queue'] and q['output_queue']['rejected']!=0:raise ValueError('output queue overflow')
            performance_seen |= d['performance'] is not None
            samples+=1
            emit('diagnostics_ok',state=status,diagnostics=d)
        except PermissionError as error:
            auth_changes+=1;emit('session_replaced',error=str(error));login()
        except Exception as error:
            errors+=1;emit('diagnostics_error',error=str(error))
        time.sleep(min(5,max(0,args.duration-(time.monotonic()-start))))
    emit('end',duration_s=time.monotonic()-start,samples=samples,errors=errors,session_replacements=auth_changes,performance_seen=performance_seen)
    if errors or not samples or not performance_seen:raise SystemExit(1)


if __name__=='__main__':main()
