#!/usr/bin/env python3
"""Analyze preserved full_link_stress logs without connecting to hardware."""
from __future__ import annotations
import argparse
import collections
from datetime import datetime, timedelta, timezone
import json
import math
from pathlib import Path
import statistics


def dist(values):
    if not values:
        return None
    a=sorted(values)
    return {'min':a[0],'p50':a[max(0,math.ceil(.5*len(a))-1)],
            'mean':statistics.mean(a),'p95':a[max(0,math.ceil(.95*len(a))-1)],
            'p99':a[max(0,math.ceil(.99*len(a))-1)],'max':a[-1]}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('directory',type=Path)
    args=p.parse_args();root=args.directory
    phases=json.loads((root/'phases.json').read_text())
    phases_by_name={s['phase']:s for s in phases}
    buckets=collections.defaultdict(list);clients=collections.defaultdict(list)
    health=[];special=[];connections=[];counters=[]
    for path in root.glob('*.jsonl'):
        link='tcp' if path.name.startswith('tcp-') else path.stem
        for line in path.open():
            row=json.loads(line)
            if row['event']=='health':health.append(row);continue
            if row['event']=='device_counters':counters.append(row);continue
            if row['event'] in ('connect','ready','disconnected','planned_reconnect','rssi'):
                connections.append({'link':link,**row})
            if row['event'] in ('request_ok','request_error') and row.get('kind','read')=='read':
                buckets[(row['phase'],link)].append(row)
                if link=='tcp':clients[(row['phase'],path.stem)].append(row)
            elif row['event'] in ('request_error','crc_error','framing_error','unexpected_transaction','notification_error','connection_error','health_error','fatal_identity_mismatch'):
                special.append({'link':link,**row})
    report=[]
    for phase in phases:
        name=phase['phase']
        for link in ('tcp','rs485','ble'):
            rows=buckets[(name,link)]
            if not rows:continue
            ok=[r for r in rows if r['event']=='request_ok'];bad=[r for r in rows if r['event']=='request_error']
            exposure=max(phase['duration_s'],max(r['epoch_s'] for r in rows)-phase['start_epoch_s'])
            by_count={}
            for count in (1,2,8,32,100,125):
                selected=[r for r in ok if (r['count'] if 'count' in r else int(r['key'].split(':')[-1]))==count]
                if selected:by_count[count]={'success':len(selected),'latency_ms':dist([r['latency_ms'] for r in selected])}
            r={'phase':name,'link':link,'attempts':len(rows),'success':len(ok),'errors':len(bad),
               'error_kinds':dict(collections.Counter(r['error'] for r in bad)),
               'success_pct':100*len(ok)/len(rows),'observation_window_s':exposure,
               'transactions_s':len(ok)/exposure,'useful_bytes_s':sum(r['useful_bytes'] for r in ok)/exposure,
               'latency_ms':dist([r['latency_ms'] for r in ok]),'by_count':by_count,
               'first_error':bad[0] if bad else None,'last_error':bad[-1] if bad else None}
            if link=='tcp':
                per_client={k[1]:{'ok':sum(r['event']=='request_ok' for r in v),'errors':sum(r['event']=='request_error' for r in v),'latency_ms':dist([r['latency_ms'] for r in v if r['event']=='request_ok'])} for k,v in clients.items() if k[0]==name}
                values=[x['ok'] for x in per_client.values()]
                r['per_client']=per_client
                r['jain_fairness_observed']=sum(values)**2/(len(values)*sum(x*x for x in values)) if any(values) else None
            report.append(r)
    health_stats={}
    for phase in phases:
        name=phase['phase'];samples=[r for r in health if r['phase']==name]
        if not samples:continue
        statuses=[r['status'] for r in samples];ios=[r['diagnostics']['io'] for r in samples if r['diagnostics'].get('io')]
        ages={}
        for key in ('worker_age_ms','di_age_ms','do_readback_age_ms'):
            values=[r[key] for r in ios if r.get(key) is not None]
            ages[key]={'valid_value_distribution':dist([v for v in values if v<2**31]),
                       'unsigned_wrap_samples':sum(v>=2**31 for v in values),
                       'all_samples_max':max(values) if values else None}
        perf=[r['diagnostics']['performance']['main_work'] for r in samples if r['diagnostics'].get('performance')]
        health_stats[name]={'samples':len(samples),'web_status_ms':dist([r['status_ms'] for r in samples]),
                            'web_io_ms':dist([r['io_ms'] for r in samples]),
                            'free_heap':dist([s['free_heap'] for s in statuses]),
                            'reset_counts':sorted(set(s['reset_count'] for s in statuses)),
                            'recoverable_values':sorted(set(s['recoverable'] for s in statuses)),
                            'degradable_values':sorted(set(s['degradable'] for s in statuses)),
                            'severe_values':sorted(set(s['severe'] for s in statuses)),
                            'di_invalid_samples':sum(not r['di_valid'] for r in ios),
                            'do_readback_invalid_samples':sum(not r['do_readback_valid'] for r in ios),
                            'io_ages':ages,
                            'io_counters_max':{k:max(r[k] for r in ios) for k in ('di_failures','do_failures','do_submit_busy','led_errors','loop_max_us')},
                            'main_work_peak_us':max((r['max_us'] for r in perf),default=None),
                            'main_work_largest_p99_upper_us':max((r['p99_upper_us'] for r in perf),default=None)}
    output={'generated':datetime.now(timezone(timedelta(hours=8))).isoformat(),'results':report,
            'health':health_stats,'special_errors':special,'connections':connections,
            'ble_drops_values':sorted(set(r['values'][4] for r in counters)),
            'counter_samples':len(counters),'request_errors':sum(r['errors'] for r in report),
            'successful_transactions':sum(r['success'] for r in report)}
    (root/'analysis.json').write_text(json.dumps(output,ensure_ascii=False,indent=2)+'\n')
    print('phase link success errors rps mean_ms p95_ms p99_ms max_ms useful_Bps')
    for r in report:
        d=r['latency_ms']
        print(r['phase'],r['link'],r['success'],r['errors'],f"{r['transactions_s']:.3f}",*(f"{d[k]:.3f}" for k in ['mean','p95','p99','max']),f"{r['useful_bytes_s']:.3f}")
    print('successful_transactions',output['successful_transactions'],'request_errors',output['request_errors'],
          'special_errors',len(special),'ble_drops_values',output['ble_drops_values'])


if __name__=='__main__':main()
