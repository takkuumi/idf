#!/usr/bin/env python3
"""Save unchanged UART settings and round-trip Web display identity across reset.

Requires --apply and GW_STABILITY_WEB_PASSWORD. Physical outputs, network
settings and UART parameters are not changed. The display name is restored.
"""
from __future__ import annotations
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import socket
import struct
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone, timedelta


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('host');p.add_argument('--apply',action='store_true');p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();password=os.environ.get('GW_STABILITY_WEB_PASSWORD')
    if not a.apply or not password:p.error('--apply and the credential environment are required')
    a.out.mkdir(parents=True,exist_ok=True)
    events=(a.out/'events.jsonl').open('a')
    def emit(event,**data):
        row={'at':datetime.now(timezone(timedelta(hours=8))).isoformat(),'event':event,**data}
        line=json.dumps(row,ensure_ascii=False);events.write(line+'\n');events.flush();print(line,flush=True)
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base='http://'+a.host+'/'
    def login():
        with opener.open(urllib.request.Request(base+'login',data=urllib.parse.urlencode({'username':'admin','pwd':password}).encode()),timeout=4) as r:r.read()
    def api(endpoint,data=None):
        req=urllib.request.Request(base+endpoint,data=None if data is None else urllib.parse.urlencode(data).encode())
        with opener.open(req,timeout=10) as r:body=json.load(r)
        if str(body.get('code'))!='0':raise RuntimeError((endpoint,body))
        return body.get('data',{})
    def reset(previous):
        with socket.create_connection((a.host,502),3) as s:
            s.settimeout(3);s.sendall(bytes.fromhex('6f010000000601050402ff00'));b=b''
            while len(b)<12:
                more=s.recv(12-len(b))
                if not more:raise RuntimeError('reset ACK truncated')
                b+=more
            assert b==bytes.fromhex('6f010000000601050402ff00'),b.hex()
        t=time.monotonic();time.sleep(1)
        while time.monotonic()-t<90:
            try:
                login();state=api('getsystemstatus')
                if state['reset_count']==previous+1:
                    assert state['reset_reason']==3,state
                    assert state['recovery_mode']=='Normal',state
                    emit('software_reset_verified',ready_after_s=round(time.monotonic()-t,3),state=state)
                    return state
                if state['reset_count']>previous+1:raise AssertionError(('unexpected extra reset',state))
            except (OSError,ValueError,TimeoutError):pass
            time.sleep(.5)
        raise RuntimeError('device did not return after planned reset')
    login();before={k:api(k) for k in ['getsysteminfo','getnetworkconfig','getportconfig','getbleconfig','getsensorconfig']}
    (a.out/'before.json').write_text(json.dumps(before,ensure_ascii=False,indent=2)+'\n')
    ports=before['getportconfig']
    for index in range(3):
        data={'index':index,**{k:ports[f'{k}_{index}'] for k in ['baud','datalen','checkmode','stopbit','masterslaveport','slaveaddress','retrycount','responeinteval','tti']}}
        t=time.monotonic();api('updateportconfig',data);assert api('getportconfig')==ports
        emit('uart_save_roundtrip',index=index,latency_ms=round((time.monotonic()-t)*1000,3),parameters_unchanged=True)
    changed=False
    try:
        previous=api('getsystemstatus')['reset_count'];changed=True
        api('updatesysteminfoconfig',{'devicename':'IMC-VERIFY-SAVE'})
        reset(previous)
        assert api('getsysteminfo')['devicename']=='IMC-VERIFY-SAVE'
        emit('web_identity_persisted_after_reset',verified=True)
    finally:
        if changed:
            login();previous=api('getsystemstatus')['reset_count']
            api('updatesysteminfoconfig',{'devicename':before['getsysteminfo']['devicename']})
            reset(previous)
            after={k:api(k) for k in before}
            (a.out/'after.json').write_text(json.dumps(after,ensure_ascii=False,indent=2)+'\n')
            assert after==before,'Web configuration differs after restoration'
            io=api('getiodata')
            emit('original_configuration_restored',verified=True,io_diagnostics=io.get('diagnostics'))
    (a.out/'summary.json').write_text(json.dumps({'passed':True,'uart_saves':3,'planned_resets':2,'original_configuration_restored':True},indent=2)+'\n')
    events.close()


if __name__=='__main__':main()
