#!/usr/bin/env python3
"""Bench-only LoRa FC16 frame-size probe; backs up and restores unused RAM mirror.

Requires --apply. Refuses any active configuration other than the known single
IO-control record, to avoid touching application-owned analog/poll destinations.
Every successful write is checked over TCP and restored over TCP in finally.
Stops on the first error and drains late RF bytes before reporting it.
"""
import argparse,json,socket,struct,time
from pathlib import Path
import serial
from lora_rtu_acceptance import crc16,tcp_read


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--host',required=True);p.add_argument('--peer',required=True)
    p.add_argument('--apply',action='store_true');p.add_argument('--out',type=Path,required=True)
    p.add_argument('--counts',default='1,16,60,100,115,123');a=p.parse_args()
    if not a.apply:p.error('--apply is required')
    counts=list(map(int,a.counts.split(',')));assert all(1<=n<=123 for n in counts)
    table=list(struct.unpack('>8H',tcp_read(a.host,3,2300,8)[3:]));assert table[:3]==[1,3,58] and table[3]&255==1,('not the exclusively IO test configuration',table)
    a.out.mkdir(parents=True,exist_ok=True);address=4096;original=list(struct.unpack('>123H',tcp_read(a.host,3,address,123)[3:]))
    backup=a.out/'original.json';assert not backup.exists();backup.write_text(json.dumps({'address':address,'words':original},indent=2)+'\n')
    def tcp_write(words):
        pdu=struct.pack('>BHHB',16,address,len(words),len(words)*2)+struct.pack('>'+'H'*len(words),*words)
        with socket.create_connection((a.host,502),3) as s:
            s.settimeout(5);s.sendall(struct.pack('>HHHB',0x7350,0,len(pdu)+1,1)+pdu);r=b''
            while len(r)<12:
                b=s.recv(12-len(r));assert b,'TCP restoration ACK lost';r+=b
            assert r[7:]==pdu[:5],r.hex()
    results=[];s=serial.Serial(port=None,baudrate=9600,timeout=.01,write_timeout=2);s.dtr=False;s.rts=False;s.port=a.peer
    with s:
        for n in counts:
            t=time.monotonic();drained=bytearray()
            while time.monotonic()-t<2:drained.extend(s.read(512))
            payload=struct.pack('>BBHHB',1,16,address,n,n*2)+struct.pack('>'+'H'*n,*([0x5aa5]*n));tx=payload+struct.pack('<H',crc16(payload))
            prefix=payload[:6];expected=prefix+struct.pack('<H',crc16(prefix));rx=bytearray();error=None
            try:
                t=time.monotonic();s.write(tx);s.flush()
                while time.monotonic()-t<8:
                    rx.extend(s.read(512))
                    if len(rx)>=len(expected):break
                elapsed=(time.monotonic()-t)*1000
                assert bytes(rx)==expected,('RF FC16 ACK missing/mismatch',bytes(rx).hex())
                actual=list(struct.unpack('>'+'H'*n,tcp_read(a.host,3,address,n)[3:]));assert actual==[0x5aa5]*n,'acknowledged words not applied'
            except (AssertionError,OSError) as exc:error=str(exc)
            finally:
                tcp_write(original)
                restored=list(struct.unpack('>123H',tcp_read(a.host,3,address,123)[3:]));assert restored==original,'restoration mismatch'
            row={'count':n,'request_bytes':len(tx),'request':tx.hex(),'response':rx.hex(),'ms':elapsed,'passed':error is None,'error':error,'original_restored':True,'pre_request_drained':drained.hex()}
            results.append(row);print(json.dumps(row),flush=True)
            if error:
                late=bytearray();t=time.monotonic()
                while time.monotonic()-t<3:late.extend(s.read(512))
                (a.out/'late_bytes.txt').write_text(late.hex()+'\n');break
    (a.out/'summary.json').write_text(json.dumps({'passed':len(results)==len(counts) and all(r['passed'] for r in results),'results':results},indent=2)+'\n')
    assert len(results)==len(counts) and all(r['passed'] for r in results),'see retained probe; original scratch words restored'

if __name__=='__main__':main()
