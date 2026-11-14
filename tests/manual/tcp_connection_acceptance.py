#!/usr/bin/env python3
"""Read-only real-device connection admission, reuse and cleanup checks."""
import argparse
import json
from pathlib import Path
import socket
import struct
import time
from datetime import datetime, timezone, timedelta


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-version',type=int,default=320)
    parser.add_argument('host');parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args();args.out.mkdir(parents=True,exist_ok=True)
    log=(args.out/'events.jsonl').open('w')
    def emit(event,**data):
        row={'at':datetime.now(timezone(timedelta(hours=8))).isoformat(),'event':event,**data}
        line=json.dumps(row);log.write(line+'\n');log.flush();print(line,flush=True)
    def version(s,tx):
        s.sendall(struct.pack('>HHHBBHH',tx,0,6,1,4,0x087e,1));data=b''
        while len(data)<11:
            part=s.recv(11-len(data))
            if not part:raise RuntimeError('connection closed before version reply')
            data+=part
        assert data==struct.pack('>HHHBBBH',tx,0,5,1,4,2,args.expected_version),data.hex()
    idle=[];trusted=None
    try:
        trusted=socket.create_connection((args.host,502),3);trusted.settimeout(2);version(trusted,1)
        for index in range(7):
            s=socket.create_connection((args.host,[502,503,504,5002][index%4]),2);s.settimeout(.3);idle.append(s)
        for second in range(12):
            time.sleep(1);version(trusted,second+2)
        closed=0
        for s in idle:
            try:
                if s.recv(1)==b'':closed+=1
            except (ConnectionResetError,BrokenPipeError):closed+=1
        assert closed==7,('unverified connections not reclaimed',closed)
        with socket.create_connection((args.host,502),3) as fresh:
            fresh.settimeout(2);version(fresh,51)
        emit('unverified_connections_reclaimed',closed=closed,verified_connection_survived=True,new_connection_responded=True)
    finally:
        for s in idle:s.close()
        if trusted:trusted.close()
    latencies=[]
    for index in range(100):
        start=time.monotonic();port=[502,503,504,5002][index%4]
        with socket.create_connection((args.host,port),3) as s:
            s.settimeout(2);version(s,index+100)
        latencies.append((time.monotonic()-start)*1000)
    ordered=sorted(latencies)
    summary={'passed':True,'reclaimed_unverified':7,'churn_requests':100,'churn_errors':0,'connect_plus_read_ms':{'mean':sum(latencies)/len(latencies),'p95':ordered[94],'max':max(latencies)}}
    (args.out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');emit('summary',**summary);log.close()


if __name__=='__main__':main()
